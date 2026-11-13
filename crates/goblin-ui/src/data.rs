//! The data the UI renders, behind a [`DataProvider`] trait so the real IPC client can replace
//! the mock later. The mock implements navigable Chat (with a simulated streaming reply), side
//! chats, notifications, status, feed/ideas/goals/library and the account-linked toggle.

use std::collections::HashMap;

pub use goblin_ipc::types::Consent;
use goblin_ui_kit::widgets::{LinkState, TurnStatus};

/// Where a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
    /// Starting.
    Connecting,
    /// The microphone is open.
    Listening,
    /// Muse is working on what was said.
    Thinking,
    /// Muse is speaking.
    Speaking,
}

/// A call in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct CallView {
    /// Phase.
    pub phase: CallPhase,
    /// The microphone is muted.
    pub muted: bool,
}

/// Pills shown by the mock provider (the live app shows Muse's suggestions instead).
pub const DEFAULT_QUICK_REPLIES: [&str; 5] = ["Yes", "No", "Continue", "Tell me more", "Stop"];

/// The id of the main chat.
pub const MAIN_CHAT: &str = "main";

/// One consent toggle (`commands.consent.set` field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentKind {
    /// `system.run`.
    Shell,
    /// `file.read`.
    FileRead,
    /// `file.write`.
    FileWrite,
    /// Owner drop-in commands.
    DropIns,
}

impl ConsentKind {
    /// Reads this toggle from `c`.
    pub fn get(self, c: &Consent) -> bool {
        match self {
            ConsentKind::Shell => c.shell,
            ConsentKind::FileRead => c.file_read,
            ConsentKind::FileWrite => c.file_write,
            ConsentKind::DropIns => c.drop_ins,
        }
    }
    /// Sets this toggle in `c`.
    pub fn set(self, c: &mut Consent, on: bool) {
        match self {
            ConsentKind::Shell => c.shell = on,
            ConsentKind::FileRead => c.file_read = on,
            ConsentKind::FileWrite => c.file_write = on,
            ConsentKind::DropIns => c.drop_ins = on,
        }
    }
}

/// One profile (a Muse account this handheld can serve), as the app lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileEntry {
    /// The profile's name.
    pub name: String,
    /// The one being served now.
    pub active: bool,
    /// Paired with a Muse account.
    pub paired: bool,
}

/// How far a pairing has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairStep {
    /// Asked for; the radio is coming up.
    Opening,
    /// Waiting for the Muse app to find the handheld.
    Waiting,
    /// The app is connected.
    Connected,
    /// Confirmed in the app; it is choosing the network.
    Confirmed,
    /// The credentials are being checked with Muse.
    Checking,
    /// Paired.
    Done,
    /// It did not work (the text says why).
    Failed,
    /// Closed by the owner.
    Cancelled,
    /// Nobody came in time.
    TimedOut,
}

impl PairStep {
    /// Whether the pairing is over, one way or another.
    pub fn ended(self) -> bool {
        matches!(
            self,
            PairStep::Done | PairStep::Failed | PairStep::Cancelled | PairStep::TimedOut
        )
    }
}

/// A pairing under way or just ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingView {
    /// The profile being paired.
    pub profile: String,
    /// The name to choose in the Muse app, once known.
    pub ble_name: String,
    /// How far it has got.
    pub step: PairStep,
    /// Why it failed.
    pub detail: String,
}

/// A single chat turn.
#[derive(Clone, Debug)]
pub struct Turn {
    /// True for a message the user sent.
    pub from_user: bool,
    /// Markdown body (plain for user turns).
    pub body: String,
    /// Delivery status.
    pub status: TurnStatus,
    /// Pictures and clips sent with the message.
    pub media: Vec<crate::media::Media>,
    /// When it was sent (Unix ms); 0 when unknown.
    pub at: u64,
    /// An emoji reaction on it.
    pub reaction: Option<String>,
}

impl Turn {
    /// A completed user turn.
    pub fn user(body: &str) -> Self {
        Turn {
            from_user: true,
            body: body.into(),
            status: TurnStatus::Done,
            media: Vec::new(),
            at: 0,
            reaction: None,
        }
    }

    /// A completed agent turn.
    pub fn agent(body: &str) -> Self {
        Turn {
            from_user: false,
            body: body.into(),
            status: TurnStatus::Done,
            media: Vec::new(),
            at: 0,
            reaction: None,
        }
    }
}

/// A side chat summary.
#[derive(Clone, Debug)]
pub struct SideChat {
    /// Chat id (as `chat.*` IPC calls name it).
    pub id: String,
    /// Title.
    pub title: String,
    /// Last-activity caption.
    pub when: String,
    /// Whether it has replies the owner has not seen.
    pub unread: bool,
}

/// A notification inbox item.
#[derive(Clone, Debug)]
pub struct Notification {
    /// Title line.
    pub title: String,
    /// Body preview.
    pub body: String,
    /// Age caption.
    pub age: String,
    /// Whether it has been seen.
    pub seen: bool,
    /// The chat it is about (A opens it), if any.
    pub chat: Option<String>,
}

/// A feed post.
#[derive(Clone, Debug)]
pub struct FeedPost {
    /// Title.
    pub title: String,
    /// Body markdown.
    pub body: String,
    /// Age caption.
    pub age: String,
}

