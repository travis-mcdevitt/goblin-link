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
/// The longest message accepted from the plugin. A voice reply travels in one.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Turn events kept for a turn nobody is reading yet.
const TURN_BACKLOG: usize = 256;

/// Why the link failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The pasted link is not a pairing link.
    #[error("not a pairing link: {0}")]
    BadLink(&'static str),
    /// None of the link's addresses answered.
    #[error("could not reach Hermes: {0}")]
    Unreachable(String),
    /// The plugin refused, in its own words (a spent code, an unknown device).
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
            .filter(|c| (4..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric()))
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
        /// `(id, label)` of each answer on offer.
        choices: Vec<(String, String)>,
    },
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
    shared: Arc<Shared>,
}

impl Turn {
    /// Waits up to `wait` for the next event. `Ok(None)` when none came; `Err` when the
    /// connection is gone and no more will.
    pub async fn next(&mut self, wait: Duration) -> Result<Option<TurnEvent>, LinkError> {
        match tokio::time::timeout(wait, self.events.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => Err(LinkError::Closed),
            Err(_) => Ok(None),
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
    }
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It was stopped from this side.
    Stopped,
    /// The connection closed or broke.
    Closed,
}

struct Shared {
    out: mpsc::UnboundedSender<Message>,
    /// The channel of each turn being followed, by its id.
    turns: Mutex<HashMap<String, mpsc::Sender<TurnEvent>>>,
    /// Who waits for `turn.accepted` or `turn.rejected`, by turn id.
    accepts: Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>,
    closed: watch::Sender<Option<Outcome>>,
}

impl Shared {
    fn send(&self, frame: Value) {
        let _ = self.out.send(Message::Text(frame.to_string()));
    }
}

/// A live, authenticated connection to the plugin.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
    agent: Option<String>,
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
    for host in hosts {
        let (scheme, connector) = if plain {
            ("ws", tokio_tungstenite::Connector::Plain)
        } else {
            let config = pin::client_config(fingerprint)
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
    }
    Err(LinkError::Unreachable(last))
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
        return Err(LinkError::Refused(text(&answer, "reason", 200)));
    }
    Ok(answer)
}

fn kind(frame: &Value) -> &str {
    frame.get("type").and_then(Value::as_str).unwrap_or("")
}

/// A string field, cut to `max` characters with control characters dropped: it is shown.
fn text(frame: &Value, key: &str, max: usize) -> String {
    frame
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max)
        .collect()
}

/// An id field: short and plain, or nothing.
fn id(frame: &Value, key: &str) -> Option<String> {
    frame
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
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
        .filter(|t| kind(&answer) == "paired" && t.len() >= 16)
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
        let agent = Some(text(&hello, "agent", 64)).filter(|a| !a.trim().is_empty());
        let (out, mut outgoing) = mpsc::unbounded_channel::<Message>();
        let shared = Arc::new(Shared {
            out,
            turns: Mutex::new(HashMap::new()),
            accepts: Mutex::new(HashMap::new()),
            closed: watch::channel(None).0,
        });
        let (mut sink, mut stream) = ws.split();
        let writer = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = shared.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                match message {
                    Message::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                            log::info!("hermes: a message that is not JSON was dropped");
                            continue;
                        };
                        dispatch(&reader, &runner, &pushed, frame).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            // Nobody waiting on this connection is left waiting.
            reader.turns.lock().unwrap().clear();
            reader.accepts.lock().unwrap().clear();
            reader.closed.send_if_modified(|state| {
                state
                    .is_none()
                    .then(|| *state = Some(Outcome::Closed))
                    .is_some()
            });
            writer.abort();
        });
        Ok(Self {
            shared,
            agent,
            next_turn: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        })
    }

    /// The agent's name, if it gave one.
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
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
        self.shared
            .turns
            .lock()
            .unwrap()
            .insert(turn_id.clone(), events_tx);
        self.shared
            .accepts
            .lock()
            .unwrap()
            .insert(turn_id.clone(), accept_tx);
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
        // Dropped on every way out but the first, which forgets the turn again.
        let mut turn = Turn {
            message_id: String::new(),
            id: turn_id.clone(),
            events,
            shared: self.shared.clone(),
        };
        match tokio::time::timeout(ANSWER_TIMEOUT, accepted).await {
            Ok(Ok(Ok(message_id))) => {
                turn.message_id = message_id;
                Ok(turn)
            }
            Ok(Ok(Err(reason))) => Err(LinkError::Refused(reason)),
            Ok(Err(_)) => Err(LinkError::Closed),
            Err(_) => {
                self.shared.accepts.lock().unwrap().remove(&turn_id);
                Err(LinkError::Timeout)
            }
        }
    }

    /// Gives the owner's answer to a [`Pushed::Prompt`].
    pub fn answer(&self, prompt_id: &str, answer: &str) {
        self.shared
            .send(json!({"type": "prompt.answer", "id": prompt_id, "answer": answer}));
    }

    /// Closes the connection.
    pub fn stop(&self) {
        self.shared.closed.send_if_modified(|state| {
            state
                .is_none()
                .then(|| *state = Some(Outcome::Stopped))
                .is_some()
        });
        let _ = self.shared.out.send(Message::Close(None));
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

/// Acts on one message from the plugin. Everything in it is checked before it is used.
async fn dispatch(
    shared: &Arc<Shared>,
    runner: &Arc<dyn Runner>,
    pushed: &mpsc::Sender<Pushed>,
    frame: Value,
) {
    let to_turn = |event: TurnEvent| {
        let Some(turn) = id(&frame, "turn") else {
            return;
        };
        let last = matches!(event, TurnEvent::Done | TurnEvent::Failed(_));
        let mut turns = shared.turns.lock().unwrap();
        if let Some(tx) = turns.get(&turn) {
            // A turn nobody reads for a long while loses events rather than memory.
            let _ = tx.try_send(event);
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
        "status" => to_turn(TurnEvent::Status {
            text: text(&frame, "text", 200),
            working: frame.get("working").and_then(Value::as_bool) != Some(false),
        }),
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
            let params = frame
                .get("params")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let timeout_ms = frame.get("timeout_ms").and_then(Value::as_i64);
            let (shared, runner) = (shared.clone(), runner.clone());
            tokio::spawn(async move {
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
            let _ = pushed
                .send(Pushed::Prompt {
                    id: prompt,
                    chat,
                    kind: text(&frame, "kind", 16),
                    text: text(&frame, "text", 2_000),
                    choices,
                })
                .await;
        }
        "notice" => {
            if let (Some(chat), Some(message_id)) = (id(&frame, "chat"), id(&frame, "message_id")) {
                let _ = pushed
                    .send(Pushed::Notice {
                        chat,
                        message_id,
                        text: text(&frame, "text", 48_000),
                    })
                    .await;
            }
        }
        "ping" => shared.send(json!({"type": "pong"})),
        other => log::debug!("hermes: ignoring a message of type {other:?}"),
    }
}
