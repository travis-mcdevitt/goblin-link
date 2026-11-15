//! Shared daemon state, the command runner, the chat engine and the IPC handler.
//!
//! [`DaemonState`] holds everything the IPC methods and the device-session
//! manager share. [`DaemonHandler`] implements the [`goblin_ipc`] [`Handler`] for
//! every method group. Events are published through the [`EventBus`].

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

use goblin_ipc::protocol::{Event, Method, PROTOCOL_VERSION};
use goblin_ipc::server::{reply, ClientInfo, EventBus, Handler};
use goblin_ipc::types::*;

use goblin_store::chats::{validate_chat_id, CacheLimits, ChatCache};
use goblin_store::commands::{summarize, CommandLog, ConsentStore};
use goblin_store::inbox::{Inbox, NewNotice};
use goblin_store::profiles::ProfileStore;
use goblin_store::settings::{LedMode, SettingsStore};

use crate::backend::Link;
use crate::backend::{CommandRunner, DeviceDescription, TurnEvent};

use crate::exec::{self, Executor};

/// An answer of the owner's that waits for the next session (in the profile's folder).
const OWED_REPLY: &str = "owed-reply.json";

/// Current wall-clock time in Unix milliseconds.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The mutable per-profile stores, guarded together.
pub struct Stores {
    /// Transcript and side-chat cache.
    pub chats: ChatCache,
    /// Notification inbox.
    pub inbox: Inbox,
    /// Device-command log.
    pub commands: CommandLog,
}

/// Live link status surfaced by `daemon.status` and `link.state`.
#[derive(Debug, Clone)]
pub struct LinkStatus {
    /// The connection state.
    pub state: LinkState,
    /// When the state last changed (ms).
    pub since: u64,
    /// The default VM name, when known.
    pub vm_name: Option<String>,
    /// Age of the device access token in seconds.
    pub token_age_s: Option<u64>,
    /// The last error string, if any.
    pub last_error: Option<String>,
    /// The agent's latest activity.
    pub activity: Option<Activity>,
    /// The agent's display name from `/identity`, once read.
    pub agent_name: Option<String>,
    /// Directory holding the agent's avatar files, once prepared (see `avatar.rs`).
    pub avatar_dir: Option<String>,
}

impl Default for LinkStatus {
    fn default() -> Self {
        Self {
            state: LinkState::Offline,
            since: now_millis(),
            vm_name: None,
            token_age_s: None,
            last_error: None,
            activity: None,
            agent_name: None,
            avatar_dir: None,
        }
    }
}

/// Everything the IPC handler and the session manager share.
pub struct DaemonState {
    /// Gadget version.
    pub version: String,
    /// This run's id (changes when the daemon restarts).
    pub run_id: String,
    started: Instant,
    /// The event bus.
    pub bus: Arc<EventBus>,
    /// Profile store.
    pub profiles: ProfileStore,
    /// Platform adapter, when one was detected.
    pub platform: Option<Arc<goblin_platform::Platform>>,
    settings: Mutex<SettingsStore>,
    consent: Mutex<ConsentStore>,
    active_name: RwLock<String>,
    stores: Mutex<Stores>,
    executor: RwLock<Arc<Executor>>,
    device: RwLock<DeviceDescription>,
    link: RwLock<LinkStatus>,
    agent: RwLock<Option<Link>>,
    main_unread: AtomicU64,
    /// Connection ids of the open Muse apps (UI clients). Empty means Muse is in the
    /// background.
    ui_clients: Mutex<std::collections::HashSet<u64>>,
    /// A reply arrived while no app was open: the presence LEDs blink until an app opens or
    /// the inbox is read.
    background_reply: std::sync::atomic::AtomicBool,
    /// Muse's suggested replies per chat (`chat.suggest`), and the chat of the latest turn.
    suggestions: Mutex<HashMap<String, Vec<String>>>,
    last_turn_chat: Mutex<String>,
    /// Where each profile's own folder lives (`<homes>/<profile>`), once known.
    homes: RwLock<Option<std::path::PathBuf>>,
    /// Where what Muse built is kept (`<built>/<profile>`), when this device keeps any.
    built: RwLock<Option<std::path::PathBuf>>,
    /// Changes to settings Muse has asked for, waiting for the owner's answer.
    asks: Mutex<Vec<crate::asks::Ask>>,
    /// Questions the agent has put to the owner as cards, waiting for an answer.
    prompts: Mutex<Vec<AgentPrompt>>,
    profile_changes: Mutex<()>,
    /// When the owner last said no to changing each setting (Unix seconds).
    declined: Mutex<HashMap<String, u64>>,
    /// The pairing window that is open, if one is.
    pair: Mutex<Option<crate::pairing::PairHandle>>,
    /// The call in progress: its id and the channel to control it.
    call: Mutex<Option<(String, tokio::sync::mpsc::Sender<crate::call::CallCmd>)>>,
    /// The voice note being recorded, with its id.
    recording: Mutex<Option<(String, crate::voice::Recording)>>,
    /// The note that reached its longest and was sent without waiting for its button: the
    /// release that follows has nothing to stop, and is not an error.
    capped_note: Mutex<Option<String>>,
    /// Plays spoken replies (one at a time, see `speak_lock`).
    speaker: Arc<crate::voice::Speaker>,
    speak_lock: Arc<tokio::sync::Mutex<()>>,
    /// Voice-note turns whose reply should be spoken, with the newest assistant message id
    /// in that chat before the note (so an older reply is never spoken by mistake).
    voice_turns: Mutex<HashMap<String, Option<String>>>,
    /// The revision of the agent's avatar asked for last; an older one still being fetched
    /// gives way to it.
    wanted_avatar: Mutex<Option<String>>,
    /// Held while the avatar's files are fetched, written or removed.
    avatar_lock: tokio::sync::Mutex<()>,
    /// Presence LEDs: the long-lived helper (it saves the firmware's LED settings on first use
    /// and restores them on release) and the `leds.mode` it was built for.
    presence: Mutex<Option<(PresenceLights, LightsBuiltFor)>>,
    turns: Mutex<HashMap<String, TurnRecord>>,
    /// Shared invoke concurrency limiter (max 4).
    pub invoke_slots: Arc<Semaphore>,
    shutdown: tokio::sync::Notify,
    restart_flag: std::sync::atomic::AtomicBool,
    /// The last connection attempt found Muse's side not answering (see `service`).
    pub(crate) unanswered: std::sync::atomic::AtomicBool,
    shutdown_flag: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone)]
struct TurnRecord {
    chat: String,
    state: TurnState,
}

impl DaemonState {
    /// Monotonic seconds since start (the clock the turn logic uses).
    pub fn now_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Seconds the daemon has been running.
    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// The active profile name.
    pub fn active(&self) -> String {
        self.active_name.read().unwrap().clone()
    }

    /// The current executor.
    pub fn executor(&self) -> Arc<Executor> {
        self.executor.read().unwrap().clone()
    }

    /// The device description for `link.register`.
    pub fn device(&self) -> DeviceDescription {
        self.device.read().unwrap().clone()
    }

    /// Consent toggles.
    pub fn consent(&self) -> Consent {
        self.consent.lock().unwrap().get()
    }

    /// Runs a closure with the stores locked.
    pub fn with_stores<R>(&self, f: impl FnOnce(&mut Stores) -> R) -> R {
        f(&mut self.stores.lock().unwrap())
    }

    /// Replaces the live link to the agent (`None` when it is gone).
    pub(crate) fn set_link(&self, link: Option<Link>) {
        let gone = link.is_none();
        *self.agent.write().unwrap() = link;
        // A question belongs to the connection it came over: nobody is left to hear the answer.
        if gone && !std::mem::take(&mut *self.prompts.lock().unwrap()).is_empty() {
            self.requests_changed();
        }
    }

    /// The live link to the agent, if connected.
    fn link(&self) -> Option<Link> {
        self.agent.read().unwrap().clone()
    }

    /// When the daemon started: the zero of [`DaemonState::now_secs`].
    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    /// Updates and publishes the link state.
    pub fn set_link_state(&self, state: LinkState, error: Option<String>) {
        let since = now_millis();
        {
            let mut link = self.link.write().unwrap();
            link.state = state;
            link.since = since;
            if error.is_some() {
                link.last_error = error.clone();
            } else if state == LinkState::Online {
                // Whatever went wrong on the way is over.
                link.last_error = None;
            }
        }
        let _ = self.bus.publish(&Event::LinkState(LinkStateEvent {
            state,
            since,
            error,
        }));
        self.update_presence();
    }

    /// Records the VM name and token age for `daemon.status`.
    pub fn set_vm(&self, name: Option<String>, token_age_s: Option<u64>) {
        let mut link = self.link.write().unwrap();
        link.vm_name = name;
        link.token_age_s = token_age_s;
    }

    /// Reads `/identity` and records the agent's display name for `daemon.status`.
    /// Best effort: the name stays "Muse" when the route is unavailable.
    pub async fn refresh_identity(self: &Arc<Self>) {
        let Some(link) = self.link() else {
            return;
        };
        let Some((name, avatar)) = link.identity().await else {
            return;
        };
        match avatar {
            Some(info) => self.sync_avatar(info),
            // A Hermes says all of who it is each time: naming no picture, it has none.
            None if link.says_its_whole_identity() => self.drop_avatar(),
            None => {}
        }
        if let Some(name) = name {
            let state = {
                let mut link = self.link.write().unwrap();
                link.agent_name = Some(name);
                link.state
            };
            // Clients refresh `daemon.status` on link events; republish so a client that
            // connected before identity arrived picks up the agent's name.
            self.set_link_state(state, None);
        }
    }

    /// Publishes an agent activity update.
    pub fn set_activity(&self, activity: Option<Activity>) {
        self.link.write().unwrap().activity = activity.clone();
        if let Some(activity) = activity {
            let _ = self.bus.publish(&Event::AgentActivity(activity));
        }
        self.update_presence();
    }

    fn link_status(&self) -> LinkStatus {
        self.link.read().unwrap().clone()
    }

    /// Drives presence LEDs from the link, Muse's activity and the inbox (no-op without a
    /// platform with LEDs). Online: working while a turn runs, attention while the inbox has
    /// unseen notices, otherwise connected (off, or dim with `presence_dim`). `leds.mode = off` or
    /// `leds.never_touch` hands the LEDs back to the firmware.
    pub fn update_presence(&self) {
        use goblin_platform::{ConnectedStyle, PresenceOptions, PresenceState};
        let Some(platform) = &self.platform else {
            return;
        };
        let (mode, never_touch, colour) = {
            let settings = self.settings.lock().unwrap();
            let s = settings.settings();
            (s.leds_mode, s.leds_never_touch, s.chat_theme)
        };
        let built_for = (mode, colour);
        let mut slot = self.presence.lock().unwrap();
        if mode == LedMode::Off || never_touch {
            if let Some((mut p, _)) = slot.take() {
                let _ = p.release();
            }
            return;
        }
        let link = self.link.read().unwrap().state;
        let state = match link {
            LinkState::Unpaired => PresenceState::NotPaired,
            // No light while Muse works (most turns are quick); blink only for a reply that
            // arrived while no app was open.
            LinkState::Online => {
                if self.background_reply.load(Ordering::Relaxed) {
                    PresenceState::Attention
                } else {
                    PresenceState::Connected
                }
            }
            LinkState::AuthFailed => PresenceState::Error,
            _ => PresenceState::Reconnecting,
        };
        // Rebuild when the mode changed (it sets the connected style) or the colour did.
        if slot.as_ref().is_some_and(|(_, m)| *m != built_for) {
            if let Some((mut p, _)) = slot.take() {
                let _ = p.release();
            }
        }
        if slot.is_none() {
            let Some(cfg) = platform.config().leds.as_ref() else {
                return;
            };
            let Ok(leds) = platform.leds() else {
                return;
            };
            let options = PresenceOptions {
                connected: if mode == LedMode::PresenceDim {
                    ConnectedStyle::Dim
                } else {
                    ConnectedStyle::Off
                },
                theme: light_colour(colour),
                ..PresenceOptions::default()
            };
            let zones = cfg.presence_zones.clone();
            let presence = goblin_platform::PresenceLeds::new(leds, zones, options)
                .with_accent(cfg.accent_zones.clone());
            *slot = Some((presence, built_for));
        }
        let Some((p, _)) = slot.as_mut() else {
            return;
        };
        let first = !p.active();
        // Best effort: LED failures never affect the daemon.
        match p.set(state) {
            Ok(()) => {
                if first && p.active() {
                    // The firmware's ceiling is low (NextUI uses 5 of 60); raise it while we
                    // signal. The snapshot taken by `set` restores it on release.
                    let _ = p.leds().set_max_brightness(PRESENCE_BRIGHTNESS);
                }
                log::debug!("presence: {}", state.name());
            }
            Err(e) => log::info!("presence LEDs: {e}"),
        }
    }

    /// Whether replies to voice notes are spoken (`voice.speak_replies` other than `never`).
    fn voice_to_voice(&self) -> bool {
        self.settings.lock().unwrap().settings().speak_replies
            != goblin_store::settings::SpeakReplies::Never
    }

