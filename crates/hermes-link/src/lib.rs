//! The handheld's link to a Hermes Agent.
//!
//! Hermes runs a Goblin Link plugin that listens for handhelds on a WebSocket with TLS, under
//! a certificate it made itself. A handheld is paired once, with a link the owner pastes
//! ([`PairLink`]): it names where the plugin listens, the fingerprint of its certificate and
//! a one-time code. From then on the handheld connects with the device token it was given
//! ([`Pairing`]) and trusts that certificate and no other.
//!
//! Over the connection go JSON objects, one per message, each with a `type`
//! (`docs/hermes-protocol.md`). This crate knows that protocol and nothing about the daemon:
//! a turn comes back as [`TurnEvent`]s, and Hermes's calls of the handheld's commands go to
//! a [`Runner`].

mod pin;
#[cfg(feature = "testsupport")]
pub mod testsupport;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

/// The protocol version this crate speaks.
pub const PROTOCOL: u32 = 1;
/// How long the plugin gets to answer a pairing, a hello or a new turn.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest message accepted from the plugin.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// The longest speech taken for one reply, as MP3.
const MAX_SPEECH_BYTES: usize = 2 * 1024 * 1024;
/// The largest picture taken as the agent's avatar. The plugin sends a small square PNG.
const MAX_AVATAR_BYTES: usize = 1024 * 1024;
/// The longest question kept for a card.
const MAX_PROMPT_CHARS: usize = 2_000;
/// How long Hermes gets to turn a reply into speech.
const SPEECH_TIMEOUT: Duration = Duration::from_secs(60);
/// Turn events kept for a turn nobody is reading yet.
const TURN_BACKLOG: usize = 256;
/// Messages waiting to be written. A plugin that stops reading is hung up on, not buffered for.
const OUT_BACKLOG: usize = 256;
/// Commands Hermes may have running on the handheld at once; more are refused, not queued.
const MAX_CALLS: usize = 8;
/// The longest a command may be given.
const MAX_CALL_MS: i64 = 600_000;
/// How often the handheld asks whether the plugin is still there, and how long silence may last.
const PING_EVERY: Duration = Duration::from_secs(20);
const SILENCE: Duration = Duration::from_secs(60);
/// How long one message may take to be written.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether `bytes` begin as an MP3 file does: an ID3 tag, or an MPEG audio frame.
pub fn is_mp3(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ID3") || matches!(bytes, [0xFF, b, ..] if b & 0xE0 == 0xE0)
}

/// Why the link failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The pasted link is not a pairing link.
    #[error("not a pairing link: {0}")]
    BadLink(&'static str),
    /// None of the link's addresses answered.
    #[error("could not reach Hermes: {0}")]
    Unreachable(String),
    /// Something answered that is not the Hermes this handheld was paired with.
    #[error("this is not the Hermes this handheld was paired with")]
    WrongCertificate,
    /// Hermes no longer knows this handheld: it has to be paired again.
    #[error("this handheld is no longer paired with Hermes")]
    Unpaired,
    /// The plugin refused, in its own words (a spent code, a busy chat).
    #[error("{0}")]
    Refused(String),
    /// The plugin did not answer in time.
    #[error("Hermes did not answer in time")]
    Timeout,
    /// The connection is gone.
    #[error("not connected")]
    Closed,
    /// The plugin sent something this crate cannot read.
    #[error("unexpected answer from Hermes: {0}")]
    Protocol(String),
}

/// What the owner pastes to pair: `goblinlink://HOST[,HOST…]:PORT/?fp=HEX&code=CODE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    /// Addresses the plugin can be reached at, in the order to try them.
    pub hosts: Vec<String>,
    pub port: u16,
    /// SHA-256 of the plugin's certificate, lower-case hex.
    pub fingerprint: String,
    /// The one-time code.
    pub code: String,
}

