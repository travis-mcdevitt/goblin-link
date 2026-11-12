//! One control session with a Muse VM.
//!
//! Port of `link_client.py` on top of [`muse_proto`]. Opens a WebSocket to
//! `/v1/noise` with the per-VM bearer, runs the Noise XX handshake via
//! [`muse_proto::noise`], then opens a long-lived `POST /link-control` stream.
//! The read loop dispatches `link.invoke` to a [`CommandRunner`] and replies
//! with `link.result`, handles `link.unpaired`, and demultiplexes HTTP-like
//! response streams. Device-originated requests (`send_chat`, `request`,
//! `subscribe_chat`, `stream_tts`) ride the same session.
//!
//! All bounds and timeouts mirror the reference.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand_core::{OsRng, RngCore};
use serde_json::{json, Map, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch, Notify, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use muse_proto::control::{encode_message, MessageDecoder};
use muse_proto::framing::random_chunk_id;
use muse_proto::noise::{NoiseXXInitiator, OsKeySource};
use muse_proto::transport::{DecryptedFrame, NoiseTransport};
use muse_proto::{ApplicationResponse, Header, ResetCode, ServiceType};

use crate::error::{LinkError, LinkResult};

const NOISE_PATH: &str = "/v1/noise";
const CONTROL_PATH: &str = "/link-control";
const CHAT_PATH: &str = "/chat/stream";
const SUBSCRIBE_PATH: &str = "/chat/subscribe";
const APP_ID: &str = "musegadget";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// How long a ping may go unanswered. The reference sets the websockets
/// library's `ping_timeout` to the ping interval, so a link with no inbound
/// traffic for interval + timeout is dead even if TCP has not noticed yet.
const PONG_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CONTROL_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_CHAT_EVENT_BYTES: usize = 256 * 1024;
const MAX_STREAM_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_TTS_BUFFER_BYTES: usize = 8 * 1024 * 1024;
/// Receive buffer of a duplex stream (a call's reply audio is read as it arrives).
const MAX_DUPLEX_BUFFER_BYTES: usize = 8 * 1024 * 1024;
/// Largest body chunk sent on a duplex stream, as in the reference.
pub const MAX_SEND_BYTES: usize = 64 * 1024;
const MAX_VOICE_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_INLINE_REQUEST_BYTES: usize = 64 * 1024;
const REQUEST_CHUNK_BYTES: usize = 16 * 1024;
const MAX_WS_MESSAGE: usize = 128 * 1024;
const MAX_CONCURRENT_INVOKES: usize = 4;
const STREAM_QUEUE: usize = 64;
/// Chunks queued on a duplex stream (a call's audio arrives as many small deltas).
const DUPLEX_QUEUE: usize = 1024;

/// Accepted image MIME types for [`LinkSession::send_image`].
pub const IMAGE_MIME_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
/// Completion channel for a buffered request: HTTP status and body.
type ReplyTx = oneshot::Sender<LinkResult<(u16, Vec<u8>)>>;

/// How a session ended, mirroring the reference `Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Connection ended; reconnect normally.
    Closed,
    /// The edge refused the VM bearer; re-fetch VMs.
    AuthRejected,
    /// Authenticated but not allowed right now.
    Forbidden,
    /// The Muse removed this device.
    Unpaired,
    /// Stopped on request.
    Stopped,
}

/// A command handler invoked by `link.invoke`.
///
/// The returned map is merged into the `link.result` message, so it should look
/// like `{"ok": true, "payload": {...}}` or `{"ok": false, "error": "..."}`.
pub trait CommandRunner: Send + Sync + 'static {
    /// Runs one command. `timeout_ms` is the VM's suggested deadline, if any.
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>>;
}

/// Description of this device, sent in `link.register`.
#[derive(Debug, Clone)]
pub struct DeviceDescription {
    /// Stable node id (`homelink-<suffix>`).
    pub node_id: String,
    /// Human-readable device name.
    pub display_name: String,
    /// Gadget version.
    pub version: String,
    /// The `commands_v2` map registered with the Muse.
    pub commands: Value,
}

impl DeviceDescription {
    fn register_params(&self) -> Value {
        json!({
            "node_id": self.node_id,
            "display_name": self.display_name,
            "platform": "linux",
            "version": self.version,
            "device_family": "homehub",
            "model_id": "linux",
            "is_wakeup_supported": false,
            "commands_v2": self.commands,
        })
    }

    fn register_message(&self, request_id: &str) -> Value {
        json!({
            "type": "req",
            "id": request_id,
            "method": "link.register",
            "params": self.register_params(),
        })
    }
}

/// A side chat looked up with [`LinkSession::open_side_chat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideChat {
    /// The chat's session id.
    pub session_id: String,
    /// Whether the Muse already has a chat with this id.
    pub exists: bool,
}

/// Parameters needed to open a session.
#[derive(Clone)]
pub struct SessionConfig {
    /// Noise host, e.g. `hatch.metaaivm.com`.
    pub noise_host: String,
    /// VM id (or name) for the `vm_id` query parameter.
    pub vm_id: String,
    /// Per-VM bearer token (secret).
    pub vm_auth_token: String,
    /// This device's registration description.
    pub device: DeviceDescription,
    /// Optional voice `app_id`.
    pub app_id: Option<String>,
    /// Whether to open the control stream and register (false preserves another
    /// connection's registration, e.g. a voice-only session).
    pub register_device: bool,
    /// Shared invoke concurrency limiter; one is created when `None`.
    pub invoke_slots: Option<Arc<Semaphore>>,
    /// Test-only: connect over `ws://` without TLS, for the simulated VM. Never
    /// set in production — the real VM is always reached over `wss://`.
    pub insecure_plaintext: bool,
}

impl SessionConfig {
    /// A production config (TLS, device registration on) for `device` on
    /// `noise_host` with the leased `vm_id`/`vm_auth_token`.
    pub fn new(
        noise_host: impl Into<String>,
        vm_id: impl Into<String>,
        vm_auth_token: impl Into<String>,
        device: DeviceDescription,
    ) -> Self {
        Self {
            noise_host: noise_host.into(),
            vm_id: vm_id.into(),
            vm_auth_token: vm_auth_token.into(),
            device,
            app_id: None,
            register_device: true,
            invoke_slots: None,
            insecure_plaintext: false,
        }
    }
}

// ---- percent-encoding (encodeURIComponent, as the firmware does) -----------------------------