    /// Replaces a voice note's placeholder text with Muse's transcript once Muse's history has
    /// it (polled for up to ~20 s).
    fn fill_transcript(self: &Arc<Self>, chat: String, message_id: String) {
        let state = self.clone();
        tokio::spawn(async move {
            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let Ok(history) = state.fetch_history(&chat, 10).await else {
                    continue;
                };
                let Some(found) = history
                    .into_iter()
                    .find(|h| h.message.id == message_id && h.message.role == Role::User)
                else {
                    continue;
                };
                let mut message = found.message;
                message.origin = Origin::Gadget;
                log::info!("voice note transcript received");
                state.with_stores(|s| {
                    let _ = s.chats.upsert(message.clone());
                    let _ = s.chats.flush();
                });
                let _ = state
                    .bus
                    .publish(&Event::ChatMessage(ChatMessageEvent { message }));
                return;
            }
            log::info!("no transcript for voice note {message_id}");
        });
    }

    /// An app connected: Muse is in the foreground, so a waiting-reply signal is over.
    pub fn ui_opened(&self, id: u64) {
        self.ui_clients.lock().unwrap().insert(id);
        self.background_reply.store(false, Ordering::Relaxed);
        self.update_presence();
    }

    /// An app disconnected.
    pub fn ui_closed(&self, id: u64) {
        let mut clients = self.ui_clients.lock().unwrap();
        if clients.remove(&id) && clients.is_empty() {
            if let Some((_, tx)) = self.call.lock().unwrap().as_ref() {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(crate::call::CallCmd::End).await;
                });
            }
            self.recording.lock().unwrap().take();
            self.speaker.stop();
        }
    }

    /// Whether `note` is still being recorded. One that has ended by itself is put away
    /// here: a note that reached the longest a note may be is sent as it is (the owner was
    /// still speaking; a minute of speech is not thrown away), and one whose capture failed
    /// is reported as failed.
    fn recording_active(self: &Arc<Self>, note: &str) -> bool {
        // The lock is let go before the recording is put away, which takes it again.
        let running = {
            let mut slot = self.recording.lock().unwrap();
            match slot.as_mut() {
                Some((id, rec)) if id == note => Some(!rec.finished()),
                _ => None,
            }
        };
        match running {
            Some(true) => true,
            Some(false) => {
                self.put_away_recording(note);
                false
            }
            None => false,
        }
    }

    /// Puts away a recording that has ended by itself, and returns what was said of it
    /// first: `Sending` for a note at its longest, `Failed` for a capture that broke.
    fn put_away_recording(self: &Arc<Self>, note: &str) -> Option<NoteState> {
        let ended = {
            let mut slot = self.recording.lock().unwrap();
            let (id, rec) = slot.as_mut()?;
            if id != note || !rec.finished() {
                return None;
            }
            slot.take().map(|(id, rec)| {
                let seconds = rec.elapsed().as_secs_f64();
                let chat = rec.chat.clone();
                // Ended while the slot is still locked, so that the gain is restored before
                // another note can start. Stopping the recorder takes a moment, no more.
                let wav = if rec.at_limit() {
                    rec.finish().ok().flatten()
                } else {
                    drop(rec);
                    None
                };
                if wav.is_some() {
                    // Said while the slot is still locked: a release that finds the slot
                    // empty then always finds this, and is not told "no such recording".
                    *self.capped_note.lock().unwrap() = Some(id.clone());
                }
                (id, seconds, chat, wav)
            })
        };
        let (id, seconds, chat, wav) = ended?;
        let say = move |state: &Self, note: &str, phase| {
            let _ = state.bus.publish(&Event::VoiceNote(VoiceNoteEvent {
                note: note.to_owned(),
                state: phase,
                seconds,
            }));
        };
        match wav {
            Some(wav) => {
                say(self, &id, NoteState::Sending);
                log::info!("voice note {id}: as long as a note may be; sending it");
                let state = self.clone();
                tokio::spawn(async move {
                    let secs = (seconds as u64).max(1);
                    let phase = match state.voice_note_send(&chat, wav, secs).await {
                        Ok(_) => NoteState::Sent,
                        Err(e) => {
                            log::warn!("voice note {id} was not sent: {}", e.message);
                            NoteState::Failed
                        }
                    };
                    say(&state, &id, phase);
                });
                Some(NoteState::Sending)
            }
            None => {
                say(self, &id, NoteState::Failed);
                Some(NoteState::Failed)
            }
        }
    }

    /// Whether no app is open.
    pub fn in_background(&self) -> bool {
        self.ui_clients.lock().unwrap().is_empty()
    }

    /// `handheld.suggest`: the agent's reply buttons for the chat of the latest turn.
    fn suggest(&self, params: &Map<String, Value>) -> Result<Value, String> {
        let replies: Vec<String> = params
            .get("replies")
            .and_then(Value::as_array)
            .ok_or("replies is a list of strings")?
            .iter()
            .map(|r| {
                r.as_str()
                    .map(str::to_owned)
                    .ok_or("replies is a list of strings")
            })
            .collect::<Result<_, _>>()?;
        let replies = check_suggestions(&replies)?;
        let chat = self.last_turn_chat.lock().unwrap().clone();
        let shown = replies.len();
        self.set_suggestions(&chat, replies);
        Ok(json!({"shown": shown}))
    }

    /// The text of a stored message, wherever it is.
    fn message_text(&self, id: &str) -> Option<String> {
        self.with_stores(|s| {
            let mut chats = vec!["main".to_owned()];
            chats.extend(s.chats.side_chats().iter().map(|c| c.id.clone()));
            chats.iter().find_map(|chat| {
                let found = s.chats.messages(chat).ok()?.iter().find(|m| m.id == id)?;
                Some(found.text.clone())
            })
        })
    }

    /// Puts what the agent heard in a voice note in place of the note's placeholder text.
    /// Only that: a message that says anything but `placeholder` (what the owner typed, or
    /// what was already heard) is the record of what the owner said, and is left alone.
    fn voice_note_heard(&self, chat: &str, message_id: &str, placeholder: &str, heard: String) {
        if heard.trim().is_empty() {
            return;
        }
        let message = self.with_stores(|s| {
            let mut message = s
                .chats
                .messages(chat)
                .ok()?
                .iter()
                .find(|m| m.id == message_id && m.role == Role::User && m.text == placeholder)?
                .clone();
            message.text = heard;
            let _ = s.chats.upsert(message.clone());
            let _ = s.chats.flush();
            Some(message)
        });
        if let Some(message) = message {
            let _ = self
                .bus
                .publish(&Event::ChatMessage(ChatMessageEvent { message }));
        }
    }

    /// Stores (or with an empty list clears) the suggested replies for `chat` and tells clients.
    pub fn set_suggestions(&self, chat: &str, replies: Vec<String>) {
        let changed = {
            let mut map = self.suggestions.lock().unwrap();
            let old = map.get(chat).cloned().unwrap_or_default();
            if replies.is_empty() {
                map.remove(chat);
            } else {
                map.insert(chat.to_owned(), replies.clone());
            }
            old != replies
        };
        if changed {
            let _ = self
                .bus
                .publish(&Event::ChatSuggestions(ChatSuggestionsEvent {
                    chat: chat.to_owned(),
                    replies,
                }));
        }
    }

    /// Stops showing the agent's own picture, and removes it: the agent has none now.
    fn drop_avatar(self: &Arc<Self>) {
        *self.wanted_avatar.lock().unwrap() = None;
        // Decided in turn with the fetches, so that one under way cannot leave its files.
        let state = self.clone();
        tokio::spawn(async move {
            let _one = state.avatar_lock.lock().await;
            // It chose a picture again while this waited: that one's task shows it.
            if state.wanted_avatar.lock().unwrap().is_some() {
                return;
            }
            if let Ok(profile) = state.profiles.profile(&state.active()) {
                crate::avatar::prune(&profile.dir().join("avatar"), "");
            }
            let (was_shown, link_state) = {
                let mut link = state.link.write().unwrap();
                (link.avatar_dir.take().is_some(), link.state)
            };
            if was_shown {
                state.set_link_state(link_state, None);
            }
        });
    }

    /// Prepares the agent's avatar (image and state clips) for the UI and records where it is.
    /// A revision already on the device is used as it is. Best effort: failures are logged and
    /// the UI keeps its drawn mascot.
    fn sync_avatar(self: &Arc<Self>, info: crate::avatar::AvatarInfo) {
        let state = self.clone();
        *self.wanted_avatar.lock().unwrap() = Some(info.revision.clone());
        tokio::spawn(async move {
            // One at a time: pictures named in quick succession are not all fetched and
            // decoded at once, and two tasks never write the same revision's files. One
            // that is no longer wanted by the time its turn comes is not fetched at all.
            let _one = state.avatar_lock.lock().await;
            let wanted = |state: &Self| {
                state.wanted_avatar.lock().unwrap().as_deref() == Some(info.revision.as_str())
            };
            if !wanted(&state) {
                return;
            }
            let Ok(profile) = state.profiles.profile(&state.active()) else {
                return;
            };
            let root = profile.dir().join("avatar");
            let dir = crate::avatar::revision_dir(&root, &info.revision);
            if !crate::avatar::ready(&dir) {
                if std::fs::create_dir_all(&dir).is_err() {
                    return;
                }
                let Some(link) = state.link() else {
                    return;
                };
                if let Some(path) = &info.image {
                    match link.file(path, crate::avatar::MAX_BYTES).await {
                        Ok(data) => {
                            let d = dir.clone();
                            let stored = tokio::task::spawn_blocking(move || {
                                crate::avatar::store_image(&data, &d)
                            })
                            .await;
                            if let Ok(Err(e)) = stored {
                                log::info!("{e}");
                            }
                        }
                        Err(e) => log::info!("could not fetch the avatar image: {e}"),
                    }
                }
                let mut frames = std::collections::BTreeMap::new();
                for (name, path) in &info.clips {
                    match link.file(path, crate::avatar::MAX_BYTES).await {
                        Ok(data) => {
                            let (d, n) = (dir.clone(), name.clone());
                            let stored = tokio::task::spawn_blocking(move || {
                                crate::avatar::store_clip(&data, &d, &n)
                            })
                            .await;
                            match stored {
                                Ok(Ok(count)) => {
                                    frames.insert(name.clone(), count);
                                }
                                Ok(Err(e)) => log::info!("{e}"),
                                Err(e) => log::info!("avatar task failed: {e}"),
                            }
                        }
                        Err(e) => log::info!("could not fetch the avatar's {name} clip: {e}"),
                    }
                }
                if !dir.join("avatar.png").exists() && frames.is_empty() {
                    // Nothing usable: no empty folder is left for it.
                    let _ = std::fs::remove_dir_all(&dir);
                    return;
                }
                if let Err(e) = crate::avatar::write_meta(&dir, &frames) {
                    log::info!("{e}");
                    return;
                }
                log::info!("avatar {} ready ({} clip(s))", info.revision, frames.len());
            }
            // The agent may have changed its picture again while this one was fetched: the
            // newer one's own task shows it, and this one must not put the older back.
            if !wanted(&state) {
                return;
            }
            crate::avatar::prune(&root, &info.revision);
            let link_state = {
                let mut link = state.link.write().unwrap();
                link.avatar_dir = Some(dir.to_string_lossy().into_owned());
                link.state
            };
            // Clients re-read daemon.status on link events and pick up the avatar.
            state.set_link_state(link_state, None);
        });
    }

    /// Where the active profile keeps downloaded media.
    fn media_dir(&self) -> Option<std::path::PathBuf> {
        let profile = self.profiles.profile(&self.active()).ok()?;
        let dir = profile.dir().join("media");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// Brings the media of `chat`'s recent messages onto the device: reads Muse's history,
    /// stores each image or clip it has not fetched yet, and publishes the messages with their
    /// local files. Best effort; failures are logged and the message keeps its placeholder.
    pub fn sync_media(self: &Arc<Self>, chat: String) {
        let state = self.clone();
        tokio::spawn(async move {
            let Some(dir) = state.media_dir() else {
                return;
            };
            let Ok(mut history) = state.fetch_history(&chat, 20).await else {
                return;
            };
            let anchors = media_anchors(&mut history);
            // Reactions can change long after a message arrived: bring those of the recent
            // messages this device already has in line with Muse's history.
            let mut reacted = Vec::new();
            state.with_stores(|s| {
                let Ok(cached) = s.chats.messages(&chat) else {
                    return;
                };
                for h in history.iter().filter(|h| h.media.is_empty()) {
                    if let Some(m) = cached.iter().find(|m| m.id == h.message.id) {
                        if m.reaction != h.message.reaction
                            || m.choices != h.message.choices
                            || ((m.text.contains("Widget — open Muse")
                                || m.text.contains("[[hatch_widget:"))
                                && m.text != h.message.text)
                        {
                            let mut m = m.clone();
                            m.reaction = h.message.reaction.clone();
                            m.choices = h.message.choices.clone();
                            m.text = h.message.text.clone();
                            reacted.push(m);
                        }
                    }
                }
                for m in &reacted {
                    let _ = s.chats.upsert(m.clone());
                }
                if !reacted.is_empty() {
                    let _ = s.chats.flush();
                }
            });
            for message in reacted {
                let _ = state
                    .bus
                    .publish(&Event::ChatMessage(ChatMessageEvent { message }));
            }
            state.fetch_media(&chat, &dir, history, &anchors).await;
            let freed = tokio::task::spawn_blocking(move || {
                crate::media::prune(
                    &dir,
                    crate::media::MAX_STORE_BYTES,
                    crate::media::KEEP_RECENT,
                )
            })
            .await
            .unwrap_or(0);
            if freed > 0 {
                log::info!("media folder trimmed by {freed} bytes");
            }
        });
    }

    /// Fetches the media of `history`'s messages that is not on the device yet, and stores
    /// and publishes each such message with its local files. `anchors` (see
    /// [`media_anchors`]) places media under the reply it follows.
    async fn fetch_media(
        self: &Arc<Self>,
        chat: &str,
        dir: &std::path::Path,
        history: Vec<HistoryMessage>,
        anchors: &HashMap<String, (String, u64)>,
    ) {
        let state = self;
        for h in history.into_iter().filter(|h| !h.media.is_empty()) {
            let Some(link) = state.link() else {
                return;
            };
            let mut message = h.message;
            // This device stamps a reply with the time it arrived, which can be later than
            // Muse's time for the media that follows it. Time the media just after the
            // stored reply, so it sits under it.
            if let Some((anchor, n)) = anchors.get(&message.id) {
                let anchored = state.with_stores(|s| {
                    s.chats
                        .messages(chat)
                        .ok()
                        .and_then(|m| m.iter().find(|x| &x.id == anchor).map(|x| x.at))
                });
                if let Some(at) = anchored {
                    message.at = at + n;
                }
            }
            let mut changed = false;
            for r in &h.media {
                let (picture, clip) = crate::media::local_files(dir, r);
                let have = match &clip {
                    Some(c) => c.exists(),
                    None => picture.exists(),
                };
                if have {
                    // Still in use: keeps it out of the way of the folder's size cap.
                    crate::media::touch(&picture);
                    if let Some(c) = &clip {
                        crate::media::touch(c);
                    }
                    // A clip whose poster could not be made earlier: try once more.
                    if let (Some(c), false) = (clip.clone(), picture.exists()) {
                        let p = picture.clone();
                        let made =
                            tokio::task::spawn_blocking(move || crate::media::make_poster(&c, &p))
                                .await;
                        changed |= matches!(made, Ok(Ok(())));
                    }
                    continue;
                }
                let video = r.kind == AttachmentKind::Video;
                let max = if video {
                    crate::media::MAX_VIDEO_BYTES
                } else {
                    crate::media::MAX_IMAGE_BYTES
                };
                if r.bytes.is_some_and(|b| b as usize > max) {
                    log::info!("media too large to fetch ({} bytes)", r.bytes.unwrap_or(0));
                    continue;
                }
                // Another task may be fetching this very file (the chat was opened twice).
                let Some(_claim) = crate::media::claim(&picture) else {
                    continue;
                };
                let data = match link.file(&r.path, max).await {
                    Ok(d) => d,
                    Err(e) => {
                        log::info!("could not fetch media: {e}");
                        continue;
                    }
                };
                let bytes = data.len();
                let stored = tokio::task::spawn_blocking(move || match clip {
                    Some(c) => crate::media::store_video(&data, &c, &picture),
                    None => crate::media::store_image(&data, &picture),
                })
                .await;
                match stored {
                    Ok(Ok(())) => {
                        log::info!(
                            "stored {} ({bytes} bytes)",
                            if video { "a clip" } else { "an image" }
                        );
                        changed = true;
                    }
                    Ok(Err(e)) => log::info!("media not stored: {e}"),
                    Err(e) => log::info!("media task failed: {e}"),
                }
            }
            message.attachments = Some(
                h.media
                    .iter()
                    .map(|r| crate::media::attachment(dir, r))
                    .collect(),
            );
            let known = state.with_stores(|s| {
                s.chats
                    .messages(chat)
                    .map(|m| {
                        m.iter()
                            .any(|x| x.id == message.id && x.attachments == message.attachments)
                    })
                    .unwrap_or(false)
            });
            if !changed && known {
                // Nothing new: still make sure it sits at its place in time.
                state.with_stores(|s| {
                    let _ = s.chats.upsert_by_time(message.clone());
                    let _ = s.chats.flush();
                });
            }
            if changed || !known {
                state.with_stores(|s| {
                    let _ = s.chats.upsert_by_time(message.clone());
                    let _ = s.chats.flush();
                });
                let _ = state
                    .bus
                    .publish(&Event::ChatMessage(ChatMessageEvent { message }));
            }
        }
    }

    /// Fetches, in the background, the media of `messages` (a page the UI asked for) that is
    /// not on the device: older messages' media, and files the size cap dropped.
    pub fn ensure_media(self: &Arc<Self>, chat: &str, messages: &[Message]) {
        let Some(dir) = self.media_dir() else {
            return;
        };
        let wanted: Vec<HistoryMessage> = messages
            .iter()
            .filter_map(|m| {
                let media = crate::media::missing(&dir, m.attachments.as_deref()?);
                (!media.is_empty()).then(|| HistoryMessage {
                    message: m.clone(),
                    seq: 0,
                    parent: None,
                    media,
                })
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        let (state, chat) = (self.clone(), chat.to_owned());
        tokio::spawn(async move {
            state
                .fetch_media(&chat, &dir, wanted, &HashMap::new())
                .await;
        });
    }

    /// Id of the newest assistant message stored for `chat`.
    fn newest_assistant(&self, chat: &str) -> Option<String> {
        self.with_stores(|s| {
            s.chats.messages(chat).ok().and_then(|m| {
                m.iter()
                    .rev()
                    .find(|m| m.role == Role::Assistant)
                    .map(|m| m.id.clone())
            })
        })
    }

    /// Fetches the reply's speech and plays it (one reply at a time). Best effort: failures
    /// are logged and never affect the chat.
    pub fn speak(self: &Arc<Self>, message_id: String) {
        let generation = {
            let call = self.call.lock().unwrap();
            let recording = self.recording.lock().unwrap();
            if call.is_some() || recording.is_some() {
                return;
            }
            self.speaker.generation()
        };
        let state = self.clone();
        tokio::spawn(async move {
            let Some(platform) = state.platform.clone() else {
                return;
            };
            let Some(link) = state.link() else {
                return;
            };
            let _turn = state.speak_lock.clone().lock_owned().await;
            if generation != state.speaker.generation() {
                return;
            }
            let wanted = || generation == state.speaker.generation();
            let text = state.message_text(&message_id);
            let mp3 = match link.speech(&message_id, text.as_deref(), &wanted).await {
                Ok(Some(mp3)) => mp3,
                Ok(None) => return,
                Err(e) => {
                    log::info!("no speech for {message_id}: {e}");
                    return;
                }
            };
            if mp3.is_empty() {
                log::info!("empty speech for {message_id}");
                return;
            }
            // Follow the system volume; at 0 (or the mute switch) there is nothing to hear.
            let volume = goblin_platform::sysvol::read_nextui();
            if volume.is_some_and(|v| !v.audible()) {
                log::info!("not speaking reply {message_id}: system volume is 0");
                return;
            }
            let speaker = state.speaker.clone();
            let _ = state.bus.publish(&Event::VoiceSpeaking(VoiceSpeakingEvent {
                message_id: Some(message_id.clone()),
                state: SpeakingState::Start,
            }));
            log::info!(
                "speaking reply ({} bytes of MP3) at system volume {}",
                mp3.len(),
                volume.map_or("unknown".into(), |v| format!("{}%", v.percent))
            );
            let played =
                tokio::task::spawn_blocking(move || speaker.play_mp3(&platform, &mp3, generation))
                    .await;
            let _ = state.bus.publish(&Event::VoiceSpeaking(VoiceSpeakingEvent {
                message_id: Some(message_id.clone()),
                state: SpeakingState::Stop,
            }));
            match played {
                Ok(Ok(())) => log::info!("spoke reply {message_id}"),
                Ok(Err(e)) => log::info!("could not speak reply: {e}"),
                Err(e) => log::info!("speech task failed: {e}"),
            }
        });
    }

    /// Restores the firmware's own LED settings (at shutdown).
    pub fn release_presence(&self) {
        if let Some((mut p, _)) = self.presence.lock().unwrap().take() {
            let _ = p.release();
        }
    }

    /// A short vibration for a new reply, if `haptics.enabled`. Runs off the async threads
    /// because the rumble hook sleeps for the pulse.
    pub fn reply_haptic(self: &Arc<Self>) {
        let enabled = self.settings.lock().unwrap().settings().haptics;
        let Some(platform) = self.platform.clone() else {
            return;
        };
        if !enabled || platform.config().rumble.is_none() {
            return;
        }
        tokio::task::spawn_blocking(move || {
            if let Err(e) = platform.rumble(REPLY_RUMBLE_MS, REPLY_RUMBLE_STRENGTH) {
                log::info!("reply rumble: {e}");
            }
        });
    }

    fn status(&self) -> Status {
        let link = self.link_status();
        let name = self.active();
        let profile = self.profiles.profile(&name).ok().map(|p| p.summary());
        let paired = profile.as_ref().is_some_and(|p| p.paired);
        // Until the agent gives its name, it is called by its kind.
        let kind = match profile.and_then(|p| p.agent) {
            Some("hermes") => "Hermes",
            _ => "Muse",
        };
        let battery = self
            .platform
            .as_ref()
            .and_then(|p| p.battery().ok())
            .and_then(|b| {
                b.percent.map(|percent| Battery {
                    percent,
                    charging: b.charging,
                })
            });
        Status {
            profile: name,
            paired,
            link: link.state,
            agent: AgentInfo {
                name: link.agent_name.clone().unwrap_or_else(|| kind.into()),
                avatar_path: link.avatar_dir.clone(),
                activity: link.activity,
            },
            vm: VmInfo { name: link.vm_name },
            token_age_s: link.token_age_s,
            last_error: link.last_error,
            battery,
            uptime_s: self.uptime_s(),
            version: self.version.clone(),
        }
    }

    fn record_turn(&self, turn: &str, chat: &str, state: TurnState) {
        *self.last_turn_chat.lock().unwrap() = chat.to_owned();
        self.turns.lock().unwrap().insert(
            turn.to_owned(),
            TurnRecord {
                chat: chat.to_owned(),
                state,
            },
        );
        let _ = self.bus.publish(&Event::ChatTurn(ChatTurnEvent {
            turn: turn.to_owned(),
            chat: chat.to_owned(),
            state,
            activity: None,
        }));
    }

    fn pending_turns(&self, chat: &str) -> u32 {
        self.turns
            .lock()
            .unwrap()
            .values()
            .filter(|t| {
                t.chat == chat
                    && !matches!(
                        t.state,
                        TurnState::Done | TurnState::Failed | TurnState::Uncertain
                    )
            })
            .count() as u32
    }
}

/// Builds the daemon state for `active` profile.
#[allow(clippy::too_many_arguments)]
pub fn build_state(
    version: String,
    bus: Arc<EventBus>,
    profiles: ProfileStore,
    platform: Option<Arc<goblin_platform::Platform>>,
    settings: SettingsStore,
    consent: ConsentStore,
    active: String,
    stores: Stores,
    executor: Arc<Executor>,
    device: DeviceDescription,
) -> Arc<DaemonState> {
    Arc::new(DaemonState {
        run_id: bus.run_id().to_owned(),
        version,
        started: Instant::now(),
        bus,
        profiles,
        platform,
        settings: Mutex::new(settings),
        consent: Mutex::new(consent),
        active_name: RwLock::new(active),
        stores: Mutex::new(stores),
        executor: RwLock::new(executor),
        device: RwLock::new(device),
        link: RwLock::new(LinkStatus::default()),
        agent: RwLock::new(None),
        main_unread: AtomicU64::new(0),
        ui_clients: Mutex::new(std::collections::HashSet::new()),
        background_reply: std::sync::atomic::AtomicBool::new(false),
        suggestions: Mutex::new(HashMap::new()),
        last_turn_chat: Mutex::new("main".into()),
        homes: RwLock::new(None),
        built: RwLock::new(None),
        asks: Mutex::new(Vec::new()),
        prompts: Mutex::new(Vec::new()),
        profile_changes: Mutex::new(()),
        declined: Mutex::new(HashMap::new()),
        pair: Mutex::new(None),
        call: Mutex::new(None),
        recording: Mutex::new(None),
        capped_note: Mutex::new(None),
        speaker: Arc::new(crate::voice::Speaker::default()),
        speak_lock: Arc::new(tokio::sync::Mutex::new(())),
        voice_turns: Mutex::new(HashMap::new()),
        wanted_avatar: Mutex::new(None),
        avatar_lock: tokio::sync::Mutex::new(()),
        presence: Mutex::new(None),
        turns: Mutex::new(HashMap::new()),
        invoke_slots: Arc::new(Semaphore::new(4)),
        shutdown: tokio::sync::Notify::new(),
        restart_flag: std::sync::atomic::AtomicBool::new(false),
        unanswered: std::sync::atomic::AtomicBool::new(false),
        shutdown_flag: std::sync::atomic::AtomicBool::new(false),
    })
}

impl DaemonState {
    /// Resolves once `daemon.shutdown` has been requested.
    pub async fn wait_shutdown(&self) {
        use std::sync::atomic::Ordering::Acquire;
        if self.shutdown_flag.load(Acquire) {
            return;
        }
        loop {
            let notified = self.shutdown.notified();
            if self.shutdown_flag.load(Acquire) {
                return;
            }
            notified.await;
            if self.shutdown_flag.load(Acquire) {
                return;
            }
        }
    }

    /// Whether shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown_flag
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Tells the daemon where the profiles' folders are, so a rename can take the folder along.
    pub fn set_homes_dir(&self, dir: std::path::PathBuf) {
        *self.homes.write().unwrap() = Some(dir);
    }

    /// Tells the daemon where installed builds are kept, and offers the profile's
    /// installed tools to Muse.
    pub fn set_built_dir(&self, dir: std::path::PathBuf) {
        *self.built.write().unwrap() = Some(dir);
        if let Some(builds) = self.builds() {
            self.executor().set_built(builds.tools());
            self.device.write().unwrap().commands = self.executor().specs();
        }
    }

    /// The store of what the profile being served has built.
    pub fn builds(&self) -> Option<crate::builds::Builds> {
        let dir = self.built.read().unwrap().clone()?;
        Some(crate::builds::Builds::new(dir.join(self.active())))
    }

    /// What the profile being served has installed or is waiting on.
    pub fn builds_list(&self) -> BuildsListResult {
        BuildsListResult {
            items: self.builds().map(|b| b.list()).unwrap_or_default(),
        }
    }

    /// Whether another profile already has a launcher entry of that kind and title.
    fn build_entry_taken(&self, kind: BuildKind, title: &str) -> bool {
        let Some(dir) = self.built.read().unwrap().clone() else {
            return false;
        };
        let me = self.active();
        std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy() != me)
            .any(|e| crate::builds::Builds::new(e.path()).has_entry(kind, title))
    }

    fn launcher(&self) -> Option<&dyn crate::builds::Launcher> {
        self.platform
            .as_deref()
            .map(|p| p as &dyn crate::builds::Launcher)
    }

    /// Tells the apps the list changed and, when the tools did, registers them with Muse
    /// afresh. That needs a new session, so the daemon starts again a moment later, once
    /// the answer to whoever asked has gone out.
    fn builds_changed(self: &Arc<Self>, tools_changed: bool) {
        let _ = self.bus.publish(&Event::BuildsChanged(self.builds_list()));
        self.requests_changed();
        if tools_changed {
            if let Some(builds) = self.builds() {
                self.executor().set_built(builds.tools());
                self.device.write().unwrap().commands = self.executor().specs();
            }
            let state = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                log::info!("the built tools changed: starting again to register them");
                state.request_restart();
            });
        }
    }

    // ---- settings Muse can see and ask about ---------------------------------------------

    /// A setting's value now.
    pub fn setting_value(&self, s: &crate::asks::Setting) -> String {
        use crate::asks::Store;
        match s.store {
            Store::Access(field) => crate::asks::access_value(&self.consent(), field).to_owned(),
            Store::Switch(key) | Store::Choice(key) => {
                let stored = self
                    .settings
                    .lock()
                    .unwrap()
                    .get(Some(&[key.to_owned()]))
                    .ok()
                    .and_then(|m| m.get(key).cloned());
                s.from_stored(stored.as_ref())
            }
        }
    }

    /// Sets a setting, as the owner's own change in Settings would, and tells the apps.
    fn setting_apply(&self, s: &crate::asks::Setting, value: &str) -> Result<(), String> {
        use crate::asks::Store;
        match s.store {
            Store::Access(field) => {
                let consent = self
                    .consent
                    .lock()
                    .unwrap()
                    .update(&crate::asks::access_update(field, value))
                    .map_err(|e| e.to_string())?;
                let _ = self.bus.publish(&Event::ConsentChanged(consent));
            }
            Store::Switch(key) | Store::Choice(key) => {
                let mut values = Map::new();
                values.insert(key.to_owned(), s.to_stored(value));
                let changed = self
                    .settings
                    .lock()
                    .unwrap()
                    .set(&values)
                    .map_err(|e| e.to_string())?;
                let _ = self
                    .bus
                    .publish(&Event::SettingsChanged(SettingsValues { values: changed }));
                self.update_presence();
            }
        }
        log::info!("setting {} is now {value}", s.id);
        Ok(())
    }

    fn timed_path(&self) -> std::path::PathBuf {
        self.profiles.layout().root().join("timed-settings.json")
    }

    /// The hour-long permissions. An error is never read as "none": a record that cannot
    /// be read may still hold the way back from a widened permission.
    fn timed(&self) -> Result<Vec<crate::asks::Timed>, goblin_store::StoreError> {
        Ok(goblin_store::fsutil::read_json(&self.timed_path())?.unwrap_or_default())
    }

    /// Saves the hour-long permissions. A grant is only as temporary as this record, so a
    /// failure is reported, never ignored.
    fn save_timed(&self, timed: &[crate::asks::Timed]) -> Result<(), String> {
        let path = self.timed_path();
        if timed.is_empty() {
            goblin_store::fsutil::remove_file(&path)
        } else {
            goblin_store::fsutil::write_json_atomic(&path, &timed, 0o600)
        }
        .map_err(|e| e.to_string())
    }

    /// `handheld.settings`: every setting Muse can see, as it is now.
    pub fn settings_report(&self) -> Value {
        let now = now_millis() / 1000;
        let profile = self.active();
        // Only a report: a record that cannot be read shows no hour, and is left alone.
        let timed = self.timed().unwrap_or_default();
        let settings: Vec<Value> = crate::asks::SETTINGS
            .iter()
            .map(|s| {
                let mut entry = json!({
                    "id": s.id,
                    "label": s.label,
                    "means": s.means,
                    "value": self.setting_value(s),
                    "can_be": s.choices.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
                    "you_may_ask_for": s.askable(),
                });
                if let Some(t) = timed
                    .iter()
                    .find(|t| t.setting == s.id && t.profile == profile)
                {
                    entry["for_a_while"] = json!({
                        "goes_back_to": t.revert_to,
                        "in_minutes": t.until.saturating_sub(now).div_ceil(60),
                    });
                }
                entry
            })
            .collect();
        json!({
            "settings": settings,
            "how": "Ask for a change with handheld.ask. A value that is in can_be but not in you_may_ask_for is the owner's alone to choose, in Settings on the handheld.",
        })
    }

    /// What is waiting for the owner's answer: installs and changes to settings.
    pub fn requests_list(&self) -> RequestsListResult {
        let answer = |id: &str, label: &str| RequestAnswer {
            id: id.into(),
            label: label.into(),
        };
        let mut items: Vec<(u64, RequestView)> = Vec::new();
        for b in self.builds_list().items {
            if b.state != BuildState::Pending {
                continue;
            }
            let kind = match b.kind {
                BuildKind::Tool => "tool",
                BuildKind::App => "app",
                BuildKind::Game => "game",
            };
            let size = if b.size_bytes >= 1024 * 1024 {
                format!("{:.1} MB", b.size_bytes as f32 / (1024.0 * 1024.0))
            } else {
                format!("{} KB", b.size_bytes.div_ceil(1024).max(1))
            };
            items.push((
                b.at,
                RequestView {
                    id: format!(
                        "install:{}:{}",
                        b.name,
                        b.request.as_deref().unwrap_or_default()
                    ),
                    heading: format!("Install {kind} \u{201c}{}\u{201d}?", b.title),
                    body: b.description.clone(),
                    facts: format!(
                        "Runs {}  \u{b7}  {} file{}, {size}  \u{b7}  Runs without limits",
                        b.run.join(" "),
                        b.files,
                        if b.files == 1 { "" } else { "s" }
                    ),
                    chat: b.chat.clone().unwrap_or_else(|| "main".into()),
                    answers: vec![answer("no", "No"), answer("yes", "Install")],
                },
            ));
        }
        for p in self.prompts.lock().unwrap().iter() {
            let (heading, body) = p.text.split_once('\n').unwrap_or((&p.text, ""));
            items.push((
                p.at,
                RequestView {
                    id: format!("prompt:{}", p.id),
                    heading: heading.to_owned(),
                    body: body.trim().to_owned(),
                    facts: String::new(),
                    chat: p.chat.clone(),
                    answers: p
                        .answers
                        .iter()
                        .map(|(button, _, label)| answer(button, label))
                        .collect(),
                },
            ));
        }
        for a in self.asks.lock().unwrap().iter() {
            let Some(s) = crate::asks::find(a.setting) else {
                continue;
            };
            items.push((
                a.at,
                RequestView {
                    id: format!("setting:{}", s.id),
                    heading: format!(
                        "Set \u{201c}{}\u{201d} to {}?",
                        s.label,
                        s.choice_label(&a.to)
                    ),
                    body: a.reason.clone(),
                    facts: String::new(),
                    chat: a.chat.clone(),
                    answers: {
                        let mut answers = vec![answer("no", "No"), answer("yes", "Yes")];
                        if s.widens(&a.from, &a.to) {
                            answers.push(answer("hour", "For an hour"));
                        }
                        answers
                    },
                },
            ));
        }
        items.sort_by_key(|(at, _)| *at);
        RequestsListResult {
            items: items.into_iter().map(|(_, r)| r).collect(),
        }
    }

    fn requests_changed(&self) {
        let _ = self
            .bus
            .publish(&Event::RequestsChanged(self.requests_list()));
    }

    /// Where the owner's recent noes are kept: in the profile's folder, so they last across
    /// restarts (which Muse can bring about) and follow a rename or delete.
    fn declined_path(&self) -> Option<std::path::PathBuf> {
        let profile = self.profiles.profile(&self.active()).ok()?;
        Some(profile.dir().join("declined.json"))
    }

    /// When the owner last said no to asking about `setting`, if they have.
    fn declined_at(&self, setting: &str) -> Option<u64> {
        let kept = self
            .declined_path()
            .and_then(|p| Self::read_declined(&p))
            .and_then(|m| m.get(setting).copied());
        let here = self.declined.lock().unwrap().get(setting).copied();
        let at = kept.max(here)?;
        let now = now_millis() / 1000;
        if at > now {
            // Dated after now (a clock moved back, a damaged file): from now on it counts
            // from now, so it holds a day and no longer.
            self.decline_until_tomorrow(setting, now);
            return Some(now);
        }
        Some(at)
    }

    /// The saved noes. A damaged file is said so and read as none: the next no replaces it.
    fn read_declined(path: &std::path::Path) -> Option<HashMap<String, u64>> {
        match goblin_store::fsutil::read_json(path) {
            Ok(kept) => kept,
            Err(e) => {
                log::warn!("could not read the owner's earlier noes: {e}");
                None
            }
        }
    }

    /// Records a no. Kept in memory too, so a failed save still holds until a restart. The
    /// memory's lock is held throughout, so two writers cannot lose each other's noes.
    fn decline_until_tomorrow(&self, setting: &str, now: u64) {
        let mut here = self.declined.lock().unwrap();
        here.insert(setting.to_owned(), now);
        let Some(path) = self.declined_path() else {
            return;
        };
        let mut kept = Self::read_declined(&path).unwrap_or_default();
        for at in kept.values_mut() {
            // None counts from later than now.
            *at = (*at).min(now);
        }
        kept.retain(|_, at| now - *at < crate::asks::ASK_AGAIN_AFTER_S);
        kept.insert(setting.to_owned(), now);
        if let Err(e) = goblin_store::fsutil::write_json_atomic(&path, &kept, 0o600) {
            log::warn!("could not keep the owner's no: {e}");
        }
    }

    /// `handheld.ask`: puts a change to a setting to the owner. Changes nothing itself.
    fn ask(self: &Arc<Self>, params: &Map<String, Value>) -> Result<Value, String> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
        let (setting, value, reason) = (text("setting"), text("value"), text("reason"));
        let now = now_millis() / 1000;
        let declined = self.declined_at(setting);
        let s = crate::asks::check(
            setting,
            value,
            reason,
            |s| self.setting_value(s),
            declined,
            now,
        )?;
        let chat = self.last_turn_chat.lock().unwrap().clone();
        {
            let mut asks = self.asks.lock().unwrap();
            // Asking again about the same setting replaces the earlier question.
            asks.retain(|a| a.setting != s.id);
            asks.push(crate::asks::Ask {
                setting: s.id,
                from: self.setting_value(s),
                to: value.to_owned(),
                reason: reason.trim().to_owned(),
                chat: chat.clone(),
                at: now,
            });
        }
        self.requests_changed();
        post_notice(
            self,
            NewNotice {
                at: now_millis(),
                kind: NoticeKind::Approval,
                title: format!("Change {}?", s.label),
                body: None,
                chat: Some(chat.clone()),
                link: Some(Route(format!("chat/{chat}"))),
            },
        );
        Ok(json!({
            "status": "waiting_for_owner",
            "note": "Nothing has changed. The handheld is now asking the owner in this chat, showing your reason; their answer arrives here as their next message. Wait for it.",
        }))
    }

    /// The owner's answer to a request, given on the handheld: `no`, `yes`, or `hour`.
    pub fn request_decide(self: &Arc<Self>, id: &str, answer: &str) -> Result<(), String> {
        if let Some(prompt_id) = id.strip_prefix("prompt:") {
            let prompt = {
                let mut prompts = self.prompts.lock().unwrap();
                let at = prompts
                    .iter()
                    .position(|p| p.id == prompt_id)
                    .ok_or("that question is no longer waiting")?;
                // Checked before it is taken: a button the card did not have answers nothing.
                if !prompts[at]
                    .answers
                    .iter()
                    .any(|(button, _, _)| button == answer)
                {
                    return Err("that is not one of its answers".into());
                }
                // Nor does a press with nobody linked to hear it.
                if self.link().is_none() {
                    return Err("Not connected".into());
                }
                prompts.remove(at)
            };
            let choice = prompt
                .answers
                .iter()
                .find(|(button, _, _)| button == answer)
                .map(|(_, choice, _)| choice.as_str())
                .unwrap_or("deny");
            let link = self.link().ok_or("Not connected")?;
            link.answer(&prompt.id, choice);
            self.requests_changed();
            return Ok(());
        }
        if let Some(rest) = id.strip_prefix("install:") {
            // `install:<name>:<request>`; a name has no colon.
            let (name, request) = rest.split_once(':').ok_or(crate::builds::CHANGED)?;
            return match answer {
                "yes" => self.build_decide(name, Some(request), true),
                "no" => self.build_decide(name, Some(request), false),
                _ => Err("an install is answered yes or no".into()),
            };
        }
        let _changes = self.profile_changes.lock().unwrap();
        let setting = id.strip_prefix("setting:").ok_or("no such request")?;
        let ask = {
            let mut asks = self.asks.lock().unwrap();
            let at = asks
                .iter()
                .position(|a| a.setting == setting)
                .ok_or("that request is no longer waiting")?;
            asks.remove(at)
        };
        let s = crate::asks::find(ask.setting).ok_or("no such setting")?;
        let now = now_millis() / 1000;
        let profile = self.active();
        let label = s.label;
        let choice = s.choice_label(&ask.to);
        let said = match answer {
            "no" => {
                self.decline_until_tomorrow(s.id, now);
                format!("No to \u{201c}{label}\u{201d}.")
            }
            "hour" if !s.widens(&ask.from, &ask.to) => {
                self.asks.lock().unwrap().push(ask);
                return Err("only a permission being widened can be given for an hour".into());
            }
            "yes" | "hour" => {
                let before = self.setting_value(s);
                let mut timed = match self.timed() {
                    Ok(timed) => timed,
                    Err(e) => {
                        log::warn!("could not read the hour-long permissions: {e}");
                        self.asks.lock().unwrap().push(ask);
                        self.requests_changed();
                        return Err(
                            "could not read the hour-long permissions, so nothing changed".into(),
                        );
                    }
                };
                let original = timed.clone();
                // An earlier "for a while" on this setting is over either way.
                let earlier = timed
                    .iter()
                    .position(|t| t.setting == s.id && t.profile == profile)
                    .map(|i| timed.remove(i));
                if answer == "hour" {
                    timed.push(crate::asks::Timed {
                        profile,
                        setting: s.id.to_owned(),
                        value: ask.to.clone(),
                        // What it was before anything temporary began.
                        revert_to: earlier.map_or(before, |e| e.revert_to),
                        until: now + crate::asks::TEMPORARY_S,
                    });
                }
                if answer == "hour" {
                    // Record the way back before widening, so a grant can never outlive a
                    // record that failed to save.
                    if let Err(e) = self.save_timed(&timed) {
                        log::warn!("could not record an hour-long permission: {e}");
                        self.asks.lock().unwrap().push(ask);
                        self.requests_changed();
                        return Err("could not record the hour, so nothing changed".into());
                    }
                }
                if let Err(e) = self.setting_apply(s, &ask.to) {
                    if answer == "hour" {
                        // Nothing changed, so neither does the record: an earlier hour
                        // on this setting still needs its way back.
                        if let Err(e) = self.save_timed(&original) {
                            log::warn!("could not drop an unused hour-long record: {e}");
                        }
                    }
                    self.asks.lock().unwrap().push(ask);
                    self.requests_changed();
                    return Err(e);
                }
                if answer == "yes" {
                    // An earlier hour on this setting is over. If this fails the old record
                    // only narrows it back later, which errs on the side of less access.
                    if let Err(e) = self.save_timed(&timed) {
                        log::warn!("could not update the hour-long permissions: {e}");
                    }
                }
                if answer == "hour" {
                    format!("Yes, \u{201c}{label}\u{201d} {choice} for an hour.")
                } else {
                    format!("Yes, set \u{201c}{label}\u{201d} to {choice}.")
                }
            }
            _ => return Err("the answer is no, yes or hour".into()),
        };
        self.requests_changed();
        self.say_in_chat(Some(ask.chat), said, false);
        Ok(())
    }

    /// Puts back the settings whose hour is up. A setting the owner has changed since is
    /// left as they set it.
    pub fn revert_due(self: &Arc<Self>) {
        let _changes = self.profile_changes.lock().unwrap();
        let now = now_millis() / 1000;
        let profile = self.active();
        let mut timed = match self.timed() {
            Ok(timed) => timed,
            Err(e @ goblin_store::StoreError::Corrupt { .. }) => {
                // Nothing in it can be put back by itself any more. Keep it for the owner
                // and say so, rather than overwrite it with the next save.
                log::warn!("hour-long permissions kept aside: {e}");
                goblin_store::fsutil::quarantine(&self.timed_path());
                post_notice(
                    self,
                    NewNotice {
                        at: now_millis(),
                        kind: NoticeKind::Activity,
                        title: "Hour-long permissions could not be read".into(),
                        body: Some("Check Settings \u{2192} Access".into()),
                        chat: None,
                        link: None,
                    },
                );
                return;
            }
            Err(e) => {
                // Perhaps passing (no file handles, a slow card): try again next pass.
                log::warn!("could not read the hour-long permissions: {e}");
                return;
            }
        };
        if timed.is_empty() {
            return;
        }
        let mut kept = Vec::new();
        for t in timed.drain(..) {
            let Some(s) = crate::asks::find(&t.setting) else {
                continue;
            };
            // A permission belongs to its profile: it waits until that profile is served.
            let mine = !matches!(s.store, crate::asks::Store::Access(_)) || t.profile == profile;
            if now < t.until || !mine {
                kept.push(t);
                continue;
            }
            if self.setting_value(s) == t.value {
                if let Err(e) = self.setting_apply(s, &t.revert_to) {
                    log::warn!("could not put back {}: {e}", t.setting);
                    kept.push(t);
                    continue;
                }
                post_notice(
                    self,
                    NewNotice {
                        at: now_millis(),
                        kind: NoticeKind::Activity,
                        title: format!("{} is back to {}", s.label, s.choice_label(&t.revert_to)),
                        body: Some("The hour is up".into()),
                        chat: None,
                        link: None,
                    },
                );
            }
        }
        if let Err(e) = self.save_timed(&kept) {
            log::warn!("could not update the hour-long permissions: {e}");
        }
    }

    /// Keeps [`DaemonState::revert_due`] running.
    pub fn start_timers(self: &Arc<Self>) {
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                state.revert_due();
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            }
        });
    }

    /// The owner's answer to an install: does it, and says so to Muse for the owner. A yes
    /// installs only the request the owner was shown, `request`; a no drops whatever waits.
    pub fn build_decide(
        self: &Arc<Self>,
        name: &str,
        request: Option<&str>,
        approve: bool,
    ) -> Result<(), String> {
        let asked = self
            .builds_list()
            .items
            .into_iter()
            .find(|i| i.name == name && i.state == BuildState::Pending);
        if approve {
            self.build_approve(name, request.ok_or(crate::builds::CHANGED)?)
                .map(drop)?;
        } else {
            self.build_decline(name)?;
        }
        if let Some(item) = asked {
            let text = if approve {
                format!("Yes, install \u{201c}{}\u{201d}.", item.title)
            } else {
                format!("No, don\u{2019}t install \u{201c}{}\u{201d}.", item.title)
            };
            let restarting = approve && item.kind == BuildKind::Tool;
            self.say_in_chat(item.chat, text, restarting);
        }
        Ok(())
    }

    /// The owner's answer to a request, said in the chat it was asked in, as their own
    /// message: pressing the button on the handheld is both the decision and the reply.
    /// When the daemon is about to start again (a tool was installed), the message waits
    /// for the new session, so Muse's answer to it is not cut off.
    fn say_in_chat(self: &Arc<Self>, chat: Option<String>, text: String, after_restart: bool) {
        let chat = chat.unwrap_or_else(|| "main".into());
        if after_restart {
            if let Ok(profile) = self.profiles.profile(&self.active()) {
                let note = json!({"chat": chat, "text": text});
                let _ = std::fs::write(profile.dir().join(OWED_REPLY), note.to_string());
            }
            return;
        }
        let state = self.clone();
        tokio::spawn(async move {
            if let Err(e) = state.chat_send(&chat, &text).await {
                log::warn!("the owner's answer was not sent: {}", e.message);
            }
        });
    }

    /// Sends an answer left by [`DaemonState::say_in_chat`] before the daemon started again.
    pub fn send_owed_reply(self: &Arc<Self>) {
        let Ok(profile) = self.profiles.profile(&self.active()) else {
            return;
        };
        let path = profile.dir().join(OWED_REPLY);
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        if let Ok(note) = serde_json::from_slice::<Value>(&bytes) {
            let chat = note["chat"].as_str().map(str::to_owned);
            if let Some(text) = note["text"].as_str() {
                self.say_in_chat(chat, text.to_owned(), false);
            }
        }
    }

    /// Installs the request called `name` (the owner approved it, or installs are free).
    pub fn build_approve(self: &Arc<Self>, name: &str, request: &str) -> Result<BuildItem, String> {
        let builds = self.builds().ok_or("this device does not keep builds")?;
        let item = builds.approve(
            name,
            request,
            self.launcher(),
            &|kind, title| self.build_entry_taken(kind, title),
            now_millis() / 1000,
        )?;
        log::info!(
            "installed {} ({:?}) for '{}'",
            item.name,
            item.kind,
            self.active()
        );
        self.builds_changed(item.kind == BuildKind::Tool);
        Ok(item)
    }

    /// Drops the request called `name` (the owner said no).
    pub fn build_decline(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let builds = self.builds().ok_or("this device does not keep builds")?;
        builds.decline(name)?;
        self.builds_changed(false);
        Ok(())
    }

    /// Removes one of the profile's items.
    pub fn build_remove(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let builds = self.builds().ok_or("this device does not keep builds")?;
        let was_tool = builds.list().iter().any(|i| {
            i.name == name && i.kind == BuildKind::Tool && i.state == BuildState::Installed
        });
        builds.remove(name, self.launcher())?;
        log::info!("removed {name} for '{}'", self.active());
        self.builds_changed(was_tool);
        Ok(())
    }

    /// `handheld.about`: what the device is, how it stands right now, and what the profile
    /// being served may do on it, each permission with what it means.
    pub fn describe(&self) -> Value {
        let mut out = match &self.platform {
            Some(p) => p.describe(),
            None => json!({"device": {"model": "unknown", "arch": std::env::consts::ARCH}}),
        };
        let mut state = Map::new();
        if let Some(v) = goblin_platform::sysvol::read_nextui() {
            state.insert("volume_percent".into(), json!(v.percent));
            state.insert("muted".into(), json!(v.muted));
            let output = if v.headphones {
                "headphones"
            } else {
                "speaker"
            };
            state.insert("sound_output".into(), json!(output));
        }
        if let Some(Ok(b)) = self.platform.as_ref().map(|p| p.battery()) {
            state.insert("battery_percent".into(), json!(b.percent));
            state.insert("charging".into(), json!(b.charging));
        }
        if let Some(p) = &self.platform {
            if let Value::Object(connections) = p.connections() {
                state.extend(connections);
            }
            if let Some(disk) = p.health().disk_gb {
                state.insert("storage_free_gb".into(), json!(disk.free));
            }
        }
        // Each place software is kept says how far the files must reach to look there.
        let storage = self.executor().storage_root();
        if let Some(places) = out
            .pointer_mut("/software/places")
            .and_then(Value::as_array_mut)
        {
            for place in places {
                let path = place.get("path").and_then(Value::as_str).unwrap_or("");
                let on_card = storage
                    .as_deref()
                    .is_some_and(|root| std::path::Path::new(path).starts_with(root));
                place["needs_file_reach"] = json!(if on_card { "storage" } else { "everything" });
            }
        }
        if let Some(software) = out.get_mut("software").and_then(Value::as_object_mut) {
            software.insert(
                "how".into(),
                json!("These are places, not a list of what is installed. Look with file.list and file.read, if your file_reach goes that far; if it does not and a task needs it, ask with handheld.ask and say why."),
            );
        }
        let consent = self.consent();
        let profile = self.active();
        let own_folder = self
            .homes
            .read()
            .unwrap()
            .as_ref()
            .map(|h| h.join(&profile).to_string_lossy().into_owned());
        let reach = match consent.files {
            FileScope::Home => "own_folder",
            FileScope::Storage => "storage",
            FileScope::Device => "everything",
        };
        let builds = match consent.builds {
            BuildAccess::Off => "no",
            BuildAccess::Ask => "ask",
            BuildAccess::Free => "freely",
        };
        let access = json!({
            "profile": profile,
            "own_folder": own_folder,
            "set_by": "The owner. You cannot change these yourself. To have one changed, ask with handheld.ask: the handheld puts the question to the owner and they answer with a button. The widest (the shell, writes outside your own folder, files everywhere, installing freely) they set only in Settings.",
            "read_files": {
                "on": consent.file_read,
                "means": "file.read works, as far as file_reach goes.",
            },
            "write_files": {
                "on": consent.file_write,
                "means": "file.write works in your own folder; writing beyond it also needs write_outside_home and file_reach.",
            },
            "write_outside_home": {
                "on": consent.file_write_outside_home,
                "means": "Allows writes beyond your own folder, within file_reach. Like the shell, this can change programs run as administrator. Only the owner can change it manually in Settings; you can explain why it is needed, but cannot request it with handheld.ask.",
            },
            "file_reach": {
                "value": reach,
                "means": "How far file.read and file.write go. own_folder: only your own folder. storage: the removable card too (games, saves, the owner's files). everything: any path on the device. Writes outside your own folder also need write_outside_home. Other profiles' folders and the app's own data are never reachable.",
            },
            "own_commands": {
                "on": consent.drop_ins,
                "means": "Commands the owner wrote and put on the device are callable; they are in your command list.",
            },
            "shell": {
                "on": consent.shell,
                "means": "system.run works: any command, as the device's administrator, on the whole device.",
            },
            "install_builds": {
                "value": builds,
                "means": "Whether build.install works, and the tools you installed can be called. no: neither. ask: each install waits for the owner to approve it on the device. freely: installs happen at once.",
            },
        });
        if let Some(o) = out.as_object_mut() {
            o.insert("state".into(), Value::Object(state));
            o.insert("access".into(), access);
        }
        out
    }

    /// Runs one of Muse's `build.*` commands. `access` is never `Off` here.
    fn run_build(
        self: &Arc<Self>,
        command: &str,
        params: &Map<String, Value>,
        access: BuildAccess,
    ) -> Result<Value, String> {
        let builds = self.builds().ok_or("this device does not keep builds")?;
        match command {
            "build.list" => Ok(json!({"items": builds.list()})),
            "build.logs" => builds.logs(
                params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("name is required")?,
            ),
            "build.remove" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("name is required")?;
                self.build_remove(name)?;
                Ok(json!({"removed": name}))
            }
            "build.install" => {
                let path = params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("path is required")?;
                // Only out of the profile's own folder, whatever else it may read.
                let source = self.executor().in_home(path).map_err(|_| {
                    "the folder must be inside your own folder on this device".to_owned()
                })?;
                let chat = self.last_turn_chat.lock().unwrap().clone();
                let item = builds.request_in(&source, now_millis() / 1000, Some(chat.clone()))?;
                if item.kind == BuildKind::Tool && self.executor().is_drop_in(&item.name) {
                    let _ = builds.decline(&item.name);
                    return Err(format!(
                        "the owner already has a command called {}; choose another name",
                        item.name
                    ));
                }
                if access == BuildAccess::Free {
                    let request = item.request.as_deref().unwrap_or_default();
                    let item = self.build_approve(&item.name, request).inspect_err(|_| {
                        self.requests_changed();
                    })?;
                    let note = if item.kind == BuildKind::Tool {
                        "Installed. The device reconnects in a moment; the tool is callable after that."
                    } else {
                        "Installed and listed in the launcher, not launch-tested. Ask the owner to launch it, then check build.logs."
                    };
                    return Ok(json!({"status": "installed", "item": item, "note": note}));
                }
                self.builds_changed(false);
                post_notice(
                    self,
                    NewNotice {
                        at: now_millis(),
                        kind: NoticeKind::Approval,
                        title: format!("Install {}?", item.title),
                        body: None,
                        chat: Some(chat.clone()),
                        link: Some(Route(format!("chat/{chat}"))),
                    },
                );
                Ok(json!({
                    "status": "waiting_for_owner",
                    "item": item,
                    "note": "Not installed yet. The handheld is now asking the owner in this chat, showing what you want to install and what it runs; their answer arrives here as their next message. Only their press on the handheld installs it: a yes typed or said anywhere else does not. Say briefly what it is for, then wait."
                }))
            }
            other => Err(format!("unsupported command: {other}")),
        }
    }

    /// The pairing window has closed.
    pub fn pair_closed(&self) {
        *self.pair.lock().unwrap() = None;
    }

    /// Requests a shutdown that the process follows by starting again (profile switch).
    pub fn request_restart(&self) {
        self.restart_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.request_shutdown();
    }

    /// Whether the shutdown under way is a restart.
    pub fn restart_requested(&self) -> bool {
        self.restart_flag.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Requests shutdown.
    pub fn request_shutdown(&self) {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.shutdown.notify_waiters();
    }
}

