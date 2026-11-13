//! Shared structs and enums of the daemon/client protocol (see `docs/ipc.md`).
//!
//! Field meanings are specified in `docs/ipc.md`; the structs here mirror that document one to one.
//! Enums that the versioning rules make additive deserialize unknown values into `Unknown`.
//! Unknown struct fields are always ignored.
#![allow(missing_docs)] // fields are documented in docs/ipc.md

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Opaque turn identifier.
pub type TurnId = String;
/// Opaque voice-note identifier.
pub type NoteId = String;
/// `"main"` or a side-chat UUID string.
pub type ChatId = String;
/// Opaque pagination cursor.
pub type Cursor = String;
/// Unix timestamp in milliseconds.
pub type Millis = u64;

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(
                #[doc = concat!("The `", $s, "` value.")]
                #[serde(rename = $s)]
                $var,
            )+
            /// A value this version does not know (forward compatibility).
            #[serde(other, rename = "unknown")]
            Unknown,
        }
        impl $name {
            /// The wire spelling (`"unknown"` for unrecognized values).
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$var => $s,)+
                    Self::Unknown => "unknown",
                }
            }
        }
    };
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

wire_enum! {
    /// Machine-readable error code.
    ErrorCode {
        BadRequest = "bad_request",
        UnknownMethod = "unknown_method",
        NotPaired = "not_paired",
        NotConnected = "not_connected",
        Busy = "busy",
        NotFound = "not_found",
        Unavailable = "unavailable",
        ConsentOff = "consent_off",
        AccountRequired = "account_required",
        AccountLost = "account_lost",
        Timeout = "timeout",
        Rejected = "rejected",
        Uncertain = "uncertain",
        Internal = "internal",
    }
}

wire_enum! {
    /// Event topic used for subscription filters.
    Topic {
        Daemon = "daemon",
        Link = "link",
        Chat = "chat",
        Voice = "voice",
        Notify = "notify",
        Commands = "commands",
        Platform = "platform",
        Settings = "settings",
        Workshop = "workshop",
        Account = "account",
        Mirror = "mirror",
    }
}

wire_enum! {
    /// Which program is talking to the daemon.
    ClientKind { Ui = "ui", Ctl = "ctl" }
}

wire_enum! {
    /// State of the link to the Muse server.
    LinkState {
        Unpaired = "unpaired",
        Offline = "offline",
        Connecting = "connecting",
        Registering = "registering",
        Online = "online",
        Reconnecting = "reconnecting",
        AuthFailed = "auth_failed",
    }
}

wire_enum! {
    /// SDK (device token) or account mode.
    Mode { Sdk = "sdk", Account = "account" }
}

wire_enum! {
    /// Account link state.
    AccountStateKind { None = "none", Linking = "linking", Ok = "ok", Lost = "lost" }
}

wire_enum! {
    /// Pairing progress.
    PairPhase {
        NeedToken = "need_token",
        Advertising = "advertising",
        Connected = "connected",
        Confirming = "confirming",
        Wifi = "wifi",
        Registering = "registering",
        Done = "done",
        Failed = "failed",
        Cancelled = "cancelled",
        Timeout = "timeout",
    }
}

wire_enum! {
    /// Author of a message.
    Role { User = "user", Assistant = "assistant", System = "system" }
}

wire_enum! {
    /// Delivery or streaming state of a message.
    MessageState {
        Sending = "sending",
        Sent = "sent",
        Uncertain = "uncertain",
        Failed = "failed",
        Streaming = "streaming",
        Done = "done",
    }
}

wire_enum! {
    /// Where a message originated.
    Origin { Gadget = "gadget", Elsewhere = "elsewhere" }
}

wire_enum! {
    /// Progress of a chat turn.
    TurnState {
        Queued = "queued",
        Acked = "acked",
        Working = "working",
        Replying = "replying",
        Done = "done",
        Failed = "failed",
        Uncertain = "uncertain",
    }
}