fn encode_uri_component(s: &str, safe: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric() || safe.as_bytes().contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn validate_app_id(app_id: &str) -> LinkResult<String> {
    if app_id == "hatch-web" {
        return Ok(app_id.to_owned());
    }
    if app_id.is_ascii()
        && !app_id.is_empty()
        && app_id.bytes().all(|b| b.is_ascii_digit())
        && app_id.len() <= 19
    {
        if let Ok(n) = app_id.parse::<i64>() {
            if n > 0 {
                return Ok(n.to_string());
            }
        }
    }
    Err(LinkError::Invalid(
        "app ID must be a positive int64 or hatch-web".into(),
    ))
}

fn noise_url(host: &str, vm_id: &str, app_id: Option<&str>, plaintext: bool) -> LinkResult<String> {
    let scheme = if plaintext { "ws" } else { "wss" };
    let mut url = format!(
        "{scheme}://{host}{NOISE_PATH}?vm_id={}",
        encode_uri_component(vm_id, "-_.!~*'()")
    );
    if let Some(app_id) = app_id {
        url.push_str("&app_id=");
        url.push_str(&encode_uri_component(&validate_app_id(app_id)?, ""));
    }
    Ok(url)
}

/// A random UUID v4 string (for request ids), from the OS CSPRNG.
pub fn random_uuid() -> String {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

// ---- pending request registry ----------------------------------------------------------------

struct BufferedReq {
    got_head: bool,
    status: u16,
    body: Vec<u8>,
    done: Option<ReplyTx>,
}

/// An item delivered to a stream consumer.
enum StreamItem {
    Head { status: u16, headers: Vec<Header> },
    Chunk(Vec<u8>),
    End,
    Err(LinkError),
}

struct StreamReg {
    got_head: bool,
    buffered: usize,
    max_buffer_bytes: usize,
    ended: bool,
    tx: mpsc::Sender<StreamItem>,
}

enum Pending {
    Buffered(BufferedReq),
    Stream(StreamReg),
}

#[derive(Default)]
struct PendingMap {
    entries: HashMap<i64, Pending>,
}

// ---- the IO half under a single lock ----------------------------------------------------------

struct Io {
    transport: NoiseTransport,
    sink: WsSink,
    closed: bool,
}

impl Io {
    async fn send_frames(&mut self, frames: Vec<Vec<u8>>) -> LinkResult<()> {
        for frame in frames {
            self.sink
                .send(Message::Binary(frame))
                .await
                .map_err(|e| LinkError::Connect(e.to_string()))?;
        }
        self.sink
            .flush()
            .await
            .map_err(|e| LinkError::Connect(e.to_string()))?;
        Ok(())
    }
}

struct Shared {
    io: tokio::sync::Mutex<Io>,
    pending: Mutex<PendingMap>,
    control_stream_id: AtomicI64,
    register_id: Mutex<String>,
    device: DeviceDescription,
    runner: Arc<dyn CommandRunner>,
    register_device: bool,
    invoke_slots: Arc<Semaphore>,
    closed: AtomicBool,
    registered: watch::Sender<RegState>,
    outcome: watch::Sender<Option<Outcome>>,
    stop: Notify,
    stopped: AtomicBool,
    /// When the last frame of any kind (including pongs) arrived.
    last_rx: Mutex<std::time::Instant>,
    /// Raised by the ping loop when the peer stops answering.
    dead: Notify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegState {
    Pending,
    Ok,
    Rejected,
}

impl Shared {
    fn fail_all(&self, err: impl Fn() -> LinkError) {
        let mut pending = self.pending.lock().unwrap();
        for (_, p) in pending.entries.drain() {
            match p {
                Pending::Buffered(mut b) => {
                    if let Some(done) = b.done.take() {
                        let _ = done.send(Err(err()));
                    }
                }
                Pending::Stream(s) => {
                    let _ = s.tx.try_send(StreamItem::Err(err()));
                }
            }
        }
    }

    fn mark_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.fail_all(|| LinkError::SessionEnded);
    }
}

/// A live control session. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct LinkSession {
    shared: Arc<Shared>,
}

impl LinkSession {
    /// Opens the WebSocket, runs the handshake, opens the control stream and
    /// (when `register_device`) sends `link.register`, then spawns the read and
    /// ping loops. Returns once the transport is ready; await
    /// [`LinkSession::wait_registered`] for the registration ack.
    pub async fn connect(
        config: SessionConfig,
        runner: Arc<dyn CommandRunner>,
    ) -> LinkResult<LinkSession> {
        let url = noise_url(
            &config.noise_host,
            &config.vm_id,
            config.app_id.as_deref(),
            config.insecure_plaintext,
        )?;
        let ws = connect_ws(&url, &config.vm_auth_token).await?;
        let (mut sink, mut source) = ws.split();
        let transport = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut sink, &mut source))
            .await
            .map_err(|_| LinkError::Connect("handshake timed out".into()))??;

        let invoke_slots = config
            .invoke_slots
            .unwrap_or_else(|| Arc::new(Semaphore::new(MAX_CONCURRENT_INVOKES)));
        let (reg_tx, _) = watch::channel(RegState::Pending);
        let (outcome_tx, _) = watch::channel(None);
        let shared = Arc::new(Shared {
            io: tokio::sync::Mutex::new(Io {
                transport,
                sink,
                closed: false,
            }),
            pending: Mutex::new(PendingMap::default()),
            control_stream_id: AtomicI64::new(0),
            register_id: Mutex::new(String::new()),
            device: config.device,
            runner,
            register_device: config.register_device,
            invoke_slots,
            closed: AtomicBool::new(false),
            registered: reg_tx,
            outcome: outcome_tx,
            stop: Notify::new(),
            stopped: AtomicBool::new(false),
            last_rx: Mutex::new(std::time::Instant::now()),
            dead: Notify::new(),
        });

        if config.register_device {
            open_control_stream(&shared).await?;
        }

        let session = LinkSession {
            shared: shared.clone(),
        };
        tokio::spawn(read_loop(shared, source));
        tokio::spawn(ping_loop(session.shared.clone()));
        Ok(session)
    }

    /// Waits for the registration ACK (not merely an open WebSocket).
    pub async fn wait_registered(&self) -> LinkResult<()> {
        if !self.shared.register_device {
            return Err(LinkError::Invalid(
                "device registration is disabled on this session".into(),
            ));
        }
        let mut rx = self.shared.registered.subscribe();
        loop {
            match *rx.borrow_and_update() {
                RegState::Ok => return Ok(()),
                RegState::Rejected => return Err(LinkError::UpgradeRejected(403)),
                RegState::Pending => {}
            }
            if self.shared.closed.load(Ordering::SeqCst) {
                return Err(LinkError::SessionEnded);
            }
            if rx.changed().await.is_err() {
                return Err(LinkError::SessionEnded);
            }
        }
    }

    /// Resolves when the session has ended, with how it ended.
    pub async fn wait_closed(&self) -> Outcome {
        let mut rx = self.shared.outcome.subscribe();
        loop {
            if let Some(outcome) = *rx.borrow_and_update() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                return Outcome::Closed;
            }
        }
    }

    /// Asks the read loop to stop; the session ends with [`Outcome::Stopped`].
    pub fn request_stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.shared.stop.notify_waiters();
    }

    /// Whether the session has closed.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// Posts a user message to the Muse. Returns the acknowledgement object.
    pub async fn send_chat(
        &self,
        message: &str,
        session_id: Option<&str>,
        output_modality: &str,
        device: Option<&str>,
    ) -> LinkResult<Value> {
        let mut body = Map::new();
        body.insert("message".into(), json!(message));
        body.insert("output_modality".into(), json!(output_modality));
        body.insert("device_id".into(), json!(self.shared.device.node_id));
        if let Some(s) = session_id {
            body.insert("session_id".into(), json!(s));
        }
        if let Some(d) = device {
            body.insert("device".into(), json!(d));
        }
        self.post_chat(Value::Object(body)).await
    }

    /// Posts a WAV voice note, optionally with instructions in `message`.
    pub async fn send_voice(
        &self,
        wav_bytes: &[u8],
        session_id: Option<&str>,
        message: &str,
        output_modality: Option<&str>,
        device: Option<&str>,
    ) -> LinkResult<Value> {
        if wav_bytes.is_empty() || wav_bytes.len() > MAX_VOICE_BYTES {
            return Err(LinkError::Invalid(
                "voice note must contain at most 2 MiB of WAV audio".into(),
            ));
        }
        let voice = json!({
            "type": "file",
            "mime_type": "audio/wav",
            "filename": "voice_note.wav",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(wav_bytes),
        });
        let mut items = Vec::new();
        if !message.trim().is_empty() {
            items.push(json!({"type": "text", "text": message}));
        }
        items.push(voice);
        let mut body = Map::new();
        body.insert("message".into(), json!(message));
        body.insert("device_id".into(), json!(self.shared.device.node_id));
        body.insert("items".into(), Value::Array(items));
        if let Some(s) = session_id {
            body.insert("session_id".into(), json!(s));
        }
        if let Some(m) = output_modality {
            body.insert("output_modality".into(), json!(m));
        }
        if let Some(d) = device {
            body.insert("device".into(), json!(d));
        }
        self.post_chat(Value::Object(body)).await
    }

    /// Posts a picture for Muse to look at, with `message` as its text.
    pub async fn send_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        session_id: Option<&str>,
        message: &str,
        filename: Option<&str>,
        output_modality: &str,
    ) -> LinkResult<Value> {
        check_image(image_bytes, mime_type)?;
        let mut image = json!({
            "type": "image",
            "mime_type": mime_type,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(image_bytes),
        });
        if let Some(f) = filename.filter(|f| !f.is_empty()) {
            image["filename"] = json!(f);
        }
        let mut items = Vec::new();
        if !message.trim().is_empty() {
            items.push(json!({"type": "text", "text": message}));
        }
        items.push(image);
        let mut body = Map::new();
        body.insert("message".into(), json!(message));
        body.insert("output_modality".into(), json!(output_modality));
        body.insert("device_id".into(), json!(self.shared.device.node_id));
        body.insert("items".into(), Value::Array(items));
        if let Some(s) = session_id {
            body.insert("session_id".into(), json!(s));
        }
        self.post_chat(Value::Object(body)).await
    }

    async fn post_chat(&self, body: Value) -> LinkResult<Value> {
        let bytes = serde_json::to_vec(&body).map_err(|_| LinkError::Invalid("bad body".into()))?;
        let headers = http_headers(&[Header::new("Content-Type", "application/json")]);
        let (status, resp) = self
            .request_raw("POST", CHAT_PATH, &bytes, headers, REQUEST_TIMEOUT)
            .await?;
        if !(200..300).contains(&status) {
            return Err(LinkError::HttpStatus {
                status,
                path: CHAT_PATH.into(),
            });
        }
        if resp.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&resp).map_err(|_| LinkError::Invalid("bad acknowledgement".into()))
    }

    /// A buffered request for a small JSON reply. Returns `(status, parsed body)`.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[Header],
    ) -> LinkResult<(u16, Value)> {
        let (status, resp) = self
            .request_raw(method, path, body, headers.to_vec(), REQUEST_TIMEOUT)
            .await?;
        let value = if resp.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&resp).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(&resp).chars().take(2000).collect())
            })
        };
        Ok((status, value))
    }

    async fn request_raw(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: Vec<Header>,
        timeout: Duration,
    ) -> LinkResult<(u16, Vec<u8>)> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(LinkError::NotConnected);
        }
        let (tx, rx) = oneshot::channel();
        let upload_in_parts = body.len() > MAX_INLINE_REQUEST_BYTES;
        let stream_id = {
            let mut io = self.shared.io.lock().await;
            if io.closed {
                return Err(LinkError::NotConnected);
            }
            let enc = if upload_in_parts {
                io.transport.start_stream_request(
                    method,
                    path,
                    ServiceType::Daemon,
                    &headers,
                    random_chunk_id(&mut OsRng),
                )?
            } else {
                io.transport.encrypt_http_request(
                    method,
                    path,
                    body,
                    ServiceType::Daemon,
                    &headers,
                    random_chunk_id(&mut OsRng),
                )?
            };
            let stream_id = enc.stream_id;
            self.shared.pending.lock().unwrap().entries.insert(
                stream_id,
                Pending::Buffered(BufferedReq {
                    got_head: false,
                    status: 0,
                    body: Vec::new(),
                    done: Some(tx),
                }),
            );
            if let Err(e) = io.send_frames(enc.frames).await {
                drop(io);
                self.shared.mark_closed();
                self.close_ws().await;
                return Err(e);
            }
            if upload_in_parts {
                let mut offset = 0;
                while offset < body.len() {
                    let end = (offset + REQUEST_CHUNK_BYTES).min(body.len());
                    let last = end == body.len();
                    let frames = io.transport.encrypt_body_chunk(
                        stream_id,
                        &body[offset..end],
                        ServiceType::Daemon,
                        last,
                        random_chunk_id(&mut OsRng),
                    )?;
                    if let Err(e) = io.send_frames(frames).await {
                        drop(io);
                        self.shared.mark_closed();
                        self.close_ws().await;
                        return Err(e);
                    }
                    offset = end;
                }
            }
            stream_id
        };

        let result = tokio::time::timeout(timeout, rx).await;
        self.shared
            .pending
            .lock()
            .unwrap()
            .entries
            .remove(&stream_id);
        match result {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(LinkError::SessionEnded),
            Err(_) => {
                self.reset_stream(stream_id).await;
                Err(LinkError::Timeout)
            }
        }
    }

    /// Looks up a side chat without reading its events.
    pub async fn open_side_chat(&self, session_id: &str) -> LinkResult<SideChat> {
        let body = serde_json::to_vec(&json!({ "session_id": session_id }))
            .map_err(|_| LinkError::Invalid("bad body".into()))?;
        let mut stream = self
            .stream_http(
                "POST",
                SUBSCRIBE_PATH,
                &body,
                &[
                    Header::new("Content-Type", "application/json"),
                    Header::new("Accept", "application/x-ndjson"),
                ],
                MAX_STREAM_BUFFER_BYTES,
            )
            .await?;
        let status = stream.status();
        stream.close().await;
        if (200..300).contains(&status) {
            Ok(SideChat {
                session_id: session_id.to_owned(),
                exists: true,
            })
        } else if status == 404 {
            Ok(SideChat {
                session_id: session_id.to_owned(),
                exists: false,
            })
        } else {
            Err(LinkError::HttpStatus {
                status,
                path: SUBSCRIBE_PATH.into(),
            })
        }
    }

    /// Subscribes to a chat's NDJSON event stream.
    pub async fn subscribe_chat(&self, session_id: Option<&str>) -> LinkResult<ChatSubscription> {
        let body = serde_json::to_vec(&match session_id {
            Some(s) => json!({ "session_id": s }),
            None => json!({}),
        })
        .map_err(|_| LinkError::Invalid("bad body".into()))?;
        let stream = self
            .stream_http(
                "POST",
                SUBSCRIBE_PATH,
                &body,
                &[
                    Header::new("Content-Type", "application/json"),
                    Header::new("Accept", "application/x-ndjson"),
                ],
                MAX_STREAM_BUFFER_BYTES,
            )
            .await?;
        let status = stream.status();
        if !(200..300).contains(&status) {
            let mut stream = stream;
            stream.close().await;
            return Err(LinkError::HttpStatus {
                status,
                path: SUBSCRIBE_PATH.into(),
            });
        }
        Ok(ChatSubscription {
            stream,
            buffered: Vec::new(),
            done: false,
        })
    }

    /// Streams MP3 for a voice-modality assistant reply (with the legacy fallback path).
    pub async fn stream_tts(&self, message_id: &str) -> LinkResult<HttpStream> {
        let encoded = encode_uri_component(message_id, "-_.!~*'()");
        let mut last_err = LinkError::NotConnected;
        for (i, prefix) in ["/api/voice/tts-stream", "/voice/tts-stream"]
            .iter()
            .enumerate()
        {
            let path = format!("{prefix}?message_id={encoded}");
            let stream = self
                .stream_http(
                    "GET",
                    &path,
                    b"",
                    &[Header::new("Accept", "audio/mpeg")],
                    MAX_TTS_BUFFER_BYTES,
                )
                .await?;
            let status = stream.status();
            if status == 404 && i == 0 {
                let mut stream = stream;
                stream.close().await;
                continue;
            }
            if !(200..300).contains(&status) {
                let mut stream = stream;
                stream.close().await;
                last_err = LinkError::HttpStatus {
                    status,
                    path: (*prefix).into(),
                };
                break;
            }
            return Ok(stream);
        }
        Err(last_err)
    }

    /// Downloads a file from Muse's workspace with `GET /fs/raw/<path>` (the avatar's image
    /// and clips, and media Muse sends in replies). Fails if it is larger than `max_bytes`.
    pub async fn fetch_workspace_file(&self, path: &str, max_bytes: usize) -> LinkResult<Vec<u8>> {
        let encoded: Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .map(|s| encode_uri_component(s, "-_.!~*'()"))
            .collect();
        let url = format!("/fs/raw/{}", encoded.join("/"));
        let mut stream = self
            .stream_http("GET", &url, b"", &[], max_bytes + 64 * 1024)
            .await?;
        let status = stream.status();
        if !(200..300).contains(&status) {
            stream.close().await;
            return Err(LinkError::HttpStatus {
                status,
                path: "/fs/raw".into(),
            });
        }
        let mut out = Vec::new();
        while let Some(chunk) = stream.next_chunk().await {
            out.extend_from_slice(&chunk?);
            if out.len() > max_bytes {
                stream.close().await;
                return Err(LinkError::Bounded("workspace file too large"));
            }
        }
        Ok(out)
    }

    async fn stream_http(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[Header],
        max_buffer_bytes: usize,
    ) -> LinkResult<HttpStream> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(LinkError::NotConnected);
        }
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let full_headers = http_headers(headers);
        let stream_id = {
            let mut io = self.shared.io.lock().await;
            if io.closed {
                return Err(LinkError::NotConnected);
            }
            let enc = io.transport.encrypt_http_request(
                method,
                path,
                body,
                ServiceType::Daemon,
                &full_headers,
                random_chunk_id(&mut OsRng),
            )?;
            let stream_id = enc.stream_id;
            self.shared.pending.lock().unwrap().entries.insert(
                stream_id,
                Pending::Stream(StreamReg {
                    got_head: false,
                    buffered: 0,
                    max_buffer_bytes,
                    ended: false,
                    tx,
                }),
            );
            if let Err(e) = io.send_frames(enc.frames).await {
                drop(io);
                self.shared.mark_closed();
                self.close_ws().await;
                return Err(e);
            }
            stream_id
        };

        let mut rx = rx;
        // Wait for the head (status + headers).
        let head = tokio::time::timeout(REQUEST_TIMEOUT, rx.recv()).await;
        match head {
            Ok(Some(StreamItem::Head { status, headers })) => Ok(HttpStream {
                shared: self.shared.clone(),
                stream_id,
                status,
                _headers: headers,
                rx,
                finished: false,
            }),
            Ok(Some(StreamItem::Err(e))) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap()
                    .entries
                    .remove(&stream_id);
                Err(e)
            }
            Ok(Some(_)) | Ok(None) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap()
                    .entries
                    .remove(&stream_id);
                Err(LinkError::SessionEnded)
            }
            Err(_) => {
                self.reset_stream(stream_id).await;
                Err(LinkError::Timeout)
            }
        }
    }

    /// Opens a request whose body keeps streaming while its response streams back (the
    /// realtime voice call). Returns at once; [`DuplexStream::wait_response`] waits for the head.
    pub async fn open_duplex(
        &self,
        method: &str,
        path: &str,
        headers: &[Header],
    ) -> LinkResult<DuplexStream> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(LinkError::NotConnected);
        }
        let (tx, rx) = mpsc::channel(DUPLEX_QUEUE);
        let full_headers = http_headers(headers);
        let mut io = self.shared.io.lock().await;
        if io.closed {
            return Err(LinkError::NotConnected);
        }
        let enc = io.transport.start_stream_request(
            method,
            path,
            ServiceType::Daemon,
            &full_headers,
            random_chunk_id(&mut OsRng),
        )?;
        let stream_id = enc.stream_id;
        self.shared.pending.lock().unwrap().entries.insert(
            stream_id,
            Pending::Stream(StreamReg {
                got_head: false,
                buffered: 0,
                max_buffer_bytes: MAX_DUPLEX_BUFFER_BYTES,
                ended: false,
                tx,
            }),
        );
        if let Err(e) = io.send_frames(enc.frames).await {
            drop(io);
            self.shared.mark_closed();
            self.close_ws().await;
            return Err(e);
        }
        drop(io);
        Ok(DuplexStream {
            session: self.clone(),
            stream_id,
            rx,
            status: None,
            ended: false,
            input_closed: false,
        })
    }

    /// Sends one body chunk on a duplex stream.
    async fn send_body_chunk(&self, stream_id: i64, data: &[u8], end_body: bool) -> LinkResult<()> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(LinkError::NotConnected);
        }
        let mut io = self.shared.io.lock().await;
        if io.closed {
            return Err(LinkError::NotConnected);
        }
        let frames = io.transport.encrypt_body_chunk(
            stream_id,
            data,
            ServiceType::Daemon,
            end_body,
            random_chunk_id(&mut OsRng),
        )?;
        if let Err(e) = io.send_frames(frames).await {
            drop(io);
            self.shared.mark_closed();
            self.close_ws().await;
            return Err(e);
        }
        Ok(())
    }

    async fn reset_stream(&self, stream_id: i64) {
        self.shared
            .pending
            .lock()
            .unwrap()
            .entries
            .remove(&stream_id);
        if self.shared.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut io = self.shared.io.lock().await;
        if io.closed {
            return;
        }
        if let Ok(frames) = io.transport.encrypt_reset(
            stream_id,
            ServiceType::Daemon,
            "request ended",
            ResetCode::Cancelled,
            random_chunk_id(&mut OsRng),
        ) {
            let _ = io.send_frames(frames).await;
        }
    }

    async fn close_ws(&self) {
        let mut io = self.shared.io.lock().await;
        io.closed = true;
        let _ = io.sink.close().await;
    }
}

