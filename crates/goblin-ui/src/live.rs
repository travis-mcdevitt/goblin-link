//! [`LiveProvider`]: the real data source. It talks to `goblind` over its Unix socket with the
//! blocking IPC client in polling mode, so the UI stays single-threaded: every frame
//! [`DataProvider::pump`] drains whatever replies and events have arrived and turns them into
//! the view model the screens already render. The UI holds no credentials and never blocks
//! on the network; if the daemon is down it shows that and retries every two seconds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use goblin_ipc::blocking::{Client, Incoming};
use goblin_ipc::types::{
    ChatCreateParams, ChatCreateResult, ChatHistoryParams, ChatHistoryResult, ChatListParams,
    ChatListResult, ChatMarkSeenParams, ChatSendParams, ClientKind, ConsentGetParams,
    ConsentUpdate, DaemonStatusParams, LinkState as IpcLink, Message, MessageState,
    NotifyListParams, NotifyListResult, NotifySeenParams, PlatformHealthParams, Role, Status,
    SubscribeParams, TurnState,
};
use goblin_ipc::{ClientError, Event, Response};
use goblin_ui_kit::widgets::{LinkState, TurnStatus};

use crate::data::{Consent, ConsentKind, DataProvider, DeviceStatus, SideChat, Turn, MAIN_CHAT};

/// How long a (re)connect may block the UI thread at most. The socket is local, so a healthy
/// daemon answers `hello` in milliseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(800);
/// Delay between reconnect attempts while the daemon is unreachable.
const RETRY_MS: u64 = 2000;
/// Messages of the main chat loaded on connect.
const HISTORY_LIMIT: u32 = 60;
/// The setting behind voice-to-voice.
const SPEAK_REPLIES: &str = "voice.speak_replies";
/// How often Status's storage and temperature are refreshed.
const HEALTH_MS: u64 = 60_000;
/// Cap on replies/events handled per frame, so a burst cannot stall rendering.
const MAX_PER_PUMP: usize = 256;

/// What an outstanding request was for.
#[derive(Debug, Clone)]
enum Pending {
    /// A `builds.*` request: the answer is the list.
    Builds,
    /// A `requests.*` request: the answer is the list.
    Requests,
    Status,
    ChatList,
    /// History of the named chat (ignored if another chat was opened meanwhile).
    History(String),
    /// Messages older than those shown, for the named chat.
    Older(String),
    Notices,
    /// `platform.health` (storage and temperature for Status).
    Health,
    /// `commands.consent.get` / `.set`; both answer with the full toggles.
    Consent,
    /// `chat.create`; the new chat opens when it arrives.
    Created,
    /// A request whose answer only matters if it fails.
    Ack,
    /// `settings.get` of the keys the UI shows.
    Settings,
    /// `platform.autostart`, read or set; both answer with the state.
    Autostart,
    /// `profile.list`.
    Profiles,
    /// `pair.start`; answers with the name to choose in the Muse app.
    PairStart,
    /// A profile change whose failure is worth showing and whose success changes the list.
    ProfileChange,
    ProfileSwitch(String),
    ProfileSwitchStatus(String),
    /// `voice.ptt_start`; carries the note id once it is known.
    PttStart,
    /// `voice.ptt_stop`.
    PttStop,
    /// `chat.stop`.
    Stop,
    /// `voice.call_start`.
    CallStart,
    /// A send; carries the index of the optimistic local turn.
    Send(String),
}

/// The turn the UI shows for a message.
fn turn_of(m: &Message) -> Turn {
    Turn {
        from_user: m.role == Role::User,
        body: m.text.clone(),
        status: map_state(m.state),
        // Pictures and clips the daemon has (or will have) on the device.
        media: m
            .attachments
            .iter()
            .flatten()
            .filter(|a| {
                matches!(
                    a.kind,
                    goblin_ipc::types::AttachmentKind::Image
                        | goblin_ipc::types::AttachmentKind::Video
                )
            })
            .map(|a| crate::media::Media {
                picture: a.path.clone(),
                video: a.url.clone(),
                is_video: a.kind == goblin_ipc::types::AttachmentKind::Video,
                label: a.name.clone(),
            })
            .collect(),
        at: m.at,
        reaction: m.reaction.clone(),
    }
}

/// Names the turn at its index in `turns`, so updates replace rather than append.
#[derive(Debug, Clone, Default)]
struct Row {
    id: String,
    choices: Vec<String>,
}

fn row_of(m: &Message) -> Row {
    Row {
        id: m.id.clone(),
        choices: m.choices.clone(),
    }
}

/// The live provider.
pub struct LiveProvider {
    socket: PathBuf,
    client: Option<Client>,
    next_connect_ms: u64,
    next_health_ms: u64,
    pending: HashMap<u64, Pending>,
    local_seq: u64,
    name: String,
    rows: Vec<Row>,
    turns: Vec<Turn>,
    sides: Vec<SideChat>,
    /// Whether the daemon holds a notice not yet marked seen.
    unseen_notices: bool,
    device: DeviceStatus,
    open_turns: HashMap<String, (String, TurnState)>,
    /// The open chat.
    active: String,
    /// Push-to-talk: the note being recorded, and whether it was released before the daemon
    /// confirmed the start (then it is stopped as soon as the id arrives).
    ptt_note: Option<String>,
    ptt_starting: bool,
    ptt_release: Option<bool>,
    ended_note: Option<String>,
    note_failed: bool,
    /// The note reached its longest and the daemon sent it without waiting for a release.
    note_sent_itself: bool,
    /// That note's id, until the daemon says whether it went, and how that ended.
    self_sent_note: Option<String>,
    self_sent_result: Option<crate::data::NoteEnd>,
    /// The note last discarded by the owner. The daemon may have sent it at its longest a
    /// moment before the discard arrived; then the owner is told that it went.
    discarded_note: Option<String>,
    /// A reply in the open chat was stopped, until the app has said so.
    reply_stopped: bool,
    /// Consent as last reported by the daemon (defaults until the first answer).
    consent: Consent,
    /// What the profile's Muse built, as the daemon last listed it.
    builds: Vec<goblin_ipc::types::BuildItem>,
    /// What Muse has asked and is waiting on.
    requests: Vec<goblin_ipc::types::RequestView>,
    /// `voice.speak_replies` is not `never`.
    voice_to_voice: bool,
    /// Start with the handheld, once the daemon has said (`None`: it cannot).
    autostart: Option<bool>,
    /// The profile the daemon serves (empty until it has said), the profiles it knows, and
    /// a pairing under way or just ended.
    profile: String,
    switching_profile: bool,
    profile_error: Option<String>,
    profiles: Vec<crate::data::ProfileEntry>,
    /// The longest a voice note may be, as the daemon last said.
    note_cap_s: u32,
    pairing: Option<crate::data::PairingView>,
    /// The kind of agent the last pairing opened here was for: what the daemon says about a
    /// pairing afterwards does not name it.
    pairing_hermes: bool,
    /// A reply is being spoken (`voice.speaking` start/stop).
    speaking: bool,
    /// The microphone level from `voice.level`, while a note records.
    mic_level: Option<f32>,
    /// Captions of the running call are numbered so each finished line keeps its own row.
    caption_seq: u64,
    /// Every setting as the daemon reports it, and a counter that moves when any changes.
    settings: serde_json::Map<String, serde_json::Value>,
    settings_rev: u64,
    /// Where the agent's avatar files are (`daemon.status` agent.avatar_path).
    avatar_dir: Option<String>,
    /// What Muse says it is doing (`agent.activity`), while a turn is open.
    activity: String,
    /// Muse's suggested replies for the open chat (`chat.suggest`).
    suggestions: Vec<String>,
    /// Older messages of the open chat: none left to ask for, a request under way, and how
    /// many were put in front since the app last asked.
    older_done: bool,
    /// The open chat's history has arrived since it was opened.
    history_loaded: bool,
    older_loading: bool,
    older_added: usize,
    /// The call in progress (from `voice.call` events), and why the last one ended.
    call: Option<crate::data::CallView>,
    call_end: Option<String>,
    connect_error: Option<String>,
    dirty: bool,
    // M3 content is not served in SDK mode; these stay empty.
}

