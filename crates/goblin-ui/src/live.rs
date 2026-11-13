//! [`LiveProvider`]: the real data source. It talks to `goblind` over its Unix socket with the
//! blocking IPC client in polling mode, so the UI stays single-threaded: every frame
//! [`DataProvider::pump`] drains whatever replies and events have arrived and turns them into
//! the view model the screens already render. The UI holds no credentials and never blocks
//! on the network; if the daemon is down it shows that and retries every two seconds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use goblin_ipc::blocking::{Client, Incoming};
use goblin_ipc::types::{
    ChatCreateParams, ChatCreateResult, ChatHistoryParams, ChatHistoryResult, ChatListParams,
    ChatListResult, ChatMarkSeenParams, ChatSendParams, ClientKind, CommandEntry, CommandState,
    CommandsLogParams, CommandsLogResult, ConsentGetParams, ConsentUpdate, DaemonStatusParams,
    LinkState as IpcLink, Message, MessageState, NotifyListParams, NotifyListResult,
    NotifySeenParams, PlatformHealthParams, Role, Status, SubscribeParams, TurnState,
};
use goblin_ipc::{ClientError, Event, Response};
use goblin_ui_kit::widgets::{LinkState, TurnStatus};

use crate::data::{
    Consent, ConsentKind, DataProvider, DeviceStatus, FeedPost, Game, Goal, Idea, LibraryItem,
    Notification, SideChat, Turn, MAIN_CHAT,
};

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
    Commands,
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
    /// `voice.ptt_start`; carries the note id once it is known.
    PttStart,
    /// `voice.ptt_stop`.
    PttStop,
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