/// An idea card.
#[derive(Clone, Debug)]
pub struct Idea {
    /// Group label (empty for the personalised top idea).
    pub group: String,
    /// Title.
    pub title: String,
    /// Explanation.
    pub detail: String,
}

/// A goal.
#[derive(Clone, Debug)]
pub struct Goal {
    /// Title.
    pub title: String,
    /// Detail / latest activity.
    pub detail: String,
    /// Whether complete.
    pub done: bool,
}

/// A library item.
#[derive(Clone, Debug)]
pub struct LibraryItem {
    /// Name.
    pub name: String,
    /// Type label (DOCX, CSV, Space…).
    pub kind: String,
}

/// A Workshop game.
#[derive(Clone, Debug)]
pub struct Game {
    /// Title.
    pub title: String,
    /// Latest status line.
    pub status: String,
    /// Whether it is installed and launchable.
    pub installed: bool,
}

/// Device status fields.
#[derive(Clone, Debug)]
pub struct DeviceStatus {
    /// Link state.
    pub link: LinkState,
    /// VM / host name.
    pub vm: String,
    /// Token age caption.
    pub token_age: String,
    /// Last error (empty if none).
    pub last_error: String,
    /// Battery percent.
    pub battery: u8,
    /// Whether charging.
    pub charging: bool,
    /// Free storage caption.
    pub storage: String,
    /// Temperature caption.
    pub temperature: String,
    /// Recent commands Muse ran (most recent first).
    pub command_log: Vec<String>,
}