impl PairLink {
    /// Reads a pasted link. Everything in it is checked: it came through a web page.
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        let rest = text
            .trim()
            .strip_prefix("goblinlink://")
            .ok_or(LinkError::BadLink("it does not start with goblinlink://"))?;
        let (authority, query) = rest
            .split_once('?')
            .ok_or(LinkError::BadLink("it has no fingerprint or code"))?;
        let authority = authority.trim_end_matches('/');
        let (hosts, port) = authority
            .rsplit_once(':')
            .ok_or(LinkError::BadLink("it has no port"))?;
        let port: u16 = port
            .parse()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(LinkError::BadLink("its port is not a number"))?;
        let hosts: Vec<String> = hosts.split(',').map(str::to_owned).collect();
        let host_ok = |h: &String| {
            !h.is_empty()
                && h.len() <= 253
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        };
        if hosts.is_empty() || hosts.len() > 8 || !hosts.iter().all(host_ok) {
            return Err(LinkError::BadLink(
                "its address is not a host name or IPv4 address",
            ));
        }
        let (mut fingerprint, mut code) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("fp", v)) => fingerprint = Some(v.to_ascii_lowercase()),
                Some(("code", v)) => code = Some(v.to_owned()),
                _ => {}
            }
        }
        let fingerprint = fingerprint
            .filter(|f| f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(LinkError::BadLink("its fingerprint is not 64 hex digits"))?;
        let code = code
            // Long enough that it cannot be guessed while it lasts.
            .filter(|c| {
                (20..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or(LinkError::BadLink("its code is missing"))?;
        Ok(Self {
            hosts,
            port,
            fingerprint,
            code,
        })
    }
}

/// What a paired handheld keeps: where Hermes is, which certificate is its, and the token
/// that says who this handheld is. The token is a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Pairing {
    pub hosts: Vec<String>,
    pub port: u16,
    pub fingerprint: String,
    pub device_id: String,
    pub token: String,
}

impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pairing")
            .field("hosts", &self.hosts)
            .field("port", &self.port)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// This handheld, as it introduces itself.
#[derive(Debug, Clone)]
pub struct Device {
    /// Stable id.
    pub id: String,
    /// The name shown on the Hermes side.
    pub name: String,
    /// The daemon's version.
    pub version: String,
    /// The handheld's commands, as the daemon describes them: Hermes is told what it may
    /// call. Empty while pairing.
    pub commands: Value,
}

/// Runs a command Hermes calls on the handheld. The answer looks like
/// `{"ok": true, "payload": {...}}` or `{"ok": false, "error": "..."}`; every permission
/// check is the runner's.
pub trait Runner: Send + Sync + 'static {
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>>;
}

/// What Hermes sends without being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    /// A question to the owner (an approval, a clarification), answered with
    /// [`Session::answer`].
    Prompt {
        id: String,
        chat: String,
        /// `approval` or `clarify`.
        kind: String,
        text: String,
        /// `text` is not all of what Hermes sent: it was longer than the handheld keeps, or
        /// had characters in it that the handheld does not show.
        cut: bool,
        /// `(id, label)` of each answer on offer.
        choices: Vec<(String, String)>,
    },
    /// Hermes no longer takes an answer to a [`Pushed::Prompt`]: it was answered somewhere
    /// else, or Hermes gave up waiting.
    PromptClosed { id: String },
    /// The agent changed its name or its picture: [`Session::agent`] and
    /// [`Session::avatar_revision`] say what they are now.
    Identity,
    /// A message nobody asked for just now (a scheduled job's result).
    Notice {
        chat: String,
        message_id: String,
        text: String,
    },
}

/// What one event means for a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// What Hermes is doing, in its words; `working` is false once it rests.
    Status { text: String, working: bool },
    /// What Hermes heard in the voice note this turn began with.
    Heard(String),
    /// A reply as it stands now: the whole text so far, not an addition.
    Message {
        message_id: String,
        text: String,
        done: bool,
    },
    /// The turn is over.
    Done,
    /// The turn failed, and why.
    Failed(String),
}