wire_enum! {
    /// Block kind in rich assistant text.
    BlockKind {
        P = "p",
        H = "h",
        Ul = "ul",
        Ol = "ol",
        Code = "code",
        Quote = "quote",
    }
}

wire_enum! {
    /// Attachment kind.
    AttachmentKind { Image = "image", File = "file", Audio = "audio", Video = "video" }
}

wire_enum! {
    /// Push-to-talk mode.
    PttMode { Note = "note", Dictate = "dictate" }
}

wire_enum! {
    /// Audio level source.
    LevelSource { Mic = "mic", Playback = "playback" }
}

wire_enum! {
    /// Voice-note state.
    NoteState { Recording = "recording", Sending = "sending", Sent = "sent", Failed = "failed" }
}

wire_enum! {
    /// Start or stop of spoken playback.
    SpeakingState { Start = "start", Stop = "stop" }
}

wire_enum! {
    /// Realtime call state.
    CallState {
        Connecting = "connecting",
        Ready = "ready",
        Listening = "listening",
        Thinking = "thinking",
        Speaking = "speaking",
        Ended = "ended",
    }
}

wire_enum! {
    /// Notification kind.
    NoticeKind {
        Reply = "reply",
        TurnDone = "turn_done",
        TurnFailed = "turn_failed",
        Link = "link",
        Command = "command",
        Account = "account",
        Approval = "approval",
        Activity = "activity",
        Workshop = "workshop",
    }
}

wire_enum! {
    /// State of a device command log entry.
    CommandState { Running = "running", Ok = "ok", Error = "error", Refused = "refused" }
}

wire_enum! {
    /// Workshop game state.
    GameState { Installed = "installed", Building = "building", Failed = "failed" }
}

/// An empty JSON object (`{}`), used for parameterless requests and empty results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// Error body of a failed reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct IpcError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
}

impl IpcError {
    /// Builds an error with the given code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Activity line of the agent (`code` is machine-readable, `text` human-readable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub code: String,
    pub text: String,
}

/// Agent block of [`Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
}

/// VM block of [`Status`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Battery block of [`Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// Account link state with context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountState {
    pub state: AccountStateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of `daemon.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub profile: String,
    pub paired: bool,
    pub link: LinkState,
    pub agent: AgentInfo,
    pub vm: VmInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_age_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub mode: Mode,
    pub account: AccountState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<Battery>,
    pub uptime_s: u64,
    pub version: String,
}

/// A chat as listed by `chat.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    pub id: ChatId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<Millis>,
    pub unread: u32,
    pub pending_turns: u32,
}

/// A run of text with optional styling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub b: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub i: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub code: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// One block of rich assistant text. `p`, `h` and `quote` use `spans`, `ul`/`ol` use `items`
/// (a list of span lists) and `code` uses `text`; `level` applies to `h`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub t: BlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Vec<Span>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
}

/// File or media attached to a message; binary data is only ever a path under the spool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Where Muse keeps the file (a workspace path), so the daemon can fetch it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub chat: ChatId,
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich: Option<Vec<Block>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    /// The emoji reaction on this message, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<String>,
    pub at: Millis,
    pub state: MessageState,
    pub origin: Origin,
}

/// A deep link, e.g. `chat/main#m-4` (see [`RouteTarget`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Route(pub String);

/// Parsed form of a [`Route`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteTarget {
    /// `chat/<id>` or `chat/<id>#<message_id>`.
    Chat { id: String, message: Option<String> },
    /// `notifications`.
    Notifications,
    /// `status`.
    Status,
    /// `settings/<section>`.
    Settings(String),
    /// `workshop` or `workshop/<slug>`.
    Workshop(Option<String>),
    /// `feed/<post>`.
    Feed(String),
    /// `goals/<id>`.
    Goals(String),
    /// `library/<id>`.
    Library(String),
    /// `monitor/approvals/<id>`.
    Approval(String),
    /// A route this version does not understand.
    Unknown(String),
}