/// One chat turn plus the daemon's message id, so updates replace rather than append.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    turn: Turn,
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
    notes: Vec<Notification>,
    device: DeviceStatus,
    open_turns: HashMap<String, TurnState>,
    /// The open chat.
    active: String,
    /// Push-to-talk: the note being recorded, and whether it was released before the daemon
    /// confirmed the start (then it is stopped as soon as the id arrives).
    ptt_note: Option<String>,
    ptt_starting: bool,
    ptt_release: Option<bool>,
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
    profiles: Vec<crate::data::ProfileEntry>,
    pairing: Option<crate::data::PairingView>,
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
    games: Vec<Game>,
    feed: Vec<FeedPost>,
    ideas: Vec<Idea>,
    goals: Vec<Goal>,
    library: Vec<LibraryItem>,
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
            name: "Muse".into(),
            rows: Vec::new(),
            turns: Vec::new(),
            sides: Vec::new(),
            notes: Vec::new(),
            device: DeviceStatus {
                link: LinkState::Reconnecting,
                vm: "—".into(),
                token_age: "—".into(),
                last_error: String::new(),
                battery: 0,
                charging: false,
                storage: "—".into(),
                temperature: "—".into(),
                command_log: Vec::new(),
            },
            open_turns: HashMap::new(),
            active: MAIN_CHAT.into(),
            ptt_note: None,
            ptt_starting: false,
            ptt_release: None,
            consent: Consent::default(),
            builds: Vec::new(),
            requests: Vec::new(),
            voice_to_voice: false,
            autostart: None,
            profile: String::new(),
            profiles: Vec::new(),
            pairing: None,
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
            games: Vec::new(),
            feed: Vec::new(),
            ideas: Vec::new(),
            goals: Vec::new(),
            library: Vec::new(),
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
                self.connect_error = None;
                self.pending.clear();
                self.request_all();
            }
            Err(e) => {
                let msg = match e {
                    ClientError::Io(_) => "The Muse service is not running".to_owned(),
                    other => format!("Could not reach the Muse service: {other}"),
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
        self.speaking = false;
        if self.call.take().is_some() {
            self.call_end = Some("lost the Muse service".into());
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
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        if self.send(params, kind) {
            self.dirty = true;
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn request_all(&mut self) {
        self.send(DaemonStatusParams {}, Pending::Status);
        self.send(ChatListParams {}, Pending::ChatList);
        self.request_history();
        self.send(NotifyListParams { limit: Some(50) }, Pending::Notices);
        self.send(CommandsLogParams { limit: Some(20) }, Pending::Commands);
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
                self.drop_client(&format!("Lost the Muse service: {e}"));
                false
            }
        }
    }

    fn on_response(&mut self, r: Response) {
        let Some(kind) = self.pending.remove(&r.id) else {
            return;
        };
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
                    let now = now_ms_wall();
                    self.sides = list
                        .chats
                        .into_iter()
                        .filter(|c| c.id != "main")
                        .map(|c| SideChat {
                            unread: c.unread > 0,
                            id: c.id,
                            title: c.title,
                            when: c.last_at.map(|t| age(now, t)).unwrap_or_default(),
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
                    let local: Vec<Row> = self
                        .rows
                        .drain(..)
                        .filter(|r| r.id.starts_with("local:"))
                        .collect();
                    for m in h.messages {
                        self.upsert(&m);
                    }
                    self.rows.extend(local);
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
                    let fresh: Vec<Row> = h
                        .messages
                        .iter()
                        .filter(|m| !self.rows.iter().any(|r| r.id == m.id))
                        .map(|m| Row {
                            id: m.id.clone(),
                            turn: turn_of(m),
                        })
                        .collect();
                    self.older_done = h.complete || fresh.is_empty();
                    self.older_added += fresh.len();
                    self.rows.splice(0..0, fresh);
                }
            }
            Pending::Notices => {
                if let Ok(n) = serde_json::from_value::<NotifyListResult>(value) {
                    let now = now_ms_wall();
                    self.notes = n
                        .items
                        .into_iter()
                        .map(|i| Notification {
                            title: i.title,
                            body: i.body.unwrap_or_default(),
                            age: age(now, i.at),
                            seen: i.seen,
                            chat: i.chat,
                        })
                        .collect();
                }
            }
            Pending::Commands => {
                if let Ok(c) = serde_json::from_value::<CommandsLogResult>(value) {
                    let now = now_ms_wall();
                    self.device.command_log =
                        c.entries.iter().map(|e| command_line(e, now)).collect();
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
                        })
                        .collect();
                }
            }
            Pending::PairStart => {
                match serde_json::from_value::<goblin_ipc::types::PairStartResult>(value) {
                    Ok(opened) => {
                        if let Some(view) = self.pairing.as_mut() {
                            view.ble_name = opened.ble_name;
                            if view.step == crate::data::PairStep::Opening {
                                view.step = crate::data::PairStep::Waiting;
                            }
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
            Pending::ProfileChange => {
                if !ok {
                    self.device.last_error = format!("Profile not changed: {err_text}");
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
                                when: "now".into(),
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
                        self.ptt_note = Some(r.note);
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
            Pending::Send(local_id) => {
                if !ok {
                    if let Some(row) = self.rows.iter_mut().find(|r| r.id == local_id) {
                        row.turn.status = TurnStatus::Failed;
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
        self.notes.clear();
        self.open_turns.clear();
        self.suggestions.clear();
        self.activity.clear();
        self.device.command_log.clear();
        self.active = MAIN_CHAT.into();
        self.history_loaded = false;
        self.older_done = false;
        self.older_loading = false;
        self.older_added = 0;
        self.name = "Muse".into();
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
        self.device.vm = s.vm.name.clone().unwrap_or_else(|| "—".into());
        self.device.token_age = s
            .token_age_s
            .map(|t| duration_caption(t * 1000))
            .unwrap_or_else(|| "—".into());
        self.device.last_error = s.last_error.clone().unwrap_or_default();
        if let Some(b) = &s.battery {
            self.device.battery = b.percent;
            self.device.charging = b.charging;
        }
    }

    fn on_event(&mut self, ev: Event) {
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
                TurnState::Done | TurnState::Failed | TurnState::Uncertain => {
                    self.open_turns.remove(&t.turn);
                    if self.open_turns.is_empty() {
                        self.activity.clear();
                    }
                }
                state => {
                    self.open_turns.insert(t.turn, state);
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
                        self.rows.retain(|r| !r.id.starts_with("caption:"));
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
                match self.rows.iter_mut().find(|r| r.id == id) {
                    Some(row) => row.turn = turn,
                    None => self.rows.push(Row { id, turn }),
                }
                if c.is_final {
                    self.caption_seq += 1;
                }
            }
            Event::VoiceSpeaking(s) => {
                self.speaking = s.state == goblin_ipc::types::SpeakingState::Start;
            }
            Event::CommandsStarted(_) | Event::CommandsFinished(_) => {
                self.send(CommandsLogParams { limit: Some(20) }, Pending::Commands);
            }
            Event::ProfileChanged(e) => {
                // The daemon is about to start again as the other profile: nothing of this
                // one stays on screen while it does. (Not so after a rename, which this
                // provider has already taken note of: it is the same account.)
                if e.profile != self.profile {
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
                        ble_name: String::new(),
                        step,
                        detail: String::new(),
                    });
                view.step = step;
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
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == m.id) {
            row.turn = turn;
            return;
        }
        if turn.from_user {
            if let Some(row) = self
                .rows
                .iter_mut()
                .find(|r| r.id.starts_with("local:") && r.turn.body == m.text)
            {
                row.id = m.id.clone();
                row.turn = turn;
                return;
            }
        }
        // New messages go at the end. Media fetched later for an older reply goes where its
        // time puts it (both times are Muse's), not after everything since.
        let pos = if turn.media.is_empty() || turn.at == 0 {
            self.rows.len()
        } else {
            self.rows
                .iter()
                .rposition(|r| r.turn.at <= turn.at)
                .map_or(0, |i| i + 1)
        };
        self.rows.insert(
            pos,
            Row {
                id: m.id.clone(),
                turn,
            },
        );
    }

    fn rebuild_turns(&mut self) {
        self.turns = self.rows.iter().map(|r| r.turn.clone()).collect();
    }
}

impl DataProvider for LiveProvider {
    fn agent_name(&self) -> &str {
        &self.name
    }
    fn main_chat(&self) -> &[Turn] {
        &self.turns
    }
    fn side_chats(&self) -> &[SideChat] {
        &self.sides
    }
    fn notifications(&self) -> &[Notification] {
        &self.notes
    }
    fn status(&self) -> &DeviceStatus {
        &self.device
    }
    fn games(&self) -> &[Game] {
        &self.games
    }
    fn feed(&self) -> &[FeedPost] {
        &self.feed
    }
    fn ideas(&self) -> &[Idea] {
        &self.ideas
    }
    fn goals(&self) -> &[Goal] {
        &self.goals
    }
    fn library(&self) -> &[LibraryItem] {
        &self.library
    }
    fn account_linked(&self) -> bool {
        false
    }
    fn account_stale(&self) -> bool {
        false
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
        self.suggestions.clear();
        self.older_done = true;
        self.history_loaded = false;
        self.older_loading = false;
        self.older_added = 0;
        self.rebuild_turns();
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
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        if let Some(c) = self.sides.iter_mut().find(|c| c.id == id) {
            c.title = title.to_owned();
        }
        self.dirty = true;
        let params = goblin_ipc::types::ChatRenameParams {
            chat: id.to_owned(),
            title: title.to_owned(),
        };
        if self.send(params, Pending::Ack) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn forget_side_chat(&mut self, id: &str) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        self.sides.retain(|c| c.id != id);
        self.dirty = true;
        let params = goblin_ipc::types::ChatForgetParams {
            chat: id.to_owned(),
        };
        if self.send(params, Pending::Ack) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn new_side_chat(&mut self) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        if self.send(ChatCreateParams { title: None }, Pending::Created) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn set_consent(&mut self, kind: ConsentKind, on: bool) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        let mut update = ConsentUpdate::default();
        match kind {
            ConsentKind::Shell => update.shell = Some(on),
            ConsentKind::FileRead => update.file_read = Some(on),
            ConsentKind::FileWrite => update.file_write = Some(on),
            ConsentKind::DropIns => update.drop_ins = Some(on),
        }
        // Optimistic; the daemon's answer (the full toggles) replaces it.
        kind.set(&mut self.consent, on);
        self.dirty = true;
        if self.send(update, Pending::Consent) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn voice_to_voice(&self) -> bool {
        self.voice_to_voice
    }

    fn speaking(&self) -> bool {
        self.speaking
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
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        let mut values = serde_json::Map::new();
        values.insert(key.to_owned(), value);
        // Shown at once; the daemon's settings.changed event confirms it.
        self.apply_settings(&values.clone());
        if self.send(goblin_ipc::types::SettingsValues { values }, Pending::Ack) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn settings_rev(&self) -> u64 {
        self.settings_rev
    }

    fn chat_ready(&self) -> bool {
        self.client.is_some() && self.history_loaded
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
        self.suggestions.clone()
    }

    fn avatar_dir(&self) -> Option<String> {
        self.avatar_dir.clone()
    }

    fn activity(&self) -> String {
        if self.open_turns.is_empty() {
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
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        let params = goblin_ipc::types::VoiceCallStartParams {
            chat: Some(self.active.clone()),
            full_duplex: false,
        };
        if !self.send(params, Pending::CallStart) {
            return Err("Lost the Muse service".into());
        }
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

    fn stop_speaking(&mut self) {
        if self.speaking {
            self.speaking = false;
            self.dirty = true;
            self.send(goblin_ipc::types::VoiceStopSpeakingParams {}, Pending::Ack);
        }
    }

    fn set_voice_to_voice(&mut self, on: bool) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        let mut values = serde_json::Map::new();
        values.insert(
            SPEAK_REPLIES.into(),
            (if on { "voice_only" } else { "never" }).into(),
        );
        self.voice_to_voice = on;
        self.dirty = true;
        if self.send(goblin_ipc::types::SettingsValues { values }, Pending::Ack) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn profiles(&self) -> Vec<crate::data::ProfileEntry> {
        self.profiles.clone()
    }

    fn switch_profile(&mut self, name: &str) -> Result<(), String> {
        self.profile_request(
            goblin_ipc::types::ProfileSwitchParams {
                name: name.to_owned(),
            },
            Pending::ProfileChange,
        )?;
        // Nothing of the profile being left stays on screen, from this moment.
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

    fn pair_start(&mut self, name: &str) -> Result<(), String> {
        self.profile_request(
            goblin_ipc::types::PairStartParams {
                profile: Some(name.to_owned()),
                timeout_s: Some(300),
                force: true,
            },
            Pending::PairStart,
        )?;
        self.pairing = Some(crate::data::PairingView {
            profile: name.to_owned(),
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
    }

    fn set_file_scope(&mut self, scope: goblin_ipc::types::FileScope) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        // Optimistic; the daemon's answer (the full consent) replaces it.
        self.consent.files = scope;
        self.dirty = true;
        let update = ConsentUpdate {
            files: Some(scope),
            ..Default::default()
        };
        if self.send(update, Pending::Consent) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
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
        if self.client.is_some() && self.send(params, Pending::Requests) {
            // Gone from the screen at once; the daemon's list follows.
            self.requests.retain(|r| r.id != id);
            self.dirty = true;
            Ok(())
        } else {
            Err("Muse service offline".into())
        }
    }

    fn set_build_access(&mut self, access: goblin_ipc::types::BuildAccess) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        self.consent.builds = access;
        self.dirty = true;
        let update = ConsentUpdate {
            builds: Some(access),
            ..Default::default()
        };
        if self.send(update, Pending::Consent) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn build_decide(&mut self, name: &str, approve: bool) -> Result<(), String> {
        let params = goblin_ipc::types::BuildsDecideParams {
            name: name.to_owned(),
            approve,
        };
        if self.client.is_some() && self.send(params, Pending::Builds) {
            Ok(())
        } else {
            Err("Muse service offline".into())
        }
    }

    fn build_remove(&mut self, name: &str) -> Result<(), String> {
        let params = goblin_ipc::types::BuildsRemoveParams {
            name: name.to_owned(),
        };
        if self.client.is_some() && self.send(params, Pending::Builds) {
            Ok(())
        } else {
            Err("Muse service offline".into())
        }
    }

    fn autostart(&self) -> Option<bool> {
        self.autostart
    }

    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        // Optimistic; the daemon's answer replaces it.
        self.autostart = Some(on);
        self.dirty = true;
        if self.send(
            goblin_ipc::types::PlatformAutostartParams { set: Some(on) },
            Pending::Autostart,
        ) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn ptt_start(&mut self) -> Result<(), String> {
        if self.client.is_none() {
            return Err("Muse service offline".into());
        }
        if self.ptt_note.is_some() || self.ptt_starting {
            return Ok(());
        }
        self.ptt_starting = true;
        self.ptt_release = None;
        let params = goblin_ipc::types::VoicePttStartParams {
            chat: self.active.clone(),
            mode: goblin_ipc::types::PttMode::Note,
        };
        if self.send(params, Pending::PttStart) {
            Ok(())
        } else {
            self.ptt_starting = false;
            Err("Lost the Muse service".into())
        }
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
        let params = goblin_ipc::types::VoicePttStopParams { note, cancel };
        if self.send(params, Pending::PttStop) {
            Ok(())
        } else {
            Err("Lost the Muse service".into())
        }
    }

    fn mark_notifications_seen(&mut self) {
        if self.notes.iter().all(|n| n.seen) {
            return;
        }
        for n in &mut self.notes {
            n.seen = true;
        }
        self.dirty = true;
        self.send(NotifySeenParams { ids: None }, Pending::Ack);
    }

    fn send_user(&mut self, text: &str) -> Result<(), String> {
        if self.client.is_none() {
            return Err(self
                .connect_error
                .clone()
                .unwrap_or_else(|| "Muse service offline".into()));
        }
        self.local_seq += 1;
        // The pills answered Muse's last reply; the daemon clears them too.
        self.suggestions.clear();
        let local_id = format!("local:{}", self.local_seq);
        self.rows.push(Row {
            id: local_id.clone(),
            turn: Turn {
                from_user: true,
                body: text.to_owned(),
                status: TurnStatus::Sending,
                media: Vec::new(),
                at: 0,
                reaction: None,
            },
        });
        self.rebuild_turns();
        let sent = self.send(
            ChatSendParams {
                chat: self.active.clone(),
                text: text.to_owned(),
                short_replies: false,
            },
            Pending::Send(local_id.clone()),
        );
        if !sent {
            if let Some(row) = self.rows.iter_mut().find(|r| r.id == local_id) {
                row.turn.status = TurnStatus::Failed;
            }
            self.rebuild_turns();
            return Err("Lost the Muse service".into());
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
                    self.drop_client(&format!("Lost the Muse service: {e}"));
                    self.next_connect_ms = now_ms + RETRY_MS;
                    break;
                }
            }
        }
        let changed = std::mem::take(&mut self.dirty);
        if changed {
            self.rebuild_turns();
        }
        changed
    }

    fn agent_busy(&self) -> bool {
        !self.open_turns.is_empty()
            || self
                .rows
                .iter()
                .any(|r| matches!(r.turn.status, TurnStatus::Sending | TurnStatus::Replying))
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

/// One Status log line: "system.run — refused · 2m", "device.health — ok, 7 ms · now".
/// Arguments are never logged by the daemon, so only the command name is shown.
pub fn command_line(e: &CommandEntry, now_ms: u64) -> String {
    let state = match e.state {
        CommandState::Running => "running",
        CommandState::Ok => "ok",
        CommandState::Error => "error",
        CommandState::Refused => "refused",
        _ => "?",
    };
    let ms = e.ms.map(|ms| format!(", {ms} ms")).unwrap_or_default();
    format!("{} — {state}{ms} · {}", e.name, age(now_ms, e.at))
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

fn now_ms_wall() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// "now", "5m", "2h", "3d" from two wall-clock millisecond values.
pub fn age(now_ms: u64, at_ms: u64) -> String {
    duration_caption(now_ms.saturating_sub(at_ms))
}

fn duration_caption(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goblin_ipc::types::Origin;

    fn msg(id: &str, role: Role, text: &str, state: MessageState) -> Message {
        Message {
            id: id.into(),
            chat: "main".into(),
            role,
            text: text.into(),
            rich: None,
            attachments: None,
            reaction: None,
            at: 1,
            state,
            origin: Origin::Gadget,
        }
    }

    #[test]
    fn status_lines() {
        let e = CommandEntry {
            id: "1".into(),
            at: 0,
            name: "system.run".into(),
            summary: "system.run".into(),
            state: CommandState::Refused,
            ms: None,
        };
        assert_eq!(command_line(&e, 120_000), "system.run — refused · 2m");
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
    fn captions() {
        assert_eq!(age(10_000, 9_000), "now");
        assert_eq!(age(10 * 60_000, 0), "10m");
        assert_eq!(age(3 * 3_600_000, 0), "3h");
        assert_eq!(age(2 * 86_400_000, 0), "2d");
        assert_eq!(age(0, 5), "now");
    }

    #[test]
    fn echo_replaces_optimistic_turn_and_reply_streams_in_place() {
        let mut p = LiveProvider::new("/nonexistent");
        p.rows.push(Row {
            id: "local:1".into(),
            turn: Turn {
                from_user: true,
                body: "hello".into(),
                status: TurnStatus::Sending,
                media: Vec::new(),
                at: 0,
                reaction: None,
            },
        });
        p.upsert(&msg("u1", Role::User, "hello", MessageState::Sent));
        p.upsert(&msg("a1", Role::Assistant, "Hi", MessageState::Streaming));
        p.upsert(&msg("a1", Role::Assistant, "Hi there", MessageState::Done));
        p.rebuild_turns();
        let t = p.main_chat();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].status, TurnStatus::Done);
        assert_eq!(t[1].body, "Hi there");
        assert_eq!(t[1].status, TurnStatus::Done);
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
}
