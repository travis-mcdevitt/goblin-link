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
use muse_link::{CommandRunner, DeviceDescription, LinkSession, MuseTurn, TurnEvent};

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
    session: RwLock<Option<LinkSession>>,
    main_unread: AtomicU64,
    /// The current VM session's config (with its secret token, memory only), for voice
    /// sessions.
    session_config: Mutex<Option<muse_link::SessionConfig>>,
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
    profile_changes: Mutex<()>,
    /// When the owner last said no to changing each setting (Unix seconds).
    declined: Mutex<HashMap<String, u64>>,
    /// The pairing window that is open, if one is.
    pair: Mutex<Option<crate::pairing::PairHandle>>,
    /// The call in progress: its id and the channel to control it.
    call: Mutex<Option<(String, tokio::sync::mpsc::Sender<crate::call::CallCmd>)>>,
    /// The voice note being recorded, with its id.
    recording: Mutex<Option<(String, crate::voice::Recording)>>,
    /// Plays spoken replies (one at a time, see `speak_lock`).
    speaker: Arc<crate::voice::Speaker>,
    speak_lock: Arc<tokio::sync::Mutex<()>>,
    /// Voice-note turns whose reply should be spoken, with the newest assistant message id
    /// in that chat before the note (so an older reply is never spoken by mistake).
    voice_turns: Mutex<HashMap<String, Option<String>>>,
    /// Presence LEDs: the long-lived helper (it saves the firmware's LED settings on first use
    /// and restores them on release) and the `leds.mode` it was built for.
    presence: Mutex<
        Option<(
            goblin_platform::PresenceLeds<goblin_platform::Leds>,
            LedMode,
        )>,
    >,
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

    /// Replaces the active session handle.
    pub fn set_session(&self, session: Option<LinkSession>) {
        *self.session.write().unwrap() = session;
    }

    /// The current session handle, if connected.
    pub fn session(&self) -> Option<LinkSession> {
        self.session.read().unwrap().clone()
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
        let Some(session) = self.session() else {
            return;
        };
        match session.request("GET", "/identity", &[], &[]).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                let identity = body.get("result").unwrap_or(&body);
                let name = identity
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty() && n.chars().count() <= 64);
                if let Some(info) = crate::avatar::parse(identity) {
                    self.sync_avatar(info);
                }
                if let Some(name) = name {
                    let state = {
                        let mut link = self.link.write().unwrap();
                        link.agent_name = Some(name.to_owned());
                        link.state
                    };
                    // Clients refresh `daemon.status` on link events; republish so a client that
                    // connected before identity arrived picks up the agent's name.
                    self.set_link_state(state, None);
                }
            }
            Ok((status, _)) => log::info!("identity returned HTTP {status}"),
            Err(e) => log::info!("could not read identity: {e}"),
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
        let (mode, never_touch) = {
            let settings = self.settings.lock().unwrap();
            let s = settings.settings();
            (s.leds_mode, s.leds_never_touch)
        };
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
        // Rebuild when the mode changed (it sets the connected style).
        if slot.as_ref().is_some_and(|(_, m)| *m != mode) {
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
                ..PresenceOptions::default()
            };
            let zones = cfg.presence_zones.clone();
            let presence = goblin_platform::PresenceLeds::new(leds, zones, options)
                .with_accent(cfg.accent_zones.clone());
            *slot = Some((presence, mode));
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

    fn recording_active(&self, note: &str) -> bool {
        let ended = {
            let mut slot = self.recording.lock().unwrap();
            let Some((id, rec)) = slot.as_mut() else {
                return false;
            };
            if id != note {
                return false;
            }
            if !rec.finished() {
                return true;
            }
            slot.take().map(|(id, rec)| {
                let seconds = rec.elapsed().as_secs_f64();
                drop(rec); // restore gain while the slot is still locked
                (id, seconds)
            })
        };
        if let Some((id, seconds)) = ended {
            let _ = self.bus.publish(&Event::VoiceNote(VoiceNoteEvent {
                note: id,
                state: NoteState::Failed,
                seconds,
            }));
        }
        false
    }

    /// Whether no app is open.
    pub fn in_background(&self) -> bool {
        self.ui_clients.lock().unwrap().is_empty()
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

    /// Prepares the agent's avatar (image and state clips) for the UI and records where it is.
    /// A revision already on the device is used as it is. Best effort: failures are logged and
    /// the UI keeps its drawn mascot.
    fn sync_avatar(self: &Arc<Self>, info: crate::avatar::AvatarInfo) {
        let state = self.clone();
        tokio::spawn(async move {
            let Ok(profile) = state.profiles.profile(&state.active()) else {
                return;
            };
            let root = profile.dir().join("avatar");
            let dir = crate::avatar::revision_dir(&root, &info.revision);
            if !crate::avatar::ready(&dir) {
                if std::fs::create_dir_all(&dir).is_err() {
                    return;
                }
                let Some(session) = state.session() else {
                    return;
                };
                if let Some(path) = &info.image {
                    match session
                        .fetch_workspace_file(path, crate::avatar::MAX_BYTES)
                        .await
                    {
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
                    match session
                        .fetch_workspace_file(path, crate::avatar::MAX_BYTES)
                        .await
                    {
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
                    return; // nothing usable
                }
                if let Err(e) = crate::avatar::write_meta(&dir, &frames) {
                    log::info!("{e}");
                    return;
                }
                log::info!("avatar {} ready ({} clip(s))", info.revision, frames.len());
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
                            || (m.text.contains("Widget — open Muse") && m.text != h.message.text)
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
            let Some(session) = state.session() else {
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
                let data = match session.fetch_workspace_file(&r.path, max).await {
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

    /// Records the VM session config (see `session_config`).
    pub fn set_session_config(&self, config: Option<muse_link::SessionConfig>) {
        *self.session_config.lock().unwrap() = config;
    }

    /// Opens a voice session to the current VM, as the SDK's `voice-call` does: its own
    /// connection with `app_id = hatch-web`, no device registration and no commands, so the
    /// daemon's registered session is untouched.
    async fn open_voice_session(&self) -> Result<LinkSession, IpcError> {
        let mut config = self
            .session_config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| IpcError::new(ErrorCode::NotConnected, "Not connected"))?;
        let device = self.device();
        config.app_id = Some(VOICE_APP_ID.into());
        config.register_device = false;
        config.invoke_slots = None;
        config.device = DeviceDescription {
            node_id: device.node_id,
            display_name: "Muse voice".into(),
            version: device.version,
            commands: json!({}),
        };
        LinkSession::connect(config, Arc::new(RefuseCommands))
            .await
            .map_err(|e| {
                IpcError::new(ErrorCode::Unavailable, format!("No voice connection ({e})"))
            })
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
            let Some(session) = state.session() else {
                return;
            };
            let _turn = state.speak_lock.clone().lock_owned().await;
            if generation != state.speaker.generation() {
                return;
            }
            let mut mp3 = Vec::new();
            match session.stream_tts(&message_id).await {
                Ok(mut stream) => {
                    while let Some(chunk) = stream.next_chunk().await {
                        if generation != state.speaker.generation() {
                            return;
                        }
                        match chunk {
                            Ok(c) => mp3.extend_from_slice(&c),
                            Err(e) => {
                                log::info!("reply speech stream ended early: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::info!("no speech for {message_id}: {e}");
                    return;
                }
            }
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
        let paired = self
            .profiles
            .profile(&name)
            .map(|p| matches!(p.pairing(), Ok(Some(_))))
            .unwrap_or(false);
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
                name: link.agent_name.clone().unwrap_or_else(|| "Muse".into()),
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
        session: RwLock::new(None),
        main_unread: AtomicU64::new(0),
        session_config: Mutex::new(None),
        ui_clients: Mutex::new(std::collections::HashSet::new()),
        background_reply: std::sync::atomic::AtomicBool::new(false),
        suggestions: Mutex::new(HashMap::new()),
        last_turn_chat: Mutex::new("main".into()),
        homes: RwLock::new(None),
        built: RwLock::new(None),
        asks: Mutex::new(Vec::new()),
        profile_changes: Mutex::new(()),
        declined: Mutex::new(HashMap::new()),
        pair: Mutex::new(None),
        call: Mutex::new(None),
        recording: Mutex::new(None),
        speaker: Arc::new(crate::voice::Speaker::default()),
        speak_lock: Arc::new(tokio::sync::Mutex::new(())),
        voice_turns: Mutex::new(HashMap::new()),
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

    fn timed(&self) -> Vec<crate::asks::Timed> {
        std::fs::read(self.timed_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_timed(&self, timed: &[crate::asks::Timed]) {
        let path = self.timed_path();
        if timed.is_empty() {
            let _ = std::fs::remove_file(path);
        } else if let Ok(text) = serde_json::to_vec(timed) {
            let _ = std::fs::write(path, text);
        }
    }

    /// `handheld.settings`: every setting Muse can see, as it is now.
    pub fn settings_report(&self) -> Value {
        let now = now_millis() / 1000;
        let profile = self.active();
        let timed = self.timed();
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
                    id: format!("install:{}", b.name),
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

    /// `handheld.ask`: puts a change to a setting to the owner. Changes nothing itself.
    fn ask(self: &Arc<Self>, params: &Map<String, Value>) -> Result<Value, String> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
        let (setting, value, reason) = (text("setting"), text("value"), text("reason"));
        let now = now_millis() / 1000;
        let declined = self.declined.lock().unwrap().get(setting).copied();
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
        if let Some(name) = id.strip_prefix("install:") {
            return match answer {
                "yes" => self.build_decide(name, true),
                "no" => self.build_decide(name, false),
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
                self.declined.lock().unwrap().insert(s.id.to_owned(), now);
                format!("No to \u{201c}{label}\u{201d}.")
            }
            "hour" if !s.widens(&ask.from, &ask.to) => {
                self.asks.lock().unwrap().push(ask);
                return Err("only a permission being widened can be given for an hour".into());
            }
            "yes" | "hour" => {
                let before = self.setting_value(s);
                self.setting_apply(s, &ask.to)?;
                let mut timed = self.timed();
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
                self.save_timed(&timed);
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
        let mut timed = self.timed();
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
        self.save_timed(&kept);
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

    /// The owner's answer to an install: does it, and says so to Muse for the owner.
    pub fn build_decide(self: &Arc<Self>, name: &str, approve: bool) -> Result<(), String> {
        let asked = self
            .builds_list()
            .items
            .into_iter()
            .find(|i| i.name == name && i.state == BuildState::Pending);
        if approve {
            self.build_approve(name).map(drop)?;
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
    pub fn build_approve(self: &Arc<Self>, name: &str) -> Result<BuildItem, String> {
        let builds = self.builds().ok_or("this device does not keep builds")?;
        let item = builds.approve(
            name,
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
                    let item = self.build_approve(&item.name).inspect_err(|_| {
                        self.requests_changed();
                    })?;
                    let note = if item.kind == BuildKind::Tool {
                        "Installed. The device reconnects in a moment; the tool is callable after that."
                    } else {
                        "Installed and listed in the launcher."
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
        let text = text.as_str();
        let session = self
            .session()
            .ok_or_else(|| IpcError::new(ErrorCode::NotConnected, "Not connected"))?;
        let session_id = if chat == "main" {
            None
        } else {
            Some(chat.to_owned())
        };

        // Open a subscription first, so the reply stream is in place. A side chat Muse does not
        // know yet answers 404: the message itself creates it, so send first and subscribe after
        // (as the reference's `send_and_wait` does).
        let mut early_sub = match session.subscribe_chat(session_id.as_deref()).await {
            Ok(sub) => Some(sub),
            Err(muse_link::LinkError::HttpStatus { status: 404, .. }) if session_id.is_some() => {
                None
            }
            Err(e) => return Err(map_link_err(e)),
        };
        let fresh = early_sub.is_none();
        let sent = match &payload {
            Payload::Text(t) => {
                session
                    .send_chat(t, session_id.as_deref(), "text", None)
                    .await
            }
            // Speech-to-text by default: Muse transcribes the note and replies as usual. With
            // voice-to-voice on, ask for a voice-modality reply, which the TTS stream can speak.
            Payload::Voice { wav, .. } => {
                let modality = self.voice_to_voice().then_some("voice");
                session
                    .send_voice(wav, session_id.as_deref(), "", modality, None)
                    .await
            }
        };
        let ack = match sent {
            Ok(a) => a,
            Err(muse_link::LinkError::SessionEnded) | Err(muse_link::LinkError::NotConnected) => {
                // Delivery is ambiguous: never auto-resend.
                return Err(IpcError::new(
                    ErrorCode::Uncertain,
                    "the message may or may not have been delivered; it was not resent",
                ));
            }
            Err(e) => return Err(map_link_err(e)),
        };

        let now = self.now_secs();
        // Seed last_activity with the current time: `MuseTurn::new` uses 0.0, which makes a new
        // turn look quiet (and so complete) before Muse has said anything.
        let mut turn = MuseTurn::with_options(session_id.clone(), false, None, 0.0, now);
        let held = turn
            .acknowledge(&ack, false, now)
            .map_err(|e| IpcError::new(ErrorCode::Internal, e.to_string()))?;
        let user_mid = ack
            .get("result")
            .and_then(|r| r.get("message_id"))
            .or_else(|| ack.get("message_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
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

        // A fresh side chat exists once Muse has acknowledged the message; subscribe now.
        let mut sub = match early_sub.take() {
            Some(sub) => sub,
            None => {
                let mut opened = None;
                for attempt in 0..5u64 {
                    match session.subscribe_chat(session_id.as_deref()).await {
                        Ok(sub) => {
                            opened = Some(sub);
                            break;
                        }
                        Err(muse_link::LinkError::HttpStatus { status: 404, .. }) => {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                300 * (attempt + 1),
                            ))
                            .await;
                        }
                        Err(e) => {
                            log::warn!("could not follow the new side chat: {e}");
                            break;
                        }
                    }
                }
                match opened {
                    Some(sub) => sub,
                    None => {
                        // Sent, but replies cannot be followed: report honestly.
                        self.record_turn(&turn_id, chat, TurnState::Uncertain);
                        return Ok(ChatSendResult {
                            turn: turn_id,
                            message_id: (!user_mid.is_empty()).then_some(user_mid),
                        });
                    }
                }
            }
        };
        if fresh {
            log::info!("created side chat {chat} with its first message");
        }

        // Follow the turn in the background.
        let state = self.clone();
        let chat_owned = chat.to_owned();
        let follow_turn_id = turn_id.clone();
        let user_mid_follow = user_mid.clone();
        tokio::spawn(async move {
            follow_turn(
                state,
                chat_owned,
                follow_turn_id,
                turn,
                held,
                &mut sub,
                FollowCtx {
                    user_mid: user_mid_follow,
                    fresh,
                },
            )
            .await;
        });

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

/// Port of the reference's `_ReplyReader` routing (service.py): on a shared chat, only the
/// events after this turn's own `message.user` (until another user message), or ones linked to
/// this turn's message or replies, reach the turn. Other people's messages on the same chat must
/// not be mistaken for the answer.
struct ReplyRouter {
    after_note: bool,
}

/// A non-empty string field (Muse sends absent ids as "").
fn nonempty_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

impl ReplyRouter {
    fn admits(&mut self, turn: &MuseTurn, event: &Value) -> bool {
        let Some(payload) = event.get("payload").filter(|p| p.is_object()) else {
            return false;
        };
        if event.get("type").and_then(Value::as_str) != Some("event") {
            return false;
        }
        let chat = nonempty_str(payload.get("session_id"))
            .or_else(|| nonempty_str(event.get("session_id")));
        if let (Some(chat), Some(mine)) = (chat, turn.session_id.as_deref()) {
            if chat != mine {
                return false;
            }
        }
        let message_id = nonempty_str(payload.get("message_id"))
            .or_else(|| nonempty_str(event.get("message_id")))
            .or_else(|| nonempty_str(payload.get("id")));
        if event.get("event").and_then(Value::as_str) == Some("message.user") {
            self.after_note = message_id.is_some_and(|m| turn.is_user_message(m));
            return false;
        }
        let parent = nonempty_str(payload.get("reply_to_message_id"))
            .or_else(|| nonempty_str(payload.get("parent_message_id")));
        self.after_note
            || parent.is_some_and(|p| turn.is_user_message(p) || turn.has_reply(p))
            || message_id.is_some_and(|m| turn.has_reply(m))
    }
}

/// What `follow_turn` needs to backfill a reply it did not see live.
struct FollowCtx {
    /// Muse's id for the user message that opened the turn ("" if unknown).
    user_mid: String,
    /// The message created the side chat, so the subscription opened late.
    fresh: bool,
}

#[allow(clippy::too_many_arguments)]
async fn follow_turn(
    state: Arc<DaemonState>,
    chat: String,
    turn_id: String,
    mut turn: MuseTurn,
    held: Vec<TurnEvent>,
    sub: &mut muse_link::ChatSubscription,
    ctx: FollowCtx,
) {
    let mut emitted_working = false;
    let mut emitted_replying = false;
    for ev in held {
        apply_turn_event(
            &state,
            &chat,
            &turn_id,
            &ev,
            &mut emitted_working,
            &mut emitted_replying,
        );
    }
    let mut router = ReplyRouter {
        after_note: ctx.fresh,
    };
    let started = std::time::Instant::now();
    let mut last_backfill = std::time::Instant::now();
    loop {
        let now = state.now_secs();
        // A fresh chat's turn looks quiet (and so complete) before Muse has answered, because
        // its live events went by before the subscription opened. Wait for a reply there.
        // As in the reference: finished when settled, or complete once reply text arrived.
        if turn.settled(now) || (emitted_replying && turn.complete(now)) {
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
            turn.task_finished = true;
            break;
        }
        if last_backfill.elapsed() >= BACKFILL_POLL {
            last_backfill = std::time::Instant::now();
        }
        let next =
            tokio::time::timeout(std::time::Duration::from_millis(500), sub.next_event()).await;
        match next {
            Ok(Some(Ok(event))) => {
                if log::log_enabled!(log::Level::Debug) {
                    let payload = event.get("payload").unwrap_or(&event);
                    log::debug!(
                        "turn {turn_id}: {} seq {}",
                        nonempty_str(event.get("event")).unwrap_or("?"),
                        payload
                            .get("seq")
                            .map(|v| v.to_string())
                            .unwrap_or_default()
                    );
                }
                if !router.admits(&turn, &event) {
                    continue;
                }
                let now = state.now_secs();
                match turn.feed(&event, now) {
                    Ok(events) => {
                        log::debug!("turn {turn_id}: {} turn events", events.len());
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
                    Err(e) => {
                        log::info!("turn {turn_id}: event rejected: {e}");
                        break;
                    }
                }
            }
            Ok(Some(Err(e))) => {
                log::info!("turn {turn_id}: chat subscription failed: {e}");
                break;
            }
            Ok(None) => {
                log::info!("turn {turn_id}: chat subscription ended");
                break;
            }
            Err(_) => {} // poll timeout; re-check completion
        }
    }
    // A reply can be missed across a reconnect: backfill from Muse's history.
    if !emitted_replying && backfill_reply(&state, &chat, &turn_id, &ctx).await {
        emitted_replying = true;
        turn.task_finished = true;
    }
    let final_state = if turn.task_finished || turn.complete(state.now_secs() + 4.0) {
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
    sub.close().await;
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

/// The voice `app_id` the SDK's `voice-call` uses by default.
const VOICE_APP_ID: &str = "hatch-web";

/// The voice session advertises no commands and refuses any.
struct RefuseCommands;

impl CommandRunner for RefuseCommands {
    fn run(
        &self,
        _command: String,
        _params: Map<String, Value>,
        _timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        Box::pin(async {
            let mut m = Map::new();
            m.insert("ok".into(), json!(false));
            m.insert("error".into(), json!("unsupported command"));
            m
        })
    }
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
enum Payload {
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

/// Parses `/chat/history`'s `chat_events` into messages. Only `message.user` and
/// `message.assistant` rows with display text count; everything else is ignored.
/// Muse's display text with attachment markers (`[file:audio/wav …]`) removed. For a voice note
/// what remains is Muse's transcript of it.
pub(crate) fn clean_display_text(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("[file:") && t.ends_with(']'))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub(crate) fn parse_history(chat: &str, body: &Value) -> Vec<HistoryMessage> {
    let events = body
        .get("result")
        .unwrap_or(body)
        .get("chat_events")
        .and_then(Value::as_array);
    let Some(events) = events else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ev in events {
        let role = match ev.get("event_name").and_then(Value::as_str) {
            Some("message.user") => Role::User,
            Some("message.assistant") => Role::Assistant,
            Some("delta.presentation") => {
                // Images or video Muse sent: a message of its own, holding the media.
                let refs = crate::media::refs_from_event(ev);
                let id = ev.get("message_id").and_then(Value::as_str).unwrap_or("");
                if refs.is_empty() || id.is_empty() || id.len() > 200 {
                    continue;
                }
                out.push(HistoryMessage {
                    message: Message {
                        id: id.to_owned(),
                        chat: chat.to_owned(),
                        role: Role::Assistant,
                        text: String::new(),
                        rich: None,
                        choices: Vec::new(),
                        attachments: None,
                        reaction: None,
                        at: ev
                            .get("occurred_at_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or_else(now_millis),
                        state: MessageState::Done,
                        origin: Origin::Elsewhere,
                    },
                    seq: ev.get("seq").and_then(Value::as_i64).unwrap_or(0),
                    parent: None,
                    media: refs,
                });
                continue;
            }
            _ => continue,
        };
        if ev.get("display_text_ready").and_then(Value::as_bool) == Some(false) {
            continue; // still being written
        }
        let raw = clean_display_text(ev.get("display_text").and_then(Value::as_str).unwrap_or(""));
        let resolved = if role == Role::Assistant {
            muse_link::choices::decode(&raw, ev)
        } else {
            muse_link::choices::Choices {
                text: raw,
                replies: Vec::new(),
            }
        };
        let text = resolved.text.as_str();
        let id = ev.get("message_id").and_then(Value::as_str).unwrap_or("");
        if text.trim().is_empty() || id.is_empty() || id.len() > 200 {
            continue;
        }
        let at = ev
            .get("occurred_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_millis);
        out.push(HistoryMessage {
            message: Message {
                id: id.to_owned(),
                chat: chat.to_owned(),
                role,
                text: text.to_owned(),
                rich: None,
                choices: resolved.replies,
                attachments: None,
                reaction: ev
                    .get("reaction_emoji")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty() && r.chars().count() <= 8)
                    .map(str::to_owned),
                at,
                state: if role == Role::User {
                    MessageState::Sent
                } else {
                    MessageState::Done
                },
                origin: Origin::Elsewhere,
            },
            seq: ev.get("seq").and_then(Value::as_i64).unwrap_or(0),
            parent: ev
                .get("parent_message_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            media: Vec::new(),
        });
    }
    out
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
        let session = self
            .session()
            .ok_or_else(|| IpcError::new(ErrorCode::NotConnected, "Not connected"))?;
        let limit = limit.clamp(1, 100);
        let mut path = format!("/chat/history?limit={limit}");
        if chat != "main" {
            path.push_str(&format!("&session_id={chat}"));
        }
        if let Some(seq) = before_seq {
            path.push_str(&format!("&before_seq={seq}"));
        }
        let (status, body) = session
            .request("GET", &path, &[], &[])
            .await
            .map_err(map_link_err)?;
        if !(200..300).contains(&status) {
            return Err(IpcError::new(
                ErrorCode::Internal,
                format!("chat history returned HTTP {status}"),
            ));
        }
        Ok(history_page(chat, &body))
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

/// Reads a `/chat/history` response as a page.
pub(crate) fn history_page(chat: &str, body: &Value) -> HistoryPage {
    let result = body.get("result").unwrap_or(body);
    let floor = result
        .get("chat_events")
        .and_then(Value::as_array)
        .and_then(|events| {
            events
                .iter()
                .filter_map(|e| e.get("seq").and_then(Value::as_i64))
                .min()
        });
    HistoryPage {
        messages: parse_history(chat, body),
        floor,
        has_more: result.get("has_more").and_then(Value::as_bool) == Some(true),
    }
}

fn map_link_err(e: muse_link::LinkError) -> IpcError {
    use muse_link::LinkError as L;
    match e {
        L::NotConnected | L::SessionEnded => IpcError::new(ErrorCode::NotConnected, e.to_string()),
        L::Timeout => IpcError::new(ErrorCode::Timeout, e.to_string()),
        L::HttpStatus { status: 403, .. } => IpcError::new(ErrorCode::Rejected, e.to_string()),
        L::Invalid(_) => IpcError::new(ErrorCode::BadRequest, e.to_string()),
        _ => IpcError::new(ErrorCode::Internal, e.to_string()),
    }
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
                })
                .collect();
            reply(&ProfileListResult { active, profiles })
        }
        Method::ProfileCreate(p) => {
            state.profiles.create(&p.name).map_err(store_err)?;
            reply(&Empty {})
        }
        Method::ProfileRename(p) => {
            let _changes = state.profile_changes.lock().unwrap();
            // A rename changes the name only: the pairing, the conversations kept on the
            // device, the access settings and the folder of files all stay with the profile.
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
            let serving = p.name == state.active();
            if serving && state.call.lock().unwrap().is_some() {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    "end the call before renaming this profile",
                ));
            }
            goblin_store::validate_profile_name(&p.new_name).map_err(store_err)?;
            state.profiles.profile(&p.name).map_err(store_err)?;
            if state
                .profiles
                .list()
                .map_err(store_err)?
                .iter()
                .any(|profile| profile.name.eq_ignore_ascii_case(&p.new_name))
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
                if root.join(&p.new_name).exists() {
                    return Err(IpcError::new(
                        ErrorCode::Rejected,
                        format!("a folder named {} is already there", p.new_name),
                    ));
                }
            }
            let mut moved = Vec::new();
            let move_files = (|| -> Result<(), String> {
                for root in &roots {
                    let (from, to) = (root.join(&p.name), root.join(&p.new_name));
                    if from.exists() {
                        std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
                        moved.push((from, to));
                    }
                }
                if let Some(root) = state.built.read().unwrap().as_ref() {
                    crate::builds::Builds::new(root.join(&p.new_name)).relist(state.launcher())?;
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
                    if let Err(e) =
                        crate::builds::Builds::new(root.join(&p.name)).relist(state.launcher())
                    {
                        log::error!("could not restore profile launcher entries: {e}");
                    }
                }
            };
            if let Err(e) = move_files {
                rollback_files();
                return Err(IpcError::new(ErrorCode::Internal, e));
            }
            let original_timed = state.timed();
            let mut renamed_timed = original_timed.clone();
            for grant in &mut renamed_timed {
                if grant.profile == p.name {
                    grant.profile.clone_from(&p.new_name);
                }
            }
            let rename = || -> Result<(), goblin_store::StoreError> {
                if renamed_timed != original_timed {
                    // Keep the old names too until the directory has moved: a crash in
                    // between must leave a reversion record under either name.
                    let mut both = original_timed.clone();
                    both.extend(
                        renamed_timed
                            .iter()
                            .filter(|t| t.profile == p.new_name)
                            .cloned(),
                    );
                    goblin_store::fsutil::write_json_atomic(&state.timed_path(), &both, 0o600)?;
                }
                if let Err(e) = state.profiles.rename(&p.name, &p.new_name) {
                    // If the store could not roll back, either name may still hold consent.
                    // Keep both expiry records until the surviving profile can be reverted.
                    if renamed_timed != original_timed
                        && state.profiles.profile(&p.name).is_ok()
                        && state.profiles.profile(&p.new_name).is_err()
                    {
                        if let Err(restore) = goblin_store::fsutil::write_json_atomic(
                            &state.timed_path(),
                            &original_timed,
                            0o600,
                        ) {
                            log::error!("could not restore timed permissions after failed rename: {restore}");
                        }
                    }
                    return Err(e);
                }
                if renamed_timed != original_timed {
                    if let Err(e) = goblin_store::fsutil::write_json_atomic(
                        &state.timed_path(),
                        &renamed_timed,
                        0o600,
                    ) {
                        log::warn!("could not remove old timed-permission names after rename: {e}");
                    }
                }
                Ok(())
            };
            if let Err(e) = rename() {
                rollback_files();
                return Err(store_err(e));
            }
            log::info!("profile '{}' renamed to '{}'", p.name, p.new_name);
            if serving {
                // The socket, the folder and the stores are all named after the profile:
                // start again under the new name.
                let _ = state
                    .bus
                    .publish(&Event::ProfileChanged(ProfileChangedEvent {
                        profile: p.new_name,
                    }));
                let restarting = state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    restarting.request_restart();
                });
            }
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
            let timed: Vec<_> = state
                .timed()
                .into_iter()
                .filter(|t| t.profile != p.name)
                .collect();
            goblin_store::fsutil::write_json_atomic(&state.timed_path(), &timed, 0o600)
                .map_err(store_err)?;
            reply(&Empty {})
        }
        Method::ProfileSwitch(p) => {
            // One daemon serves one profile. Switching stores the choice and starts the
            // daemon again from nothing, so nothing of this profile (its session with
            // Muse, its conversations in memory, its access rules) is there for the next.
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
            let name = p.profile.unwrap_or_else(|| state.active());
            let profile = state.profiles.profile(&name).map_err(store_err)?;
            if profile.pairing().map_err(store_err)?.is_some() && !p.force {
                return Err(IpcError::new(
                    ErrorCode::Rejected,
                    format!("profile {name} is already paired"),
                ));
            }
            let cancel = Arc::new(tokio::sync::Notify::new());
            {
                let mut open = state.pair.lock().unwrap();
                if open.is_some() {
                    return Err(IpcError::new(ErrorCode::Busy, "Already pairing"));
                }
                *open = Some(crate::pairing::PairHandle {
                    profile: name.clone(),
                    cancel: cancel.clone(),
                });
            }
            let timeout_s = p
                .timeout_s
                .unwrap_or(crate::pairing::DEFAULT_WINDOW_S)
                .clamp(30, 1800);
            // The Muse app refuses a gadget that has no SDK token, after the Bluetooth
            // exchange and with a message that does not say what to do. So the token comes
            // first: from a file left on the card, or from the owner's phone through a
            // page this handheld serves while it waits.
            if profile.sdk_token().map_err(store_err)?.is_none() {
                let homes = state.homes.read().unwrap().clone();
                let dropped = homes
                    .as_deref()
                    .is_some_and(|h| h.join(crate::pairing::SDK_TOKEN_DROP).is_file());
                let page = match (dropped, crate::tokenpage::local_ip()) {
                    (false, Some(ip)) => crate::tokenpage::Page::open(ip).await.ok(),
                    _ => None,
                };
                if let Some(page) = page {
                    let ble_name = profile.identity().map_err(store_err)?.ble_name();
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
                        let ended = match page.serve(profile.clone(), cancel.clone(), wait).await {
                            Outcome::Saved => {
                                match crate::pairing::prepare(
                                    state.clone(),
                                    profile,
                                    timeout_s,
                                    cancel,
                                ) {
                                    Ok((_, run)) => {
                                        run.await;
                                        return;
                                    }
                                    Err(why) => (PairPhase::Failed, Some(why)),
                                }
                            }
                            Outcome::Cancelled => (PairPhase::Cancelled, None),
                            Outcome::TimedOut => (PairPhase::Timeout, None),
                            Outcome::Failed(why) => (PairPhase::Failed, Some(why)),
                        };
                        state.pair_closed();
                        crate::pairing::publish(&state, &name, ended.0, ended.1);
                    });
                    return reply(&PairStartResult { ble_name });
                }
                // No network to serve the page on, or a file is waiting: the file it is.
                if let Err(why) = crate::pairing::take_dropped_sdk_token(&profile, homes.as_deref())
                {
                    state.pair_closed();
                    return Err(IpcError::new(ErrorCode::Rejected, why));
                }
            }
            match crate::pairing::prepare(state.clone(), profile, timeout_s, cancel) {
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
            let id = muse_link::random_uuid();
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
            reply(&VoicePttStartResult { note })
        }
        Method::VoicePttStop(p) => {
            let taken = {
                let mut slot = state.recording.lock().unwrap();
                match slot.as_ref() {
                    Some((id, _)) if *id == p.note => slot.take(),
                    _ => None,
                }
            };
            let (_, rec) =
                taken.ok_or_else(|| IpcError::new(ErrorCode::NotFound, "no such recording"))?;
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
            if state.session().is_none() {
                return Err(IpcError::new(ErrorCode::NotConnected, "Not connected"));
            }
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
            let device_id = state.device().node_id;
            let st = state.clone();
            let call_id = id.clone();
            tokio::spawn(async move {
                log::info!("call {call_id} starting");
                let reason = match st.open_voice_session().await {
                    Ok(session) => {
                        let greeting = st.settings.lock().unwrap().settings().call_greeting;
                        let r = crate::call::run(
                            session.clone(),
                            platform,
                            device_id,
                            thread.clone(),
                            rx,
                            observer,
                            greeting,
                        )
                        .await;
                        session.request_stop();
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
                    Err(e) => e.message,
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
                .build_decide(&p.name, p.approve)
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
mod history_tests {
    use super::*;
    use serde_json::json;

    fn ev(name: &str, id: &str, seq: i64, parent: Option<&str>, text: &str) -> Value {
        json!({"event_name": name, "message_id": id, "seq": seq, "parent_message_id": parent,
               "display_text": text, "occurred_at_ms": 1_000 + seq, "role": "x"})
    }

    fn body(events: Vec<Value>) -> Value {
        json!({"ok": true, "result": {"chat_events": events, "has_more": false}})
    }

    #[test]
    fn a_history_page_knows_its_floor_and_whether_more_exists() {
        // Events that are not messages still count toward the floor.
        let mut b = body(vec![
            ev("delta.tool", "t", 7, None, ""),
            ev("message.user", "u", 8, None, "hi"),
            ev("message.assistant", "a", 9, Some("u"), "hello"),
        ]);
        let page = history_page("main", &b);
        assert_eq!(page.floor, Some(7));
        assert_eq!(page.messages.len(), 2);
        assert!(!page.has_more);
        b["result"]["has_more"] = json!(true);
        assert!(history_page("main", &b).has_more);
        let empty = history_page("main", &body(vec![]));
        assert_eq!(empty.floor, None);
        assert!(empty.messages.is_empty());
    }

    #[test]
    fn media_and_reactions_from_history() {
        let media = |id: &str, seq: i64| {
            json!({"event_name": "delta.presentation", "message_id": id, "seq": seq,
                   "occurred_at_ms": 1_000 + seq,
                   "payload": {"kind": "image", "data": {"images": [
                       {"path": "sandbox://workspace/imagine_media/a.webp", "label": "a"}]}}})
        };
        let mut reacted = ev("message.user", "u1", 1, None, "draw it");
        reacted["reaction_emoji"] = json!("\u{1F44D}");
        // The next message comes seconds later, as in life (the fixture's are 1 ms apart).
        let mut later = ev("message.user", "u2", 5, None, "thanks");
        later["occurred_at_ms"] = json!(9_000);
        let b = body(vec![
            reacted,
            ev("message.assistant", "a1", 2, Some("u1"), "Here it is."),
            media("p1", 3),
            media("p2", 4),
            later,
        ]);
        let mut h = parse_history("c-1", &b);
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].message.reaction.as_deref(), Some("\u{1F44D}"));
        assert_eq!(h[2].media.len(), 1);
        // Muse's order is text, then media; each media item is anchored to that text.
        let anchors = media_anchors(&mut h);
        assert_eq!(anchors["p1"], ("a1".to_owned(), 1));
        assert_eq!(anchors["p2"], ("a1".to_owned(), 2));
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn parses_user_and_assistant_rows_only() {
        let b = body(vec![
            ev("message.user", "u1", 1, None, "hello"),
            ev("task.status", "t1", 2, None, "working"),
            ev("message.assistant", "a1", 3, Some("u1"), "hi"),
            ev("message.assistant", "a2", 4, Some("u1"), "   "),
            json!({"event_name": "message.assistant", "message_id": "a3", "seq": 5,
                   "display_text": "half", "display_text_ready": false}),
        ]);
        let h = parse_history("c-1", &b);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].message.role, Role::User);
        assert_eq!(h[1].message.text, "hi");
        assert_eq!(h[1].message.state, MessageState::Done);
        assert_eq!(h[1].parent.as_deref(), Some("u1"));
    }

    #[test]
    fn replies_by_parent_or_after_a_fresh_message() {
        let b = body(vec![
            ev("message.user", "u0", 1, None, "older"),
            ev("message.assistant", "a0", 2, Some("u0"), "older reply"),
            ev("message.user", "u1", 3, None, "mine"),
            ev("message.assistant", "a1", 4, None, "parentless reply"),
            ev("message.assistant", "a2", 5, Some("u1"), "named reply"),
        ]);
        let h = parse_history("c-1", &b);
        let ids = |v: Vec<Message>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(replies_to(&h, "u1", false)), vec!["a2"]);
        assert_eq!(ids(replies_to(&h, "u1", true)), vec!["a1", "a2"]);
        assert!(replies_to(&h, "missing", true).is_empty());
    }

    #[test]
    fn tolerates_missing_or_odd_bodies() {
        assert!(parse_history("main", &json!(null)).is_empty());
        assert!(parse_history("main", &json!({"result": {"chat_events": "x"}})).is_empty());
        assert!(parse_history("main", &json!({"chat_events": [{"event_name": 3}]})).is_empty());
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use serde_json::json;

    fn ev(name: &str, payload: Value) -> Value {
        json!({"type": "event", "event": name, "payload": payload})
    }

    #[test]
    fn opens_on_own_message_and_closes_on_another() {
        let mut turn = MuseTurn::with_options(None, false, None, 0.0, 1.0);
        turn.acknowledge(&json!({"result": {"message_id": "u1"}}), false, 1.0)
            .unwrap();
        let mut r = ReplyRouter { after_note: false };
        // Before our message: a parentless reply on the shared chat is not ours.
        assert!(!r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "x0", "reply_to_message_id": ""})
            )
        ));
        assert!(!r.admits(&turn, &ev("message.user", json!({"message_id": "u1"}))));
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "reply_to_message_id": ""})
            )
        ));
        // Someone else speaks: later parentless events are theirs.
        assert!(!r.admits(&turn, &ev("message.user", json!({"message_id": "u2"}))));
        assert!(!r.admits(
            &turn,
            &ev("delta.message_start", json!({"message_id": "x2"}))
        ));
        // But anything naming our message still counts.
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a3", "parent_message_id": "u1"})
            )
        ));
    }

    #[test]
    fn other_chats_are_ignored() {
        let mut turn = MuseTurn::with_options(Some("s-1".into()), false, None, 0.0, 1.0);
        turn.acknowledge(&json!({"result": {"message_id": "u1"}}), false, 1.0)
            .unwrap();
        let mut r = ReplyRouter { after_note: true };
        assert!(!r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "session_id": "s-2"})
            )
        ));
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "session_id": "s-1"})
            )
        ));
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

#[cfg(test)]
mod transcript_tests {
    use super::clean_display_text;

    #[test]
    fn file_markers_are_dropped() {
        assert_eq!(
            clean_display_text(
                "Voice test, okay?\n[file:audio/wav workspace/user/files/voice_note.wav]"
            ),
            "Voice test, okay?"
        );
        assert_eq!(clean_display_text("[file:audio/wav x.wav]"), "");
        assert_eq!(
            clean_display_text("plain [file: inline] text"),
            "plain [file: inline] text"
        );
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::check_suggestions;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn native_history_choices_are_bound_to_the_assistant_message() {
        let body = serde_json::json!({"chat_events":[{
            "event_name":"message.assistant", "message_id":"a1", "display_text":"Where? [[hatch_widget:w]]",
            "payload":{"widgets":[{"widget_id":"w", "kind":"option", "data":{"options":["On the Brick", "On the web"]}}]}
        }]});
        let history = super::parse_history("main", &body);
        assert_eq!(history[0].message.choices, ["On the Brick", "On the web"]);
        assert!(!history[0].message.text.contains("hatch_widget"));
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
                &TurnEvent::Status(muse_link::AgentStatus {
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
            None,
            settings,
            consent,
            "test".into(),
            stores,
            executor,
            device,
        );
        (tmp, state)
    }

    #[test]
    fn finished_recorder_releases_its_slot() {
        let (_tmp, state) = state();
        *state.recording.lock().unwrap() =
            Some(("note".into(), crate::voice::Recording::finished_for_test()));
        assert!(!state.recording_active("note"));
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