// ---- command runner ---------------------------------------------------------------------------

/// The [`CommandRunner`] the device session uses: enforces consent, logs to the
/// command log, emits `commands.*` events, and runs the [`Executor`].
pub struct DaemonRunner {
    state: Arc<DaemonState>,
}

impl DaemonRunner {
    /// Wraps the daemon state as a command runner.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

/// A Hermes calls the handheld's commands through the same runner as a Muse: one set of
/// commands, one set of checks.
impl hermes_link::Runner for DaemonRunner {
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        CommandRunner::run(self, command, params, timeout_ms)
    }
}

impl CommandRunner for DaemonRunner {
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        let state = self.state.clone();
        Box::pin(async move {
            let executor = state.executor();
            let is_drop_in = executor.is_drop_in(&command);
            let is_built = executor.is_built(&command);
            let is_build_command = command.starts_with("build.");
            let consent = state.consent();
            let summary = summarize(&command);
            let at = now_millis();

            // A tool Muse built answers to the builds switch, not to the owner-commands one.
            let allowed = if is_built {
                consent.builds != BuildAccess::Off
            } else {
                exec::command_allowed(&command, &consent, is_drop_in)
            };
            if !allowed {
                let entry = state.with_stores(|s| {
                    let e = s.commands.refused(&command, &summary, at);
                    let _ = s.commands.save();
                    e
                });
                let _ = state
                    .bus
                    .publish(&Event::CommandsFinished(CommandEvent { entry }));
                log::info!("refused {command}: consent off");
                return exec::refusal(&command);
            }

            let entry = state.with_stores(|s| {
                let e = s.commands.start(&command, &summary, at);
                let _ = s.commands.save();
                e
            });
            let _ = state.bus.publish(&Event::CommandsStarted(CommandEvent {
                entry: entry.clone(),
            }));

            let started = Instant::now();
            let exec_command = command.clone();
            let build_state = state.clone();
            let result = tokio::task::spawn_blocking(move || {
                if exec_command.starts_with("handheld.") {
                    let answer = match exec_command.as_str() {
                        "handheld.about" => Ok(build_state.describe()),
                        "handheld.settings" => Ok(build_state.settings_report()),
                        "handheld.ask" => build_state.ask(&params),
                        "handheld.suggest" => build_state.suggest(&params),
                        other => Err(format!("unsupported command: {other}")),
                    };
                    let mut m = Map::new();
                    match answer {
                        Ok(payload) => {
                            m.insert("ok".into(), json!(true));
                            m.insert("payload".into(), payload);
                        }
                        Err(why) => {
                            m.insert("ok".into(), json!(false));
                            m.insert("error".into(), json!(why));
                        }
                    }
                    return m;
                }
                if is_build_command {
                    let mut m = Map::new();
                    match build_state.run_build(&exec_command, &params, consent.builds) {
                        Ok(payload) => {
                            m.insert("ok".into(), json!(true));
                            m.insert("payload".into(), payload);
                        }
                        Err(why) => {
                            m.insert("ok".into(), json!(false));
                            m.insert("error".into(), json!(why));
                        }
                    }
                    return m;
                }
                executor.run_scoped(
                    &exec_command,
                    &params,
                    timeout_ms,
                    consent.files,
                    consent.file_write_outside_home,
                )
            })
            .await
            .unwrap_or_else(|_| {
                let mut m = Map::new();
                m.insert("ok".into(), json!(false));
                m.insert("error".into(), json!("command task failed"));
                m
            });

            let ms = started.elapsed().as_millis() as u64;
            let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let cmd_state = if ok {
                CommandState::Ok
            } else {
                CommandState::Error
            };
            if let Some(finished) = state.with_stores(|s| {
                let f = s.commands.finish(&entry.id, cmd_state, ms);
                let _ = s.commands.save();
                f
            }) {
                let _ = state
                    .bus
                    .publish(&Event::CommandsFinished(CommandEvent { entry: finished }));
            }
            result
        })
    }
}