fn check_image(image_bytes: &[u8], mime_type: &str) -> LinkResult<()> {
    if !IMAGE_MIME_TYPES.contains(&mime_type) {
        return Err(LinkError::Invalid(
            "images must be JPEG, PNG, GIF or WebP".into(),
        ));
    }
    if image_bytes.is_empty() {
        return Err(LinkError::Invalid("the image is empty".into()));
    }
    if image_bytes.len() > MAX_IMAGE_BYTES {
        return Err(LinkError::Invalid("images must be 2 MiB or less".into()));
    }
    Ok(())
}

fn http_headers(extra: &[Header]) -> Vec<Header> {
    let mut headers = extra.to_vec();
    headers.push(Header::new("x-request-id", random_uuid()));
    headers.push(Header::new("x-app-id", APP_ID));
    headers
}

// ---- connection setup -------------------------------------------------------------------------

async fn connect_ws(url: &str, token: &str) -> LinkResult<WsStream> {
    let mut request = url
        .into_client_request()
        .map_err(|e| LinkError::Connect(e.to_string()))?;
    let headers = request.headers_mut();
    headers.insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| LinkError::Invalid("bad bearer token".into()))?,
    );
    headers.insert(
        "User-Agent",
        crate::api::user_agent()
            .parse()
            .map_err(|_| LinkError::Invalid("bad user agent".into()))?,
    );
    let config = WebSocketConfig {
        max_message_size: Some(MAX_WS_MESSAGE),
        max_frame_size: Some(MAX_WS_MESSAGE),
        ..Default::default()
    };
    let connector = Connector::Rustls(crate::tls::client_config());
    match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            false,
            Some(connector),
        ),
    )
    .await
    {
        Ok(Ok((ws, _resp))) => Ok(ws),
        Ok(Err(e)) => Err(map_ws_error(e)),
        Err(_) => Err(LinkError::Connect("connection timed out".into())),
    }
}