/// What a turn sends.
#[derive(Debug, Clone, Copy)]
pub enum Payload<'a> {
    Text(&'a str),
    /// A voice note, as a WAV file; Hermes transcribes it.
    Voice(&'a [u8]),
}

/// One message sent, and what Hermes says about it from then on.
pub struct Turn {
    /// Hermes's id for the message.
    pub message_id: String,
    id: String,
    events: mpsc::Receiver<TurnEvent>,
    kept: Arc<Kept>,
    shared: Arc<Shared>,
}

/// What must not be lost when a turn's backlog is full: the reply as it last stood, and how
/// the turn ended.
#[derive(Default)]
struct Kept {
    latest: Mutex<Option<TurnEvent>>,
    end: Mutex<Option<TurnEvent>>,
}

impl Turn {
    /// Waits up to `wait` for the next event. `Ok(None)` when none came. After
    /// [`TurnEvent::Done`] or [`TurnEvent::Failed`], and when the connection is gone, `Err`.
    pub async fn next(&mut self, wait: Duration) -> Result<Option<TurnEvent>, LinkError> {
        let take = |slot: &Mutex<Option<TurnEvent>>| slot.lock().unwrap().take();
        match tokio::time::timeout(wait, self.events.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            // Nothing more will be queued: hand over what was kept aside, the end last.
            Ok(None) => match take(&self.kept.latest).or_else(|| take(&self.kept.end)) {
                Some(event) => Ok(Some(event)),
                None => Err(LinkError::Closed),
            },
            Err(_) => Ok(take(&self.kept.latest)),
        }
    }

    /// Asks Hermes to stop working on it.
    pub fn cancel(&self) {
        self.shared
            .send(json!({"type": "turn.cancel", "id": self.id}));
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.shared.turns.lock().unwrap().remove(&self.id);
        self.shared.accepts.lock().unwrap().remove(&self.id);
    }
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It was stopped from this side.
    Stopped,
    /// The connection closed, broke or went silent.
    Closed,
}

/// Where a turn's events go, what is kept aside for it, and the chat it is in.
type TurnSlot = (mpsc::Sender<TurnEvent>, Arc<Kept>, String);
/// Where a reply's speech, or why there is none, is sent.
type Spoken = oneshot::Sender<Result<Vec<u8>, String>>;

struct Shared {
    out: mpsc::Sender<Message>,
    /// Each turn being followed, by its id.
    turns: Mutex<HashMap<String, TurnSlot>>,
    /// Who waits for `turn.accepted` or `turn.rejected`, by turn id.
    accepts: Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>,
    /// Who waits for a reply's speech, by request id.
    speeches: Mutex<HashMap<String, Spoken>>,
    /// Who waits for the agent's picture, by request id.
    avatars: Mutex<HashMap<String, Spoken>>,
    /// Who the agent says it is, as last heard.
    identity: Mutex<Identity>,
    closed: watch::Sender<Option<Outcome>>,
}

/// The agent's name and the revision of its picture, each if it gave one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Identity {
    agent: Option<String>,
    avatar: Option<String>,
}

impl Identity {
    /// Reads a `hello.ok` or an `identity` message. A revision names a folder on the
    /// handheld, so only one that is safe as a file name is taken.
    fn read(frame: &Value) -> Self {
        let revision = frame
            .get("avatar")
            .and_then(|a| a.get("revision"))
            .and_then(Value::as_str)
            .filter(|r| {
                (1..=64).contains(&r.len())
                    && r.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            });
        Self {
            // One line: a name is shown where there is room for one.
            agent: Some(text(frame, "agent", 64).replace('\n', " "))
                .filter(|a| !a.trim().is_empty()),
            avatar: revision.map(str::to_owned),
        }
    }
}

impl Shared {
    /// Queues a message. A plugin that has stopped reading gets no buffer: the session ends.
    fn send(&self, frame: Value) {
        if self.out.try_send(Message::Text(frame.to_string())).is_err() {
            self.close(Outcome::Closed);
        }
    }

    /// Ends the session, once; the task that owns the connection sees it and lets go.
    fn close(&self, how: Outcome) {
        self.closed.send_if_modified(|state| {
            let first = state.is_none();
            if first {
                *state = Some(how);
            }
            first
        });
    }
}

/// Stops the session when the last [`Session`] handle is dropped.
struct Handle(Arc<Shared>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.close(Outcome::Stopped);
    }
}