impl Route {
    /// Parses the route string; unrecognized shapes become [`RouteTarget::Unknown`].
    pub fn parse(&self) -> RouteTarget {
        let s = self.0.as_str();
        let nonempty = |x: &str| (!x.is_empty()).then(|| x.to_owned());
        let unknown = || RouteTarget::Unknown(self.0.clone());
        match s {
            "notifications" => return RouteTarget::Notifications,
            "status" => return RouteTarget::Status,
            "workshop" => return RouteTarget::Workshop(None),
            _ => {}
        }
        if let Some(rest) = s.strip_prefix("chat/") {
            let (id, msg) = match rest.split_once('#') {
                Some((i, m)) => (i, nonempty(m)),
                None => (rest, None),
            };
            return match nonempty(id) {
                Some(id) => RouteTarget::Chat { id, message: msg },
                None => unknown(),
            };
        }
        let tail = |p: &str| s.strip_prefix(p).and_then(nonempty);
        if let Some(x) = tail("settings/") {
            RouteTarget::Settings(x)
        } else if let Some(x) = tail("workshop/") {
            RouteTarget::Workshop(Some(x))
        } else if let Some(x) = tail("feed/") {
            RouteTarget::Feed(x)
        } else if let Some(x) = tail("goals/") {
            RouteTarget::Goals(x)
        } else if let Some(x) = tail("library/") {
            RouteTarget::Library(x)
        } else if let Some(x) = tail("monitor/approvals/") {
            RouteTarget::Approval(x)
        } else {
            unknown()
        }
    }
}

/// An inbox notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub at: Millis,
    pub kind: NoticeKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Route>,
    pub seen: bool,
}

/// One entry of the device command log. `summary` is at most 120 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntry {
    pub id: String,
    pub at: Millis,
    pub name: String,
    pub summary: String,
    pub state: CommandState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

/// How much of the device the file commands (`file.read`, `file.write`) may reach.
///
/// A profile's credentials, the other profiles' folders and the owner's command files are
/// out of reach at every level. The shell (`system.run`) is not held to any of this: a
/// profile with the shell on can reach the whole device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScope {
    /// Only the profile's own folder.
    Home,
    /// The removable storage (the SD card on a handheld): games, saves, the owner's files.
    Storage,
    /// Any path on the device, as the upstream SDK allows.
    #[default]
    Device,
}

impl FileScope {
    /// Every level, narrowest first.
    pub const ALL: [FileScope; 3] = [FileScope::Home, FileScope::Storage, FileScope::Device];
}

/// Whether Muse may install what it has built (tools, apps and games) on this device.
///
/// Nothing on a handheld confines what is installed: a tool runs as the daemon's account
/// when Muse calls it, and an app as whatever the launcher runs things as. So this is as
/// wide a grant as the shell, and it is off unless the owner turns it on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildAccess {
    /// Muse may not install anything; what is installed stays but its tools are not run.
    #[default]
    Off,
    /// Each install waits until the owner approves it on the device.
    Ask,
    /// Installs happen at once.
    Free,
}

impl BuildAccess {
    /// Every level, narrowest first.
    pub const ALL: [BuildAccess; 3] = [BuildAccess::Off, BuildAccess::Ask, BuildAccess::Free];
}

/// What Muse may do on this device for one profile (`commands.consent.get`): the command
/// toggles and the reach of the file commands. Each profile has its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    pub shell: bool,
    pub file_read: bool,
    pub file_write: bool,
    pub drop_ins: bool,
    /// Reach of the file commands. Absent in files written before it existed, which
    /// allowed any path.
    #[serde(default)]
    pub files: FileScope,
    /// Whether Muse may install what it has built. Absent in older files: off.
    #[serde(default)]
    pub builds: BuildAccess,
}

impl Default for Consent {
    /// All on and any path, matching the upstream SDK (and what a profile set up before
    /// per-profile access had).
    fn default() -> Self {
        Self {
            shell: true,
            file_read: true,
            file_write: true,
            drop_ins: true,
            files: FileScope::Device,
            builds: BuildAccess::Off,
        }
    }
}