fn map_ws_error(e: tokio_tungstenite::tungstenite::Error) -> LinkError {
    use tokio_tungstenite::tungstenite::Error as WsError;
    if let WsError::Http(resp) = &e {
        let status = resp.status().as_u16();
        return LinkError::UpgradeRejected(status);
    }
    LinkError::Connect(e.to_string())
}

/// Drives the Noise XX handshake over an already-open split WebSocket.
async fn handshake(
    sink: &mut WsSink,
    source: &mut SplitStream<WsStream>,
) -> LinkResult<NoiseTransport> {
    let mut initiator = NoiseXXInitiator::new(OsKeySource);
    initiator.initialize()?;
    let msg1 = initiator.write_message1()?;
    sink.send(Message::Binary(msg1))
        .await
        .map_err(|e| LinkError::Connect(e.to_string()))?;
    sink.flush()
        .await
        .map_err(|e| LinkError::Connect(e.to_string()))?;
    let msg2 = recv_binary(source).await?;
    initiator.read_message2(&msg2)?;
    let msg3 = initiator.write_message3()?;
    sink.send(Message::Binary(msg3))
        .await
        .map_err(|e| LinkError::Connect(e.to_string()))?;
    sink.flush()
        .await
        .map_err(|e| LinkError::Connect(e.to_string()))?;
    let (send, recv) = initiator.split()?;
    log::info!("Noise session established");
    Ok(NoiseTransport::new(send, recv))
}