/// A live, authenticated connection to the plugin. Dropping every clone closes it.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
    _handle: Arc<Handle>,
    next_turn: Arc<std::sync::atomic::AtomicU64>,
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the WebSocket to the first of `hosts` that answers, trusting only the certificate
/// with `fingerprint`. `plain` is for tests alone: no TLS.
async fn open(
    hosts: &[String],
    port: u16,
    fingerprint: &str,
    plain: bool,
) -> Result<Ws, LinkError> {
    let mut last = String::from("no address");
    // Another machine may sit at one of the addresses (a LAN address, away from home): keep
    // trying the rest, but if nothing answers as Hermes, the owner should hear of it.
    let mut stranger = false;
    for host in hosts {
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scheme, connector) = if plain {
            ("ws", tokio_tungstenite::Connector::Plain)
        } else {
            let config = pin::client_config(fingerprint, refused.clone())
                .map_err(|_| LinkError::BadLink("its fingerprint is not 64 hex digits"))?;
            ("wss", tokio_tungstenite::Connector::Rustls(config))
        };
        let url = format!("{scheme}://{host}:{port}/goblin-link");
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(MAX_FRAME_BYTES),
            max_frame_size: Some(MAX_FRAME_BYTES),
            ..Default::default()
        };
        let connecting = tokio_tungstenite::connect_async_tls_with_config(
            url,
            Some(config),
            false,
            Some(connector),
        );
        match tokio::time::timeout(Duration::from_secs(8), connecting).await {
            Ok(Ok((ws, _))) => return Ok(ws),
            Ok(Err(e)) => last = format!("{host}: {e}"),
            Err(_) => last = format!("{host}: no answer"),
        }
        stranger |= refused.load(std::sync::atomic::Ordering::Relaxed);
    }
    Err(if stranger {
        LinkError::WrongCertificate
    } else {
        LinkError::Unreachable(last)
    })
}

/// Sends one object and reads the plugin's answer to it.
async fn ask(ws: &mut Ws, frame: Value) -> Result<Value, LinkError> {
    ws.send(Message::Text(frame.to_string()))
        .await
        .map_err(|_| LinkError::Closed)?;
    let answer = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str::<Value>(&text)
                        .map_err(|e| LinkError::Protocol(e.to_string()))
                }
                Some(Ok(Message::Close(_))) | None => return Err(LinkError::Closed),
                Some(Ok(_)) => {}
                Some(Err(_)) => return Err(LinkError::Closed),
            }
        }
    };
    let answer = tokio::time::timeout(ANSWER_TIMEOUT, answer)
        .await
        .map_err(|_| LinkError::Timeout)??;
    if kind(&answer) == "error" {
        // `code` is for the handheld to act on; `reason` is for the owner to read.
        return Err(match answer.get("code").and_then(Value::as_str) {
            Some("unpaired") => LinkError::Unpaired,
            _ => LinkError::Refused(text(&answer, "reason", 200)),
        });
    }
    Ok(answer)
}

fn kind(frame: &Value) -> &str {
    frame.get("type").and_then(Value::as_str).unwrap_or("")
}

/// A string field as it may be shown: cut to `max` characters, without control characters
/// or the invisible ones that reorder or hide text (a prompt must read as it was written).
fn text(frame: &Value, key: &str, max: usize) -> String {
    let hidden = |c: char| matches!(c, '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}');
    frame
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| (!c.is_control() || *c == '\n') && !hidden(*c))
        .take(max)
        .collect()
}

/// [`text`], and whether anything was lost on the way: cut off at `max`, taken out, or
/// never there as text. A reader of the result has then not read what was sent.
fn text_and_lost(frame: &Value, key: &str, max: usize) -> (String, bool) {
    let shown = text(frame, key, max);
    let lost = frame.get(key).and_then(Value::as_str) != Some(shown.as_str());
    (shown, lost)
}

/// An id field: short, and made only of what is safe in a file name or a log line.
fn id(frame: &Value, key: &str) -> Option<String> {
    frame
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && !s.starts_with('.')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
        .map(str::to_owned)
}

/// Pairs this handheld with the Hermes the link names, spending its code.
pub async fn pair(link: &PairLink, device: &Device) -> Result<Pairing, LinkError> {
    pair_over(link, device, false).await
}

async fn pair_over(link: &PairLink, device: &Device, plain: bool) -> Result<Pairing, LinkError> {
    let mut ws = open(&link.hosts, link.port, &link.fingerprint, plain).await?;
    let answer = ask(
        &mut ws,
        json!({"type": "pair", "protocol": PROTOCOL, "code": link.code,
               "device": {"id": device.id, "name": device.name, "version": device.version}}),
    )
    .await?;
    let _ = ws.close(None).await;
    let token = id(&answer, "token")
        .filter(|t| kind(&answer) == "paired" && t.len() >= 32)
        .ok_or_else(|| LinkError::Protocol("no device token".into()))?;
    Ok(Pairing {
        hosts: link.hosts.clone(),
        port: link.port,
        fingerprint: link.fingerprint.clone(),
        device_id: device.id.clone(),
        token,
    })
}