impl Consent {
    /// What a newly created profile starts with: files in its own folder and nothing
    /// else, until the owner grants more.
    pub fn for_new_profile() -> Self {
        Self {
            shell: false,
            file_read: true,
            file_write: true,
            drop_ins: false,
            files: FileScope::Home,
            builds: BuildAccess::Off,
        }
    }
}

/// Partial consent update (`commands.consent.set`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_read: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_write: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_ins: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<FileScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builds: Option<BuildAccess>,
}

impl Consent {
    /// Applies the present fields of `u`.
    pub fn apply(&mut self, u: &ConsentUpdate) {
        if let Some(v) = u.shell {
            self.shell = v;
        }
        if let Some(v) = u.file_read {
            self.file_read = v;
        }
        if let Some(v) = u.file_write {
            self.file_write = v;
        }
        if let Some(v) = u.drop_ins {
            self.drop_ins = v;
        }
        if let Some(v) = u.files {
            self.files = v;
        }
        if let Some(v) = u.builds {
            self.builds = v;
        }
    }
}

/// What kind of thing Muse built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildKind {
    /// A command Muse can call on this device.
    Tool,
    /// A program listed among the launcher's tools.
    App,
    /// A program listed among the launcher's games.
    Game,
}

/// Where an install stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    /// Waiting for the owner to approve it on the device.
    Pending,
    /// Installed.
    Installed,
}

/// Something a profile's Muse built and installed, or is asking to (`builds.list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildItem {
    /// Its name: the tool's command name, and the folder it is kept in.
    pub name: String,
    pub kind: BuildKind,
    pub state: BuildState,
    /// What the owner sees: the launcher entry of an app or game.
    pub title: String,
    /// What Muse says it is for.
    pub description: String,
    /// What is run, as written in the bundle (relative to it).
    pub run: Vec<String>,
    pub files: u32,
    pub size_bytes: u64,
    /// Unix seconds: when it was installed, or asked for.
    pub at: u64,
    /// The chat Muse asked in, where the owner is asked and their answer goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
}

/// Params of `builds.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildsListParams {}

/// Result of `builds.list`: the profile in use's items, pending first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildsListResult {
    pub items: Vec<BuildItem>,
}

/// Params of `builds.decide`: the owner's answer to a pending install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildsDecideParams {
    pub name: String,
    pub approve: bool,
}

/// One way the owner can answer a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAnswer {
    /// `no`, `yes` or `hour`.
    pub id: String,
    /// The words on its button.
    pub label: String,
}

/// Something Muse has asked the owner, waiting to be answered in a chat (`requests.list`).
/// The handheld writes every word of it except `body`, which is Muse's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestView {
    /// `install:<name>` or `setting:<id>`.
    pub id: String,
    /// What is asked, in the handheld's words.
    pub heading: String,
    /// What Muse says about it (a description, or its reason).
    pub body: String,
    /// What the handheld knows about it.
    pub facts: String,
    /// The chat it was asked in.
    pub chat: String,
    /// The answers on offer, the one that changes nothing first.
    pub answers: Vec<RequestAnswer>,
}

/// Params of `requests.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestsListParams {}

/// Result of `requests.list`, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestsListResult {
    pub items: Vec<RequestView>,
}

/// Params of `requests.decide`: the owner's answer, by the answer's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestsDecideParams {
    pub id: String,
    pub answer: String,
}

/// Params of `builds.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildsRemoveParams {
    pub name: String,
}

/// A Workshop game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub slug: String,
    pub title: String,
    pub runtime: String,
    pub version: String,
    pub versions: Vec<String>,
    pub size_bytes: u64,
    pub chat: ChatId,
    pub installed_at: Millis,
    pub state: GameState,
}

/// Profile summary of `profile.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub paired: bool,
    pub has_sdk_token: bool,
    pub has_account: bool,
}

/// Listener address of an account link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

// ---- request params and results -------------------------------------------------------------