async fn recv_binary(source: &mut SplitStream<WsStream>) -> LinkResult<Vec<u8>> {
    loop {
        match source.next().await {
            Some(Ok(Message::Binary(b))) => return Ok(b),
            Some(Ok(Message::Text(_))) => {
                return Err(LinkError::Connect("handshake got a text frame".into()))
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(LinkError::Connect(e.to_string())),
            None => {
                return Err(LinkError::Connect(
                    "connection closed during handshake".into(),
                ))
            }
        }
    }
}

async fn open_control_stream(shared: &Arc<Shared>) -> LinkResult<()> {
    let register_id = random_uuid();
    let message = shared.device.register_message(&register_id);
    let encoded = encode_message(&message)?;
    if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(LinkError::Bounded(
            "link.register exceeds the VM's control message budget",
        ));
    }
    let mut io = shared.io.lock().await;
    let enc = io.transport.start_stream_request(
        "POST",
        CONTROL_PATH,
        ServiceType::Daemon,
        &[],
        random_chunk_id(&mut OsRng),
    )?;
    let stream_id = enc.stream_id;
    shared.control_stream_id.store(stream_id, Ordering::SeqCst);
    *shared.register_id.lock().unwrap() = register_id;
    io.send_frames(enc.frames).await?;
    // The register message as a body chunk on the control stream.
    let frames = io.transport.encrypt_body_chunk(
        stream_id,
        &encoded,
        ServiceType::Daemon,
        false,
        random_chunk_id(&mut OsRng),
    )?;
    io.send_frames(frames).await?;
    log::info!("sent link.register as {}", shared.device.node_id);
    Ok(())
}

/// Sends one control message (a body chunk on the control stream).
async fn send_control(shared: &Arc<Shared>, message: &Value) -> LinkResult<()> {
    let encoded = encode_message(message)?;
    let stream_id = shared.control_stream_id.load(Ordering::SeqCst);
    let mut io = shared.io.lock().await;
    if io.closed {
        return Err(LinkError::NotConnected);
    }
    let frames = io.transport.encrypt_body_chunk(
        stream_id,
        &encoded,
        ServiceType::Daemon,
        false,
        random_chunk_id(&mut OsRng),
    )?;
    io.send_frames(frames).await
}

// ---- receive loop -----------------------------------------------------------------------------