impl LiveProvider {
    /// A provider for the daemon listening on `socket` (usually `<runtime>/active`).
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        LiveProvider {
            socket: socket.into(),
            client: None,
            next_connect_ms: 0,
            next_health_ms: 0,
            pending: HashMap::new(),
            local_seq: 0,
            // Not known until the daemon says: no name is shown rather than a guess.
            name: String::new(),
            rows: Vec::new(),
            turns: Vec::new(),
            sides: Vec::new(),
            unseen_notices: false,
            device: DeviceStatus {
                link: LinkState::Reconnecting,
                last_error: String::new(),
                battery: 0,
                charging: false,
                storage: "—".into(),
                temperature: "—".into(),
            },
            open_turns: HashMap::new(),
            active: MAIN_CHAT.into(),
            ptt_note: None,
            ptt_starting: false,
            ptt_release: None,
            ended_note: None,
            note_failed: false,
            note_sent_itself: false,
            self_sent_note: None,
            self_sent_result: None,
            reply_stopped: false,
            discarded_note: None,
            consent: Consent::default(),
            builds: Vec::new(),
            requests: Vec::new(),
            voice_to_voice: false,
            autostart: None,
            profile: String::new(),
            switching_profile: false,
            profile_error: None,
            profiles: Vec::new(),
            note_cap_s: 60,
            pairing: None,
            pairing_hermes: false,
            speaking: false,
            mic_level: None,
            caption_seq: 0,
            settings: serde_json::Map::new(),
            settings_rev: 0,
            avatar_dir: None,
            activity: String::new(),
            suggestions: Vec::new(),
            older_done: true,
            history_loaded: false,
            older_loading: false,
            older_added: 0,
            call: None,
            call_end: None,
            connect_error: None,
            dirty: true,
        }
    }

    /// The default socket: `$GOBLIN_SOCKET`, else `<runtime dir>/active` where the runtime dir is
    /// `$GOBLIND_RUNTIME_DIR`, `$XDG_RUNTIME_DIR/goblin-link` or `/run/goblin-link`.
    pub fn default_socket() -> PathBuf {
        if let Some(s) = std::env::var_os("GOBLIN_SOCKET") {
            return PathBuf::from(s);
        }
        let runtime = std::env::var_os("GOBLIND_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("goblin-link"))
            })
            .unwrap_or_else(|| PathBuf::from("/run/goblin-link"));
        goblin_ipc::paths::active_path(&runtime)
    }

    /// Whether the daemon connection is up.
    pub fn connected(&self) -> bool {
        self.client.is_some()
    }

    fn try_connect(&mut self, now_ms: u64) {
        self.next_connect_ms = now_ms + RETRY_MS;
        let client = Client::connect(
            &self.socket,
            ClientKind::Ui,
            env!("CARGO_PKG_VERSION"),
            CONNECT_TIMEOUT,
        )
        .and_then(|mut c| {
            c.subscribe(
                SubscribeParams {
                    after_seq: None,
                    topics: None,
                },
                CONNECT_TIMEOUT,
            )?;
            c.set_nonblocking(true)?;
            Ok(c)
        });
        match client {
            Ok(c) => {
                self.client = Some(c);
                self.switching_profile = false;
                self.connect_error = None;
                self.pending.clear();
                self.request_all();
            }
            Err(e) => {
                let msg = match e {
                    ClientError::Io(_) => "Goblin Link is not running".to_owned(),
                    other => format!("Could not reach the Goblin Link service: {other}"),
                };
                if self.connect_error.as_deref() != Some(&msg) {
                    self.device.link = LinkState::Error;
                    self.device.last_error = msg.clone();
                    self.connect_error = Some(msg);
                    self.dirty = true;
                }
            }
        }
    }

    fn drop_client(&mut self, why: &str) {
        self.client = None;
        // Whether a note on its way arrived will not be heard now: stop waiting for it.
        self.discarded_note = None;
        if self.self_sent_note.take().is_some() {
            self.self_sent_result = Some(crate::data::NoteEnd::Unknown);
        }
        self.speaking = false;
        if self.call.take().is_some() {
            self.call_end = Some("lost the Goblin Link service".into());
        }
        self.pending.clear();
        self.older_loading = false;
        self.open_turns.clear();
        self.device.link = LinkState::Error;
        self.device.last_error = why.to_owned();
        self.connect_error = Some(why.to_owned());
        self.dirty = true;
    }

    /// Sends a profile or pairing request.
    fn profile_request<P: goblin_ipc::Request>(
        &mut self,
        params: P,
        kind: Pending,
    ) -> Result<(), String> {
        self.online()?;
        self.request(params, kind)?;
        self.dirty = true;
        Ok(())
    }

    fn online(&self) -> Result<(), String> {
        if self.client.is_some() {
            Ok(())
        } else {
            Err("Goblin Link service offline".into())
        }
    }

    /// Sends a request whose failure the caller reports.
    fn request<P: goblin_ipc::Request>(&mut self, params: P, kind: Pending) -> Result<(), String> {
        if self.send(params, kind) {
            Ok(())
        } else {
            Err("Lost the Goblin Link service".into())
        }
    }

    fn request_all(&mut self) {
        self.send(DaemonStatusParams {}, Pending::Status);
        self.send(ChatListParams {}, Pending::ChatList);
        self.request_history();
        self.send(NotifyListParams { limit: Some(50) }, Pending::Notices);
        self.send(ConsentGetParams {}, Pending::Consent);
        self.send(goblin_ipc::types::BuildsListParams {}, Pending::Builds);
        self.send(goblin_ipc::types::RequestsListParams {}, Pending::Requests);
        self.send(PlatformHealthParams {}, Pending::Health);
        self.send(
            goblin_ipc::types::PlatformAutostartParams { set: None },
            Pending::Autostart,
        );
        self.send(goblin_ipc::types::ProfileListParams {}, Pending::Profiles);
        self.send(
            goblin_ipc::types::SettingsGetParams { keys: None },
            Pending::Settings,
        );
    }

    fn apply_settings(&mut self, values: &serde_json::Map<String, serde_json::Value>) {
        for (k, v) in values {
            self.settings.insert(k.clone(), v.clone());
        }
        self.settings_rev += 1;
        self.dirty = true;
        if let Some(v) = values.get(SPEAK_REPLIES).and_then(|v| v.as_str()) {
            self.voice_to_voice = v != "never";
            self.dirty = true;
        }
    }

    fn request_history(&mut self) {
        let chat = self.active.clone();
        self.send(
            ChatHistoryParams {
                chat: chat.clone(),
                before: None,
                limit: Some(HISTORY_LIMIT),
            },
            Pending::History(chat),
        );
    }

    fn send<P: goblin_ipc::Request>(&mut self, params: P, kind: Pending) -> bool {
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        match client.send(params) {
            Ok(id) => {
                self.pending.insert(id, kind);
                true
            }
            Err(e) => {
                self.drop_client(&format!("Lost the Goblin Link service: {e}"));
                false
            }
        }
    }

    fn on_response(&mut self, r: Response) {
        let Some(kind) = self.pending.remove(&r.id) else {
            return;
        };
        // Replies from the old profile must not repopulate a cleared screen.
        if self.switching_profile
            && !matches!(
                kind,
                Pending::ProfileSwitch(_) | Pending::ProfileSwitchStatus(_)
            )
        {
            return;
        }
        let ok = r.ok;
        let err_text = r
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_default();
        let value = r.result.unwrap_or(serde_json::Value::Null);
        match kind {
            Pending::Status => {
                if let Ok(s) = serde_json::from_value::<Status>(value) {
                    self.apply_status(&s);
                }
            }
            Pending::ChatList => {
                if let Ok(list) = serde_json::from_value::<ChatListResult>(value) {
                    self.sides = list
                        .chats
                        .into_iter()
                        .filter(|c| c.id != "main")
                        .map(|c| SideChat {
                            unread: c.unread > 0,
                            id: c.id,
                            title: c.title,
                        })
                        .collect();
                }
            }
            Pending::History(chat) => {
                if chat != self.active {
                    return;
                }
                if let Some(s) = value.get("suggestions").and_then(|s| s.as_array()) {
                    self.suggestions = s
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect();
                }
                if let Ok(h) = serde_json::from_value::<ChatHistoryResult>(value) {
                    self.older_done = h.complete;
                    self.history_loaded = true;
                    // Keep optimistic local turns that the daemon has not echoed yet.
                    self.retain_rows(|r| r.id.starts_with("local:"));
                    let local = std::mem::take(&mut self.rows);
                    let local_turns = std::mem::take(&mut self.turns);
                    for m in h.messages {
                        self.upsert(&m);
                    }
                    self.rows.extend(local);
                    self.turns.extend(local_turns);
                }
            }
            Pending::Older(chat) => {
                if chat != self.active {
                    return;
                }
                self.older_loading = false;
                // A failed request ends the asking until the chat is opened again, so a
                // broken link is not asked over and over.
                self.older_done = true;
                if let Ok(h) = serde_json::from_value::<ChatHistoryResult>(value) {
                    let (rows, turns): (Vec<Row>, Vec<Turn>) = h
                        .messages
                        .iter()
                        .filter(|m| self.row_index(&m.id).is_none())
                        .map(|m| (row_of(m), turn_of(m)))
                        .unzip();
                    self.older_done = h.complete || rows.is_empty();
                    self.older_added += rows.len();
                    self.rows.splice(0..0, rows);
                    self.turns.splice(0..0, turns);
                }
            }
            Pending::Notices => {
                if let Ok(n) = serde_json::from_value::<NotifyListResult>(value) {
                    self.unseen_notices = n.items.iter().any(|i| !i.seen);
                }
            }
            Pending::Settings => {
                if let Ok(s) = serde_json::from_value::<goblin_ipc::types::SettingsValues>(value) {
                    self.apply_settings(&s.values);
                }
            }
            Pending::Profiles => {
                if let Ok(list) =
                    serde_json::from_value::<goblin_ipc::types::ProfileListResult>(value)
                {
                    self.profiles = list
                        .profiles
                        .into_iter()
                        .map(|p| crate::data::ProfileEntry {
                            active: p.name == list.active,
                            name: p.name,
                            paired: p.paired,
                            agent: match p.agent.as_deref() {
                                Some("hermes") => Some(crate::data::AgentKind::Hermes),
                                Some(_) => Some(crate::data::AgentKind::Muse),
                                None => None,
                            },
                        })
                        .collect();
                }
            }
            Pending::PairStart => {
                match serde_json::from_value::<goblin_ipc::types::PairStartResult>(value) {
                    Ok(opened) => {
                        if let Some(view) = self.pairing.as_mut() {
                            view.ble_name = opened.ble_name;
                            // Advertising, not request acceptance, means the phone can find us.
                        }
                    }
                    Err(_) => {
                        if let Some(view) = self.pairing.as_mut() {
                            view.step = crate::data::PairStep::Failed;
                            view.detail = err_text.clone();
                        }
                    }
                }
            }
            Pending::ProfileSwitchStatus(target) => {
                if let Ok(status) = serde_json::from_value::<Status>(value) {
                    if status.profile == target {
                        self.switching_profile = false;
                        self.apply_status(&status);
                        self.request_all();
                    }
                }
            }
            Pending::ProfileSwitch(target) => {
                if !ok {
                    self.switching_profile = false;
                    self.profile_error = Some(err_text);
                    // A refused switch never restarts the daemon: restore its data now.
                    self.request_all();
                } else {
                    // Switching to the served profile succeeds without a restart.
                    self.send(DaemonStatusParams {}, Pending::ProfileSwitchStatus(target));
                }
            }
            Pending::ProfileChange => {
                if !ok {
                    self.profile_error = Some(err_text);
                    self.send(DaemonStatusParams {}, Pending::Status);
                }
                self.send(goblin_ipc::types::ProfileListParams {}, Pending::Profiles);
            }
            Pending::Autostart => {
                match serde_json::from_value::<goblin_ipc::types::PlatformAutostartResult>(value) {
                    Ok(a) => self.autostart = Some(a.enabled),
                    // A refused change: read the real state so the switch does not lie.
                    Err(_) if self.autostart.is_some() => {
                        self.autostart = None;
                        self.device.last_error = "Autostart not changed".into();
                        self.send(
                            goblin_ipc::types::PlatformAutostartParams { set: None },
                            Pending::Autostart,
                        );
                    }
                    Err(_) => {}
                }
            }
            Pending::Health => {
                let (storage, temperature) = health_captions(&value);
                self.device.storage = storage;
                self.device.temperature = temperature;
            }
            Pending::Builds => {
                if let Ok(list) =
                    serde_json::from_value::<goblin_ipc::types::BuildsListResult>(value)
                {
                    self.builds = list.items;
                } else if !ok {
                    self.device.last_error = format!("Not done: {err_text}");
                }
            }
            Pending::Requests => {
                if let Ok(list) =
                    serde_json::from_value::<goblin_ipc::types::RequestsListResult>(value)
                {
                    self.requests = list.items;
                } else if !ok {
                    self.device.last_error = format!("Not done: {err_text}");
                }
            }
            Pending::Consent => {
                if let Ok(c) = serde_json::from_value::<Consent>(value) {
                    self.consent = c;
                } else if !ok {
                    self.device.last_error = format!("Consent not changed: {err_text}");
                    // Re-read the real state so the toggles do not lie.
                    self.send(ConsentGetParams {}, Pending::Consent);
                }
            }
            Pending::Created => match serde_json::from_value::<ChatCreateResult>(value) {
                Ok(c) => {
                    let id = c.chat.id.clone();
                    if !self.sides.iter().any(|s| s.id == id) {
                        self.sides.insert(
                            0,
                            SideChat {
                                id: id.clone(),
                                title: c.chat.title,
                                unread: false,
                            },
                        );
                    }
                    self.open_chat(&id);
                }
                Err(_) => {
                    self.device.last_error = format!("Could not start a side chat: {err_text}");
                }
            },
            Pending::PttStart => {
                self.ptt_starting = false;
                match serde_json::from_value::<goblin_ipc::types::VoicePttStartResult>(value) {
                    Ok(r) => {
                        if self.ended_note.as_deref() == Some(&r.note) {
                            self.ptt_release = None;
                            return;
                        }
                        self.ptt_note = Some(r.note);
                        if let Some(cap) = r.max_seconds.filter(|s| *s > 0) {
                            self.note_cap_s = cap;
                        }
                        if let Some(cancel) = self.ptt_release.take() {
                            let _ = self.ptt_stop(cancel);
                        }
                    }
                    Err(_) => {
                        self.ptt_release = None;
                        self.device.last_error = format!("Microphone: {err_text}");
                    }
                }
            }
            Pending::CallStart => {
                if !ok {
                    self.call = None;
                    self.call_end = Some(if err_text.is_empty() {
                        "could not start".into()
                    } else {
                        err_text.clone()
                    });
                }
            }
            Pending::PttStop => {
                if !ok {
                    self.device.last_error = format!("Voice note not sent: {err_text}");
                }
            }
            Pending::Ack => {
                if !ok {
                    self.device.last_error = err_text;
                }
            }
            Pending::Stop => {
                if !ok {
                    self.device.last_error = format!("Not stopped: {err_text}");
                }
            }
            Pending::Send(local_id) => {
                if !ok {
                    if let Some(i) = self.row_index(&local_id) {
                        self.turns[i].status = TurnStatus::Failed;
                    }
                    self.device.last_error = if err_text.is_empty() {
                        "Message not sent".into()
                    } else {
                        err_text
                    };
                }
            }
        }
        self.dirty = true;
    }

    /// Drops everything shown that belongs to the profile served until now: its
    /// conversation, side chats, inbox, command log and what Muse was doing. Each profile is
    /// another account; none of one may stay on screen under another's name.
    fn forget_profile(&mut self) {
        self.rows.clear();
        self.builds.clear();
        self.requests.clear();
        self.turns.clear();
        self.sides.clear();
        self.unseen_notices = false;
        self.open_turns.clear();
        self.suggestions.clear();
        self.activity.clear();
        self.active = MAIN_CHAT.into();
        self.history_loaded = false;
        self.older_done = false;
        self.older_loading = false;
        self.older_added = 0;
        self.name.clear();
        self.avatar_dir = None;
        self.consent = Consent::for_new_profile();
        self.dirty = true;
    }

    fn apply_status(&mut self, s: &Status) {
        if s.profile != self.profile {
            if !self.profile.is_empty() {
                self.forget_profile();
                self.request_all();
            }
            self.profile = s.profile.clone();
        }
        self.name = s.agent.name.clone();
        self.avatar_dir = s.agent.avatar_path.clone();
        self.device.link = map_link(s.link);
        self.device.last_error = s.last_error.clone().unwrap_or_default();
        if let Some(b) = &s.battery {
            self.device.battery = b.percent;
            self.device.charging = b.charging;
        }
    }

    fn on_event(&mut self, ev: Event) {
        if self.switching_profile {
            return;
        }
        match ev {
            Event::LinkState(l) => {
                self.device.link = map_link(l.state);
                if let Some(e) = l.error {
                    self.device.last_error = e;
                }
                // The agent name and VM can change with the link; refresh the full status.
                self.send(DaemonStatusParams {}, Pending::Status);
                // A chat asked for before the link was up came back empty where the device
                // had nothing cached (a profile's first use): ask again now Muse can answer.
                if l.state == goblin_ipc::types::LinkState::Online && self.rows.is_empty() {
                    self.request_history();
                    self.send(ChatListParams {}, Pending::ChatList);
                }
            }
            Event::ChatMessage(m) => {
                if m.message.chat == self.active {
                    self.upsert(&m.message);
                }
            }
            Event::ChatTurn(t) => match t.state {
                state if state.ended() => {
                    if state == TurnState::Stopped && t.chat == self.active {
                        self.reply_stopped = true;
                    }
                    self.open_turns.remove(&t.turn);
                    if self.open_turns.is_empty() {
                        self.activity.clear();
                    }
                }
                state => {
                    self.open_turns.insert(t.turn, (t.chat, state));
                }
            },
            Event::ChatListChanged(_) | Event::ChatUnread(_) => {
                self.send(ChatListParams {}, Pending::ChatList);
            }
            Event::NotifyNew(_) | Event::NotifyCount(_) => {
                self.send(NotifyListParams { limit: Some(50) }, Pending::Notices);
            }
            Event::SettingsChanged(s) => self.apply_settings(&s.values),
            Event::AgentActivity(a) => {
                self.activity = a.text;
            }
            Event::ChatSuggestions(s) => {
                if s.chat == self.active {
                    self.suggestions = s.replies;
                }
            }
            // How the note the daemon sent at its longest ended.
            Event::VoiceNote(note)
                if self.self_sent_note.as_deref() == Some(&note.note)
                    && matches!(
                        note.state,
                        goblin_ipc::types::NoteState::Sent | goblin_ipc::types::NoteState::Failed
                    ) =>
            {
                self.self_sent_note = None;
                // The app says a failure once, where the owner is looking; it is not also
                // left standing as the device's last error.
                self.self_sent_result = Some(if note.state == goblin_ipc::types::NoteState::Sent {
                    crate::data::NoteEnd::Sent
                } else {
                    crate::data::NoteEnd::Failed
                });
            }
            Event::VoiceNote(note) if note.state == goblin_ipc::types::NoteState::Failed => {
                if self.ptt_note.as_deref() == Some(&note.note) || self.ptt_starting {
                    self.ended_note = Some(note.note);
                    self.note_failed = true;
                    self.ptt_note = None;
                    self.ptt_release = None;
                    self.mic_level = None;
                }
            }
            // The daemon ended the note at its longest and is sending it: the button is
            // still held, and its release has nothing left to stop.
            Event::VoiceNote(note)
                if note.state == goblin_ipc::types::NoteState::Sending
                    && (self.ptt_note.as_deref() == Some(&note.note)
                        || self.discarded_note.as_deref() == Some(&note.note)) =>
            {
                self.discarded_note = None;
                self.self_sent_note = Some(note.note.clone());
                self.ended_note = Some(note.note);
                self.note_sent_itself = true;
                self.ptt_note = None;
                self.ptt_release = None;
                self.mic_level = None;
            }
            Event::VoiceCall(c) => {
                use crate::data::{CallPhase, CallView};
                use goblin_ipc::types::CallState;
                let phase = match c.state {
                    CallState::Connecting | CallState::Ready => Some(CallPhase::Connecting),
                    CallState::Listening => Some(CallPhase::Listening),
                    CallState::Thinking => Some(CallPhase::Thinking),
                    CallState::Speaking => Some(CallPhase::Speaking),
                    _ => None,
                };
                match phase {
                    Some(phase) => {
                        self.call = Some(CallView {
                            phase,
                            muted: c.muted,
                        })
                    }
                    None => {
                        if self.call.take().is_some() {
                            self.call_end = Some(c.reason.unwrap_or_else(|| "ended".into()));
                        }
                        // The daemon now brings the call's real messages in; drop the captions.
                        self.retain_rows(|r| !r.id.starts_with("caption:"));
                        self.request_history();
                    }
                }
            }
            Event::VoiceLevel(l) => {
                if l.source == goblin_ipc::types::LevelSource::Mic {
                    self.mic_level = Some(l.rms as f32);
                }
            }
            Event::VoiceCaption(c) => {
                // What is being said on a call, shown in the chat as it grows. The real
                // messages replace these rows when the call ends.
                let who = if c.role == Role::Assistant { "a" } else { "u" };
                let id = format!("caption:{who}:{}", self.caption_seq);
                let turn = Turn {
                    from_user: c.role != Role::Assistant,
                    body: c.text,
                    status: if c.is_final || c.role != Role::Assistant {
                        TurnStatus::Done
                    } else {
                        TurnStatus::Replying
                    },
                    media: Vec::new(),
                    at: 0,
                    reaction: None,
                };
                match self.row_index(&id) {
                    Some(i) => self.turns[i] = turn,
                    None => self.push_row(id, turn),
                }
                if c.is_final {
                    self.caption_seq += 1;
                }
            }
            Event::VoiceSpeaking(s) => {
                self.speaking = s.state == goblin_ipc::types::SpeakingState::Start;
            }
            Event::ProfileChanged(e) => {
                // The daemon is about to start again as the other profile: nothing of this
                // one stays on screen while it does. (Not so after a rename, which this
                // provider has already taken note of: it is the same account.)
                if e.profile != self.profile {
                    self.switching_profile = true;
                    self.forget_profile();
                }
                self.device.link = LinkState::Reconnecting;
            }
            Event::BuildsChanged(list) => {
                self.builds = list.items;
            }
            Event::RequestsChanged(list) => {
                self.requests = list.items;
            }
            Event::ConsentChanged(consent) => {
                self.consent = consent;
            }
            Event::PairState(e) => {
                use crate::data::PairStep;
                use goblin_ipc::types::PairPhase;
                let step = match e.state {
                    PairPhase::NeedToken => PairStep::Token,
                    PairPhase::Connecting => PairStep::Connecting,
                    PairPhase::Advertising => PairStep::Waiting,
                    PairPhase::Connected => PairStep::Connected,
                    PairPhase::Confirming | PairPhase::Wifi => PairStep::Confirmed,
                    PairPhase::Registering => PairStep::Checking,
                    PairPhase::Done => PairStep::Done,
                    PairPhase::Failed => PairStep::Failed,
                    PairPhase::Cancelled => PairStep::Cancelled,
                    PairPhase::Timeout => PairStep::TimedOut,
                    PairPhase::Unknown => return,
                };
                let view = self
                    .pairing
                    .get_or_insert_with(|| crate::data::PairingView {
                        profile: e.profile.clone().unwrap_or_default(),
                        hermes: self.pairing_hermes,
                        ble_name: String::new(),
                        step,
                        detail: String::new(),
                    });
                view.step = step;
                // The owner may have named the profile on the phone meanwhile.
                if let Some(name) = e.profile.filter(|n| !n.is_empty()) {
                    view.profile = name;
                }
                // Only a Hermes is connected to this way, whatever the pairing was opened as.
                if e.state == PairPhase::Connecting {
                    view.hermes = true;
                }
                match (e.state, e.detail) {
                    (PairPhase::Advertising, Some(name)) => view.ble_name = name,
                    (PairPhase::NeedToken, Some(page)) => view.detail = page,
                    (PairPhase::Failed, Some(why)) => view.detail = why,
                    _ => {}
                }
                if step.ended() {
                    self.send(goblin_ipc::types::ProfileListParams {}, Pending::Profiles);
                }
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// Inserts or updates a message by id. A user message echoed by the daemon replaces the
    /// optimistic local turn with the same text.
    fn upsert(&mut self, m: &Message) {
        let turn = turn_of(m);
        if let Some(i) = self.row_index(&m.id) {
            self.turns[i] = turn;
            self.rows[i].choices = m.choices.clone();
            return;
        }
        if turn.from_user {
            if let Some(i) = (0..self.rows.len())
                .find(|&i| self.rows[i].id.starts_with("local:") && self.turns[i].body == m.text)
            {
                self.rows[i].id = m.id.clone();
                self.turns[i] = turn;
                return;
            }
        }
        // New messages go at the end. Media fetched later for an older reply goes where its
        // time puts it (both times are Muse's), not after everything since.
        let pos = if turn.media.is_empty() || turn.at == 0 {
            self.rows.len()
        } else {
            self.turns
                .iter()
                .rposition(|t| t.at <= turn.at)
                .map_or(0, |i| i + 1)
        };
        self.rows.insert(pos, row_of(m));
        self.turns.insert(pos, turn);
    }

    fn row_index(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    fn push_row(&mut self, id: String, turn: Turn) {
        self.rows.push(Row {
            id,
            ..Row::default()
        });
        self.turns.push(turn);
    }

    /// Keeps the rows, and their turns, that `keep` accepts.
    fn retain_rows(&mut self, keep: impl Fn(&Row) -> bool) {
        (self.rows, self.turns) = std::mem::take(&mut self.rows)
            .into_iter()
            .zip(std::mem::take(&mut self.turns))
            .filter(|(r, _)| keep(r))
            .unzip();
    }
}

impl DataProvider for LiveProvider {
    fn agent_name(&self) -> &str {
        // Before the daemon has said who the agent is, the app is named, not an agent
        // guessed: the header needs a name to hang the link's state on.
        if self.name.is_empty() {
            "Goblin Link"
        } else {
            &self.name
        }
    }
    fn main_chat(&self) -> &[Turn] {
        &self.turns
    }
    fn side_chats(&self) -> &[SideChat] {
        &self.sides
    }
    fn unseen_notices(&self) -> bool {
        self.unseen_notices
    }
    fn status(&self) -> &DeviceStatus {
        &self.device
    }
    fn active_chat(&self) -> &str {
        &self.active
    }
    fn consent(&self) -> Consent {
        self.consent
    }

    fn open_chat(&mut self, id: &str) {
        if id == self.active {
            return;
        }
        self.active = id.to_owned();
        self.rows.clear();
        self.turns.clear();
        self.suggestions.clear();
        self.older_done = true;
        self.history_loaded = false;
        self.older_loading = false;
        self.older_added = 0;
        self.dirty = true;
        self.request_history();
        self.send(
            ChatMarkSeenParams {
                chat: id.to_owned(),
                message_id: String::new(),
            },
            Pending::Ack,
        );
    }

    fn rename_side_chat(&mut self, id: &str, title: &str) -> Result<(), String> {
        self.online()?;
        if let Some(c) = self.sides.iter_mut().find(|c| c.id == id) {
            c.title = title.to_owned();
        }
        self.dirty = true;
        let params = goblin_ipc::types::ChatRenameParams {
            chat: id.to_owned(),
            title: title.to_owned(),
        };
        self.request(params, Pending::Ack)
    }

    fn forget_side_chat(&mut self, id: &str) -> Result<(), String> {
        self.online()?;
        self.sides.retain(|c| c.id != id);
        self.dirty = true;
        let params = goblin_ipc::types::ChatForgetParams {
            chat: id.to_owned(),
        };
        self.request(params, Pending::Ack)
    }

    fn new_side_chat(&mut self) -> Result<(), String> {
        self.online()?;
        self.request(ChatCreateParams { title: None }, Pending::Created)
    }

    fn set_consent(&mut self, kind: ConsentKind, on: bool) -> Result<(), String> {
        self.online()?;
        let mut update = ConsentUpdate::default();
        match kind {
            ConsentKind::Shell => update.shell = Some(on),
            ConsentKind::FileRead => update.file_read = Some(on),
            ConsentKind::FileWrite => update.file_write = Some(on),
            ConsentKind::FileWriteOutsideHome => update.file_write_outside_home = Some(on),
            ConsentKind::DropIns => update.drop_ins = Some(on),
        }
        // Optimistic; the daemon's answer (the full toggles) replaces it.
        kind.set(&mut self.consent, on);
        self.dirty = true;
        self.request(update, Pending::Consent)
    }

    fn voice_to_voice(&self) -> bool {
        self.voice_to_voice
    }

    fn speaking(&self) -> bool {
        self.speaking
    }

    fn note_cap_s(&self) -> u32 {
        self.note_cap_s
    }
    fn mic_level(&self) -> Option<f32> {
        // Only while a note is being recorded.
        (self.ptt_note.is_some() || self.ptt_starting)
            .then_some(self.mic_level)
            .flatten()
    }

    fn setting(&self, key: &str) -> Option<serde_json::Value> {
        self.settings.get(key).cloned()
    }

    fn set_setting(&mut self, key: &str, value: serde_json::Value) -> Result<(), String> {
        self.online()?;
        let mut values = serde_json::Map::new();
        values.insert(key.to_owned(), value);
        // Shown at once; the daemon's settings.changed event confirms it.
        self.apply_settings(&values);
        self.request(goblin_ipc::types::SettingsValues { values }, Pending::Ack)
    }

    fn settings_rev(&self) -> u64 {
        self.settings_rev
    }

    fn load_older(&mut self) -> bool {
        if self.older_done || self.older_loading {
            return false;
        }
        // The oldest message the daemon knows (not a turn still local to this screen).
        let Some(oldest) = self
            .rows
            .iter()
            .find(|r| !r.id.starts_with("local:") && !r.id.starts_with("caption:"))
            .map(|r| r.id.clone())
        else {
            return false;
        };
        let chat = self.active.clone();
        self.older_loading = self.send(
            ChatHistoryParams {
                chat: chat.clone(),
                before: Some(oldest),
                limit: Some(HISTORY_LIMIT),
            },
            Pending::Older(chat),
        );
        self.older_loading
    }
    fn take_older_added(&mut self) -> usize {
        std::mem::take(&mut self.older_added)
    }
    fn quick_replies(&self) -> Vec<String> {
        if let Some(i) = self
            .rows
            .iter()
            .rposition(|r| !r.id.starts_with("caption:"))
        {
            let (row, turn) = (&self.rows[i], &self.turns[i]);
            if !turn.from_user && turn.status == TurnStatus::Done && !row.choices.is_empty() {
                return row.choices.clone();
            }
        }
        self.suggestions.clone()
    }

    fn avatar_dir(&self) -> Option<String> {
        self.avatar_dir.clone()
    }

    fn activity(&self) -> String {
        if !self
            .open_turns
            .values()
            .any(|(chat, state)| chat == &self.active && *state == TurnState::Working)
        {
            String::new()
        } else {
            self.activity.clone()
        }
    }

    fn call(&self) -> Option<crate::data::CallView> {
        self.call.clone()
    }

    fn take_call_end(&mut self) -> Option<String> {
        self.call_end.take()
    }

    fn call_start(&mut self) -> Result<(), String> {
        self.online()?;
        let params = goblin_ipc::types::VoiceCallStartParams {
            chat: Some(self.active.clone()),
        };
        self.request(params, Pending::CallStart)?;
        self.call = Some(crate::data::CallView {
            phase: crate::data::CallPhase::Connecting,
            muted: false,
        });
        self.dirty = true;
        Ok(())
    }

    fn call_end(&mut self) {
        self.send(goblin_ipc::types::VoiceCallEndParams {}, Pending::Ack);
    }

    fn call_barge_in(&mut self) {
        self.send(goblin_ipc::types::VoiceBargeInParams {}, Pending::Ack);
    }

    fn call_mute(&mut self, muted: bool) {
        self.send(
            goblin_ipc::types::VoiceCallMuteParams { muted },
            Pending::Ack,
        );
    }

    fn can_stop_reply(&self) -> bool {
        self.agent_kind() == Some(crate::data::AgentKind::Hermes)
            && self
                .open_turns
                .values()
                .any(|(chat, _)| chat == &self.active)
    }

    fn stop_reply(&mut self) -> Result<(), String> {
        let params = goblin_ipc::types::ChatStopParams {
            chat: self.active.clone(),
        };
        self.request(params, Pending::Stop)
    }

    fn take_reply_stopped(&mut self) -> bool {
        std::mem::take(&mut self.reply_stopped)
    }

    fn stop_speaking(&mut self) {
        if self.speaking {
            self.speaking = false;
            self.dirty = true;
            self.send(goblin_ipc::types::VoiceStopSpeakingParams {}, Pending::Ack);
        }
    }

    fn set_voice_to_voice(&mut self, on: bool) -> Result<(), String> {
        self.online()?;
        let mut values = serde_json::Map::new();
        values.insert(
            SPEAK_REPLIES.into(),
            (if on { "voice_only" } else { "never" }).into(),
        );
        self.voice_to_voice = on;
        self.dirty = true;
        self.request(goblin_ipc::types::SettingsValues { values }, Pending::Ack)
    }

    fn profiles(&self) -> Vec<crate::data::ProfileEntry> {
        self.profiles.clone()
    }

    fn take_profile_error(&mut self) -> Option<String> {
        self.profile_error.take()
    }

    fn switch_profile(&mut self, name: &str) -> Result<(), String> {
        if self.switching_profile {
            return Err("Profile switch in progress".into());
        }
        if name == self.profile {
            return Ok(());
        }
        self.profile_request(
            goblin_ipc::types::ProfileSwitchParams {
                name: name.to_owned(),
            },
            Pending::ProfileSwitch(name.to_owned()),
        )?;
        // Nothing of the profile being left stays on screen, from this moment.
        self.switching_profile = true;
        self.forget_profile();
        self.device.link = LinkState::Reconnecting;
        Ok(())
    }

    fn create_profile(&mut self) -> Result<String, String> {
        let name = (2..100)
            .map(|n| format!("profile-{n}"))
            .find(|n| !self.profiles.iter().any(|p| p.name.eq_ignore_ascii_case(n)))
            .ok_or("Too many profiles")?;
        self.profile_request(
            goblin_ipc::types::ProfileCreateParams { name: name.clone() },
            Pending::ProfileChange,
        )?;
        // Shown at once; the daemon's list replaces it.
        self.profiles.push(crate::data::ProfileEntry {
            name: name.clone(),
            active: false,
            paired: false,
            agent: None,
        });
        Ok(name)
    }

    fn rename_profile(&mut self, name: &str, new_name: &str) -> Result<(), String> {
        if self
            .profiles
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(new_name) && p.name != name)
        {
            return Err("That name is taken".into());
        }
        self.profile_request(
            goblin_ipc::types::ProfileRenameParams {
                name: name.to_owned(),
                new_name: new_name.to_owned(),
            },
            Pending::ProfileChange,
        )?;
        // Shown at once; the daemon's list replaces it. The profile being served keeps its
        // conversation on screen: it is the same account under another name.
        for p in &mut self.profiles {
            if p.name == name {
                p.name = new_name.to_owned();
            }
        }
        if self.profile == name {
            self.profile = new_name.to_owned();
        }
        Ok(())
    }

    fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        self.profile_request(
            goblin_ipc::types::ProfileDeleteParams {
                name: name.to_owned(),
            },
            Pending::ProfileChange,
        )
    }

    fn unpair_profile(&mut self, name: &str) -> Result<(), String> {
        self.profile_request(
            goblin_ipc::types::PairUnpairParams {
                profile: Some(name.to_owned()),
            },
            Pending::ProfileChange,
        )
    }

    fn pair_start(&mut self, name: &str, hermes: bool) -> Result<(), String> {
        self.profile_request(
            goblin_ipc::types::PairStartParams {
                profile: Some(name.to_owned()),
                // A link is good for ten minutes; the page waits as long.
                timeout_s: Some(if hermes { 600 } else { 300 }),
                force: true,
                agent: Some(if hermes { "hermes" } else { "muse" }.into()),
            },
            Pending::PairStart,
        )?;
        self.pairing_hermes = hermes;
        self.pairing = Some(crate::data::PairingView {
            profile: name.to_owned(),
            hermes,
            ble_name: String::new(),
            step: crate::data::PairStep::Opening,
            detail: String::new(),
        });
        Ok(())
    }

    fn pair_cancel(&mut self) {
        self.send(goblin_ipc::types::PairCancelParams {}, Pending::Ack);
    }

    fn pairing(&self) -> Option<crate::data::PairingView> {
        self.pairing.clone()
    }

    fn pairing_seen(&mut self) {
        self.pairing = None;
        // A Muse pairing names its profile just after it says it is done.
        self.send(goblin_ipc::types::ProfileListParams {}, Pending::Profiles);
    }

    fn set_file_scope(&mut self, scope: goblin_ipc::types::FileScope) -> Result<(), String> {
        self.online()?;
        // Optimistic; the daemon's answer (the full consent) replaces it.
        self.consent.files = scope;
        self.dirty = true;
        let update = ConsentUpdate {
            files: Some(scope),
            ..Default::default()
        };
        self.request(update, Pending::Consent)
    }

    fn builds(&self) -> Vec<goblin_ipc::types::BuildItem> {
        self.builds.clone()
    }

    fn requests(&self) -> Vec<goblin_ipc::types::RequestView> {
        self.requests.clone()
    }

    fn shutdown_daemon(&mut self) {
        self.send(goblin_ipc::types::DaemonShutdownParams {}, Pending::Ack);
    }

    fn request_decide(&mut self, id: &str, answer: &str) -> Result<(), String> {
        let params = goblin_ipc::types::RequestsDecideParams {
            id: id.to_owned(),
            answer: answer.to_owned(),
        };
        self.online()?;
        self.request(params, Pending::Requests)?;
        // Gone from the screen at once; the daemon's list follows.
        self.requests.retain(|r| r.id != id);
        self.dirty = true;
        Ok(())
    }

    fn set_build_access(&mut self, access: goblin_ipc::types::BuildAccess) -> Result<(), String> {
        self.online()?;
        self.consent.builds = access;
        self.dirty = true;
        let update = ConsentUpdate {
            builds: Some(access),
            ..Default::default()
        };
        self.request(update, Pending::Consent)
    }

    fn build_decide(
        &mut self,
        name: &str,
        request: Option<&str>,
        approve: bool,
    ) -> Result<(), String> {
        let params = goblin_ipc::types::BuildsDecideParams {
            name: name.to_owned(),
            approve,
            request: request.map(str::to_owned),
        };
        self.online()?;
        self.request(params, Pending::Builds)
    }

    fn build_remove(&mut self, name: &str) -> Result<(), String> {
        let params = goblin_ipc::types::BuildsRemoveParams {
            name: name.to_owned(),
        };
        self.online()?;
        self.request(params, Pending::Builds)
    }

    fn autostart(&self) -> Option<bool> {
        self.autostart
    }

    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        self.online()?;
        // Optimistic; the daemon's answer replaces it.
        self.autostart = Some(on);
        self.dirty = true;
        self.request(
            goblin_ipc::types::PlatformAutostartParams { set: Some(on) },
            Pending::Autostart,
        )
    }

    fn take_note_end(&mut self) -> bool {
        std::mem::take(&mut self.note_failed)
    }

    fn take_note_sent(&mut self) -> bool {
        std::mem::take(&mut self.note_sent_itself)
    }

    fn take_self_sent_result(&mut self) -> Option<crate::data::NoteEnd> {
        self.self_sent_result.take()
    }

    fn ptt_start(&mut self) -> Result<(), String> {
        self.online()?;
        if self.ptt_note.is_some() || self.ptt_starting {
            return Ok(());
        }
        self.ended_note = None;
        self.discarded_note = None;
        self.note_failed = false;
        self.ptt_starting = true;
        self.ptt_release = None;
        let params = goblin_ipc::types::VoicePttStartParams {
            chat: self.active.clone(),
            mode: goblin_ipc::types::PttMode::Note,
        };
        let sent = self.request(params, Pending::PttStart);
        if sent.is_err() {
            self.ptt_starting = false;
        }
        sent
    }

    fn ptt_stop(&mut self, cancel: bool) -> Result<(), String> {
        if self.ptt_starting {
            // Released before the daemon answered: stop once the id arrives.
            self.ptt_release = Some(cancel);
            return Ok(());
        }
        let Some(note) = self.ptt_note.take() else {
            return Ok(());
        };
        self.discarded_note = cancel.then(|| note.clone());
        let params = goblin_ipc::types::VoicePttStopParams { note, cancel };
        self.request(params, Pending::PttStop)
    }

    fn mark_notices_seen(&mut self) {
        if !self.unseen_notices {
            return;
        }
        self.unseen_notices = false;
        self.send(NotifySeenParams { ids: None }, Pending::Ack);
    }

    fn send_user(&mut self, text: &str) -> Result<(), String> {
        if self.client.is_none() {
            return Err(self
                .connect_error
                .clone()
                .unwrap_or_else(|| "Goblin Link service offline".into()));
        }
        self.local_seq += 1;
        // The pills answered Muse's last reply; the daemon clears them too.
        self.suggestions.clear();
        let local_id = format!("local:{}", self.local_seq);
        self.push_row(
            local_id.clone(),
            Turn {
                from_user: true,
                body: text.to_owned(),
                status: TurnStatus::Sending,
                media: Vec::new(),
                at: 0,
                reaction: None,
            },
        );
        let sent = self.send(
            ChatSendParams {
                chat: self.active.clone(),
                text: text.to_owned(),
            },
            Pending::Send(local_id.clone()),
        );
        if !sent {
            if let Some(i) = self.row_index(&local_id) {
                self.turns[i].status = TurnStatus::Failed;
            }
            return Err("Lost the Goblin Link service".into());
        }
        Ok(())
    }

    fn pump(&mut self, now_ms: u64) -> bool {
        if self.client.is_none() && now_ms >= self.next_connect_ms {
            self.try_connect(now_ms);
            self.next_health_ms = now_ms + HEALTH_MS;
        } else if self.client.is_some() && now_ms >= self.next_health_ms {
            self.next_health_ms = now_ms + HEALTH_MS;
            self.send(PlatformHealthParams {}, Pending::Health);
        }
        for _ in 0..MAX_PER_PUMP {
            let Some(client) = self.client.as_mut() else {
                break;
            };
            match client.try_recv() {
                Ok(Some(Incoming::Response(r))) => self.on_response(r),
                Ok(Some(Incoming::Event(e))) => {
                    if let Ok(ev) = e.typed() {
                        self.on_event(ev);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    self.drop_client(&format!("Lost the Goblin Link service: {e}"));
                    self.next_connect_ms =
                        now_ms + if self.switching_profile { 0 } else { RETRY_MS };
                    break;
                }
            }
        }
        std::mem::take(&mut self.dirty)
    }

    fn agent_busy(&self) -> bool {
        // Acknowledged requests may be waiting for permission or missing a reply. They
        // are not evidence that Muse is typing. Finished text stops its own animation.
        self.open_turns
            .values()
            .any(|(chat, state)| chat == &self.active && *state == TurnState::Working)
            || self
                .turns
                .iter()
                .any(|t| matches!(t.status, TurnStatus::Sending | TurnStatus::Replying))
    }
}

fn map_link(l: IpcLink) -> LinkState {
    match l {
        IpcLink::Online => LinkState::Connected,
        IpcLink::Unpaired => LinkState::NotPaired,
        IpcLink::AuthFailed => LinkState::Error,
        _ => LinkState::Reconnecting,
    }
}

fn map_state(s: MessageState) -> TurnStatus {
    match s {
        MessageState::Sending => TurnStatus::Sending,
        MessageState::Sent | MessageState::Done => TurnStatus::Done,
        MessageState::Streaming => TurnStatus::Replying,
        MessageState::Failed => TurnStatus::Failed,
        MessageState::Uncertain => TurnStatus::Unsent,
        _ => TurnStatus::Done,
    }
}

/// Storage ("2.7 GB free of 57.9 GB") and temperature ("30 °C") from `platform.health`.
pub fn health_captions(v: &serde_json::Value) -> (String, String) {
    let storage = match (
        v.pointer("/disk_gb/free").and_then(|x| x.as_f64()),
        v.pointer("/disk_gb/total").and_then(|x| x.as_f64()),
    ) {
        (Some(free), Some(total)) => format!("{free:.1} GB free of {total:.1} GB"),
        _ => "—".into(),
    };
    let temperature = v
        .get("temperature_c")
        .and_then(|x| x.as_f64())
        .map(|t| format!("{t:.0} °C"))
        .unwrap_or_else(|| "—".into());
    (storage, temperature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::NoteEnd;

    #[test]
    fn native_choices_belong_to_the_latest_assistant_and_clear_on_reply() {
        let mut p = LiveProvider::new("/nonexistent");
        let mut offered = msg("a1", Role::Assistant, "Where?", MessageState::Done);
        offered.choices = vec!["On the Brick".into(), "On the web".into()];
        p.upsert(&offered);
        assert_eq!(p.quick_replies(), offered.choices);
        p.upsert(&msg("u1", Role::User, "On the Brick", MessageState::Sent));
        assert!(p.quick_replies().is_empty());
        // A late history enrichment of the old message must not revive its buttons.
        p.upsert(&offered);
        assert!(p.quick_replies().is_empty());
        p.upsert(&msg("a2", Role::Assistant, "Okay", MessageState::Done));
        assert!(p.quick_replies().is_empty());
        p.forget_profile();
        assert!(p.quick_replies().is_empty());
    }

    #[test]
    fn pending_requests_are_not_typing_and_activity_stays_in_its_chat() {
        let mut p = LiveProvider::new("/nonexistent");
        let event = |chat: &str, state| {
            Event::ChatTurn(goblin_ipc::types::ChatTurnEvent {
                turn: chat.into(),
                chat: chat.into(),
                state,
                activity: None,
            })
        };
        p.on_event(event("main", TurnState::Acked));
        p.upsert(&msg("reply", Role::Assistant, "Done", MessageState::Done));
        assert!(
            !p.agent_busy(),
            "an unanswered request is not evidence of typing"
        );
        p.on_event(event("side", TurnState::Working));
        assert!(!p.agent_busy(), "another chat must not animate this one");
        p.on_event(event("main", TurnState::Working));
        assert!(p.agent_busy());
        p.on_event(event("main", TurnState::Acked));
        assert!(
            !p.agent_busy(),
            "idle leaves the request pending, not typing"
        );
        p.upsert(&msg(
            "reply2",
            Role::Assistant,
            "Hello",
            MessageState::Streaming,
        ));
        assert!(p.agent_busy());
        p.upsert(&msg("reply2", Role::Assistant, "Hello", MessageState::Done));
        p.on_event(event("main", TurnState::Replying));
        assert!(
            !p.agent_busy(),
            "finished text must not keep animating while the turn settles"
        );
    }

    #[test]
    fn pairing_waits_for_advertising_and_late_replies_preserve_failure() {
        use crate::data::{PairStep, PairingView};
        use goblin_ipc::types::{PairPhase, PairStateEvent};
        let mut p = LiveProvider::new("/nonexistent");
        p.pairing = Some(PairingView {
            profile: "work".into(),
            hermes: false,
            ble_name: String::new(),
            step: PairStep::Opening,
            detail: String::new(),
        });
        p.pending.insert(1, Pending::PairStart);
        p.on_response(Response::success(
            1,
            serde_json::json!({"ble_name":"MuseGadget123456"}),
        ));
        assert_eq!(p.pairing.as_ref().unwrap().step, PairStep::Opening);
        for (state, expected) in [
            (PairPhase::Advertising, PairStep::Waiting),
            (PairPhase::Failed, PairStep::Failed),
        ] {
            p.on_event(Event::PairState(PairStateEvent {
                profile: Some("work".into()),
                state,
                detail: None,
            }));
            p.pending.insert(2, Pending::PairStart);
            p.on_response(Response::success(
                2,
                serde_json::json!({"ble_name":"MuseGadget123456"}),
            ));
            assert_eq!(p.pairing.as_ref().unwrap().step, expected);
        }
    }

    /// Once a Hermes link is pasted the page is gone: the screen leaves its code for
    /// "connecting", which the owner can still stop.
    #[test]
    fn a_pasted_hermes_link_shows_connecting_in_place_of_the_code() {
        use crate::data::PairStep;
        use goblin_ipc::types::{PairPhase, PairStateEvent};
        let mut p = LiveProvider::new("/nonexistent");
        // Opened from elsewhere, without a word on which kind of agent.
        for (state, detail, step) in [
            (
                PairPhase::NeedToken,
                Some("http://192.168.1.20:8674/"),
                PairStep::Token,
            ),
            (PairPhase::Connecting, None, PairStep::Connecting),
        ] {
            p.on_event(Event::PairState(PairStateEvent {
                profile: Some("work".into()),
                state,
                detail: detail.map(str::to_owned),
            }));
            assert_eq!(p.pairing.as_ref().unwrap().step, step);
        }
        let view = p.pairing.as_ref().unwrap();
        assert!(view.hermes, "only a Hermes is connected to this way");
        assert!(!view.step.ended());
    }

    #[test]
    fn profiles_say_which_agent_and_a_late_pairing_event_keeps_its_kind() {
        use crate::data::AgentKind;
        use goblin_ipc::types::{PairPhase, PairStateEvent};
        let mut p = LiveProvider::new("/nonexistent");
        p.pending.insert(1, Pending::Profiles);
        p.on_response(Response::success(
            1,
            serde_json::json!({"active": "a", "profiles": [
                {"name": "a", "paired": true, "has_sdk_token": false, "agent": "hermes"},
                {"name": "b", "paired": true, "has_sdk_token": true, "agent": "muse"},
                {"name": "c", "paired": false, "has_sdk_token": false}
            ]}),
        ));
        let kinds: Vec<_> = p.profiles.iter().map(|e| e.agent).collect();
        assert_eq!(
            kinds,
            [Some(AgentKind::Hermes), Some(AgentKind::Muse), None]
        );
        assert_eq!(p.agent_kind(), Some(AgentKind::Hermes));
        // The owner stopped a Hermes pairing and its view was put away; the daemon's word
        // that it is cancelled arrives after, and is still about a Hermes.
        p.pairing_hermes = true;
        p.on_event(Event::PairState(PairStateEvent {
            profile: Some("c".into()),
            state: PairPhase::Cancelled,
            detail: None,
        }));
        assert!(p.pairing.as_ref().unwrap().hermes);
    }

    #[test]
    fn the_longest_note_is_what_the_daemon_says() {
        let mut p = LiveProvider::new("/nonexistent");
        assert_eq!(p.note_cap_s(), 60);
        p.pending.insert(1, Pending::PttStart);
        p.on_response(Response::success(
            1,
            serde_json::json!({"note": "n-1", "max_seconds": 300}),
        ));
        assert_eq!(p.note_cap_s(), 300);
        // An older daemon does not say: what was last known stands, and never nothing.
        for (id, reply) in [
            (2, serde_json::json!({"note": "n-2"})),
            (3, serde_json::json!({"note": "n-3", "max_seconds": 0})),
        ] {
            p.pending.insert(id, Pending::PttStart);
            p.on_response(Response::success(id, reply));
            assert_eq!(p.note_cap_s(), 300);
        }
    }

    #[test]
    fn the_app_is_named_until_the_agent_is() {
        let mut p = LiveProvider::new("/nonexistent");
        assert_eq!(p.agent_name(), "Goblin Link");
        p.name = "Hermes".into();
        assert_eq!(p.agent_name(), "Hermes");
    }

    #[test]
    fn failed_recording_clears_the_live_note() {
        let mut p = LiveProvider::new("/nonexistent");
        p.ptt_note = Some("note".into());
        p.on_event(Event::VoiceNote(goblin_ipc::types::VoiceNoteEvent {
            note: "note".into(),
            state: goblin_ipc::types::NoteState::Failed,
            seconds: 60.0,
        }));
        assert!(p.ptt_note.is_none());
        assert!(p.take_note_end());
        assert!(!p.take_note_end());
    }

    #[test]
    fn a_note_sent_at_its_longest_is_not_a_failure() {
        let mut p = LiveProvider::new("/nonexistent");
        p.ptt_note = Some("note".into());
        let event = |note: &str, state| {
            Event::VoiceNote(goblin_ipc::types::VoiceNoteEvent {
                note: note.into(),
                state,
                seconds: 60.0,
            })
        };
        // Another note's news is not this one's.
        p.on_event(event("other", goblin_ipc::types::NoteState::Sending));
        assert!(p.ptt_note.is_some() && !p.take_note_sent());
        p.on_event(event("note", goblin_ipc::types::NoteState::Sending));
        assert!(p.ptt_note.is_none());
        assert!(p.take_note_sent() && !p.take_note_sent());
        assert!(!p.take_note_end(), "sent, not failed");
        // The button is let go afterwards: there is nothing to stop, and nothing is asked.
        assert!(p.ptt_stop(false).is_ok());
        // Whether it went is said when the daemon knows; a note that did not is an error
        // the owner sees, and it does not end a note begun meanwhile.
        assert_eq!(p.take_self_sent_result(), None);
        p.ptt_starting = true;
        p.on_event(event("note", goblin_ipc::types::NoteState::Failed));
        assert_eq!(p.take_self_sent_result(), Some(NoteEnd::Failed));
        assert_eq!(p.device.last_error, "", "said once, by the app");
        assert!(!p.take_note_end(), "the new note goes on");
        p.ptt_starting = false;
        p.ptt_note = Some("next".into());
        p.on_event(event("next", goblin_ipc::types::NoteState::Sending));
        p.on_event(event("next", goblin_ipc::types::NoteState::Sent));
        assert_eq!(p.take_self_sent_result(), Some(NoteEnd::Sent));
    }

    #[test]
    fn a_note_discarded_just_after_it_was_sent_is_said_to_have_gone() {
        let mut p = LiveProvider::new("/nonexistent");
        let sending = |note: &str| {
            Event::VoiceNote(goblin_ipc::types::VoiceNoteEvent {
                note: note.into(),
                state: goblin_ipc::types::NoteState::Sending,
                seconds: 60.0,
            })
        };
        // B is pressed; the daemon had already ended the note and says so a moment later.
        p.ptt_note = Some("note".into());
        p.discarded_note = None;
        let _ = p.ptt_stop(true);
        assert_eq!(p.discarded_note.as_deref(), Some("note"));
        p.on_event(sending("note"));
        assert!(p.take_note_sent(), "the owner is told it went");
        assert_eq!(p.self_sent_note.as_deref(), Some("note"));
        // A note that was sent on release is not one the daemon sent by itself.
        p.ptt_note = Some("kept".into());
        let _ = p.ptt_stop(false);
        assert!(p.discarded_note.is_none());
        p.on_event(sending("kept"));
        assert!(!p.take_note_sent());
    }

    #[test]
    fn a_note_on_its_way_is_forgotten_when_the_daemon_goes() {
        let mut p = LiveProvider::new("/nonexistent");
        p.ptt_note = Some("note".into());
        p.on_event(Event::VoiceNote(goblin_ipc::types::VoiceNoteEvent {
            note: "note".into(),
            state: goblin_ipc::types::NoteState::Sending,
            seconds: 60.0,
        }));
        assert!(p.take_note_sent());
        p.drop_client("gone");
        assert!(p.self_sent_note.is_none());
        assert_eq!(p.take_self_sent_result(), Some(NoteEnd::Unknown));
        assert_eq!(p.take_self_sent_result(), None);
        // Nothing was on its way: a disconnect says nothing of notes.
        p.drop_client("gone");
        assert_eq!(p.take_self_sent_result(), None);
    }

    #[test]
    fn a_stopped_reply_is_over_and_said_once_in_its_own_chat() {
        let mut p = LiveProvider::new("/nonexistent");
        let turn = |turn: &str, chat: &str, state| {
            Event::ChatTurn(goblin_ipc::types::ChatTurnEvent {
                turn: turn.into(),
                chat: chat.into(),
                state,
                activity: None,
            })
        };
        let open = p.active.clone();
        p.on_event(turn("t1", &open, TurnState::Acked));
        p.on_event(turn("t2", "elsewhere", TurnState::Working));
        assert_eq!(p.open_turns.len(), 2);
        // Another chat's reply stopped: over, and nothing is said here.
        p.on_event(turn("t2", "elsewhere", TurnState::Stopped));
        assert!(!p.take_reply_stopped());
        p.on_event(turn("t1", &open, TurnState::Stopped));
        assert!(p.open_turns.is_empty());
        assert!(p.take_reply_stopped() && !p.take_reply_stopped());
    }

    use goblin_ipc::types::Origin;

    fn msg(id: &str, role: Role, text: &str, state: MessageState) -> Message {
        Message {
            id: id.into(),
            chat: "main".into(),
            role,
            text: text.into(),
            rich: None,
            choices: Vec::new(),
            attachments: None,
            reaction: None,
            at: 1,
            state,
            origin: Origin::Gadget,
        }
    }

    #[test]
    fn status_lines() {
        let v =
            serde_json::json!({"disk_gb": {"total": 57.9, "free": 2.7}, "temperature_c": 30.17});
        assert_eq!(
            health_captions(&v),
            ("2.7 GB free of 57.9 GB".into(), "30 °C".into())
        );
        assert_eq!(
            health_captions(&serde_json::json!({})),
            ("—".into(), "—".into())
        );
    }

    #[test]
    fn echo_replaces_optimistic_turn_and_reply_streams_in_place() {
        let mut p = LiveProvider::new("/nonexistent");
        p.push_row(
            "local:1".into(),
            Turn {
                from_user: true,
                body: "hello".into(),
                status: TurnStatus::Sending,
                media: Vec::new(),
                at: 0,
                reaction: None,
            },
        );
        p.upsert(&msg("u1", Role::User, "hello", MessageState::Sent));
        p.upsert(&msg("a1", Role::Assistant, "Hi", MessageState::Streaming));
        p.upsert(&msg("a1", Role::Assistant, "Hi there", MessageState::Done));
        let t = p.main_chat();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].status, TurnStatus::Done);
        assert_eq!(t[1].body, "Hi there");
        assert_eq!(t[1].status, TurnStatus::Done);
    }

    /// `rows[i]` names `turns[i]` however the chat changes: a history load that keeps a local
    /// turn, older messages put in front, media placed by its time, captions dropped when a
    /// call ends, and a profile switch.
    #[test]
    fn each_row_stays_beside_its_turn() {
        let mut p = LiveProvider::new("/nonexistent");
        // Every turn's text here is its row's id, so a row out of step shows.
        let ids = |p: &LiveProvider| {
            assert_eq!(p.rows.len(), p.turns.len());
            for (r, t) in p.rows.iter().zip(&p.turns) {
                assert_eq!(r.id, t.body);
            }
            p.rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
        };
        let at = |id: &str, at| Message {
            at,
            ..msg(id, Role::Assistant, id, MessageState::Done)
        };
        let page = |ms: Vec<Message>| serde_json::json!({"messages": ms, "complete": false});
        p.push_row(
            "local:1".into(),
            Turn {
                from_user: true,
                body: "local:1".into(),
                status: TurnStatus::Sending,
                media: Vec::new(),
                at: 0,
                reaction: None,
            },
        );
        p.pending.insert(1, Pending::History(MAIN_CHAT.into()));
        p.on_response(Response::success(1, page(vec![at("m3", 30), at("m4", 40)])));
        assert_eq!(ids(&p), ["m3", "m4", "local:1"]);
        p.pending.insert(2, Pending::Older(MAIN_CHAT.into()));
        let older = vec![at("m1", 10), at("m2", 20), at("m3", 30)];
        p.on_response(Response::success(2, page(older)));
        assert_eq!(ids(&p), ["m1", "m2", "m3", "m4", "local:1"]);
        // Echoed or not, a local turn has no time: without it the picture goes by its own.
        p.retain_rows(|r| !r.id.starts_with("local:"));
        let mut pic = at("m25", 25);
        pic.attachments = Some(vec![goblin_ipc::types::Attachment {
            kind: goblin_ipc::types::AttachmentKind::Image,
            name: "pic.png".into(),
            path: Some("/pic.png".into()),
            url: None,
            bytes: None,
            mime: None,
            source: None,
        }]);
        p.upsert(&pic);
        assert_eq!(ids(&p), ["m1", "m2", "m25", "m3", "m4"]);
        p.on_event(Event::VoiceCaption(goblin_ipc::types::VoiceCaptionEvent {
            role: Role::Assistant,
            text: "caption:a:0".into(),
            is_final: false,
        }));
        assert_eq!(ids(&p), ["m1", "m2", "m25", "m3", "m4", "caption:a:0"]);
        p.on_event(Event::VoiceCall(goblin_ipc::types::VoiceCallEvent {
            state: goblin_ipc::types::CallState::Ended,
            reason: None,
            muted: false,
            seconds: 0.0,
        }));
        assert_eq!(ids(&p), ["m1", "m2", "m25", "m3", "m4"]);
        p.forget_profile();
        assert!(ids(&p).is_empty());
    }

    #[test]
    fn missing_daemon_is_reported_not_fatal() {
        let mut p = LiveProvider::new("/nonexistent/muse.sock");
        assert!(p.pump(0));
        assert!(!p.connected());
        assert_eq!(p.status().link, LinkState::Error);
        assert!(p.send_user("hi").is_err());
        // No retry storm: the next attempt waits RETRY_MS.
        assert!(!p.pump(10));
    }
    #[test]
    fn switching_discards_old_responses_and_recovers_from_rejection() {
        let mut p = LiveProvider::new("/missing");
        p.profile = "first".into();
        p.switching_profile = true;
        p.pending.insert(1, Pending::Profiles);
        p.on_response(Response::success(1, serde_json::json!({"active":"first", "profiles":[{"name":"first","paired":true,"has_sdk_token":true}]})));
        assert!(p.profiles.is_empty());
        assert!(p
            .switch_profile("first")
            .unwrap_err()
            .contains("in progress"));
        p.pending.insert(2, Pending::ProfileSwitch("second".into()));
        p.on_response(Response::failure(
            2,
            goblin_ipc::IpcError::new(goblin_ipc::ErrorCode::Busy, "end the call first"),
        ));
        assert!(!p.switching_profile);
        assert_eq!(
            p.take_profile_error().as_deref(),
            Some("end the call first")
        );
        assert!(p.take_profile_error().is_none());
        assert!(p.switch_profile("first").is_ok());
    }
    #[test]
    fn a_successful_noop_switch_resumes_without_a_disconnect() {
        let mut p = LiveProvider::new("/missing");
        p.profile = "stale-name".into();
        p.switching_profile = true;
        p.pending
            .insert(1, Pending::ProfileSwitchStatus("first".into()));
        p.on_response(Response::success(
            1,
            serde_json::json!({
                "profile":"first", "paired":false, "link":"not_paired",
                "agent":{"name":"Muse"}, "vm":{}, "uptime_s":0, "version":"test"
            }),
        ));
        assert!(!p.switching_profile);
        assert_eq!(p.profile, "first");
    }
}