macro_rules! empty_params {
    ($($(#[$m:meta])* $name:ident),+ $(,)?) => {
        $(
            $(#[$m])*
            #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
            pub struct $name {}
        )+
    };
}

empty_params! {
    /// Params of `daemon.status`.
    DaemonStatusParams,
    /// Params of `daemon.shutdown`.
    DaemonShutdownParams,
    /// Params of `profile.list`.
    ProfileListParams,
    /// Params of `pair.cancel`.
    PairCancelParams,
    /// Params of `chat.list`.
    ChatListParams,
    /// Params of `voice.stop_speaking`.
    VoiceStopSpeakingParams,
    /// Params of `voice.call_end`.
    VoiceCallEndParams,
    /// Params of `voice.barge_in`.
    VoiceBargeInParams,
    /// Params of `voice.devices`.
    VoiceDevicesParams,
    /// Params of `notify.clear`.
    NotifyClearParams,
    /// Params of `commands.consent.get`.
    ConsentGetParams,
    /// Params of `platform.info`.
    PlatformInfoParams,
    /// Params of `platform.health`.
    PlatformHealthParams,
    /// Params of `workshop.list`.
    WorkshopListParams,
    /// Params of `account.status`.
    AccountStatusParams,
    /// Params of `account.link_start`.
    AccountLinkStartParams,
    /// Params of `account.link_cancel`.
    AccountLinkCancelParams,
    /// Params of `account.unlink`.
    AccountUnlinkParams,
}

/// Params of `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub client: ClientKind,
    pub version: String,
}

/// Result of `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub daemon_version: String,
    pub protocol: u32,
    pub run_id: String,
    pub profile: String,
    #[serde(default)]
    pub capabilities: Map<String, Value>,
}

/// Params of `subscribe`. Omitted `topics` means all; an empty list means none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<Topic>>,
}

/// Result of `subscribe`: the newest sequence number at subscription time and whether a gap exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub seq: u64,
    pub gap: bool,
}

/// Result of `profile.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileListResult {
    pub active: String,
    pub profiles: Vec<ProfileInfo>,
}

/// Params of a request that names one profile (`profile.create`, `.delete`, `.switch`).
macro_rules! name_params {
    ($($(#[$m:meta])* $name:ident),+) => {
        $(
            $(#[$m])*
            #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
            pub struct $name { pub name: String }
        )+
    };
}
name_params! {
    /// Params of `profile.create`.
    ProfileCreateParams,
    /// Params of `profile.delete`.
    ProfileDeleteParams,
    /// Params of `profile.switch`.
    ProfileSwitchParams
}

/// Params of `profile.rename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRenameParams {
    pub name: String,
    pub new_name: String,
}

/// Params of `profile.set_sdk_token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSetSdkTokenParams {
    pub name: String,
    pub token_path: String,
}

/// Params of `pair.unpair`: the profile to unpair (the one being served when absent).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairUnpairParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Params of `pair.start`; `timeout_s` defaults to 600 in the daemon, and `profile` to the
/// one being served. `force` pairs a profile that is already paired.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

/// Result of `pair.start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStartResult {
    pub ble_name: String,
}

/// Result of `chat.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatListResult {
    pub chats: Vec<Chat>,
}

/// Params of `chat.create`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Result of `chat.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCreateResult {
    pub chat: Chat,
}

/// Params of `chat.forget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatForgetParams {
    pub chat: ChatId,
}

/// Params of `chat.rename`: a new title for a side chat on this device (1 to 60 characters).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRenameParams {
    pub chat: ChatId,
    pub title: String,
}

/// Params of `chat.history`; `limit` defaults to 50 in the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryParams {
    pub chat: ChatId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result of `chat.history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryResult {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    pub complete: bool,
    /// Reply suggestions Muse set for this chat (`chat.suggest`), newest reply only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Params of `chat.suggest`: choices Muse offers the owner for what to say back to it, shown
/// as tappable pills (tapping sends one as the owner's message). Without
/// `chat`, they belong to the chat Muse answered most recently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSuggestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatId>,
    pub replies: Vec<String>,
}

/// Result of `chat.suggest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSuggestResult {
    pub chat: ChatId,
}