// ---- chat engine ------------------------------------------------------------------------------

impl DaemonState {
    /// Sends a text message and follows its turn, emitting chat events.
    pub async fn chat_send(
        self: &Arc<Self>,
        chat: &str,
        text: &str,
    ) -> Result<ChatSendResult, IpcError> {
        if text.trim().is_empty() {
            return Err(IpcError::new(ErrorCode::BadRequest, "text is required"));
        }
        self.send_turn(chat, Payload::Text(text.to_owned())).await
    }

    /// Sends a voice note; the reply is spoken when it is done (`voice.speak_replies`).
    pub async fn voice_note_send(
        self: &Arc<Self>,
        chat: &str,
        wav: Vec<u8>,
        secs: u64,
    ) -> Result<ChatSendResult, IpcError> {
        self.send_turn(chat, Payload::Voice { wav, secs }).await
    }

    /// Sends a text message or a voice note and follows the turn.
    async fn send_turn(
        self: &Arc<Self>,
        chat: &str,
        payload: Payload,
    ) -> Result<ChatSendResult, IpcError> {
        validate_chat_id(chat)
            .map_err(|_| IpcError::new(ErrorCode::BadRequest, "invalid chat id"))?;
        // Suggestions answer Muse's last reply; a new message makes them stale.
        self.set_suggestions(chat, Vec::new());
        let text = match &payload {
            Payload::Text(t) => t.clone(),
            Payload::Voice { secs, .. } => format!("Voice note · {secs} s"),
        };
        // What stands in for a voice note until the agent says what it heard.
        let placeholder = matches!(payload, Payload::Voice { .. }).then(|| text.clone());
        let text = text.as_str();
        let link = self
            .link()
            .ok_or_else(|| IpcError::new(ErrorCode::NotConnected, "Not connected"))?;
        let crate::backend::Sent {
            message_id: user_mid,
            mut turn,
        } = link
            .send(
                chat,
                &payload,
                matches!(payload, Payload::Voice { .. }) && self.voice_to_voice(),
            )
            .await?;
        let turn_id = format!("t-{}", now_millis());

        // Store and emit the user message.
        let user_message = Message {
            id: if user_mid.is_empty() {
                turn_id.clone()
            } else {
                user_mid.clone()
            },
            chat: chat.to_owned(),
            role: Role::User,
            text: text.to_owned(),
            rich: None,
            choices: Vec::new(),
            attachments: None,
            reaction: None,
            at: now_millis(),
            state: MessageState::Sent,
            origin: Origin::Gadget,
        };
        self.with_stores(|s| {
            let _ = s.chats.upsert(user_message.clone());
            let _ = s.chats.flush();
        });
        let _ = self.bus.publish(&Event::ChatMessage(ChatMessageEvent {
            message: user_message,
        }));
        if matches!(payload, Payload::Voice { .. }) {
            if self.voice_to_voice() {
                let before = self.newest_assistant(chat);
                self.voice_turns
                    .lock()
                    .unwrap()
                    .insert(turn_id.clone(), before);
            }
            if !user_mid.is_empty() {
                self.fill_transcript(chat.to_owned(), user_mid.clone());
            }
        }
        self.record_turn(&turn_id, chat, TurnState::Acked);

        if !turn.open().await {
            // Sent, but replies cannot be followed: report honestly.
            self.record_turn(&turn_id, chat, TurnState::Uncertain);
            return Ok(ChatSendResult {
                turn: turn_id,
                message_id: (!user_mid.is_empty()).then_some(user_mid),
            });
        }
        if turn.fresh() {
            log::info!("created side chat {chat} with its first message");
        }

        // Follow the turn in the background.
        let state = self.clone();
        let chat_owned = chat.to_owned();
        let follow_turn_id = turn_id.clone();
        let user_mid_follow = user_mid.clone();
        tokio::spawn(follow_turn(
            state,
            chat_owned,
            follow_turn_id,
            turn,
            user_mid_follow,
            placeholder,
        ));

        Ok(ChatSendResult {
            turn: turn_id,
            message_id: if user_mid.is_empty() {
                None
            } else {
                Some(user_mid)
            },
        })
    }
}