/// The read-only data the UI renders.
pub trait DataProvider {
    /// The agent's display name.
    fn agent_name(&self) -> &str;
    /// The transcript of the open chat (the main chat unless a side chat is open).
    fn main_chat(&self) -> &[Turn];
    /// The id of the open chat ([`MAIN_CHAT`] or a side chat id).
    fn active_chat(&self) -> &str {
        MAIN_CHAT
    }
    /// The open side chat's title, or `None` for the main chat.
    fn active_chat_title(&self) -> Option<&str> {
        let id = self.active_chat();
        (id != MAIN_CHAT).then(|| {
            self.side_chats()
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.title.as_str())
                .unwrap_or("Side chat")
        })
    }
    /// Side chats the gadget knows about.
    fn side_chats(&self) -> &[SideChat];
    /// Notifications inbox.
    fn notifications(&self) -> &[Notification];
    /// Device status.
    fn status(&self) -> &DeviceStatus;
    /// Workshop games.
    fn games(&self) -> &[Game];
    /// Feed posts (account mode).
    fn feed(&self) -> &[FeedPost];
    /// Idea cards (account mode).
    fn ideas(&self) -> &[Idea];
    /// Goals (account mode).
    fn goals(&self) -> &[Goal];
    /// Library items (account mode).
    fn library(&self) -> &[LibraryItem];
    /// Whether an account is linked (gates the M3 tabs).
    fn account_linked(&self) -> bool;
    /// Whether M3 content is stale (fallback to SDK mode).
    fn account_stale(&self) -> bool;

    // ---- actions and time ------------------------------------------------------------------

    /// Command consent toggles as the daemon enforces them.
    fn consent(&self) -> Consent;

    /// Sends a user message to the open chat. `Err` carries a short message for a toast.
    fn send_user(&mut self, text: &str) -> Result<(), String>;
    /// Opens a chat by id ([`MAIN_CHAT`] or a side chat id).
    fn open_chat(&mut self, id: &str);
    /// Starts a new side chat and opens it. Muse creates it with its first message.
    fn new_side_chat(&mut self) -> Result<(), String>;
    /// Gives a side chat a new title on this device.
    fn rename_side_chat(&mut self, _id: &str, _title: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Forgets a side chat on this device (Muse keeps the conversation).
    fn forget_side_chat(&mut self, _id: &str) -> Result<(), String> {
        Ok(())
    }
    /// Turns one consent toggle on or off.
    fn set_consent(&mut self, kind: ConsentKind, on: bool) -> Result<(), String>;
    /// Marks every notification seen (the inbox was opened).
    fn mark_notifications_seen(&mut self);
    /// A device setting as the daemon holds it (`settings.get` key), if known.
    fn setting(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    /// Changes a device setting; the daemon saves it.
    fn set_setting(&mut self, _key: &str, _value: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
    /// Changes whenever any setting does (so the app re-applies them).
    fn settings_rev(&self) -> u64 {
        0
    }
    /// Whether replies to voice notes are spoken (voice-to-voice); off means speech-to-text.
    fn voice_to_voice(&self) -> bool {
        false
    }
    /// Turns voice-to-voice on or off.
    fn set_voice_to_voice(&mut self, _on: bool) -> Result<(), String> {
        Ok(())
    }
    /// The profiles on this handheld; empty when they cannot be listed.
    fn profiles(&self) -> Vec<ProfileEntry> {
        Vec::new()
    }
    /// Serves another profile. Everything shown belongs to the profile served, so the
    /// conversation on screen is dropped at once.
    fn switch_profile(&mut self, _name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Creates an empty profile and returns its name.
    fn create_profile(&mut self) -> Result<String, String> {
        Err("Not available".into())
    }
    /// Gives a profile another name. Its pairing, chats, files and access stay with it.
    fn rename_profile(&mut self, _name: &str, _new_name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Deletes a profile's credentials and cached conversations.
    fn delete_profile(&mut self, _name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Forgets a profile's pairing on this handheld.
    fn unpair_profile(&mut self, _name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Opens Bluetooth pairing for a profile.
    fn pair_start(&mut self, _name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Closes an open pairing.
    fn pair_cancel(&mut self) {}
    /// The pairing under way, or the one that just ended.
    fn pairing(&self) -> Option<PairingView> {
        None
    }
    /// Forgets a pairing that has ended (its result has been shown).
    fn pairing_seen(&mut self) {}
    /// How far the file commands may reach for the profile served.
    fn set_file_scope(&mut self, _scope: goblin_ipc::types::FileScope) -> Result<(), String> {
        Err("Not available".into())
    }
    /// What Muse has asked the owner and is waiting on, oldest first.
    fn requests(&self) -> Vec<goblin_ipc::types::RequestView> {
        Vec::new()
    }
    /// The owner's answer to a request (`no`, `yes` or `hour`).
    fn request_decide(&mut self, _id: &str, _answer: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// What the profile's Muse has built and installed here, those waiting first.
    fn builds(&self) -> Vec<goblin_ipc::types::BuildItem> {
        Vec::new()
    }
    /// Whether Muse may install what it builds.
    fn set_build_access(&mut self, _access: goblin_ipc::types::BuildAccess) -> Result<(), String> {
        Err("Not available".into())
    }
    /// The owner's answer to an install that is waiting.
    fn build_decide(&mut self, _name: &str, _approve: bool) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Removes something Muse installed.
    fn build_remove(&mut self, _name: &str) -> Result<(), String> {
        Err("Not available".into())
    }
    /// Whether the daemon starts when the handheld does; `None` where the device has no way
    /// to arrange that (the Settings row is then left out).
    fn autostart(&self) -> Option<bool> {
        None
    }
    /// Turns start with the handheld on or off.
    fn set_autostart(&mut self, _on: bool) -> Result<(), String> {
        Err("Not available on this device".into())
    }
    /// Whether the open chat's messages have been read from the daemon and it can be sent
    /// to (false right after opening a chat, and while the daemon is out of reach).
    fn chat_ready(&self) -> bool {
        true
    }
    /// Asks for messages older than those shown in the open chat, if there may be any and
    /// none are on their way. Returns whether a request was started.
    fn load_older(&mut self) -> bool {
        false
    }
    /// How many older messages were put in front of the open chat since the last call.
    fn take_older_added(&mut self) -> usize {
        0
    }
    /// The quick-reply pills for the open chat (Muse's suggestions in the live app).
    fn quick_replies(&self) -> Vec<String> {
        DEFAULT_QUICK_REPLIES
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
    /// The call in progress, if any.
    fn call(&self) -> Option<CallView> {
        None
    }
    /// Why the last call ended (taken once).
    fn take_call_end(&mut self) -> Option<String> {
        None
    }
    /// Starts a call (in the open chat).
    fn call_start(&mut self) -> Result<(), String> {
        Err("Calls are not available here".into())
    }
    /// Hangs up.
    fn call_end(&mut self) {}
    /// Interrupts Muse's reply.
    fn call_barge_in(&mut self) {}
    /// Mutes or unmutes the microphone.
    fn call_mute(&mut self, _muted: bool) {}
    /// Where the agent's own avatar files are, once the daemon has prepared them.
    fn avatar_dir(&self) -> Option<String> {
        None
    }
    /// The microphone's level while recording (0..1), when the provider measures it.
    fn mic_level(&self) -> Option<f32> {
        None
    }
    /// Whether a reply is being spoken.
    fn speaking(&self) -> bool {
        false
    }
    /// Stops the reply being spoken.
    fn stop_speaking(&mut self) {}
    /// Starts recording a voice note for the open chat (push-to-talk pressed).
    fn ptt_start(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Ends the recording: sends it, or discards it with `cancel`.
    fn ptt_stop(&mut self, _cancel: bool) -> Result<(), String> {
        Ok(())
    }
    /// Advances provider state (mock streaming, live IPC traffic). Call once per frame; returns
    /// true when anything visible changed.
    fn pump(&mut self, now_ms: u64) -> bool;
    /// What Muse says it is doing right now ("Researching facts"), or empty.
    fn activity(&self) -> String {
        String::new()
    }
    /// Whether Muse is working on or replying to a turn (drives the mascot).
    fn agent_busy(&self) -> bool;
    /// Whether the provider needs frames at display rate (e.g. text streaming in).
    fn animating(&self) -> bool {
        false
    }
    /// Demo/screenshot scenario ("markdown", "empty", "offline", "long", "working"). Live
    /// providers ignore it.
    fn set_demo_scenario(&mut self, _name: &str) {}
    /// Demo/screenshot control of the reply pills. Live providers ignore it.
    fn set_demo_quick_replies(&mut self, _replies: Vec<String>) {}
    /// Demo/screenshot control of the account-linked state. Live providers ignore it.
    fn set_linked_demo(&mut self, _linked: bool) {}
}

/// An in-memory mock provider used by the shell and the golden tests.
pub struct MockProvider {
    /// Agent name.
    pub name: String,
    /// Main chat turns.
    pub chat: Vec<Turn>,
    /// Side chats.
    pub sides: Vec<SideChat>,
    /// Notifications.
    pub notes: Vec<Notification>,
    /// Status.
    pub device: DeviceStatus,
    /// Games.
    pub games: Vec<Game>,
    /// Feed.
    pub feed: Vec<FeedPost>,
    /// Ideas.
    pub ideas: Vec<Idea>,
    /// Goals.
    pub goals: Vec<Goal>,
    /// Library.
    pub library: Vec<LibraryItem>,
    /// Account linked.
    pub linked: bool,
    /// Account stale.
    pub stale: bool,
    /// Consent toggles.
    pub consent: Consent,
    /// Voice-to-voice setting.
    pub voice_to_voice: bool,
    /// Start with the handheld (`None`: the device cannot).
    pub autostart: Option<bool>,
    /// The profiles, and a pairing being shown.
    pub profiles: Vec<ProfileEntry>,
    /// A pairing under way.
    pub pairing: Option<PairingView>,
    /// A reply is being spoken (tests set it).
    pub speaking: bool,
    /// Simulated call.
    pub call: Option<CallView>,
    /// Settings changed on this device (keys as the daemon names them).
    pub settings: HashMap<String, serde_json::Value>,
    settings_rev: u64,
    /// Demo: what Muse built.
    pub builds: Vec<goblin_ipc::types::BuildItem>,
    /// Demo: what Muse has asked.
    pub requests: Vec<goblin_ipc::types::RequestView>,
    /// Demo: the last request answered, and how.
    pub answered: Option<(String, String)>,
    /// Demo: directory of prepared avatar files (`GOBLIN_DEMO_AVATAR`).
    pub avatar_dir: Option<String>,
    /// Demo: what Muse is doing (also makes it busy).
    pub demo_activity: String,
    /// Reply pills (defaults unless a demo sets them).
    pub quick: Vec<String>,
    call_end_reason: Option<String>,
    /// The open chat id.
    active: String,
    /// Transcripts of the chats that are not open (the open one lives in `chat`).
    parked: HashMap<String, Vec<Turn>>,
    /// A simulated reply is due to start on the next pump.
    reply_pending: bool,
    /// Next reveal time of the simulated streaming reply.
    reply_next_ms: Option<u64>,
}

impl MockProvider {
    /// Builds a populated mock. Content is synthetic and generic (no app text copied).
    pub fn new() -> Self {
        let chat = vec![
            Turn::user("Can you keep an eye on the handheld's battery and tell me when it dips below 20%?"),
            Turn::agent(
                "Done. I'll watch the battery and ping you here when it crosses **20%**.\n\nWhile I'm at it, here's what I can do from this device right now:\n\n- Read your main chat and side chats\n- Send text, images and voice notes\n- Run device commands you approve\n\nWant me to set up a low-battery reminder with a custom threshold?",
            ),
            Turn::user("20% is fine. Also, what's our plan for the game you were building?"),
            Turn::agent(
                "Here's where the breakout clone stands:\n\n1. Paddle and ball physics are in\n2. Brick layout loads from `levels/01.toml`\n3. Still to do: sound and a win screen\n\nI will have a new build ready in a few minutes and tell you here when it is.",
            ),
        ];
        let sides = vec![
            SideChat {
                id: "side-1".into(),
                title: "Breakout clone — build log".into(),
                when: "2m".into(),
                unread: true,
            },
            SideChat {
                id: "side-2".into(),
                title: "Weekend trip ideas".into(),
                when: "1h".into(),
                unread: true,
            },
            SideChat {
                id: "side-3".into(),
                title: "Grocery list".into(),
                when: "1d".into(),
                unread: false,
            },
            SideChat {
                id: "side-4".into(),
                title: "Reading notes: handheld UX".into(),
                when: "2d".into(),
                unread: false,
            },
            SideChat {
                id: "side-5".into(),
                title: "Synthetic marker test thread".into(),
                when: "3d".into(),
                unread: false,
            },
        ];
        let notes = vec![
            Notification {
                title: "Breakout clone: new build".into(),
                body: "Sound is in. Have a look when you can.".into(),
                age: "now".into(),
                seen: false,
                chat: None,
            },
            Notification {
                title: "Reply ready".into(),
                body: "I finished the weekend trip summary.".into(),
                age: "12m".into(),
                seen: false,
                chat: Some("side-2".into()),
            },
            Notification {
                title: "Command finished".into(),
                body: "device.health ran in 7 ms.".into(),
                age: "1h".into(),
                seen: true,
                chat: None,
            },
        ];
        let device = DeviceStatus {
            link: LinkState::Connected,
            vm: "vm-brick-01".into(),
            token_age: "18m".into(),
            last_error: String::new(),
            battery: 72,
            charging: false,
            storage: "890 MB free".into(),
            temperature: "41°C CPU".into(),
            command_log: vec![
                "device.health — ok, 7 ms · 2m".into(),
                "system.run — ok, 22 ms · 5m".into(),
                "file.write — ok, 31 ms · 1h".into(),
            ],
        };
        let games = vec![
            Game {
                title: "Breakout".into(),
                status: "Installed · v3 · built 2m ago".into(),
                installed: true,
            },
            Game {
                title: "Starfield runner".into(),
                status: "Building… writing assets".into(),
                installed: false,
            },
        ];
        let feed = vec![
            FeedPost { title: "A quieter way to track battery".into(), body: "Your handheld can now warn you before it sleeps. Here's the gist and why it matters for long sessions.".into(), age: "4h".into() },
            FeedPost { title: "Little games, built on the go".into(), body: "Ask once and a playable build lands in minutes. [How it works](https://example.com) covers the runtime.".into(), age: "1d".into() },
        ];
        let ideas = vec![
            Idea { group: String::new(), title: "Draft your weekend plan".into(), detail: "I can turn your trip chat into a packing list and a day-by-day outline for your approval.".into() },
            Idea { group: "Handheld".into(), title: "Set a low-battery reminder".into(), detail: "Tell me a threshold and I'll nudge you here before the device sleeps.".into() },
            Idea { group: "Handheld".into(), title: "Make a tiny game".into(), detail: "Describe a game and I will build it while you watch the log.".into() },
        ];
        let goals = vec![
            Goal {
                title: "Ship the handheld client".into(),
                detail: "M1 core UI in progress; chat and status screens drawn.".into(),
                done: false,
            },
            Goal {
                title: "Daily reading".into(),
                detail: "Logged 4 of 7 days this week.".into(),
                done: false,
            },
            Goal {
                title: "Set up the dev profile".into(),
                detail: "Paired and registered.".into(),
                done: true,
            },
        ];
        let library = vec![
            LibraryItem {
                name: "handheld-ux-notes.docx".into(),
                kind: "DOCX".into(),
            },
            LibraryItem {
                name: "battery-samples.csv".into(),
                kind: "CSV".into(),
            },
            LibraryItem {
                name: "Trip planner".into(),
                kind: "Space".into(),
            },
            LibraryItem {
                name: "paddle-sketch.png".into(),
                kind: "Image".into(),
            },
        ];
        MockProvider {
            // `GOBLIN_DEMO_NAME` renames the made-up agent (for pictures of the app).
            name: std::env::var("GOBLIN_DEMO_NAME").unwrap_or_else(|_| "Pip".into()),
            chat,
            sides,
            notes,
            device,
            games,
            feed,
            ideas,
            goals,
            library,
            linked: false,
            stale: false,
            consent: Consent {
                builds: goblin_ipc::types::BuildAccess::Ask,
                ..Consent::default()
            },
            voice_to_voice: false,
            autostart: Some(false),
            profiles: vec![
                ProfileEntry {
                    name: "personal".into(),
                    active: true,
                    paired: true,
                },
                ProfileEntry {
                    name: "work".into(),
                    active: false,
                    paired: true,
                },
            ],
            pairing: None,
            speaking: false,
            call: None,
            settings: HashMap::new(),
            settings_rev: 0,
            builds: demo_builds(),
            requests: Vec::new(),
            answered: None,
            avatar_dir: std::env::var("GOBLIN_DEMO_AVATAR").ok(),
            demo_activity: String::new(),
            quick: DEFAULT_QUICK_REPLIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            call_end_reason: None,
            active: MAIN_CHAT.into(),
            parked: HashMap::new(),
            reply_pending: false,
            reply_next_ms: None,
        }
    }

    /// Appends a user turn (from the composer) and queues a simulated agent reply.
    pub fn push_user(&mut self, text: &str) {
        self.chat.push(Turn {
            from_user: true,
            body: text.into(),
            status: TurnStatus::Sending,
            media: Vec::new(),
            at: 0,
            reaction: None,
        });
        self.reply_pending = true;
    }

    /// Marks the last user turn delivered and starts an agent reply that streams in `update`.
    pub fn start_reply(&mut self) {
        if let Some(last) = self.chat.last_mut() {
            if last.from_user {
                last.status = TurnStatus::Done;
            }
        }
        self.chat.push(Turn {
            from_user: false,
            body: String::new(),
            status: TurnStatus::Replying,
            media: Vec::new(),
            at: 0,
            reaction: None,
        });
    }

    /// The full text a streaming reply builds toward.
    pub fn reply_target() -> &'static str {
        "Got it — I'll take care of that.\n\nHere's my plan:\n\n1. Confirm the request on-device\n2. Do the work in my cloud VM\n3. Report back here when it's ready\n\nI'll keep this thread updated as I go."
    }

    /// Advances a streaming reply by revealing more characters; returns true while still growing.
    pub fn advance_reply(&mut self, chars: usize) -> bool {
        let target = Self::reply_target();
        if let Some(last) = self.chat.last_mut() {
            if last.status == TurnStatus::Replying {
                let shown = last.body.chars().count();
                if shown >= target.chars().count() {
                    last.status = TurnStatus::Done;
                    return false;
                }
                let take = (shown + chars).min(target.chars().count());
                last.body = target.chars().take(take).collect();
                if take >= target.chars().count() {
                    last.status = TurnStatus::Done;
                    return false;
                }
                return true;
            }
        }
        false
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        MockProvider::new()
    }
}

impl DataProvider for MockProvider {
    fn agent_name(&self) -> &str {
        &self.name
    }
    fn main_chat(&self) -> &[Turn] {
        &self.chat
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
        self.linked
    }
    fn active_chat(&self) -> &str {
        &self.active
    }
    fn consent(&self) -> Consent {
        self.consent
    }
    fn send_user(&mut self, text: &str) -> Result<(), String> {
        self.push_user(text);
        Ok(())
    }
    fn open_chat(&mut self, id: &str) {
        if id == self.active {
            return;
        }
        let open = std::mem::take(&mut self.chat);
        self.parked.insert(std::mem::take(&mut self.active), open);
        self.chat = self.parked.remove(id).unwrap_or_default();
        self.active = id.to_owned();
        self.reply_pending = false;
        self.reply_next_ms = None;
    }
    fn new_side_chat(&mut self) -> Result<(), String> {
        let id = format!("side-new-{}", self.sides.len() + 1);
        self.sides.insert(
            0,
            SideChat {
                id: id.clone(),
                title: "New chat".into(),
                when: "now".into(),
                unread: false,
            },
        );
        self.open_chat(&id);
        Ok(())
    }
    fn set_consent(&mut self, kind: ConsentKind, on: bool) -> Result<(), String> {
        kind.set(&mut self.consent, on);
        Ok(())
    }
    fn rename_side_chat(&mut self, id: &str, title: &str) -> Result<(), String> {
        match self.sides.iter_mut().find(|c| c.id == id) {
            Some(c) => {
                c.title = title.to_owned();
                Ok(())
            }
            None => Err("No such chat".into()),
        }
    }
    fn forget_side_chat(&mut self, id: &str) -> Result<(), String> {
        self.sides.retain(|c| c.id != id);
        self.parked.remove(id);
        Ok(())
    }
    fn voice_to_voice(&self) -> bool {
        self.voice_to_voice
    }
    fn set_voice_to_voice(&mut self, on: bool) -> Result<(), String> {
        self.voice_to_voice = on;
        Ok(())
    }
    fn autostart(&self) -> Option<bool> {
        self.autostart
    }
    fn set_autostart(&mut self, on: bool) -> Result<(), String> {
        self.autostart = Some(on);
        Ok(())
    }
    fn profiles(&self) -> Vec<ProfileEntry> {
        self.profiles.clone()
    }
    fn switch_profile(&mut self, name: &str) -> Result<(), String> {
        if !self.profiles.iter().any(|p| p.name == name) {
            return Err("No such profile".into());
        }
        for p in &mut self.profiles {
            p.active = p.name == name;
        }
        Ok(())
    }
    fn create_profile(&mut self) -> Result<String, String> {
        let name = (2..)
            .map(|n| format!("profile-{n}"))
            .find(|n| !self.profiles.iter().any(|p| &p.name == n))
            .unwrap_or_default();
        self.profiles.push(ProfileEntry {
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
        for p in &mut self.profiles {
            if p.name == name {
                p.name = new_name.to_owned();
            }
        }
        Ok(())
    }
    fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        if self.profiles.iter().any(|p| p.name == name && p.active) {
            return Err("The profile in use cannot be deleted".into());
        }
        self.profiles.retain(|p| p.name != name);
        Ok(())
    }
    fn unpair_profile(&mut self, name: &str) -> Result<(), String> {
        for p in &mut self.profiles {
            if p.name == name {
                p.paired = false;
            }
        }
        Ok(())
    }
    fn pair_start(&mut self, name: &str) -> Result<(), String> {
        self.pairing = Some(PairingView {
            profile: name.to_owned(),
            ble_name: "MuseGadgetA1B2C3".into(),
            step: PairStep::Waiting,
            detail: String::new(),
        });
        Ok(())
    }
    fn pair_cancel(&mut self) {
        self.pairing = None;
    }
    fn pairing(&self) -> Option<PairingView> {
        self.pairing.clone()
    }
    fn pairing_seen(&mut self) {
        self.pairing = None;
    }
    fn set_file_scope(&mut self, scope: goblin_ipc::types::FileScope) -> Result<(), String> {
        self.consent.files = scope;
        Ok(())
    }
    fn builds(&self) -> Vec<goblin_ipc::types::BuildItem> {
        self.builds.clone()
    }
    fn requests(&self) -> Vec<goblin_ipc::types::RequestView> {
        self.requests.clone()
    }
    fn request_decide(&mut self, id: &str, answer: &str) -> Result<(), String> {
        self.requests.retain(|r| r.id != id);
        self.answered = Some((id.to_owned(), answer.to_owned()));
        match id.strip_prefix("install:") {
            Some(name) => self.build_decide(name, answer == "yes"),
            None => Ok(()),
        }
    }
    fn set_build_access(&mut self, access: goblin_ipc::types::BuildAccess) -> Result<(), String> {
        self.consent.builds = access;
        Ok(())
    }
    fn build_decide(&mut self, name: &str, approve: bool) -> Result<(), String> {
        if approve {
            for b in self.builds.iter_mut().filter(|b| b.name == name) {
                b.state = goblin_ipc::types::BuildState::Installed;
            }
        } else {
            self.builds.retain(|b| b.name != name);
        }
        Ok(())
    }
    fn build_remove(&mut self, name: &str) -> Result<(), String> {
        self.builds.retain(|b| b.name != name);
        Ok(())
    }
    fn setting(&self, key: &str) -> Option<serde_json::Value> {
        self.settings.get(key).cloned()
    }
    fn set_setting(&mut self, key: &str, value: serde_json::Value) -> Result<(), String> {
        self.settings.insert(key.to_owned(), value);
        self.settings_rev += 1;
        Ok(())
    }
    fn settings_rev(&self) -> u64 {
        self.settings_rev
    }
    fn speaking(&self) -> bool {
        self.speaking
    }
    fn stop_speaking(&mut self) {
        self.speaking = false;
    }
    fn quick_replies(&self) -> Vec<String> {
        self.quick.clone()
    }
    fn set_demo_quick_replies(&mut self, replies: Vec<String>) {
        self.quick = replies;
    }
    fn set_demo_scenario(&mut self, name: &str) {
        match name {
            // Everything Markdown can throw at a 3.2-inch screen.
            "markdown" => {
                self.chat = vec![
                    Turn::user("Show me everything you can format."),
                    Turn::agent(
                        "# Heading one\n\n## Heading two\n\nA paragraph with **bold**, *italic*, `inline code`, a [link to the docs](https://example.com/a/very/long/path/that/keeps/going) and a_very_long_unbroken_identifier_that_cannot_wrap_at_spaces_at_all.\n\n1. First step\n   - nested bullet one\n   - nested bullet two with enough text to wrap onto a second line on this screen\n2. Second step\n\n> A quoted line from earlier.\n\n```rust\nfn main() {\n    println!(\"a line of code that is much wider than the bubble can show\");\n}\n```\n\n| Item | Qty | Price |\n|---|---|---|\n| Paddle | 1 | $4.00 |\n| Bricks | 48 | $0.10 |\n\nDone ✅ — total: 12 345,67 €.",
                    ),
                ];
                self.quick = vec!["Thanks".into(), "Explain the table".into()];
            }
            "empty" => {
                self.chat.clear();
                self.sides.clear();
                self.notes.clear();
                self.games.clear();
                self.device.command_log.clear();
                self.quick.clear();
            }
            "offline" => {
                self.device.link = LinkState::Error;
                self.device.last_error = "The Muse service is not running".into();
                self.device.vm = "—".into();
                self.device.battery = 9;
                self.quick.clear();
                if let Some(last) = self.chat.last_mut() {
                    last.status = TurnStatus::Failed;
                }
                self.chat.push(Turn {
                    from_user: true,
                    body: "Are you there?".into(),
                    status: TurnStatus::Failed,
                    media: Vec::new(),
                    at: 0,
                    reaction: None,
                });
            }
            "long" => {
                self.name = "Bartholomew the Extraordinarily Verbose Assistant".into();
                for (i, s) in self.sides.iter_mut().enumerate() {
                    s.title = format!(
                        "A side chat with a very long title that goes on and on, number {} of several",
                        i + 1
                    );
                }
                for n in &mut self.notes {
                    n.title = format!("{} — and a title too long to fit on one line here", n.title);
                    n.body = format!("{} This body text keeps going well past the width of the row to test truncation.", n.body);
                }
                self.quick = vec![
                    "Yes please".into(),
                    "No thanks".into(),
                    "Maybe later".into(),
                    "Tell me more".into(),
                    "Why not?".into(),
                    "Sure, go on".into(),
                ];
                self.device.vm = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d-and-then-some".into();
                self.device.storage = "2.9 GB free of 62.2 GB".into();
            }
            // Muse has asked, in the main chat, to install the tool that is waiting.
            "request" => {
                let answer = |id: &str, label: &str| goblin_ipc::types::RequestAnswer {
                    id: id.into(),
                    label: label.into(),
                };
                self.requests = vec![goblin_ipc::types::RequestView {
                    id: "install:battery.check".into(),
                    heading: "Muse asks to install \u{201c}battery.check\u{201d}: a tool it can \
                              call on this handheld"
                        .into(),
                    body: "Reads the battery level and whether it is charging, so I can warn \
                           you before it runs low."
                        .into(),
                    facts: "Runs bin/check.sh  \u{b7}  2 files, 1 KB  \u{b7}  Once run, nothing \
                            here holds it back"
                        .into(),
                    chat: MAIN_CHAT.into(),
                    answers: vec![answer("no", "Not now"), answer("yes", "Install it")],
                }];
                self.chat.push(Turn::agent(
                    "I made a small tool that reads the battery, so I can warn you before it \
                     runs low. The handheld is asking you whether to install it.",
                ));
            }
            // Muse has asked for a setting to be changed.
            "request-setting" => {
                let answer = |id: &str, label: &str| goblin_ipc::types::RequestAnswer {
                    id: id.into(),
                    label: label.into(),
                };
                self.requests = vec![goblin_ipc::types::RequestView {
                    id: "setting:file_reach".into(),
                    heading: "Muse asks to change \u{201c}Files it can reach\u{201d} to The SD \
                              card"
                        .into(),
                    body: "So I can read your Zelda save and tell you where you are.".into(),
                    facts: "Now: Its own folder  \u{b7}  For an hour puts it back afterwards"
                        .into(),
                    chat: MAIN_CHAT.into(),
                    answers: vec![
                        answer("no", "Not now"),
                        answer("yes", "Yes"),
                        answer("hour", "For an hour"),
                    ],
                }];
                self.chat.push(Turn::agent(
                    "To look at your save I need to read the SD card.",
                ));
            }
            // Pictures and a clip in a reply. `GOBLIN_DEMO_PICTURE` may name a PNG to show.
            "media" => {
                let picture = std::env::var("GOBLIN_DEMO_PICTURE").ok();
                let tile = |label: &str, is_video: bool| crate::media::Media {
                    picture: picture.clone(),
                    video: is_video.then(|| "/nonexistent/demo.mp4".to_owned()),
                    is_video,
                    label: label.into(),
                };
                self.chat = vec![
                    Turn::user("Show me the level art and a clip of the game."),
                    Turn {
                        from_user: false,
                        body: "Here are two takes on the level art, and the clip.".into(),
                        status: TurnStatus::Done,
                        media: vec![
                            tile("Level art, warm", false),
                            tile("Level art, cool", false),
                            tile("Gameplay clip", true),
                        ],
                        at: 0,
                        reaction: None,
                    },
                ];
                self.quick = vec!["Use the warm one".into(), "Try again".into()];
            }
            // Timestamps between groups, a reaction, and enough to scroll back through.
            "details" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                let at = |mins_ago: u64| now.saturating_sub(mins_ago * 60_000);
                let turn = |from_user: bool, body: &str, mins: u64, reaction: Option<&str>| Turn {
                    from_user,
                    body: body.into(),
                    status: TurnStatus::Done,
                    media: Vec::new(),
                    at: at(mins),
                    reaction: reaction.map(str::to_owned),
                };
                self.chat = vec![
                    turn(true, "Morning. Anything overnight?", 26 * 60, None),
                    turn(
                        false,
                        "Two things: the build passed, and your parcel is out for delivery.",
                        26 * 60,
                        Some("\u{1F44D}"),
                    ),
                    turn(true, "Nice.", 26 * 60 - 1, None),
                    turn(true, "How long until the battery needs a charge?", 95, None),
                    turn(false, "About three hours at this brightness.", 95, None),
                    turn(true, "OK", 3, None),
                    turn(false, "Sure.", 3, Some("\u{2764}")),
                ];
                self.quick.clear();
            }
            "working" => {
                self.chat.push(Turn::user("Build me a tiny platformer."));
                self.quick.clear();
                self.demo_activity = "🛠 Sketching the level".into();
            }
            _ => {}
        }
    }
    fn call(&self) -> Option<CallView> {
        self.call.clone()
    }
    fn take_call_end(&mut self) -> Option<String> {
        self.call_end_reason.take()
    }
    fn call_start(&mut self) -> Result<(), String> {
        self.call = Some(CallView {
            phase: CallPhase::Listening,
            muted: false,
        });
        Ok(())
    }
    fn call_end(&mut self) {
        if self.call.take().is_some() {
            self.call_end_reason = Some("local_end".into());
        }
    }
    fn call_barge_in(&mut self) {
        if let Some(c) = &mut self.call {
            c.phase = CallPhase::Listening;
        }
    }
    fn call_mute(&mut self, muted: bool) {
        if let Some(c) = &mut self.call {
            c.muted = muted;
        }
    }
    fn mark_notifications_seen(&mut self) {
        for n in &mut self.notes {
            n.seen = true;
        }
    }

    fn pump(&mut self, now_ms: u64) -> bool {
        if self.reply_pending {
            self.reply_pending = false;
            self.start_reply();
            self.reply_next_ms = Some(now_ms + 300);
            return true;
        }
        match self.reply_next_ms {
            Some(next) if now_ms >= next => {
                let more = self.advance_reply(3);
                self.reply_next_ms = more.then_some(now_ms + 45);
                true
            }
            _ => false,
        }
    }

    fn agent_busy(&self) -> bool {
        self.reply_pending || self.reply_next_ms.is_some() || !self.demo_activity.is_empty()
    }

    fn activity(&self) -> String {
        self.demo_activity.clone()
    }

    fn avatar_dir(&self) -> Option<String> {
        self.avatar_dir.clone()
    }

    fn animating(&self) -> bool {
        self.reply_pending || self.reply_next_ms.is_some()
    }

    fn set_linked_demo(&mut self, linked: bool) {
        self.linked = linked;
    }

    fn account_stale(&self) -> bool {
        self.stale
    }
}

/// Made-up builds for the demo: a tool waiting for the owner and a game already installed.
fn demo_builds() -> Vec<goblin_ipc::types::BuildItem> {
    use goblin_ipc::types::{BuildItem, BuildKind, BuildState};
    vec![
        BuildItem {
            name: "battery.check".into(),
            kind: BuildKind::Tool,
            state: BuildState::Pending,
            title: "battery.check".into(),
            description: "Reads the battery level and whether it is charging, so I can warn \
                          you before it runs low."
                .into(),
            run: vec!["bin/check.sh".into()],
            files: 2,
            size_bytes: 812,
            at: 1_794_500_000,
            chat: None,
        },
        BuildItem {
            name: "breakout".into(),
            kind: BuildKind::Game,
            state: BuildState::Installed,
            title: "Breakout".into(),
            description: "The brick game we made together.".into(),
            run: vec!["start.sh".into()],
            files: 14,
            size_bytes: 386_000,
            at: 1_794_400_000,
            chat: None,
        },
    ]
}