/// Payload of `chat.suggestions`: the pills for `chat` (empty clears them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSuggestionsEvent {
    pub chat: ChatId,
    pub replies: Vec<String>,
}

/// The choices share one row of pills: their lengths together are the limit
/// (`MAX_SUGGESTION_TOTAL_CHARS`); `MAX_SUGGESTIONS` only bounds the per-pill padding.
pub const MAX_SUGGESTION_TOTAL_CHARS: usize = 60;
/// Most choices (each pill also costs padding, so a full row of short ones).
pub const MAX_SUGGESTIONS: usize = 6;

/// Params of `chat.send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSendParams {
    pub chat: ChatId,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub short_replies: bool,
}

/// Result of `chat.send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSendResult {
    pub turn: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// Params of `chat.send_image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSendImageParams {
    pub chat: ChatId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub image_path: String,
}

/// Params of `chat.send_voice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSendVoiceParams {
    pub chat: ChatId,
    pub note: NoteId,
}

/// Result carrying only a turn id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnResult {
    pub turn: TurnId,
}

/// Params of `chat.mark_seen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMarkSeenParams {
    pub chat: ChatId,
    pub message_id: String,
}

/// Params of `chat.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCancelParams {
    pub turn: TurnId,
}

/// Params of `voice.ptt_start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePttStartParams {
    pub chat: ChatId,
    pub mode: PttMode,
}

/// Result of `voice.ptt_start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePttStartResult {
    pub note: NoteId,
}

/// Params of `voice.ptt_stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePttStopParams {
    pub note: NoteId,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancel: bool,
}

/// Result of `voice.ptt_stop`: `turn` for notes, `text` for dictation, neither when cancelled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePttStopResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Params of `voice.speak`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSpeakParams {
    pub message_id: String,
}

/// Params of `voice.call_start`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCallStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_duplex: bool,
}

/// Result of `voice.call_start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCallStartResult {
    pub call: String,
}

/// Result of `voice.call_end`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCallEndResult {
    pub reason: String,
}

/// Params of `voice.call_mute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCallMuteParams {
    pub muted: bool,
}

/// Result of `voice.devices`; device descriptions are platform-defined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceDevicesResult {
    pub inputs: Vec<Value>,
    pub outputs: Vec<Value>,
    pub selected: Value,
}

/// Params of `notify.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result of `notify.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyListResult {
    pub items: Vec<Notice>,
    pub unseen: u32,
}

/// Params of `notify.seen`; all notices when `ids` is omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifySeenParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
}

/// Params of `commands.log`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandsLogParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result of `commands.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandsLogResult {
    pub entries: Vec<CommandEntry>,
}

/// Params of `settings.get`; all keys when omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
}

/// A map of settings keys to values (`settings.get` result, `settings.changed` payload).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsValues {
    pub values: Map<String, Value>,
}

/// Params of `settings.set`.
pub type SettingsSetParams = SettingsValues;

macro_rules! percent_params {
    ($($(#[$m:meta])* $name:ident),+) => {
        $(
            $(#[$m])*
            #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
            pub struct $name {
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub set: Option<u8>,
            }
        )+
    };
}
percent_params! {
    /// Params of `platform.volume` (`set` is 0..=100).
    PlatformVolumeParams,
    /// Params of `platform.brightness` (`set` is 0..=100).
    PlatformBrightnessParams
}

/// Result of `platform.volume` and `platform.brightness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PercentResult {
    pub percent: u8,
}

/// Params of `platform.leds_test`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformLedsTestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

/// Params of `platform.rumble_test`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformRumbleTestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u32>,
}

/// Params of `platform.autostart`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformAutostartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<bool>,
}

/// Result of `platform.autostart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformAutostartResult {
    pub enabled: bool,
}