async fn ping_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(PING_INTERVAL);
    interval.tick().await; // first tick is immediate
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shared.stop.notified() => return,
        }
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        let silent = shared.last_rx.lock().unwrap().elapsed();
        if silent > PING_INTERVAL + PONG_TIMEOUT {
            log::warn!(
                "no reply from the VM for {}s; closing the link to reconnect",
                silent.as_secs()
            );
            shared.dead.notify_one();
            return;
        }
        let mut io = shared.io.lock().await;
        if io.closed {
            return;
        }
        if io.sink.send(Message::Ping(Vec::new())).await.is_err() {
            return;
        }
        let _ = io.sink.flush().await;
    }
}

async fn read_loop(shared: Arc<Shared>, mut source: SplitStream<WsStream>) {
    let outcome = run_read_loop(&shared, &mut source).await;
    shared.mark_closed();
    {
        let mut io = shared.io.lock().await;
        io.closed = true;
        let _ = io.sink.close().await;
    }
    // Registration waiters must wake even if no ack arrived.
    shared.registered.send_if_modified(|s| {
        if *s == RegState::Pending {
            *s = RegState::Rejected;
            true
        } else {
            false
        }
    });
    let _ = shared.outcome.send(Some(outcome));
}

async fn run_read_loop(shared: &Arc<Shared>, source: &mut SplitStream<WsStream>) -> Outcome {
    let mut decoder = MessageDecoder::new();
    loop {
        let msg = tokio::select! {
            m = source.next() => m,
            _ = shared.stop.notified() => return Outcome::Stopped,
            _ = shared.dead.notified() => return Outcome::Closed,
        };
        if matches!(msg, Some(Ok(_))) {
            *shared.last_rx.lock().unwrap() = std::time::Instant::now();
        }
        if shared.stopped.load(Ordering::SeqCst) {
            return Outcome::Stopped;
        }
        let raw = match msg {
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(Message::Close(_))) | None => return Outcome::Closed,
            Some(Ok(Message::Text(_))) => {
                log::warn!("ignoring text frame on Noise connection");
                continue;
            }
            Some(Ok(_)) => continue, // Ping/Pong/Frame handled by the library
            Some(Err(e)) => {
                log::info!("control connection closed: {e}");
                return Outcome::Closed;
            }
        };

        let frame = {
            let mut io = shared.io.lock().await;
            match io.transport.decrypt_frame(&raw) {
                Ok(f) => f,
                Err(_) => return Outcome::Closed,
            }
        };
        let Some(frame) = frame else { continue };

        let control_id = shared.control_stream_id.load(Ordering::SeqCst);
        let stream_id = frame_stream_id(&frame);

        if !shared.register_device || stream_id != control_id {
            dispatch_stream_frame(shared, frame).await;
            continue;
        }

        // Control stream frame.
        match frame {
            DecryptedFrame::Reset { reset, .. } => {
                log::warn!("control stream reset: {}", reset.reason);
                return Outcome::Closed;
            }
            DecryptedFrame::Response { response, .. } => {
                if !(200..300).contains(&response.status) {
                    log::warn!("/link-control refused: HTTP {}", response.status);
                    return if response.status == 403 {
                        Outcome::Forbidden
                    } else {
                        Outcome::Closed
                    };
                }
                if let Some(outcome) = feed_control(shared, &mut decoder, &response.body).await {
                    return outcome;
                }
                if response.end_body {
                    log::info!("control stream ended by VM");
                    return Outcome::Closed;
                }
            }
            DecryptedFrame::BodyChunk { chunk, .. } => {
                if let Some(outcome) = feed_control(shared, &mut decoder, &chunk.data).await {
                    return outcome;
                }
                if chunk.end_body {
                    log::info!("control stream ended by VM");
                    return Outcome::Closed;
                }
            }
        }
    }
}

async fn feed_control(
    shared: &Arc<Shared>,
    decoder: &mut MessageDecoder,
    data: &[u8],
) -> Option<Outcome> {
    let messages = match decoder.feed(data) {
        Ok(m) => m,
        Err(_) => return Some(Outcome::Closed),
    };
    for message in messages {
        if let Some(outcome) = handle_control_message(shared, message).await {
            return Some(outcome);
        }
    }
    None
}

async fn handle_control_message(shared: &Arc<Shared>, message: Value) -> Option<Outcome> {
    let obj = message.as_object()?;
    let id = obj.get("id").and_then(Value::as_str);
    let register_id = shared.register_id.lock().unwrap().clone();
    if id == Some(register_id.as_str()) && obj.get("method").is_none() {
        let rejected = obj.get("error").is_some_and(|v| !v.is_null())
            || obj.get("ok") == Some(&Value::Bool(false));
        if rejected {
            shared.registered.send_replace(RegState::Rejected);
            log::error!("link.register rejected");
            return Some(Outcome::Forbidden);
        }
        shared.registered.send_replace(RegState::Ok);
        log::info!("registered with the Muse");
        return None;
    }
    if let Some(event) = obj.get("event").and_then(Value::as_str) {
        if event == "link.unpaired" || event == "node.unpaired" {
            log::warn!("the Muse removed this device");
            return Some(Outcome::Unpaired);
        }
    }
    if obj.get("method").and_then(Value::as_str) == Some("link.invoke") {
        let shared = shared.clone();
        tokio::spawn(async move {
            handle_invoke(shared, message).await;
        });
    }
    None
}

async fn handle_invoke(shared: Arc<Shared>, message: Value) {
    let obj = match message.as_object() {
        Some(o) => o,
        None => return,
    };
    let invoke_id = match obj.get("id") {
        Some(v) if !v.is_null() => v.clone(),
        _ => return,
    };
    let command = obj.get("command").cloned().unwrap_or(Value::Null);
    let params = obj
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let timeout_ms = obj
        .get("timeout_ms")
        .and_then(Value::as_i64)
        .filter(|v| *v != 0);

    let Some(command) = command.as_str().map(str::to_owned) else {
        let _ = send_control(
            &shared,
            &json!({"method": "link.result", "id": invoke_id, "ok": false, "error": "command must be a string"}),
        )
        .await;
        return;
    };

    let permit = match shared.invoke_slots.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => return,
    };
    log::info!("invoke {}", printable(&command));
    let result = shared.runner.run(command.clone(), params, timeout_ms).await;
    drop(permit);

    // Validate it encodes before sending, so a broken transport is never
    // treated as a command error.
    let mut result_message = Map::new();
    result_message.insert("method".into(), json!("link.result"));
    result_message.insert("id".into(), invoke_id.clone());
    for (k, v) in &result {
        result_message.insert(k.clone(), v.clone());
    }
    let message = Value::Object(result_message);
    let outgoing = if encode_message(&message).is_ok() {
        message
    } else {
        json!({"method": "link.result", "id": invoke_id, "ok": false, "error": "command failed"})
    };
    if let Err(e) = send_control(&shared, &outgoing).await {
        log::info!("could not send link.result: {e}");
    }
}

fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn frame_stream_id(frame: &DecryptedFrame) -> i64 {
    match frame {
        DecryptedFrame::Response { stream_id, .. } => *stream_id,
        DecryptedFrame::BodyChunk { stream_id, .. } => *stream_id,
        DecryptedFrame::Reset { stream_id, .. } => *stream_id,
    }
}