impl Session {
    /// Connects with a pairing and says hello. `runner` answers Hermes's command calls;
    /// `pushed` receives what Hermes sends unasked.
    pub async fn connect(
        pairing: &Pairing,
        device: &Device,
        runner: Arc<dyn Runner>,
        pushed: mpsc::Sender<Pushed>,
    ) -> Result<Self, LinkError> {
        Self::connect_over(pairing, device, runner, pushed, false).await
    }

    async fn connect_over(
        pairing: &Pairing,
        device: &Device,
        runner: Arc<dyn Runner>,
        pushed: mpsc::Sender<Pushed>,
        plain: bool,
    ) -> Result<Self, LinkError> {
        let mut ws = open(&pairing.hosts, pairing.port, &pairing.fingerprint, plain).await?;
        let hello = ask(
            &mut ws,
            json!({"type": "hello", "protocol": PROTOCOL, "device": pairing.device_id,
                   "token": pairing.token, "name": device.name, "version": device.version,
                   "commands": device.commands}),
        )
        .await?;
        if kind(&hello) != "hello.ok" {
            return Err(LinkError::Protocol(format!("{} for hello", kind(&hello))));
        }
        let (out, outgoing) = mpsc::channel::<Message>(OUT_BACKLOG);
        let shared = Arc::new(Shared {
            out,
            turns: Mutex::new(HashMap::new()),
            accepts: Mutex::new(HashMap::new()),
            speeches: Mutex::new(HashMap::new()),
            avatars: Mutex::new(HashMap::new()),
            identity: Mutex::new(Identity::read(&hello)),
            closed: watch::channel(None).0,
        });
        tokio::spawn(serve(ws, outgoing, shared.clone(), runner, pushed));
        Ok(Self {
            _handle: Arc::new(Handle(shared.clone())),
            shared,
            next_turn: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        })
    }

    /// The agent's name, if it gave one.
    pub fn agent(&self) -> Option<String> {
        self.shared.identity.lock().unwrap().agent.clone()
    }

    /// What names the agent's picture now, if it has one: it changes when the picture does.
    pub fn avatar_revision(&self) -> Option<String> {
        self.shared.identity.lock().unwrap().avatar.clone()
    }