/// A question the agent put to the owner (an approval, a clarification), shown as a card.
struct AgentPrompt {
    /// The agent's id for it.
    id: String,
    chat: String,
    text: String,
    /// `(the card's button, the agent's choice, its label)`.
    answers: Vec<(String, String, String)>,
    at: u64,
}

/// A card has three buttons, which the app knows as `yes`, `no` and `hour`. In an approval,
/// "no" is the agent's `deny` and nothing else, and the other choices take `yes` then `hour`.
/// In any other question the choices take `yes`, `no`, `hour` in the order given. A choice
/// that finds no button is not offered: the owner can still say it in words.
fn card_answers(kind: &str, choices: &[(String, String)]) -> Vec<(String, String, String)> {
    let approval = kind == "approval";
    let mut buttons = if approval {
        vec!["yes", "hour"].into_iter()
    } else {
        vec!["yes", "no", "hour"].into_iter()
    };
    choices
        .iter()
        .filter_map(|(choice, label)| {
            let button = if approval && choice == "deny" {
                "no"
            } else {
                buttons.next()?
            };
            let label: String = label.chars().take(24).collect();
            Some((button.to_owned(), choice.clone(), label))
        })
        .collect()
}

impl DaemonState {
    /// Takes what the agent sent unasked: a question for the owner, or a message.
    pub(crate) fn agent_pushed(self: &Arc<Self>, pushed: hermes_link::Pushed) {
        match pushed {
            hermes_link::Pushed::Prompt {
                id,
                chat,
                kind,
                text,
                choices,
            } => {
                if validate_chat_id(&chat).is_err() {
                    return;
                }
                let answers = card_answers(&kind, &choices);
                let title = text.lines().next().unwrap_or("").to_owned();
                let dropped = {
                    let mut prompts = self.prompts.lock().unwrap();
                    // The same question again replaces itself.
                    prompts.retain(|p| p.id != id);
                    // A handful at most: more than that is not a conversation. The oldest
                    // gives way, and the agent is told no rather than left waiting.
                    let dropped = (prompts.len() >= 8).then(|| prompts.remove(0).id);
                    prompts.push(AgentPrompt {
                        id,
                        chat: chat.clone(),
                        text,
                        answers,
                        at: now_millis() / 1000,
                    });
                    dropped
                };
                if let (Some(dropped), Some(link)) = (dropped, self.link()) {
                    link.answer(&dropped, "deny");
                }
                self.requests_changed();
                post_notice(
                    self,
                    NewNotice {
                        at: now_millis(),
                        kind: NoticeKind::Approval,
                        title,
                        body: None,
                        chat: Some(chat.clone()),
                        link: Some(Route(format!("chat/{chat}"))),
                    },
                );
            }
            hermes_link::Pushed::Identity => {
                let state = self.clone();
                tokio::spawn(async move { state.refresh_identity().await });
            }
            hermes_link::Pushed::Notice {
                chat,
                message_id,
                text,
            } => {
                if validate_chat_id(&chat).is_err() || text.trim().is_empty() {
                    return;
                }
                let message = Message {
                    id: message_id,
                    chat: chat.clone(),
                    role: Role::Assistant,
                    text,
                    rich: None,
                    choices: Vec::new(),
                    attachments: None,
                    reaction: None,
                    at: now_millis(),
                    state: MessageState::Done,
                    origin: Origin::Elsewhere,
                };
                self.with_stores(|s| {
                    let _ = s.chats.upsert(message.clone());
                    let _ = s.chats.flush();
                });
                let _ = self
                    .bus
                    .publish(&Event::ChatMessage(ChatMessageEvent { message }));
                let (title, body) = reply_notice_text(self, &chat);
                post_notice(
                    self,
                    NewNotice {
                        at: now_millis(),
                        kind: NoticeKind::Reply,
                        title,
                        body,
                        chat: Some(chat.clone()),
                        link: Some(Route(format!("chat/{chat}"))),
                    },
                );
            }
        }
    }
}

/// Longest a turn is followed live before it is left as uncertain.
const MAX_TURN_FOLLOW: std::time::Duration = std::time::Duration::from_secs(600);
/// How often a turn with no live reply checks Muse's history.
const BACKFILL_POLL: std::time::Duration = std::time::Duration::from_secs(4);

/// Stores and publishes replies to `ctx.user_mid` found in Muse's history. Returns whether
/// any were found.
async fn backfill_reply(
    state: &Arc<DaemonState>,
    chat: &str,
    turn_id: &str,
    ctx: &FollowCtx,
) -> bool {
    if ctx.user_mid.is_empty() {
        return false;
    }
    let history = match state.fetch_history(chat, 20).await {
        Ok(h) => h,
        Err(e) => {
            log::info!(
                "could not read chat history to recover a reply: {}",
                e.message
            );
            return false;
        }
    };
    let replies = replies_to(&history, &ctx.user_mid, ctx.fresh);
    if replies.is_empty() {
        return false;
    }
    for m in replies {
        state.with_stores(|st| {
            let _ = st.chats.upsert(m.clone());
            let _ = st.chats.flush();
        });
        let _ = state
            .bus
            .publish(&Event::ChatMessage(ChatMessageEvent { message: m }));
    }
    log::info!("reply for {turn_id} read from chat history");
    true
}

/// What `follow_turn` needs to backfill a reply it did not see live.
struct FollowCtx {
    /// Muse's id for the user message that opened the turn ("" if unknown).
    user_mid: String,
    /// The message created the side chat, so the subscription opened late.
    fresh: bool,
}

/// The presence lights' helper, and what it was built for: the `leds.mode` and the colour.
type PresenceLights = goblin_platform::PresenceLeds<goblin_platform::Leds>;
type LightsBuiltFor = (LedMode, goblin_store::settings::ChatTheme);

/// The lights' colour for the colour chosen in Settings: the same hue as the app's accent,
/// at full strength, since an LED shows a pale colour as white.
fn light_colour(chosen: goblin_store::settings::ChatTheme) -> goblin_platform::Rgb {
    use goblin_platform::Rgb;
    use goblin_store::settings::ChatTheme as C;
    match chosen {
        C::Avatar | C::Default => Rgb(0, 120, 255),
        C::Monochrome => Rgb(255, 255, 255),
        C::Beige => Rgb(255, 150, 60),
        C::Blue => Rgb(0, 70, 255),
        C::Purple => Rgb(150, 50, 255),
        C::Pink => Rgb(255, 40, 140),
        C::Orange => Rgb(255, 90, 0),
        C::Green => Rgb(110, 255, 20),
    }
}

/// Gives a profile the name its owner typed on the pairing page, once the pairing window is
/// closed and before anyone is told how the pairing went, so that what is said next names
/// the profile as it is. Returns the name it has afterwards, and whether it changed. A name
/// that can no longer be given (another profile took it meanwhile) leaves the old one: the
/// pairing matters more. Nothing is announced and nothing restarted here: the caller says
/// how the pairing ended, and starts the daemon again if the profile served was renamed.
pub(crate) fn name_after_pairing(
    state: &Arc<DaemonState>,
    name: &str,
    typed: Option<String>,
) -> (String, bool) {
    let Some(new) = typed else {
        return (name.to_owned(), false);
    };
    match rename_profile(state, name, &new, false) {
        Ok(()) => (new, true),
        Err(e) => {
            log::warn!("profile '{name}' keeps its name: {}", e.message);
            (name.to_owned(), false)
        }
    }
}

/// Renames a profile. The name changes and nothing else: the pairing, the conversations kept
/// on the device, the access settings and the folder of files all stay with the profile.
/// Not while a pairing is open for it: that holds the profile's folder by its old name.
/// `announce` tells the apps and starts the daemon again when the profile served was the one
/// renamed; without it both are left to the caller.
fn rename_profile(
    state: &Arc<DaemonState>,
    old: &str,
    new: &str,
    announce: bool,
) -> Result<(), IpcError> {
    let _changes = state.profile_changes.lock().unwrap();
    if state
        .pair
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|o| o.profile == old)
    {
        return Err(IpcError::new(
            ErrorCode::Busy,
            "pairing is open for this profile",
        ));
    }
    let serving = old == state.active();
    if serving && state.call.lock().unwrap().is_some() {
        return Err(IpcError::new(
            ErrorCode::Busy,
            "end the call before renaming this profile",
        ));
    }
    goblin_store::validate_profile_name(new).map_err(store_err)?;
    state.profiles.profile(old).map_err(store_err)?;
    if state
        .profiles
        .list()
        .map_err(store_err)?
        .iter()
        .any(|profile| profile.name.eq_ignore_ascii_case(new))
    {
        return Err(IpcError::new(
            ErrorCode::Rejected,
            "a profile with that name already exists",
        ));
    }
    // The folder goes first: if it cannot move (a folder of the new name is in the
    // way), nothing has changed yet.
    let roots: Vec<_> = [
        state.homes.read().unwrap().clone(),
        state.built.read().unwrap().clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    for root in &roots {
        if root.join(new).exists() {
            return Err(IpcError::new(
                ErrorCode::Rejected,
                format!("a folder named {} is already there", new),
            ));
        }
    }
    // Read before anything moves: a rename must carry the hour-long permissions.
    let original_timed = state.timed().map_err(store_err)?;
    let mut moved = Vec::new();
    let move_files = (|| -> Result<(), String> {
        for root in &roots {
            let (from, to) = (root.join(old), root.join(new));
            if from.exists() {
                std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
                moved.push((from, to));
            }
        }
        if let Some(root) = state.built.read().unwrap().as_ref() {
            crate::builds::Builds::new(root.join(new)).relist(state.launcher())?;
        }
        Ok(())
    })();
    let rollback_files = || {
        for (from, to) in moved.iter().rev() {
            if let Err(e) = std::fs::rename(to, from) {
                log::error!("could not restore {}: {e}", from.display());
            }
        }
        if let Some(root) = state.built.read().unwrap().as_ref() {
            if let Err(e) = crate::builds::Builds::new(root.join(old)).relist(state.launcher()) {
                log::error!("could not restore profile launcher entries: {e}");
            }
        }
    };
    if let Err(e) = move_files {
        rollback_files();
        return Err(IpcError::new(ErrorCode::Internal, e));
    }
    let mut renamed_timed = original_timed.clone();
    for grant in &mut renamed_timed {
        if grant.profile == old {
            grant.profile = new.to_owned();
        }
    }
    let rename = || -> Result<(), goblin_store::StoreError> {
        if renamed_timed != original_timed {
            // Keep the old names too until the directory has moved: a crash in
            // between must leave a reversion record under either name.
            let mut both = original_timed.clone();
            both.extend(renamed_timed.iter().filter(|t| t.profile == new).cloned());
            goblin_store::fsutil::write_json_atomic(&state.timed_path(), &both, 0o600)?;
        }
        if let Err(e) = state.profiles.rename(old, new) {
            // If the store could not roll back, either name may still hold consent.
            // Keep both expiry records until the surviving profile can be reverted.
            if renamed_timed != original_timed
                && state.profiles.profile(old).is_ok()
                && state.profiles.profile(new).is_err()
            {
                if let Err(restore) = goblin_store::fsutil::write_json_atomic(
                    &state.timed_path(),
                    &original_timed,
                    0o600,
                ) {
                    log::error!(
                        "could not restore timed permissions after failed rename: {restore}"
                    );
                }
            }
            return Err(e);
        }
        if renamed_timed != original_timed {
            if let Err(e) =
                goblin_store::fsutil::write_json_atomic(&state.timed_path(), &renamed_timed, 0o600)
            {
                log::warn!("could not remove old timed-permission names after rename: {e}");
            }
        }
        Ok(())
    };
    if let Err(e) = rename() {
        rollback_files();
        return Err(store_err(e));
    }
    log::info!("profile '{}' renamed to '{}'", old, new);
    if serving && announce {
        // The socket, the folder and the stores are all named after the profile:
        // start again under the new name.
        let _ = state
            .bus
            .publish(&Event::ProfileChanged(ProfileChangedEvent {
                profile: new.to_owned(),
            }));
        let restarting = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            restarting.request_restart();
        });
    }
    Ok(())
}

async fn follow_turn(
    state: Arc<DaemonState>,
    chat: String,
    turn_id: String,
    mut turn: crate::backend::Turn,
    user_mid: String,
    // The text standing in for a voice note, when the turn began with one.
    placeholder: Option<String>,
) {
    let ctx = FollowCtx {
        user_mid,
        fresh: turn.fresh(),
    };
    let mut emitted_working = false;
    let mut emitted_replying = false;
    // Found in history, not seen live: as good as finished.
    let mut recovered = false;
    for ev in turn.take_held() {
        apply_turn_event(
            &state,
            &chat,
            &turn_id,
            &ev,
            &mut emitted_working,
            &mut emitted_replying,
        );
    }
    let started = std::time::Instant::now();
    let mut last_backfill = std::time::Instant::now();
    loop {
        // A fresh chat's turn looks quiet (and so complete) before Muse has answered, because
        // its live events went by before the subscription opened. Wait for a reply there.
        // As in the reference: finished when settled, or complete once reply text arrived.
        if turn.finished(emitted_replying) {
            break;
        }
        if started.elapsed() >= MAX_TURN_FOLLOW {
            log::info!(
                "stopped following {turn_id} after {}s",
                MAX_TURN_FOLLOW.as_secs()
            );
            break;
        }
        // A fresh side chat's live events can precede the subscription, leaving the turn
        // "busy" forever; Muse's history is the source of truth there.
        if ctx.fresh
            && !emitted_replying
            && last_backfill.elapsed() >= BACKFILL_POLL
            && backfill_reply(&state, &chat, &turn_id, &ctx).await
        {
            emitted_replying = true;
            recovered = true;
            break;
        }
        if last_backfill.elapsed() >= BACKFILL_POLL {
            last_backfill = std::time::Instant::now();
        }
        let next = turn.next(std::time::Duration::from_millis(500)).await;
        if let (Some(heard), Some(placeholder)) = (turn.take_heard(), placeholder.as_deref()) {
            let note = if ctx.user_mid.is_empty() {
                &turn_id
            } else {
                &ctx.user_mid
            };
            state.voice_note_heard(&chat, note, placeholder, heard);
        }
        match next {
            Ok(events) => {
                for ev in events {
                    apply_turn_event(
                        &state,
                        &chat,
                        &turn_id,
                        &ev,
                        &mut emitted_working,
                        &mut emitted_replying,
                    );
                }
            }
            Err(why) => {
                log::info!("turn {turn_id}: {why}");
                break;
            }
        }
    }
    // A reply can be missed across a reconnect: backfill from Muse's history.
    if !emitted_replying && backfill_reply(&state, &chat, &turn_id, &ctx).await {
        emitted_replying = true;
        recovered = true;
    }
    let final_state = if recovered || turn.done() {
        TurnState::Done
    } else {
        TurnState::Uncertain
    };
    state.record_turn(&turn_id, &chat, final_state);
    state.set_activity(None);
    // A finished reply becomes an inbox notification (local-state driven).
    if final_state == TurnState::Done && emitted_replying {
        let (title, body) = reply_notice_text(&state, &chat);
        post_notice(
            &state,
            NewNotice {
                at: now_millis(),
                kind: NoticeKind::Reply,
                title,
                body,
                chat: Some(chat.clone()),
                link: Some(Route(format!("chat/{chat}"))),
            },
        );
    }
    turn.close().await;
    // Images or video sent with the reply arrive as separate events: fetch them now.
    state.sync_media(chat.clone());
    // A voice note's reply is spoken.
    let voice_before = state.voice_turns.lock().unwrap().remove(&turn_id);
    if let Some(before) = voice_before {
        let speak = state.settings.lock().unwrap().settings().speak_replies;
        let reply = state.newest_assistant(&chat);
        if final_state == TurnState::Done
            && speak != goblin_store::settings::SpeakReplies::Never
            && reply.is_some()
            && reply != before
        {
            state.speak(reply.unwrap_or_default());
        }
    }
}