async fn dispatch_stream_frame(shared: &Arc<Shared>, frame: DecryptedFrame) {
    let stream_id = frame_stream_id(&frame);
    if log::log_enabled!(log::Level::Debug) {
        let (kind, len, end) = match &frame {
            DecryptedFrame::Reset { .. } => ("reset", 0, true),
            DecryptedFrame::Response { response, .. } => {
                ("head", response.body.len(), response.end_body)
            }
            DecryptedFrame::BodyChunk { chunk, .. } => ("chunk", chunk.data.len(), chunk.end_body),
        };
        log::debug!("stream {stream_id}: {kind} {len} bytes end={end}");
    }
    let mut pending = shared.pending.lock().unwrap();
    let Some(entry) = pending.entries.get_mut(&stream_id) else {
        log::debug!("stream {stream_id}: no pending entry, frame dropped");
        return;
    };
    match entry {
        Pending::Buffered(req) => {
            if dispatch_buffered(req, &frame) {
                pending.entries.remove(&stream_id);
            }
        }
        Pending::Stream(reg) => {
            if !dispatch_stream(reg, frame) {
                // The consumer fell behind or the stream ended; drop it.
                pending.entries.remove(&stream_id);
            }
        }
    }
}

/// Returns true when the buffered request is complete and can be removed.
fn dispatch_buffered(req: &mut BufferedReq, frame: &DecryptedFrame) -> bool {
    let (data, ended) = match frame {
        DecryptedFrame::Reset { reset, .. } => {
            if let Some(done) = req.done.take() {
                let _ = done.send(Err(LinkError::Invalid(format!(
                    "stream reset: {}",
                    reset.reason
                ))));
            }
            return true;
        }
        DecryptedFrame::Response { response, .. } => {
            if req.got_head || !(100..=599).contains(&response.status) {
                if let Some(done) = req.done.take() {
                    let _ = done.send(Err(LinkError::Invalid(
                        "invalid or duplicate response head".into(),
                    )));
                }
                return true;
            }
            req.got_head = true;
            req.status = status_u16(response);
            (response.body.clone(), response.end_body)
        }
        DecryptedFrame::BodyChunk { chunk, .. } => {
            if !req.got_head {
                if let Some(done) = req.done.take() {
                    let _ = done.send(Err(LinkError::Invalid("response body before head".into())));
                }
                return true;
            }
            (chunk.data.clone(), chunk.end_body)
        }
    };
    if req.body.len() + data.len() > MAX_RESPONSE_BYTES {
        if let Some(done) = req.done.take() {
            let _ = done.send(Err(LinkError::Bounded("response too large")));
        }
        return true;
    }
    req.body.extend_from_slice(&data);
    if ended {
        if let Some(done) = req.done.take() {
            let _ = done.send(Ok((req.status, std::mem::take(&mut req.body))));
        }
        return true;
    }
    false
}

/// Returns false when the stream should be dropped from the registry.
fn dispatch_stream(reg: &mut StreamReg, frame: DecryptedFrame) -> bool {
    if reg.ended {
        return true;
    }
    let (data, ended) = match frame {
        DecryptedFrame::Reset { .. } => {
            let _ = reg.tx.try_send(StreamItem::Err(LinkError::Invalid(
                "Muse response stream reset".into(),
            )));
            reg.ended = true;
            return false;
        }
        DecryptedFrame::Response { response, .. } => {
            if !reg.got_head {
                reg.got_head = true;
                let _ = reg.tx.try_send(StreamItem::Head {
                    status: status_u16(&response),
                    headers: response.headers.clone(),
                });
            }
            (response.body, response.end_body)
        }
        DecryptedFrame::BodyChunk { chunk, .. } => {
            if !reg.got_head {
                let _ = reg.tx.try_send(StreamItem::Err(LinkError::Invalid(
                    "Muse response body arrived without headers".into(),
                )));
                return false;
            }
            (chunk.data, chunk.end_body)
        }
    };
    if !data.is_empty() {
        if reg.buffered + data.len() > reg.max_buffer_bytes {
            let _ = reg.tx.try_send(StreamItem::Err(LinkError::Bounded(
                "Muse response consumer fell behind",
            )));
            return false;
        }
        reg.buffered += data.len();
        if reg.tx.try_send(StreamItem::Chunk(data)).is_err() {
            let _ = reg.tx.try_send(StreamItem::Err(LinkError::Bounded(
                "Muse response consumer fell behind",
            )));
            return false;
        }
    }
    if ended {
        reg.ended = true;
        let _ = reg.tx.try_send(StreamItem::End);
        return false;
    }
    true
}

fn status_u16(response: &ApplicationResponse) -> u16 {
    if (0..=u16::MAX as i32).contains(&response.status) {
        response.status as u16
    } else {
        0
    }
}

// ---- consumer types ---------------------------------------------------------------------------

/// A bounded HTTP response stream. Dropping or [`HttpStream::close`]ing it
/// cancels the underlying request.
pub struct HttpStream {
    shared: Arc<Shared>,
    stream_id: i64,
    status: u16,
    _headers: Vec<Header>,
    rx: mpsc::Receiver<StreamItem>,
    finished: bool,
}

impl HttpStream {
    /// The response status.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The next body chunk, `None` at end of stream.
    pub async fn next_chunk(&mut self) -> Option<LinkResult<Vec<u8>>> {
        if self.finished {
            return None;
        }
        loop {
            match self.rx.recv().await {
                Some(StreamItem::Chunk(b)) => {
                    // Account the buffer back to the registry.
                    let mut pending = self.shared.pending.lock().unwrap();
                    if let Some(Pending::Stream(reg)) = pending.entries.get_mut(&self.stream_id) {
                        reg.buffered = reg.buffered.saturating_sub(b.len());
                    }
                    return Some(Ok(b));
                }
                Some(StreamItem::End) => {
                    self.finished = true;
                    return None;
                }
                Some(StreamItem::Err(e)) => {
                    self.finished = true;
                    return Some(Err(e));
                }
                Some(StreamItem::Head { .. }) => continue,
                None => {
                    self.finished = true;
                    return None;
                }
            }
        }
    }

    /// Closes the stream, resetting it upstream if it had not ended.
    pub async fn close(&mut self) {
        let ended = self.finished;
        self.finished = true;
        self.rx.close();
        if !ended {
            let session = LinkSession {
                shared: self.shared.clone(),
            };
            session.reset_stream(self.stream_id).await;
        } else {
            self.shared
                .pending
                .lock()
                .unwrap()
                .entries
                .remove(&self.stream_id);
        }
    }
}

impl Drop for HttpStream {
    fn drop(&mut self) {
        self.shared
            .pending
            .lock()
            .unwrap()
            .entries
            .remove(&self.stream_id);
    }
}