/// Free-form platform description (`platform.info`, `platform.health`).
pub type FreeForm = Map<String, Value>;

/// Params of `workshop.request`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopRequestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteId>,
}

/// Result of `workshop.request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopRequestResult {
    pub chat: ChatId,
    pub turn: TurnId,
}

/// Params of `workshop.modify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopModifyParams {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteId>,
}

/// Result of `workshop.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopListResult {
    pub games: Vec<Game>,
}

/// Params naming a game (`workshop.launch`, `.remove`, `.log`).
macro_rules! slug_params {
    ($($(#[$m:meta])* $name:ident),+) => {
        $(
            $(#[$m])*
            #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
            pub struct $name { pub slug: String }
        )+
    };
}
slug_params! {
    /// Params of `workshop.launch`.
    WorkshopLaunchParams,
    /// Params of `workshop.remove`.
    WorkshopRemoveParams,
    /// Params of `workshop.log`.
    WorkshopLogParams
}

/// Params of `workshop.rollback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopRollbackParams {
    pub slug: String,
    pub version: String,
}

/// Result of `workshop.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopLogResult {
    pub lines: Vec<String>,
}

/// Params of `workshop.install` (CLI only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopInstallParams {
    pub path: String,
}

/// Result of `account.link_start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLinkStartResult {
    pub code: String,
    pub expires_at: Millis,
    pub listen: Listen,
    pub drop_path: String,
}

/// Params of `mirror.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorGetParams {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<Value>,
}

/// Result of `mirror.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorGetResult {
    pub snapshot: Value,
    pub fetched_at: Millis,
    pub live: bool,
}

/// Params of `mirror.refresh`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorRefreshParams {
    pub kind: String,
}

/// Params of `mirror.act`: an action name plus action-specific arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorActParams {
    pub action: String,
    #[serde(flatten)]
    pub args: Map<String, Value>,
}

/// Result of `mirror.act`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorActResult {
    pub outcome: Value,
}

// ---- event payloads --------------------------------------------------------------------------

/// Payload of `daemon.profile_changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileChangedEvent {
    pub profile: String,
}

/// Payload of `link.state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStateEvent {
    pub state: LinkState,
    pub since: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Payload of `pair.state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStateEvent {
    /// The profile being paired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub state: PairPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Payload of `chat.message`: a new message or a whole-message replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessageEvent {
    pub message: Message,
}

/// Payload of `chat.delta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDeltaEvent {
    pub chat: ChatId,
    pub message_id: String,
    pub text_append: String,
}

/// Payload of `chat.turn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurnEvent {
    pub turn: TurnId,
    pub chat: ChatId,
    pub state: TurnState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
}

/// Payload of `chat.unread`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUnreadEvent {
    pub chat: ChatId,
    pub unread: u32,
}

/// Payload of `voice.level`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoiceLevelEvent {
    pub source: LevelSource,
    pub rms: f64,
}

/// Payload of `voice.note`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceNoteEvent {
    pub note: NoteId,
    pub state: NoteState,
    pub seconds: f64,
}

/// Payload of `voice.speaking`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSpeakingEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub state: SpeakingState,
}

/// Payload of `voice.call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceCallEvent {
    pub state: CallState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub muted: bool,
    pub seconds: f64,
}

/// Payload of `voice.caption`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCaptionEvent {
    pub role: Role,
    pub text: String,
    #[serde(rename = "final")]
    pub is_final: bool,
}

/// Payload of `notify.new`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyNewEvent {
    pub notice: Notice,
}

/// Payload of `notify.count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyCountEvent {
    pub unseen: u32,
}

/// Payload of `commands.started` and `commands.finished`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEvent {
    pub entry: CommandEntry,
}

/// Payload of `workshop.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopChangedEvent {
    pub slug: String,
}

/// Payload of `workshop.exited`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopExitedEvent {
    pub slug: String,
    pub code: i32,
    pub tail: String,
}

/// Payload of `mirror.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorChangedEvent {
    pub kind: String,
}