/// Validates the owner's choices: one line each, no duplicates, at most `MAX_SUGGESTIONS`, and
/// together at most `MAX_SUGGESTION_TOTAL_CHARS` characters (they share one row of pills). The
/// messages tell Muse exactly what to fix.
pub fn check_suggestions(replies: &[String]) -> Result<Vec<String>, String> {
    if replies.len() > MAX_SUGGESTIONS {
        return Err(format!(
            "at most {MAX_SUGGESTIONS} choices, got {}",
            replies.len()
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for r in replies {
        let t = r.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            return Err("choices must not be empty".into());
        }
        if out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            return Err(format!("{t:?} is listed twice"));
        }
        out.push(t);
    }
    let total: usize = out.iter().map(|t| t.chars().count()).sum();
    if total > MAX_SUGGESTION_TOTAL_CHARS {
        return Err(format!(
            "the choices together must be at most {MAX_SUGGESTION_TOTAL_CHARS} characters; these have {total}. Use fewer or shorter choices"
        ));
    }
    Ok(out)
}

/// Publishes a call's state as `voice.call` events.
struct CallEvents {
    bus: Arc<EventBus>,
}

impl crate::call::CallObserver for CallEvents {
    fn caption(&self, assistant: bool, text: &str, done: bool) {
        let _ = self.bus.publish(&Event::VoiceCaption(VoiceCaptionEvent {
            role: if assistant {
                Role::Assistant
            } else {
                Role::User
            },
            text: text.to_owned(),
            is_final: done,
        }));
    }

    fn phase(&self, phase: crate::call::Phase, muted: bool, seconds: f64) {
        use crate::call::Phase;
        let state = match phase {
            Phase::Connecting => CallState::Connecting,
            Phase::Listening => CallState::Listening,
            Phase::Thinking => CallState::Thinking,
            Phase::Speaking => CallState::Speaking,
        };
        let _ = self.bus.publish(&Event::VoiceCall(VoiceCallEvent {
            state,
            reason: None,
            muted,
            seconds,
        }));
    }
}

/// What a turn sends.
pub(crate) enum Payload {
    Text(String),
    Voice { wav: Vec<u8>, secs: u64 },
}

fn apply_turn_event(
    state: &Arc<DaemonState>,
    chat: &str,
    turn_id: &str,
    ev: &TurnEvent,
    emitted_working: &mut bool,
    emitted_replying: &mut bool,
) {
    match ev {
        TurnEvent::Status(s) => {
            // Idle is not completion: the request can still be awaiting an answer.
            let working = s.activity_code.as_deref().map_or(*emitted_working, |code| {
                !matches!(
                    code,
                    "" | "idle"
                        | "online"
                        | "completed"
                        | "failed"
                        | "errored"
                        | "cancelled"
                        | "canceled"
                )
            });
            if working != *emitted_working {
                *emitted_working = working;
                state.record_turn(
                    turn_id,
                    chat,
                    if working {
                        TurnState::Working
                    } else {
                        TurnState::Acked
                    },
                );
            }
            state.set_activity(Some(Activity {
                code: s.activity_code.clone().unwrap_or_default(),
                text: s.activity_text.clone().unwrap_or_default(),
            }));
        }
        TurnEvent::Reply(r) => {
            if !*emitted_replying {
                *emitted_replying = true;
                state.record_turn(turn_id, chat, TurnState::Replying);
            }
            let message = Message {
                id: r.message_id.clone(),
                chat: chat.to_owned(),
                role: Role::Assistant,
                text: r.display_text.clone(),
                rich: None,
                choices: r.choices.clone(),
                attachments: None,
                reaction: None,
                at: now_millis(),
                state: if r.done {
                    MessageState::Done
                } else {
                    MessageState::Streaming
                },
                origin: Origin::Elsewhere,
            };
            state.with_stores(|st| {
                let _ = st.chats.upsert(message.clone());
                let _ = st.chats.flush();
            });
            let _ = state
                .bus
                .publish(&Event::ChatMessage(ChatMessageEvent { message }));
        }
        TurnEvent::Finished(_) => {}
    }
}

/// One message read from Muse's `GET /chat/history`.
#[derive(Debug, Clone)]
pub(crate) struct HistoryMessage {
    pub message: Message,
    pub seq: i64,
    pub parent: Option<String>,
    /// Media named by a presentation event (empty for ordinary messages).
    pub media: Vec<crate::media::MediaRef>,
}

/// For each media message in `history`: the id of the text message it follows (Muse sends a
/// reply's text, then its media) and its position among that reply's media. The Muse app shows
/// the media under that text.
pub(crate) fn media_anchors(history: &mut [HistoryMessage]) -> HashMap<String, (String, u64)> {
    history.sort_by_key(|h| h.seq);
    let mut out = HashMap::new();
    let mut anchor: Option<(String, u64)> = None;
    for h in history.iter() {
        if h.media.is_empty() {
            anchor = Some((h.message.id.clone(), 0));
        } else if let Some((id, n)) = anchor.as_mut() {
            *n += 1;
            out.insert(h.message.id.clone(), (id.clone(), *n));
        }
    }
    out
}

/// Assistant messages answering `user_mid`: those naming it as parent, and for a chat the
/// message created (`fresh`), every assistant message after it.
pub(crate) fn replies_to(history: &[HistoryMessage], user_mid: &str, fresh: bool) -> Vec<Message> {
    let user_seq = history
        .iter()
        .find(|h| h.message.id == user_mid)
        .map(|h| h.seq);
    history
        .iter()
        .filter(|h| h.message.role == Role::Assistant)
        .filter(|h| {
            h.parent.as_deref() == Some(user_mid) || (fresh && user_seq.is_some_and(|s| h.seq > s))
        })
        .map(|h| h.message.clone())
        .collect()
}

impl DaemonState {
    /// Reads the latest `limit` messages of `chat` ("main" or a side chat id) from Muse.
    pub(crate) async fn fetch_history(
        &self,
        chat: &str,
        limit: u32,
    ) -> Result<Vec<HistoryMessage>, IpcError> {
        Ok(self.fetch_history_page(chat, limit, None).await?.messages)
    }

    /// Reads up to `limit` history events of `chat` from Muse: the newest, or with
    /// `before_seq` those just older than that sequence number.
    pub(crate) async fn fetch_history_page(
        &self,
        chat: &str,
        limit: u32,
        before_seq: Option<i64>,
    ) -> Result<HistoryPage, IpcError> {
        validate_chat_id(chat)
            .map_err(|_| IpcError::new(ErrorCode::BadRequest, "invalid chat id"))?;
        let link = self
            .link()
            .ok_or_else(|| IpcError::new(ErrorCode::NotConnected, "Not connected"))?;
        link.history(chat, limit.clamp(1, 100), before_seq).await
    }

    /// Brings messages older than those cached for `chat` in from Muse, until at least `want`
    /// were added, Muse has no more, or the cache is full. Returns whether more may be asked
    /// for later.
    pub(crate) async fn load_older(self: &Arc<Self>, chat: &str, want: usize) -> bool {
        let mut added = 0;
        // A page counts events, and many are not messages: allow a few pages per request.
        for _ in 0..6 {
            let (floor, full) = self.with_stores(|s| {
                (
                    s.chats.history_floor(chat).ok().flatten(),
                    s.chats.full(chat).unwrap_or(true),
                )
            });
            if full {
                return false;
            }
            let page = match self.fetch_history_page(chat, 100, floor).await {
                Ok(p) => p,
                Err(e) => {
                    log::info!("could not read older history: {}", e.message);
                    return true;
                }
            };
            let Some(page_floor) = page.floor else {
                self.with_stores(|s| {
                    let _ = s.chats.mark_history_complete(chat);
                    let _ = s.chats.flush();
                });
                return false;
            };
            if floor.is_some_and(|f| page_floor >= f) {
                // Muse did not go further back: stop rather than ask for the same page again.
                log::info!("older history did not advance past {page_floor}");
                return false;
            }
            let more = page.has_more;
            let dir = self.media_dir();
            let older = page
                .messages
                .into_iter()
                .map(|h| {
                    let mut m = h.message;
                    if let (false, Some(dir)) = (h.media.is_empty(), &dir) {
                        // Media shows what is on the device already, else a named placeholder
                        // until the page it is on is asked for (`ensure_media`).
                        m.attachments = Some(
                            h.media
                                .iter()
                                .map(|r| crate::media::attachment(dir, r))
                                .collect(),
                        );
                    }
                    (h.seq, m)
                })
                .collect();
            added += self.with_stores(|s| {
                let n = s.chats.add_older(chat, older, page_floor).unwrap_or(0);
                if !more {
                    let _ = s.chats.mark_history_complete(chat);
                }
                let _ = s.chats.flush();
                n
            });
            if !more {
                return false;
            }
            if added >= want {
                break;
            }
        }
        true
    }
}

/// One page of Muse's `/chat/history`.
pub(crate) struct HistoryPage {
    /// Its messages.
    pub messages: Vec<HistoryMessage>,
    /// The lowest sequence number among its events (`None` for an empty page).
    pub floor: Option<i64>,
    /// Whether Muse has older events still.
    pub has_more: bool,
}

// ---- IPC handler ------------------------------------------------------------------------------

/// Implements the IPC [`Handler`] over an [`Arc<DaemonState>`].
#[derive(Clone)]
pub struct DaemonHandler {
    state: Arc<DaemonState>,
}

impl DaemonHandler {
    /// Wraps the shared state.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

fn unavailable(what: &str) -> IpcError {
    IpcError::new(ErrorCode::Unavailable, format!("{what}: not available"))
}

impl Handler for DaemonHandler {
    fn handle(
        &self,
        client: ClientInfo,
        method: Method,
    ) -> impl Future<Output = Result<Value, IpcError>> + Send {
        let state = self.state.clone();
        async move {
            let ui_hello = matches!(&method, Method::Hello(p) if p.client == ClientKind::Ui);
            let result = dispatch(state.clone(), method).await;
            if ui_hello && result.is_ok() {
                state.ui_opened(client.id);
            }
            result
        }
    }

    fn disconnected(&self, client: ClientInfo) {
        if client.kind == ClientKind::Ui {
            self.state.ui_closed(client.id);
        }
    }
}

async fn dispatch(state: Arc<DaemonState>, method: Method) -> Result<Value, IpcError> {
    match method {
        Method::Hello(_) => reply(&HelloResult {
            daemon_version: state.version.clone(),
            protocol: PROTOCOL_VERSION,
            run_id: state.run_id.clone(),
            profile: state.active(),
            capabilities: capabilities(&state),
        }),
        Method::Subscribe(_) => Ok(Value::Null), // answered by the server
        Method::DaemonStatus(_) => reply(&state.status()),
        Method::DaemonShutdown(_) => {
            state.request_shutdown();
            reply(&Empty {})
        }

        Method::ProfileList(_) => {
            let active = state.active();
            let profiles = state
                .profiles
                .list()
                .map_err(store_err)?
                .into_iter()
                .map(|p| ProfileInfo {
                    name: p.name,
                    paired: p.paired,
                    has_sdk_token: p.has_sdk_token,
                    agent: p.agent.map(str::to_owned),
                })
                .collect();
            reply(&ProfileListResult { active, profiles })
        }
        Method::ProfileCreate(p) => {
            state.profiles.create(&p.name).map_err(store_err)?;
            reply(&Empty {})
        }
        Method::ProfileRename(p) => {
            rename_profile(&state, &p.name, &p.new_name, true)?;
            reply(&Empty {})
        }
        Method::ProfileDelete(p) => {
            let _changes = state.profile_changes.lock().unwrap();
            state.profiles.profile(&p.name).map_err(store_err)?;
            if state.profiles.active().as_deref() == Some(&p.name) || p.name == state.active() {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "switch profile before deleting it",
                ));
            }
            if state
                .pair
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|o| o.profile == p.name)
            {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "pairing is open for this profile",
                ));
            }
            if let Some(root) = state.built.read().unwrap().as_ref() {
                let builds = crate::builds::Builds::new(root.join(&p.name));
                let items = builds.list();
                for item in &items {
                    if item.state == BuildState::Installed
                        && item.kind != BuildKind::Tool
                        && state.launcher().is_none()
                    {
                        return Err(unavailable("launcher"));
                    }
                }
                let names: std::collections::BTreeSet<_> = items.iter().map(|i| &i.name).collect();
                for name in names {
                    builds
                        .remove(name, state.launcher())
                        .map_err(|e| IpcError::new(ErrorCode::Internal, e))?;
                }
            }
            // Read before anything is removed: other profiles' records must survive.
            let timed = state.timed().map_err(store_err)?;
            for root in [
                state.homes.read().unwrap().clone(),
                state.built.read().unwrap().clone(),
            ]
            .into_iter()
            .flatten()
            {
                match std::fs::remove_dir_all(root.join(&p.name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(IpcError::new(ErrorCode::Internal, e.to_string())),
                }
            }
            // A failed deletion must leave surviving consent with its expiry record.
            state.profiles.delete(&p.name).map_err(store_err)?;
            let timed: Vec<_> = timed.into_iter().filter(|t| t.profile != p.name).collect();
            goblin_store::fsutil::write_json_atomic(&state.timed_path(), &timed, 0o600)
                .map_err(store_err)?;
            reply(&Empty {})
        }
        Method::ProfileSwitch(p) => {
            // One daemon serves one profile. Switching stores the choice and starts the
            // daemon again from nothing, so nothing of this profile (its session with
            // Muse, its conversations in memory, its access rules) is there for the next.
            let _changes = state.profile_changes.lock().unwrap();
            if state.profiles.active().as_deref() != Some(state.active().as_str()) {
                return Err(IpcError::new(ErrorCode::Busy, "profile switch in progress"));
            }
            if state.pair.lock().unwrap().is_some() {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "cancel pairing before switching profile",
                ));
            }
            if p.name == state.active() {
                return reply(&Empty {});
            }
            state.profiles.profile(&p.name).map_err(store_err)?;
            if state.call.lock().unwrap().is_some() {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "end the call before switching profile",
                ));
            }
            state.profiles.set_active(&p.name).map_err(store_err)?;
            let _ = state
                .bus
                .publish(&Event::ProfileChanged(ProfileChangedEvent {
                    profile: p.name,
                }));
            // Let the answer and the event leave first.
            let restarting = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                restarting.request_restart();
            });
            reply(&Empty {})
        }
        Method::ProfileSetSdkToken(p) => {
            let profile = state.profiles.profile(&p.name).map_err(store_err)?;
            // The file is deleted once read. One inside the data directory is another
            // profile's own token, which must stay where it is.
            let source = std::path::Path::new(&p.token_path);
            let real = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
            let data = state.profiles.layout().root();
            let data = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
            if real.starts_with(&data) {
                return Err(IpcError::new(
                    ErrorCode::Rejected,
                    "that file belongs to a profile; give a file of your own with the token in it",
                ));
            }
            profile.import_sdk_token_file(source).map_err(store_err)?;
            reply(&Empty {})
        }

        Method::PairStart(p) => {
            // Validate before claiming the window. A corrupt token or identity must
            // not leave every later attempt stuck at "Already pairing".
            let (name, profile, sdk_token, ble_name, cancel) = {
                let _changes = state.profile_changes.lock().unwrap();
                if state.profiles.active().as_deref() != Some(state.active().as_str()) {
                    return Err(IpcError::new(ErrorCode::Busy, "profile switch in progress"));
                }
                let name = p.profile.unwrap_or_else(|| state.active());
                let profile = state.profiles.profile(&name).map_err(store_err)?;
                // Not even with `force`: the old agent would stay linked beside the new one.
                if let Some(why) = crate::hermes::blocks_pairing(&profile) {
                    return Err(IpcError::new(ErrorCode::Rejected, why));
                }
                if profile.is_paired() && !p.force {
                    return Err(IpcError::new(
                        ErrorCode::Rejected,
                        format!("profile {name} is already paired"),
                    ));
                }
                let mut open = state.pair.lock().unwrap();
                if open.is_some() {
                    return Err(IpcError::new(ErrorCode::Busy, "Already pairing"));
                }
                let sdk_token = profile.sdk_token().map_err(store_err)?;
                let ble_name = profile.identity().map_err(store_err)?.ble_name();
                let cancel = Arc::new(tokio::sync::Notify::new());
                *open = Some(crate::pairing::PairHandle {
                    profile: name.clone(),
                    cancel: cancel.clone(),
                });
                (name, profile, sdk_token, ble_name, cancel)
            };
            let timeout_s = p
                .timeout_s
                .unwrap_or(crate::pairing::DEFAULT_WINDOW_S)
                .clamp(30, 1800);
            // The Muse app refuses a gadget that has no SDK token, after the Bluetooth
            // exchange and with a message that does not say what to do. So the token comes
            // first: from a file left on the card, or from the owner's phone through a
            // page this handheld serves while it waits.
            let agent_named = p.agent.is_some();
            let hermes = match p.agent.as_deref() {
                None | Some("muse") => false,
                // One agent to a profile: said now, not after the owner has pasted a link.
                Some("hermes") if !matches!(profile.pairing(), Ok(None)) => {
                    state.pair_closed();
                    return Err(IpcError::new(
                        ErrorCode::BadRequest,
                        "This profile is paired with a Muse. Unpair it first.",
                    ));
                }
                Some("hermes") => true,
                Some(_) => {
                    state.pair_closed();
                    return Err(IpcError::new(
                        ErrorCode::BadRequest,
                        "agent is muse or hermes",
                    ));
                }
            };
            if sdk_token.is_none() || hermes {
                let homes = state.homes.read().unwrap().clone();
                let dropped = !hermes
                    && homes
                        .as_deref()
                        .is_some_and(|h| h.join(crate::pairing::SDK_TOKEN_DROP).is_file());
                let page = if dropped {
                    None
                } else {
                    match crate::tokenpage::Page::for_network(crate::tokenpage::local_ip()).await {
                        Ok(page) => Some(page),
                        Err(why) => {
                            state.pair_closed();
                            return Err(IpcError::new(ErrorCode::Unavailable, why));
                        }
                    }
                };
                if let Some(page) = page {
                    crate::pairing::publish(
                        &state,
                        &name,
                        PairPhase::NeedToken,
                        Some(page.url.clone()),
                    );
                    let state = state.clone();
                    tokio::spawn(async move {
                        use crate::tokenpage::Outcome;
                        let wait = std::time::Duration::from_secs(u64::from(timeout_s));
                        let kind = match (hermes, agent_named) {
                            (true, _) => crate::tokenpage::For::Hermes,
                            (false, true) => crate::tokenpage::For::Muse,
                            (false, false) => crate::tokenpage::For::Any,
                        };
                        let taken = state.profiles.names().unwrap_or_default();
                        let served = page.serve(profile.clone(), cancel.clone(), wait, kind, taken);
                        // The name typed on the page, for the ways out that end here.
                        let mut typed_name = None;
                        let ended = match served.await {
                            Outcome::Saved(typed) => {
                                match crate::pairing::prepare(
                                    state.clone(),
                                    profile,
                                    timeout_s,
                                    cancel,
                                    typed.clone(),
                                ) {
                                    Ok((_, run)) => {
                                        run.await;
                                        return;
                                    }
                                    Err(why) => {
                                        typed_name = typed;
                                        (PairPhase::Failed, Some(why))
                                    }
                                }
                            }
                            Outcome::Hermes(link, typed) => {
                                let paired =
                                    crate::hermes::pair(&profile, state.device(), &link).await;
                                typed_name = typed;
                                match paired {
                                    Ok(()) => (PairPhase::Done, None),
                                    Err(why) => (PairPhase::Failed, Some(why)),
                                }
                            }
                            Outcome::Cancelled => (PairPhase::Cancelled, None),
                            Outcome::TimedOut => (PairPhase::Timeout, None),
                            Outcome::Failed(why) => (PairPhase::Failed, Some(why)),
                        };
                        // In this order: the window closes, the profile takes its name, the
                        // apps hear how it went (under that name), and only then does the
                        // daemon start again, if the profile it serves has new credentials
                        // or a new name.
                        state.pair_closed();
                        let was_active = name == state.active();
                        let (name, renamed) = name_after_pairing(&state, &name, typed_name);
                        let paired = ended.0 == PairPhase::Done;
                        crate::pairing::publish(&state, &name, ended.0, ended.1);
                        if was_active && (paired || renamed) {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            state.request_restart();
                        }
                    });
                    return reply(&PairStartResult { ble_name });
                }
                // An explicitly supplied file remains an alternative to the phone page.
                if let Err(why) = crate::pairing::take_dropped_sdk_token(&profile, homes.as_deref())
                {
                    state.pair_closed();
                    return Err(IpcError::new(ErrorCode::Rejected, why));
                }
            }
            match crate::pairing::prepare(state.clone(), profile, timeout_s, cancel, None) {
                Ok((opening, run)) => {
                    tokio::spawn(run);
                    reply(&PairStartResult {
                        ble_name: opening.ble_name,
                    })
                }
                Err(why) => {
                    state.pair_closed();
                    Err(IpcError::new(ErrorCode::Internal, why))
                }
            }
        }
        Method::PairCancel(_) => {
            if let Some(open) = state.pair.lock().unwrap().as_ref() {
                open.cancel.notify_one();
            }
            reply(&Empty {})
        }
        Method::PairUnpair(p) => {
            let name = p.profile.unwrap_or_else(|| state.active());
            if state
                .pair
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|o| o.profile == name)
            {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "pairing is open for this profile",
                ));
            }
            let profile = state.profiles.profile(&name).map_err(store_err)?;
            profile.delete_pairing().map_err(store_err)?;
            profile.delete_hermes_pairing().map_err(store_err)?;
            log::info!("profile '{name}' unpaired on this device");
            if name == state.active() {
                // Drop the session that was using the credentials.
                let restarting = state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    restarting.request_restart();
                });
            }
            reply(&Empty {})
        }

        Method::ChatList(_) => {
            let main_unread = state.main_unread.load(Ordering::Relaxed) as u32;
            let turns = &state;
            let chats = state
                .with_stores(|s| s.chats.chat_list(main_unread, |c| turns.pending_turns(c)))
                .map_err(store_err)?;
            reply(&ChatListResult { chats })
        }
        Method::ChatCreate(p) => {
            let id = crate::backend::random_uuid();
            let title = p.title.clone().unwrap_or_else(|| "Side chat".into());
            let chat = state
                .with_stores(|s| {
                    let chat = s.chats.add_side_chat(&id, &title, now_millis()).cloned();
                    // Persist now: a chat whose first send fails must still survive a restart.
                    let _ = s.chats.flush();
                    chat
                })
                .map_err(store_err)?;
            let _ = state.bus.publish(&Event::ChatListChanged(Empty {}));
            reply(&ChatCreateResult {
                chat: Chat {
                    id: chat.id,
                    title: chat.title,
                    last_text: chat.last_text,
                    last_at: Some(chat.updated_at),
                    unread: chat.unread,
                    pending_turns: 0,
                },
            })
        }
        Method::ChatForget(p) => {
            state
                .with_stores(|s| {
                    let r = s.chats.forget(&p.chat);
                    let _ = s.chats.flush();
                    r
                })
                .map_err(store_err)?;
            let _ = state.bus.publish(&Event::ChatListChanged(Empty {}));
            reply(&Empty {})
        }
        Method::ChatRename(p) => {
            let title = p.title.trim();
            if title.is_empty() || title.chars().count() > 60 {
                return Err(IpcError::new(
                    ErrorCode::BadRequest,
                    "a title is 1 to 60 characters",
                ));
            }
            state
                .with_stores(|s| {
                    let r = s.chats.rename_side_chat(&p.chat, title);
                    let _ = s.chats.flush();
                    r
                })
                .map_err(store_err)?;
            let _ = state.bus.publish(&Event::ChatListChanged(Empty {}));
            reply(&Empty {})
        }
        Method::ChatHistory(p) => {
            let limit = p.limit.unwrap_or(50) as usize;
            let cached_empty = p.before.is_none()
                && state
                    .with_stores(|s| s.chats.page(&p.chat, None, 1))
                    .map(|pg| pg.messages.is_empty())
                    .unwrap_or(false);
            if cached_empty && validate_chat_id(&p.chat).is_ok() {
                // First look at a chat on this device: seed the cache from Muse's history.
                if let Ok(history) = state.fetch_history(&p.chat, limit.min(100) as u32).await {
                    state.with_stores(|s| {
                        for h in history {
                            let _ = s.chats.upsert(h.message);
                        }
                        let _ = s.chats.flush();
                    });
                }
            }
            let mut page = state
                .with_stores(|s| s.chats.page(&p.chat, p.before.as_deref(), limit))
                .map_err(store_err)?;
            let mut more_at_muse = !page.complete;
            if p.before.is_some() && page.messages.len() < limit && !page.complete {
                // The cache ran out before the page was full: read further back from Muse.
                more_at_muse = state.load_older(&p.chat, limit - page.messages.len()).await;
                page = state
                    .with_stores(|s| s.chats.page(&p.chat, p.before.as_deref(), limit))
                    .map_err(store_err)?;
            }
            if p.before.is_none() {
                // Opening a chat also brings in any media it has that is not on the device yet.
                state.sync_media(p.chat.clone());
            }
            state.ensure_media(&p.chat, &page.messages);
            let suggestions = if p.before.is_none() {
                state
                    .suggestions
                    .lock()
                    .unwrap()
                    .get(&p.chat)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            reply(&ChatHistoryResult {
                messages: page.messages,
                complete: page.complete || (page.next.is_none() && !more_at_muse),
                next: page.next,
                suggestions,
            })
        }
        Method::ChatSend(p) => {
            let result = state.chat_send(&p.chat, &p.text).await?;
            reply(&result)
        }
        Method::ChatMarkSeen(p) => {
            if p.chat == "main" {
                state.main_unread.store(0, Ordering::Relaxed);
            } else {
                let _ = state.with_stores(|s| s.chats.set_unread(&p.chat, 0));
            }
            let _ = state.bus.publish(&Event::ChatUnread(ChatUnreadEvent {
                chat: p.chat,
                unread: 0,
            }));
            reply(&Empty {})
        }
        Method::ChatSuggest(p) => {
            let replies = check_suggestions(&p.replies)
                .map_err(|e| IpcError::new(ErrorCode::BadRequest, e))?;
            let chat = match p.chat {
                Some(c) => {
                    validate_chat_id(&c)
                        .map_err(|_| IpcError::new(ErrorCode::BadRequest, "invalid chat id"))?;
                    c
                }
                None => state.last_turn_chat.lock().unwrap().clone(),
            };
            log::info!("chat.suggest: {} replies for {chat}", replies.len());
            state.set_suggestions(&chat, replies);
            reply(&ChatSuggestResult { chat })
        }

        Method::VoicePttStart(p) => {
            // Hold the call lock until recording starts, in the same order as call start.
            let call = state.call.lock().unwrap();
            if call.is_some() {
                return Err(IpcError::new(ErrorCode::Busy, "on a call"));
            }
            if p.mode == PttMode::Dictate {
                return Err(unavailable("dictation"));
            }
            validate_chat_id(&p.chat)
                .map_err(|_| IpcError::new(ErrorCode::BadRequest, "invalid chat id"))?;
            let platform = state
                .platform
                .clone()
                .ok_or_else(|| unavailable("microphone"))?;
            let mut slot = state.recording.lock().unwrap();
            if slot.is_some() {
                return Err(IpcError::new(ErrorCode::Busy, "already recording"));
            }
            // Stop any reply being spoken: the speaker would be in the take.
            state.speaker.stop();
            let rec = crate::voice::start_capture(&platform, &p.chat)
                .map_err(|e| IpcError::new(ErrorCode::Unavailable, e))?;
            let note = format!("n-{}", now_millis());
            log::info!("voice note {note} recording");
            let level = rec.level.clone();
            *slot = Some((note.clone(), rec));
            // Publish the microphone level about 12 times a second while this take records.
            let meter_state = state.clone();
            let meter_note = note.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    if !meter_state.recording_active(&meter_note) {
                        break;
                    }
                    let rms = f32::from_bits(level.load(Ordering::Relaxed)) as f64;
                    let _ = meter_state.bus.publish(&Event::VoiceLevel(VoiceLevelEvent {
                        source: LevelSource::Mic,
                        rms,
                    }));
                }
            });
            reply(&VoicePttStartResult {
                note,
                max_seconds: Some(crate::voice::MAX_NOTE.as_secs() as u32),
            })
        }
        Method::VoicePttStop(p) => {
            let taken = {
                let mut slot = state.recording.lock().unwrap();
                match slot.as_ref() {
                    Some((id, _)) if *id == p.note => slot.take(),
                    _ => None,
                }
            };
            let Some((_, rec)) = taken else {
                // The note that was sent at its longest: the button was let go after.
                let mut capped = state.capped_note.lock().unwrap();
                if capped.as_deref() == Some(p.note.as_str()) {
                    *capped = None;
                    return reply(&VoicePttStopResult::default());
                }
                return Err(IpcError::new(ErrorCode::NotFound, "no such recording"));
            };
            let chat = rec.chat.clone();
            let secs = rec.elapsed().as_secs().max(1);
            let wav = tokio::task::spawn_blocking(move || rec.finish())
                .await
                .map_err(|e| IpcError::new(ErrorCode::Internal, e.to_string()))?
                .map_err(|e| IpcError::new(ErrorCode::Unavailable, e))?;
            if p.cancel {
                return reply(&VoicePttStopResult::default());
            }
            let Some(wav) = wav else {
                return Err(IpcError::new(ErrorCode::BadRequest, "too short to send"));
            };
            log::info!("voice note: {} s, {} bytes", secs, wav.len());
            let sent = state.voice_note_send(&chat, wav, secs).await?;
            reply(&VoicePttStopResult {
                turn: Some(sent.turn),
                text: None,
            })
        }
        Method::VoiceSpeak(p) => {
            state
                .platform
                .as_ref()
                .ok_or_else(|| unavailable("speaker"))?;
            state.speak(p.message_id);
            reply(&Empty {})
        }
        Method::VoiceStopSpeaking(_) => {
            state.speaker.stop();
            reply(&Empty {})
        }
        Method::VoiceCallStart(p) => {
            let platform = state.platform.clone().ok_or_else(|| unavailable("calls"))?;
            let Some(link) = state.link() else {
                return Err(IpcError::new(ErrorCode::NotConnected, "Not connected"));
            };
            let thread = match p.chat.as_deref() {
                None | Some("main") => None,
                Some(c) => {
                    validate_chat_id(c)
                        .map_err(|_| IpcError::new(ErrorCode::BadRequest, "invalid chat id"))?;
                    Some(c.to_owned())
                }
            };
            let mut slot = state.call.lock().unwrap();
            if slot.is_some() {
                return Err(IpcError::new(ErrorCode::Busy, "Already on a call"));
            }
            if state.recording.lock().unwrap().is_some() {
                return Err(IpcError::new(ErrorCode::Busy, "a voice note is recording"));
            }
            state.speaker.stop();
            let id = format!("c-{}", now_millis());
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            *slot = Some((id.clone(), tx));
            drop(slot);
            let observer: Arc<dyn crate::call::CallObserver> = Arc::new(CallEvents {
                bus: state.bus.clone(),
            });
            let st = state.clone();
            let call_id = id.clone();
            tokio::spawn(async move {
                log::info!("call {call_id} starting");
                let greeting = || st.settings.lock().unwrap().settings().call_greeting;
                let call = link
                    .call(
                        st.device(),
                        platform,
                        thread.clone(),
                        rx,
                        observer,
                        &greeting,
                    )
                    .await;
                let reason = match call {
                    Ok(r) => {
                        // What was said during the call is in Muse's history: bring it into
                        // this device's copy of the chat.
                        let chat = thread.unwrap_or_else(|| "main".into());
                        if let Ok(history) = st.fetch_history(&chat, 20).await {
                            for h in history {
                                let message = h.message;
                                let fresh = st.with_stores(|s| {
                                    matches!(
                                        s.chats.upsert(message.clone()),
                                        Ok(goblin_store::chats::Upsert::Appended)
                                    )
                                });
                                if fresh {
                                    let _ = st
                                        .bus
                                        .publish(&Event::ChatMessage(ChatMessageEvent { message }));
                                }
                            }
                            st.with_stores(|s| {
                                let _ = s.chats.flush();
                            });
                        }
                        r
                    }
                    Err(why) => why,
                };
                log::info!("call {call_id} ended: {reason}");
                st.call.lock().unwrap().take();
                let _ = st.bus.publish(&Event::VoiceCall(VoiceCallEvent {
                    state: CallState::Ended,
                    reason: Some(reason),
                    muted: false,
                    seconds: 0.0,
                }));
            });
            reply(&VoiceCallStartResult { call: id })
        }
        Method::VoiceCallEnd(_) => {
            let tx = state
                .call
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, tx)| tx.clone());
            let tx = tx.ok_or_else(|| IpcError::new(ErrorCode::NotFound, "no call"))?;
            let _ = tx.send(crate::call::CallCmd::End).await;
            reply(&VoiceCallEndResult {
                reason: "local_end".into(),
            })
        }
        Method::VoiceCallMute(p) => {
            let tx = state
                .call
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, tx)| tx.clone());
            let tx = tx.ok_or_else(|| IpcError::new(ErrorCode::NotFound, "no call"))?;
            let _ = tx.send(crate::call::CallCmd::Mute(p.muted)).await;
            reply(&Empty {})
        }
        Method::VoiceBargeIn(_) => {
            let tx = state
                .call
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, tx)| tx.clone());
            let tx = tx.ok_or_else(|| IpcError::new(ErrorCode::NotFound, "no call"))?;
            let _ = tx.send(crate::call::CallCmd::BargeIn).await;
            reply(&Empty {})
        }

        Method::NotifyList(p) => {
            let (items, unseen) = state
                .with_stores(|s| (s.inbox.list(p.limit.map(|l| l as usize)), s.inbox.unseen()));
            reply(&NotifyListResult { items, unseen })
        }
        Method::NotifySeen(p) => {
            state.with_stores(|s| {
                s.inbox.mark_seen(p.ids.as_deref());
                let _ = s.inbox.save();
            });
            let unseen = state.with_stores(|s| s.inbox.unseen());
            let _ = state
                .bus
                .publish(&Event::NotifyCount(NotifyCountEvent { unseen }));
            if unseen == 0 {
                state.background_reply.store(false, Ordering::Relaxed);
            }
            state.update_presence();
            reply(&Empty {})
        }
        Method::NotifyClear(_) => {
            state.with_stores(|s| {
                s.inbox.clear();
                let _ = s.inbox.save();
            });
            state.background_reply.store(false, Ordering::Relaxed);
            state.update_presence();
            reply(&Empty {})
        }

        Method::CommandsLog(p) => {
            let entries = state.with_stores(|s| s.commands.entries(p.limit.map(|l| l as usize)));
            reply(&CommandsLogResult { entries })
        }
        Method::BuildsList(_) => reply(&state.builds_list()),
        Method::BuildsDecide(p) => {
            state
                .build_decide(&p.name, p.request.as_deref(), p.approve)
                .map_err(|why| IpcError::new(ErrorCode::Rejected, why))?;
            reply(&state.builds_list())
        }
        Method::RequestsList(_) => reply(&state.requests_list()),
        Method::RequestsDecide(p) => {
            state
                .request_decide(&p.id, &p.answer)
                .map_err(|why| IpcError::new(ErrorCode::Rejected, why))?;
            reply(&state.requests_list())
        }
        Method::BuildsRemove(p) => {
            state
                .build_remove(&p.name)
                .map_err(|why| IpcError::new(ErrorCode::Rejected, why))?;
            reply(&state.builds_list())
        }
        Method::CommandsConsentGet(_) => reply(&state.consent()),
        Method::CommandsConsentSet(update) => {
            let consent = state
                .consent
                .lock()
                .unwrap()
                .update(&update)
                .map_err(store_err)?;
            let _ = state.bus.publish(&Event::ConsentChanged(consent));
            reply(&consent)
        }

        Method::SettingsGet(p) => {
            let values = state
                .settings
                .lock()
                .unwrap()
                .get(p.keys.as_deref())
                .map_err(store_err)?;
            reply(&SettingsValues { values })
        }
        Method::SettingsSet(p) => {
            let changed = state
                .settings
                .lock()
                .unwrap()
                .set(&p.values)
                .map_err(store_err)?;
            let _ = state
                .bus
                .publish(&Event::SettingsChanged(SettingsValues { values: changed }));
            state.update_presence();
            reply(&Empty {})
        }

        Method::PlatformInfo(_) => reply(&platform_info(&state)),
        Method::PlatformHealth(_) => reply(&platform_health(&state)),
        Method::PlatformVolume(p) => platform_percent(&state, p.set, PercentKind::Volume),
        Method::PlatformBrightness(p) => platform_percent(&state, p.set, PercentKind::Brightness),
        Method::PlatformRumbleTest(p) => {
            let platform = state
                .platform
                .as_ref()
                .ok_or_else(|| unavailable("rumble"))?;
            platform
                .rumble(p.ms.unwrap_or(200), 60)
                .map_err(|_| unavailable("rumble"))?;
            reply(&Empty {})
        }
        Method::PlatformAutostart(p) => {
            let platform = state
                .platform
                .as_ref()
                .ok_or_else(|| unavailable("autostart"))?;
            if let Some(set) = p.set {
                platform
                    .set_autostart(set)
                    .map_err(|_| unavailable("autostart"))?;
            }
            let enabled = platform.autostart_enabled().unwrap_or(false);
            reply(&PlatformAutostartResult { enabled })
        }
    }
}