/// A request with independent upload and download (from [`LinkSession::open_duplex`]).
/// Dropping or [`DuplexStream::close`]ing it resets the stream.
pub struct DuplexStream {
    session: LinkSession,
    stream_id: i64,
    rx: mpsc::Receiver<StreamItem>,
    status: Option<u16>,
    ended: bool,
    input_closed: bool,
}

impl DuplexStream {
    /// Sends a body chunk (at most [`MAX_SEND_BYTES`]).
    pub async fn send(&mut self, data: &[u8]) -> LinkResult<()> {
        if self.input_closed {
            return Err(LinkError::Invalid("request body already closed".into()));
        }
        if data.len() > MAX_SEND_BYTES {
            return Err(LinkError::Invalid("request chunk exceeds 64 KiB".into()));
        }
        self.session
            .send_body_chunk(self.stream_id, data, false)
            .await
    }

    /// Ends the upload; the response keeps streaming.
    pub async fn half_close(&mut self) -> LinkResult<()> {
        if self.input_closed {
            return Ok(());
        }
        self.input_closed = true;
        self.session
            .send_body_chunk(self.stream_id, b"", true)
            .await
    }

    /// Waits for the response head and returns its status (2xx or an error).
    pub async fn wait_response(&mut self, timeout: std::time::Duration) -> LinkResult<u16> {
        if let Some(s) = self.status {
            return Ok(s);
        }
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(StreamItem::Head { status, .. })) => {
                self.status = Some(status);
                if (200..300).contains(&status) {
                    Ok(status)
                } else {
                    Err(LinkError::HttpStatus {
                        status,
                        path: "duplex".into(),
                    })
                }
            }
            Ok(Some(StreamItem::Err(e))) => Err(e),
            Ok(Some(_)) | Ok(None) => Err(LinkError::SessionEnded),
            Err(_) => Err(LinkError::Timeout),
        }
    }

    /// The next response body chunk, `None` at end of stream.
    pub async fn next_chunk(&mut self) -> Option<LinkResult<Vec<u8>>> {
        if self.ended {
            return None;
        }
        loop {
            match self.rx.recv().await {
                Some(StreamItem::Chunk(b)) => {
                    let mut pending = self.session.shared.pending.lock().unwrap();
                    if let Some(Pending::Stream(reg)) = pending.entries.get_mut(&self.stream_id) {
                        reg.buffered = reg.buffered.saturating_sub(b.len());
                    }
                    return Some(Ok(b));
                }
                Some(StreamItem::Head { status, .. }) => {
                    self.status = Some(status);
                    continue;
                }
                Some(StreamItem::End) | None => {
                    self.ended = true;
                    return None;
                }
                Some(StreamItem::Err(e)) => {
                    self.ended = true;
                    return Some(Err(e));
                }
            }
        }
    }

    /// Resets the stream unless both directions have ended.
    pub async fn close(&mut self) {
        let done = self.ended && self.input_closed;
        self.ended = true;
        self.input_closed = true;
        self.rx.close();
        if done {
            self.session
                .shared
                .pending
                .lock()
                .unwrap()
                .entries
                .remove(&self.stream_id);
        } else {
            self.session.reset_stream(self.stream_id).await;
        }
    }
}

impl Drop for DuplexStream {
    fn drop(&mut self) {
        self.session
            .shared
            .pending
            .lock()
            .unwrap()
            .entries
            .remove(&self.stream_id);
    }
}

/// An NDJSON chat-event subscription (from [`LinkSession::subscribe_chat`]).
pub struct ChatSubscription {
    stream: HttpStream,
    buffered: Vec<u8>,
    done: bool,
}

impl ChatSubscription {
    /// The next chat event (`{"type":"event", ...}`), or `None` at end.
    ///
    /// Non-event records and malformed lines are skipped, as in the reference.
    pub async fn next_event(&mut self) -> Option<LinkResult<Value>> {
        loop {
            if let Some(pos) = self.buffered.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buffered.drain(..=pos).collect();
                let event = parse_chat_event(&line);
                if log::log_enabled!(log::Level::Debug) {
                    let ty = serde_json::from_slice::<Value>(&line)
                        .ok()
                        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned));
                    log::debug!(
                        "chat line {} bytes, type {:?}, kept {}",
                        line.len(),
                        ty,
                        event.is_some()
                    );
                }
                if let Some(event) = event {
                    return Some(Ok(event));
                }
                continue;
            }
            if self.done {
                if !self.buffered.is_empty() {
                    let line = std::mem::take(&mut self.buffered);
                    if let Some(event) = parse_chat_event(&line) {
                        return Some(Ok(event));
                    }
                }
                return None;
            }
            match self.stream.next_chunk().await {
                Some(Ok(chunk)) => {
                    self.buffered.extend_from_slice(&chunk);
                    if self.buffered.len() > MAX_CHAT_EVENT_BYTES {
                        self.done = true;
                        return Some(Err(LinkError::Bounded("Muse chat event exceeds 256 KiB")));
                    }
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                }
            }
        }
    }

    /// Closes the subscription.
    pub async fn close(&mut self) {
        self.stream.close().await;
    }
}

fn parse_chat_event(raw: &[u8]) -> Option<Value> {
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let event: Value = serde_json::from_slice(raw).ok()?;
    if event.is_object() && event.get("type").and_then(Value::as_str) == Some("event") {
        Some(event)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_component_encoding_matches_firmware() {
        assert_eq!(encode_uri_component("a b/c", "-_.!~*'()"), "a%20b%2Fc");
        assert_eq!(encode_uri_component("x.y-z_~", "-_.!~*'()"), "x.y-z_~");
    }

    #[test]
    fn app_id_validation() {
        assert_eq!(validate_app_id("hatch-web").unwrap(), "hatch-web");
        assert_eq!(validate_app_id("42").unwrap(), "42");
        assert!(validate_app_id("0").is_err());
        assert!(validate_app_id("-1").is_err());
        assert!(validate_app_id("abc").is_err());
    }

    #[test]
    fn noise_url_builds_with_and_without_app_id() {
        let u = noise_url("host", "vm 1", None, false).unwrap();
        assert_eq!(u, "wss://host/v1/noise?vm_id=vm%201");
        let u = noise_url("host", "vm1", Some("42"), false).unwrap();
        assert!(u.ends_with("&app_id=42"));
        let u = noise_url("host", "vm1", None, true).unwrap();
        assert!(u.starts_with("ws://host"));
    }

    #[test]
    fn uuid_shape() {
        let id = random_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
    }

    #[test]
    fn check_image_rules() {
        assert!(check_image(b"x", "image/png").is_ok());
        assert!(check_image(b"", "image/png").is_err());
        assert!(check_image(b"x", "image/tiff").is_err());
    }

    #[test]
    fn parse_chat_event_filters_non_events() {
        assert!(parse_chat_event(b"   \n").is_none());
        assert!(parse_chat_event(br#"{"type":"other"}"#).is_none());
        assert!(parse_chat_event(br#"{"type":"event","event":"x"}"#).is_some());
    }
}