    /// Fetches the agent's picture of that revision, as PNG. Refused when the agent has
    /// changed it since.
    pub async fn avatar(&self, revision: &str) -> Result<Vec<u8>, LinkError> {
        use std::sync::atomic::Ordering;
        if self.shared.closed.borrow().is_some() {
            return Err(LinkError::Closed);
        }
        let id = format!("a{}", self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.shared.avatars.lock().unwrap().insert(id.clone(), tx);
        self.shared
            .send(json!({"type": "avatar.get", "id": id, "revision": revision}));
        let answer = tokio::time::timeout(ANSWER_TIMEOUT, rx).await;
        self.shared.avatars.lock().unwrap().remove(&id);
        match answer {
            Ok(Ok(Ok(png))) => Ok(png),
            Ok(Ok(Err(why))) => Err(LinkError::Refused(why)),
            Ok(Err(_)) => Err(LinkError::Closed),
            Err(_) => Err(LinkError::Timeout),
        }
    }

    /// Sends a message to `chat` and waits for Hermes to accept it.
    pub async fn send(&self, chat: &str, payload: Payload<'_>) -> Result<Turn, LinkError> {
        use std::sync::atomic::Ordering;
        if self.shared.closed.borrow().is_some() {
            return Err(LinkError::Closed);
        }
        let turn_id = format!("t{}", self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (events_tx, events) = mpsc::channel(TURN_BACKLOG);
        let (accept_tx, accepted) = oneshot::channel();
        let kept = Arc::new(Kept::default());
        self.shared
            .turns
            .lock()
            .unwrap()
            .insert(turn_id.clone(), (events_tx, kept.clone(), chat.to_owned()));
        self.shared
            .accepts
            .lock()
            .unwrap()
            .insert(turn_id.clone(), accept_tx);
        // Dropped on every way out but the first, which forgets the turn again.
        let mut turn = Turn {
            message_id: String::new(),
            id: turn_id.clone(),
            events,
            kept,
            shared: self.shared.clone(),
        };
        let mut frame = json!({"type": "turn", "id": turn_id, "chat": chat});
        match payload {
            Payload::Text(t) => frame["text"] = json!(t),
            Payload::Voice(wav) => {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(wav);
                frame["voice_wav_b64"] = json!(encoded);
            }
        }
        self.shared.send(frame);
        match tokio::time::timeout(ANSWER_TIMEOUT, accepted).await {
            Ok(Ok(Ok(message_id))) => {
                turn.message_id = message_id;
                Ok(turn)
            }
            Ok(Ok(Err(reason))) => Err(LinkError::Refused(reason)),
            Ok(Err(_)) => Err(LinkError::Closed),
            Err(_) => Err(LinkError::Timeout),
        }
    }

    /// Has Hermes say `text` aloud with its own configured speech, and returns the audio as
    /// MP3.
    pub async fn speech(&self, text: &str) -> Result<Vec<u8>, LinkError> {
        use std::sync::atomic::Ordering;
        if self.shared.closed.borrow().is_some() {
            return Err(LinkError::Closed);
        }
        let id = format!("s{}", self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.shared.speeches.lock().unwrap().insert(id.clone(), tx);
        self.shared
            .send(json!({"type": "speech.get", "id": id, "text": text}));
        let answer = tokio::time::timeout(SPEECH_TIMEOUT, rx).await;
        self.shared.speeches.lock().unwrap().remove(&id);
        match answer {
            // Only MP3 goes on to the handheld's decoder, whatever Hermes sends.
            Ok(Ok(Ok(mp3))) if is_mp3(&mp3) => Ok(mp3),
            Ok(Ok(Ok(_))) => Err(LinkError::Refused(
                "Hermes sent something that is not MP3".into(),
            )),
            Ok(Ok(Err(why))) => Err(LinkError::Refused(why)),
            Ok(Err(_)) => Err(LinkError::Closed),
            Err(_) => Err(LinkError::Timeout),
        }
    }

    /// Whether a turn is under way in `chat`: sent from here, and neither ended by Hermes
    /// nor let go of (its [`Turn`] dropped). A question belongs to such a turn.
    pub fn turn_in(&self, chat: &str) -> bool {
        let turns = self.shared.turns.lock().unwrap();
        turns.values().any(|(_, _, of)| of == chat)
    }

    /// Gives the owner's answer to a [`Pushed::Prompt`].
    pub fn answer(&self, prompt_id: &str, answer: &str) {
        self.shared
            .send(json!({"type": "prompt.answer", "id": prompt_id, "answer": answer}));
    }

    /// Closes the connection.
    pub fn stop(&self) {
        self.shared.close(Outcome::Stopped);
    }

    /// Waits until the session is over and says how.
    pub async fn wait_closed(&self) -> Outcome {
        let mut closed = self.shared.closed.subscribe();
        loop {
            if let Some(outcome) = *closed.borrow_and_update() {
                return outcome;
            }
            if closed.changed().await.is_err() {
                return Outcome::Closed;
            }
        }
    }
}

/// Owns the connection until the session is over: writes what is queued, reads and acts on
/// what arrives, asks now and then whether the plugin is still there, and on the way out
/// stops every command still running and leaves nobody waiting.
async fn serve(
    ws: Ws,
    mut outgoing: mpsc::Receiver<Message>,
    shared: Arc<Shared>,
    runner: Arc<dyn Runner>,
    pushed: mpsc::Sender<Pushed>,
) {
    let (mut sink, mut stream) = ws.split();
    let mut closed = shared.closed.subscribe();
    let mut calls = tokio::task::JoinSet::new();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard = tokio::time::Instant::now();
    loop {
        if closed.borrow_and_update().is_some() {
            break;
        }
        tokio::select! {
            _ = closed.changed() => {}
            Some(message) = outgoing.recv() => {
                // A plugin that is there but has stopped reading must not hold this task:
                // stopping, and the silence check, still have to be seen.
                let written = tokio::select! {
                    w = tokio::time::timeout(WRITE_TIMEOUT, sink.send(message)) => {
                        matches!(w, Ok(Ok(())))
                    }
                    _ = closed.changed() => false,
                };
                if !written {
                    break;
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() >= SILENCE {
                    log::info!("hermes: nothing heard for {}s", SILENCE.as_secs());
                    break;
                }
                shared.send(json!({"type": "ping"}));
            }
            Some(_) = calls.join_next(), if !calls.is_empty() => {}
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heard = tokio::time::Instant::now();
                    match serde_json::from_str::<Value>(&text) {
                        Ok(frame) => dispatch(&shared, &runner, &pushed, &mut calls, frame),
                        Err(_) => log::info!("hermes: a message that is not JSON was dropped"),
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => heard = tokio::time::Instant::now(),
            },
        }
    }
    shared.close(Outcome::Closed);
    calls.shutdown().await;
    shared.turns.lock().unwrap().clear();
    shared.accepts.lock().unwrap().clear();
    shared.speeches.lock().unwrap().clear();
    shared.avatars.lock().unwrap().clear();
    let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
}

/// Acts on one message from the plugin. Everything in it is checked before it is used.
fn dispatch(
    shared: &Arc<Shared>,
    runner: &Arc<dyn Runner>,
    pushed: &mpsc::Sender<Pushed>,
    calls: &mut tokio::task::JoinSet<()>,
    frame: Value,
) {
    let to_turn = |event: TurnEvent| {
        let Some(turn) = id(&frame, "turn") else {
            return;
        };
        let last = matches!(event, TurnEvent::Done | TurnEvent::Failed(_));
        let mut turns = shared.turns.lock().unwrap();
        if let Some((tx, kept, _)) = turns.get(&turn) {
            // A turn nobody reads for a long while loses events, but never the reply as it
            // last stood nor how the turn ended; and what is read stays in order: the
            // backlog, then the reply kept aside, then the end.
            let full = |sent: Result<(), mpsc::error::TrySendError<TurnEvent>>| match sent {
                Err(mpsc::error::TrySendError::Full(event)) => Some(event),
                _ => None,
            };
            let mut latest = kept.latest.lock().unwrap();
            match event {
                // A newer snapshot replaces the one kept aside. One is kept for the whole
                // turn: with the backlog full, an earlier message's last text can give way
                // to a later message's.
                TurnEvent::Message { .. } => *latest = full(tx.try_send(event)),
                // Not worth queueing behind a reply that is still waiting for room.
                TurnEvent::Status { .. } | TurnEvent::Heard(_) => {
                    if latest.is_none() {
                        let _ = tx.try_send(event);
                    }
                }
                TurnEvent::Done | TurnEvent::Failed(_) => {
                    if let Some(waiting) = latest.take() {
                        *latest = full(tx.try_send(waiting));
                    }
                    let unsent = if latest.is_some() {
                        Some(event)
                    } else {
                        full(tx.try_send(event))
                    };
                    *kept.end.lock().unwrap() = unsent;
                }
            }
        }
        if last {
            turns.remove(&turn);
        }
    };
    match kind(&frame) {
        "turn.accepted" | "turn.rejected" => {
            let Some(turn) = id(&frame, "id") else {
                return;
            };
            let Some(waiting) = shared.accepts.lock().unwrap().remove(&turn) else {
                return;
            };
            let answer = if kind(&frame) == "turn.accepted" {
                Ok(id(&frame, "message_id").unwrap_or_default())
            } else {
                shared.turns.lock().unwrap().remove(&turn);
                Err(text(&frame, "reason", 200))
            };
            let _ = waiting.send(answer);
        }
        "status" if frame.get("heard").and_then(Value::as_bool) == Some(true) => {
            to_turn(TurnEvent::Heard(text(&frame, "text", 8_192)))
        }
        "status" => to_turn(TurnEvent::Status {
            text: text(&frame, "text", 200),
            working: frame.get("working").and_then(Value::as_bool) != Some(false),
        }),
        "speech" | "speech.failed" => {
            let Some(waiting) =
                id(&frame, "id").and_then(|i| shared.speeches.lock().unwrap().remove(&i))
            else {
                return;
            };
            let audio = (kind(&frame) == "speech")
                .then(|| frame.get("mp3_b64").and_then(Value::as_str))
                .flatten()
                .and_then(|b64| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.decode(b64).ok()
                })
                .filter(|mp3| !mp3.is_empty() && mp3.len() <= MAX_SPEECH_BYTES);
            let _ = waiting.send(audio.ok_or_else(|| {
                Some(text(&frame, "reason", 200))
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "no speech".into())
            }));
        }
        "identity" => {
            let now = Identity::read(&frame);
            let changed = {
                let mut identity = shared.identity.lock().unwrap();
                let changed = *identity != now;
                *identity = now;
                changed
            };
            // A full queue drops the news, not the identity: whoever reads it next, at the
            // next change or connection at the latest, gets the newest.
            if changed {
                let _ = pushed.try_send(Pushed::Identity);
            }
        }
        "avatar" | "avatar.failed" => {
            let Some(waiting) =
                id(&frame, "id").and_then(|i| shared.avatars.lock().unwrap().remove(&i))
            else {
                return;
            };
            let png = (kind(&frame) == "avatar")
                .then(|| frame.get("png_b64").and_then(Value::as_str))
                .flatten()
                // Checked before it is decoded: four characters carry three bytes.
                .filter(|b64| b64.len() <= MAX_AVATAR_BYTES / 3 * 4 + 4)
                .and_then(|b64| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.decode(b64).ok()
                })
                .filter(|png| {
                    png.len() <= MAX_AVATAR_BYTES && png.starts_with(b"\x89PNG\r\n\x1a\n")
                });
            let _ = waiting.send(png.ok_or_else(|| {
                Some(text(&frame, "reason", 200))
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "no picture".into())
            }));
        }
        "message" => {
            if let Some(message_id) = id(&frame, "message_id") {
                to_turn(TurnEvent::Message {
                    message_id,
                    text: text(&frame, "text", 48_000),
                    done: frame.get("done").and_then(Value::as_bool) == Some(true),
                });
            }
        }
        "turn.done" => to_turn(TurnEvent::Done),
        "turn.failed" => to_turn(TurnEvent::Failed(text(&frame, "reason", 200))),
        "tool.call" => {
            let (Some(call), Some(command)) = (id(&frame, "id"), id(&frame, "command")) else {
                return;
            };
            if calls.len() >= MAX_CALLS {
                shared.send(json!({"type": "tool.result", "id": call, "result":
                    {"ok": false, "error": "the handheld is busy with other commands"}}));
                return;
            }
            let params = frame
                .get("params")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let timeout_ms = frame
                .get("timeout_ms")
                .and_then(Value::as_i64)
                .map(|t| t.clamp(1, MAX_CALL_MS));
            let (shared, runner) = (shared.clone(), runner.clone());
            calls.spawn(async move {
                let result = runner.run(command, params, timeout_ms).await;
                shared.send(json!({"type": "tool.result", "id": call, "result": result}));
            });
        }
        "prompt" => {
            let (Some(prompt), Some(chat)) = (id(&frame, "id"), id(&frame, "chat")) else {
                return;
            };
            let choices = frame
                .get("choices")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|c| Some((id(c, "id")?, text(c, "label", 40))))
                        .take(6)
                        .collect()
                })
                .unwrap_or_default();
            let (text, cut) = text_and_lost(&frame, "text", MAX_PROMPT_CHARS);
            // A full queue of unanswered questions drops the newest, not the connection.
            let _ = pushed.try_send(Pushed::Prompt {
                id: prompt,
                chat,
                kind: self::text(&frame, "kind", 16),
                text,
                cut,
                choices,
            });
        }
        "prompt.closed" => {
            if let Some(prompt) = id(&frame, "id") {
                let _ = pushed.try_send(Pushed::PromptClosed { id: prompt });
            }
        }
        "notice" => {
            if let (Some(chat), Some(message_id)) = (id(&frame, "chat"), id(&frame, "message_id")) {
                let _ = pushed.try_send(Pushed::Notice {
                    chat,
                    message_id,
                    text: text(&frame, "text", 48_000),
                });
            }
        }
        "ping" => shared.send(json!({"type": "pong"})),
        other => log::debug!("hermes: ignoring a message of type {other:?}"),
    }
}