enum PercentKind {
    Volume,
    Brightness,
}

fn platform_percent(
    state: &Arc<DaemonState>,
    set: Option<u8>,
    kind: PercentKind,
) -> Result<Value, IpcError> {
    let platform = state
        .platform
        .as_ref()
        .ok_or_else(|| unavailable("platform"))?;
    let percent = match kind {
        PercentKind::Volume => {
            if let Some(v) = set {
                platform.set_volume(v).map_err(|_| unavailable("volume"))?;
            }
            platform.volume().map_err(|_| unavailable("volume"))?
        }
        PercentKind::Brightness => {
            if let Some(v) = set {
                platform
                    .set_brightness(v)
                    .map_err(|_| unavailable("brightness"))?;
            }
            platform
                .brightness()
                .map_err(|_| unavailable("brightness"))?
        }
    };
    reply(&PercentResult { percent })
}

fn capabilities(state: &Arc<DaemonState>) -> Map<String, Value> {
    let mut caps = Map::new();
    caps.insert("chat".into(), json!(true));
    caps.insert("platform".into(), json!(state.platform.is_some()));
    caps
}

fn platform_info(state: &Arc<DaemonState>) -> Map<String, Value> {
    let mut m = Map::new();
    match &state.platform {
        Some(p) => {
            m.insert("id".into(), json!(p.id()));
            if let Ok(d) = p.display() {
                m.insert(
                    "display".into(),
                    json!({"width": d.width, "height": d.height}),
                );
            }
        }
        None => {
            m.insert("id".into(), json!("none"));
        }
    }
    m
}

fn platform_health(state: &Arc<DaemonState>) -> Map<String, Value> {
    let mut health = match &state.platform {
        Some(p) => p.health(),
        None => goblin_platform::power::read_health(None),
    };
    // Storage that matters is where this daemon keeps its data (the SD card on handhelds),
    // not the root filesystem.
    if let Some(d) = goblin_platform::power::disk_usage_at(state.profiles.layout().root()) {
        health.disk_gb = Some(d);
    }
    serde_json::to_value(health)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn store_err(e: goblin_store::StoreError) -> IpcError {
    use goblin_store::StoreError;
    match e {
        StoreError::NotFound(_) => IpcError::new(ErrorCode::NotFound, e.to_string()),
        StoreError::AlreadyExists(_) => IpcError::new(ErrorCode::BadRequest, e.to_string()),
        StoreError::InvalidId(_) | StoreError::InvalidName(_) => {
            IpcError::new(ErrorCode::BadRequest, e.to_string())
        }
        _ => IpcError::new(ErrorCode::Internal, e.to_string()),
    }
}

/// LED ceiling (of the driver's 60) while presence is shown.
const PRESENCE_BRIGHTNESS: u32 = 30;
/// Vibration for a new reply.
const REPLY_RUMBLE_MS: u32 = 180;
const REPLY_RUMBLE_STRENGTH: u8 = 60;

/// Longest reply preview kept in a notice.
const NOTICE_PREVIEW_CHARS: usize = 140;

/// Title ("Muse replied" / "Muse replied in <side chat>") and a one-line preview of the newest
/// assistant message in `chat`, for the inbox.
fn reply_notice_text(state: &Arc<DaemonState>, chat: &str) -> (String, Option<String>) {
    state.with_stores(|s| {
        let title = s
            .chats
            .side_chats()
            .iter()
            .find(|c| c.id == chat)
            .map(|c| format!("Muse replied in {}", c.title))
            .unwrap_or_else(|| "Muse replied".into());
        let body = s.chats.messages(chat).ok().and_then(|m| {
            m.iter()
                .rev()
                .find(|m| m.role == Role::Assistant && !m.text.trim().is_empty())
                .map(|m| preview(&m.text, NOTICE_PREVIEW_CHARS))
        });
        (title, body)
    })
}

/// The text on one line, cut to `max` characters with an ellipsis.
fn preview(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut cut: String = flat.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

/// Posts a notification and emits `notify.new`/`notify.count`.
pub fn post_notice(state: &Arc<DaemonState>, notice: NewNotice) {
    let kind = notice.kind;
    let (notice, unseen) = state.with_stores(|s| {
        let n = s.inbox.push(notice);
        let _ = s.inbox.save();
        (n, s.inbox.unseen())
    });
    let _ = state
        .bus
        .publish(&Event::NotifyNew(NotifyNewEvent { notice }));
    let _ = state
        .bus
        .publish(&Event::NotifyCount(NotifyCountEvent { unseen }));
    if kind == NoticeKind::Reply && state.in_background() {
        state.background_reply.store(true, Ordering::Relaxed);
    }
    state.update_presence();
    if kind == NoticeKind::Reply {
        state.reply_haptic();
    }
}

/// Builds [`CacheLimits`] used by the daemon.
pub fn cache_limits() -> CacheLimits {
    CacheLimits::default()
}

#[cfg(test)]
mod card_tests {
    use super::card_answers;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn an_approvals_no_button_is_only_ever_no() {
        let offered = pairs(&[
            ("once", "Run once"),
            ("session", "This session"),
            ("always", "Always"),
            ("deny", "No"),
        ]);
        let got = card_answers("approval", &offered);
        let buttons: Vec<(&str, &str)> = got
            .iter()
            .map(|(b, c, _)| (b.as_str(), c.as_str()))
            .collect();
        assert_eq!(
            buttons,
            [("yes", "once"), ("hour", "session"), ("no", "deny")]
        );
        // Deny only: the no button, and nothing on yes.
        let got = card_answers("approval", &pairs(&[("deny", "No")]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "no");
        // An approval that offers no deny puts nothing on the no button.
        let got = card_answers("approval", &pairs(&[("once", "Run once")]));
        assert!(got.iter().all(|(b, _, _)| b != "no"));
    }

    #[test]
    fn a_question_offers_up_to_three_choices_in_order() {
        let offered = pairs(&[
            ("c0", "Apples"),
            ("c1", "Pears"),
            ("c2", "Plums"),
            ("c3", "Figs"),
        ]);
        let got = card_answers("clarify", &offered);
        let buttons: Vec<(&str, &str)> = got
            .iter()
            .map(|(b, c, _)| (b.as_str(), c.as_str()))
            .collect();
        assert_eq!(buttons, [("yes", "c0"), ("no", "c1"), ("hour", "c2")]);
        assert!(card_answers("clarify", &[]).is_empty());
        let long = card_answers("clarify", &pairs(&[("c0", &"x".repeat(80))]));
        assert_eq!(long[0].2.chars().count(), 24);
    }
}

#[cfg(test)]
mod notice_tests {
    use super::preview;

    #[test]
    fn preview_flattens_and_cuts() {
        assert_eq!(preview("Got it.\n\n  Done", 140), "Got it. Done");
        let long = "x".repeat(200);
        let p = preview(&long, 10);
        assert_eq!(p.chars().count(), 10);
        assert!(p.ends_with('…'));
    }
}

/// A daemon state over a fresh temporary data directory, for tests.
#[cfg(test)]
pub(crate) fn test_state(
    platform: Option<Arc<goblin_platform::Platform>>,
) -> (tempfile::TempDir, Arc<DaemonState>) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = goblin_store::Layout::new(tmp.path().join("data"));
    let profiles = ProfileStore::open(layout.clone()).unwrap();
    let profile = profiles.create("test").unwrap();
    let settings = SettingsStore::open(&layout.settings_file()).unwrap();
    let consent = ConsentStore::open(&profile.consent_file()).unwrap();
    let stores = Stores {
        chats: ChatCache::open(&profile.chats_dir(), cache_limits()).unwrap(),
        inbox: Inbox::open(&profile.notifications_file(), 10).unwrap(),
        commands: CommandLog::in_memory(10),
    };
    let executor = Arc::new(Executor::new(
        exec::RunAs::current(),
        Vec::new(),
        Arc::new(|| json!({})),
    ));
    let device = DeviceDescription {
        node_id: "test".into(),
        display_name: "test".into(),
        version: "0".into(),
        commands: executor.specs(),
    };
    let state = build_state(
        "0".into(),
        Arc::new(EventBus::new("test")),
        profiles,
        platform,
        settings,
        consent,
        "test".into(),
        stores,
        executor,
        device,
    );
    (tmp, state)
}

#[cfg(test)]
mod suggestion_tests {
    use super::check_suggestions;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn limits() {
        assert_eq!(
            check_suggestions(&v(&["Yes", " Not  today "])).unwrap(),
            v(&["Yes", "Not today"])
        );
        // Four short choices are fine; the total is what counts.
        assert_eq!(
            check_suggestions(&v(&["Yes", "No", "Later", "Why?"]))
                .unwrap()
                .len(),
            4
        );
        assert!(check_suggestions(&v(&["a", "b", "c", "d", "e", "f", "g"]))
            .unwrap_err()
            .contains("at most 6"));
        // One long choice within the total is fine; three long ones are not.
        assert!(check_suggestions(&v(&["This one is long but still fits in the row"])).is_ok());
        assert!(check_suggestions(&v(&[
            "I'd like the first option, please",
            "Show me the second one instead",
            "Neither of those"
        ]))
        .unwrap_err()
        .contains("at most 60 characters"));
        assert!(check_suggestions(&v(&["Yes", "yes"])).is_err());
        assert!(check_suggestions(&v(&[""])).is_err());
        assert!(check_suggestions(&[]).unwrap().is_empty());
    }
}

#[cfg(test)]
mod voice_tests {
    use super::*;

    #[test]
    fn idle_status_stops_working_without_completing_the_request() {
        let (_tmp, state) = state();
        let mut working = false;
        let mut replying = false;
        for (code, expected) in [
            ("working", TurnState::Working),
            ("idle", TurnState::Acked),
            ("working", TurnState::Working),
            ("online", TurnState::Acked),
            ("tool", TurnState::Working),
            ("completed", TurnState::Acked),
        ] {
            apply_turn_event(
                &state,
                "main",
                "turn",
                &TurnEvent::Status(crate::backend::AgentStatus {
                    activity_code: Some(code.into()),
                    activity_text: None,
                }),
                &mut working,
                &mut replying,
            );
            assert_eq!(
                state.turns.lock().unwrap().get("turn").unwrap().state,
                expected
            );
            assert_eq!(state.pending_turns("main"), 1);
        }
    }

    fn state() -> (tempfile::TempDir, Arc<DaemonState>) {
        super::test_state(None)
    }

    #[test]
    fn a_profile_takes_the_name_typed_while_pairing_unless_it_cannot() {
        let (_tmp, state) = state();
        state.profiles.create("profile-2").unwrap();
        state.profiles.create("den").unwrap();
        // Nothing typed, or a name that is taken by now: it keeps the one it has.
        let unchanged = ("profile-2".to_owned(), false);
        assert_eq!(name_after_pairing(&state, "profile-2", None), unchanged);
        let kept = name_after_pairing(&state, "profile-2", Some("Den".into()));
        assert_eq!(kept, unchanged);
        assert!(state.profiles.profile("profile-2").is_ok());
        // A free one: the profile has it, and the old name is gone.
        let named = name_after_pairing(&state, "profile-2", Some("study".into()));
        assert_eq!(named, ("study".to_owned(), true));
        assert!(state.profiles.profile("study").is_ok());
        assert!(state.profiles.profile("profile-2").is_err());
        // Not while a pairing is open for it, whoever asks: its folder is in use by name.
        *state.pair.lock().unwrap() = Some(crate::pairing::PairHandle {
            profile: "study".into(),
            cancel: Arc::new(tokio::sync::Notify::new()),
        });
        let held = name_after_pairing(&state, "study", Some("attic".into()));
        assert_eq!(held, ("study".to_owned(), false));
        assert!(
            rename_profile(&state, "den", "attic", true).is_ok(),
            "another profile may"
        );
    }

    /// A pairing that ends (here: no network, so at once and badly) has given the profile
    /// the name typed for it by the time it is over, and has let go of the window.
    #[tokio::test]
    async fn a_pairing_that_ends_has_named_its_profile() {
        let (_tmp, state) = state();
        let profile = state.profiles.create("profile-2").unwrap();
        *state.pair.lock().unwrap() = Some(crate::pairing::PairHandle {
            profile: "profile-2".into(),
            cancel: Arc::new(tokio::sync::Notify::new()),
        });
        let (_, run) = crate::pairing::prepare(
            state.clone(),
            profile,
            30,
            Arc::new(tokio::sync::Notify::new()),
            Some("den".into()),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), run)
            .await
            .expect("the pairing ends");
        assert!(state.pair.lock().unwrap().is_none());
        assert!(state.profiles.profile("den").is_ok());
        assert!(state.profiles.profile("profile-2").is_err());
    }

    #[test]
    fn the_lights_take_the_colour_chosen() {
        use goblin_store::settings::ChatTheme as C;
        let all = [
            C::Avatar,
            C::Default,
            C::Monochrome,
            C::Beige,
            C::Blue,
            C::Purple,
            C::Pink,
            C::Orange,
            C::Green,
        ];
        // The usual choice is the blue the lights always were.
        assert_eq!(light_colour(C::Default), goblin_platform::Rgb(0, 120, 255));
        assert_eq!(light_colour(C::Avatar), light_colour(C::Default));
        // Every other choice is its own colour, and bright enough for an LED to show.
        let mut seen = Vec::new();
        for choice in all.into_iter().skip(1) {
            let colour = light_colour(choice);
            assert!(!seen.contains(&colour), "{choice:?} repeats a colour");
            assert!(
                colour.0.max(colour.1).max(colour.2) == 255,
                "{choice:?} is dim"
            );
            seen.push(colour);
        }
    }

    #[test]
    fn a_voice_note_shows_what_was_heard_once_the_agent_says() {
        let (_tmp, state) = state();
        let note = Message {
            id: "u1".into(),
            chat: "main".into(),
            role: Role::User,
            text: "Voice note \u{b7} 3 s".into(),
            rich: None,
            choices: Vec::new(),
            attachments: None,
            reaction: None,
            at: 1,
            state: MessageState::Sent,
            origin: Origin::Gadget,
        };
        let mut reply = note.clone();
        (reply.id, reply.role, reply.text) = ("a1".into(), Role::Assistant, "Noon.".into());
        state.with_stores(|s| {
            s.chats.upsert(note).unwrap();
            s.chats.upsert(reply).unwrap();
        });
        let placeholder = "Voice note \u{b7} 3 s";
        // Nothing heard, or a message that is not the owner's note: left as it is.
        state.voice_note_heard("main", "u1", placeholder, "  ".into());
        state.voice_note_heard("main", "a1", "Noon.", "not the agent's to rewrite".into());
        assert_eq!(
            state.message_text("u1").as_deref(),
            Some("Voice note \u{b7} 3 s")
        );
        assert_eq!(state.message_text("a1").as_deref(), Some("Noon."));
        state.voice_note_heard("main", "u1", placeholder, "What time is it?".into());
        assert_eq!(
            state.message_text("u1").as_deref(),
            Some("What time is it?")
        );
        // Once: what was heard, like anything the owner typed, is not rewritten.
        state.voice_note_heard("main", "u1", placeholder, "Yes, install it".into());
        assert_eq!(
            state.message_text("u1").as_deref(),
            Some("What time is it?")
        );
        assert_eq!(state.message_text("missing"), None);
    }

    #[test]
    fn finished_recorder_releases_its_slot() {
        let (_tmp, state) = state();
        *state.recording.lock().unwrap() =
            Some(("note".into(), crate::voice::Recording::finished_for_test()));
        assert!(!state.recording_active("note"));
        assert!(state.recording.lock().unwrap().is_none());
    }

    /// A note that ran its whole length is put away too, on the way to being sent (here
    /// there is nothing to send to; it must neither stay in the slot nor stop the daemon).
    #[tokio::test]
    async fn a_note_at_its_limit_releases_its_slot() {
        let (_tmp, state) = state();
        let rec = crate::voice::Recording::at_limit_for_test();
        assert!(rec.at_limit());
        assert!(!crate::voice::Recording::finished_for_test().at_limit());
        *state.recording.lock().unwrap() = Some(("note".into(), rec));
        // It is on its way (there is audio to send), not failed; and is remembered, so that
        // letting the button go afterwards is not answered as a mistake.
        assert_eq!(state.put_away_recording("note"), Some(NoteState::Sending));
        assert!(state.recording.lock().unwrap().is_none());
        assert_eq!(state.capped_note.lock().unwrap().as_deref(), Some("note"));
        assert_eq!(state.put_away_recording("note"), None, "once");
        tokio::task::yield_now().await;
        // A capture that broke is failed, as before, and not remembered as sent.
        *state.recording.lock().unwrap() = Some((
            "broken".into(),
            crate::voice::Recording::finished_for_test(),
        ));
        assert_eq!(state.put_away_recording("broken"), Some(NoteState::Failed));
        assert_eq!(state.capped_note.lock().unwrap().as_deref(), Some("note"));
    }

    /// The note is remembered as sent before its slot is seen empty, so a release that
    /// lands just then is never answered "no such recording". Shown by holding the
    /// remembering back: the slot must stay locked for as long.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_note_at_its_limit_is_remembered_before_its_slot_is_free() {
        let (_tmp, state) = state();
        *state.recording.lock().unwrap() =
            Some(("note".into(), crate::voice::Recording::at_limit_for_test()));
        let held = state.capped_note.lock().unwrap();
        let putting = {
            let state = state.clone();
            tokio::spawn(async move { state.put_away_recording("note") })
        };
        // Until it waits for what is held here; the slot is locked from then on.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.recording.try_lock().is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "never locked the slot"
            );
            assert!(!putting.is_finished(), "put away without remembering");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            state.recording.try_lock().is_err(),
            "the slot was let go first"
        );
        drop(held);
        assert_eq!(putting.await.unwrap(), Some(NoteState::Sending));
        assert_eq!(state.capped_note.lock().unwrap().as_deref(), Some("note"));
        assert!(state.recording.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn last_app_disconnect_ends_the_call() {
        let (_tmp, state) = state();
        state.ui_opened(1);
        state.ui_opened(2);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        *state.call.lock().unwrap() = Some(("call-test".into(), tx));
        state.ui_closed(1);
        assert!(rx.try_recv().is_err());
        state.ui_closed(2);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await,
            Ok(Some(crate::call::CallCmd::End))
        ));
    }

    #[tokio::test]
    async fn voice_note_is_busy_during_a_call() {
        let (_tmp, state) = state();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        *state.call.lock().unwrap() = Some(("call-test".into(), tx));
        let error = dispatch(
            state.clone(),
            Method::VoicePttStart(VoicePttStartParams {
                chat: "main".into(),
                mode: PttMode::Note,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert_eq!(error.message, "on a call");
        assert!(state.recording.lock().unwrap().is_none());
    }
}
