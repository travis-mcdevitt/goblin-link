//! A simulated Muse VM for testing the whole client with no real network.
//!
//! Enabled by the `testsupport` feature. [`SimVm`] speaks the same Noise
//! transport and control/chat framing as a real VM, using [`muse_proto`] on the
//! responder side: it accepts a plaintext WebSocket on loopback, runs the Noise
//! XX responder handshake, acknowledges `link.register`, can push `link.invoke`
//! requests and collect their `link.result`s, answers `/chat/stream` with a
//! scripted acknowledgement, and streams scripted events on `/chat/subscribe`.
//!
//! Clients reach it by setting [`crate::SessionConfig::insecure_plaintext`],
//! which makes the client connect over `ws://` instead of `wss://`. That path
//! exists only for this test server and is never used in production.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand_core::OsRng;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use muse_proto::control::{encode_message, MessageDecoder};
use muse_proto::framing::{encode_noise_frames, random_chunk_id, NoiseFrameDecoder};
use muse_proto::noise::{CipherState, NoiseXXResponder, OsKeySource};
use muse_proto::transport::{decode_request_envelope, encode_response_envelope};
use muse_proto::{
    ApplicationRequest, ApplicationResponse, BodyChunk, FrameBody, Header, ServiceFrame,
};

type ServerWs = WebSocketStream<TcpStream>;

/// What the simulated VM should say in response to chat requests.
#[derive(Clone)]
pub struct Script {
    /// The acknowledgement object returned for `/chat/stream` and used as the
    /// session acknowledgement a [`crate::MuseTurn`] binds to.
    pub chat_ack: Value,
    /// Chat events streamed on `/chat/subscribe` after a `/chat/stream` arrives.
    pub subscribe_events: Vec<Value>,
    /// Whether `/chat/subscribe` reports the chat as known (200) or absent (404).
    pub subscribe_known: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            chat_ack: json!({"message_id": "u1", "session_id": "main"}),
            subscribe_events: Vec::new(),
            subscribe_known: true,
        }
    }
}

struct SendCtx {
    sink: SplitSink<ServerWs, Message>,
    send_cipher: CipherState,
}

struct Shared {
    send: tokio::sync::Mutex<Option<SendCtx>>,
    control_stream_id: AtomicI64,
    invokes: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    subscribe_streams: Mutex<Vec<i64>>,
    pending_events: Mutex<Vec<Value>>,
    registered: watch::Sender<bool>,
    node_id: Mutex<Option<String>>,
    script: Script,
}

impl Shared {
    async fn send_frame(&self, frame: &ServiceFrame) {
        let envelope = encode_response_envelope(frame);
        let proto_frames = match encode_noise_frames(&envelope, random_chunk_id(&mut OsRng)) {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut guard = self.send.lock().await;
        let Some(ctx) = guard.as_mut() else { return };
        for pf in proto_frames {
            let ciphertext = match ctx.send_cipher.encrypt_with_ad(b"", &pf) {
                Ok(c) => c,
                Err(_) => return,
            };
            if ctx.sink.send(Message::Binary(ciphertext)).await.is_err() {
                return;
            }
        }
        let _ = ctx.sink.flush().await;
    }

    async fn send_control(&self, message: &Value) {
        let data = match encode_message(message) {
            Ok(d) => d,
            Err(_) => return,
        };
        let stream_id = self.control_stream_id.load(Ordering::SeqCst);
        let frame = ServiceFrame::body_chunk(
            stream_id,
            BodyChunk {
                data,
                end_body: false,
            },
        );
        self.send_frame(&frame).await;
    }
}

/// A running simulated VM. Drop it (or call [`SimVm::shutdown`]) to stop.
pub struct SimVm {
    addr: std::net::SocketAddr,
    shared: Arc<Shared>,
    _task: tokio::task::JoinHandle<()>,
}

impl SimVm {
    /// Starts the VM on a loopback port with the given script.
    pub async fn start(script: Script) -> SimVm {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (reg_tx, _) = watch::channel(false);
        // A placeholder send context is replaced after the handshake.
        let shared = Arc::new(Shared {
            send: tokio::sync::Mutex::new(None),
            control_stream_id: AtomicI64::new(0),
            invokes: Mutex::new(HashMap::new()),
            subscribe_streams: Mutex::new(Vec::new()),
            pending_events: Mutex::new(Vec::new()),
            registered: reg_tx,
            node_id: Mutex::new(None),
            script,
        });
        let task = tokio::spawn(run_server(listener, shared.clone()));
        SimVm {
            addr,
            shared,
            _task: task,
        }
    }

    /// The `host:port` the client should connect to (`noise_host`).
    pub fn host(&self) -> String {
        self.addr.to_string()
    }

    /// Resolves once the device has registered; returns its node id.
    pub async fn wait_registered(&self) -> String {
        let mut rx = self.shared.registered.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return self
                    .shared
                    .node_id
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_default();
            }
            if rx.changed().await.is_err() {
                return String::new();
            }
        }
    }

    /// Sends a `link.invoke` to the device and awaits its `link.result`.
    pub async fn invoke(&self, command: &str, params: Value) -> Value {
        let id = format!("inv-{}", random_chunk_id(&mut OsRng).unsigned_abs());
        let (tx, rx) = oneshot::channel();
        self.shared.invokes.lock().unwrap().insert(id.clone(), tx);
        self.shared
            .send_control(&json!({
                "method": "link.invoke",
                "id": id,
                "command": command,
                "params": params,
            }))
            .await;
        rx.await.unwrap_or(Value::Null)
    }

    /// Sends a `link.unpaired` event to the device.
    pub async fn unpair(&self) {
        self.shared
            .send_control(&json!({"event": "link.unpaired"}))
            .await;
    }

    /// Stops the server.
    pub fn shutdown(self) {
        self._task.abort();
    }
}

async fn run_server(listener: TcpListener, shared: Arc<Shared>) {
    let Ok((tcp, _)) = listener.accept().await else {
        return;
    };
    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();

    // Noise XX responder handshake.
    let mut responder = NoiseXXResponder::new(OsKeySource, Vec::new());
    if responder.initialize().is_err() {
        return;
    }
    let Some(msg1) = recv_binary(&mut source).await else {
        return;
    };
    let msg2 = match responder.read_message1_and_write_message2(&msg1) {
        Ok(m) => m,
        Err(_) => return,
    };
    if sink.send(Message::Binary(msg2)).await.is_err() {
        return;
    }
    let _ = sink.flush().await;
    let Some(msg3) = recv_binary(&mut source).await else {
        return;
    };
    if responder.read_message3(&msg3).is_err() {
        return;
    }
    let (send_cipher, recv_cipher) = match responder.split() {
        Ok(p) => p,
        Err(_) => return,
    };

    // Install the real send context.
    {
        let mut guard = shared.send.lock().await;
        *guard = Some(SendCtx { sink, send_cipher });
    }

    read_loop(shared, source, recv_cipher).await;
}

async fn read_loop(shared: Arc<Shared>, mut source: SplitStream<ServerWs>, mut recv: CipherState) {
    let mut decoder = NoiseFrameDecoder::new();
    let mut control_decoder = MessageDecoder::new();
    let mut partial: HashMap<i64, ApplicationRequest> = HashMap::new();

    while let Some(msg) = source.next().await {
        let raw = match msg {
            Ok(Message::Binary(b)) => b,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };
        let plain = match recv.decrypt_with_ad(b"", &raw) {
            Ok(p) => p,
            Err(_) => break,
        };
        let reassembled = match decoder.decode(&plain) {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(_) => break,
        };
        let frame = match decode_request_envelope(&reassembled) {
            Ok(f) => f,
            Err(_) => break,
        };
        let stream_id = frame.stream_id;
        let control_id = shared.control_stream_id.load(Ordering::SeqCst);

        match frame.body {
            Some(FrameBody::Request(req)) => {
                if req.path == "/link-control" {
                    shared.control_stream_id.store(stream_id, Ordering::SeqCst);
                    // Open the control response stream (kept open).
                    open_response(&shared, stream_id).await;
                    continue;
                }
                if req.end_body {
                    handle_request(&shared, stream_id, req).await;
                } else {
                    partial.insert(stream_id, req);
                }
            }
            Some(FrameBody::BodyChunk(chunk)) => {
                if stream_id == control_id {
                    for message in control_decoder.feed(&chunk.data).unwrap_or_default() {
                        handle_control(&shared, message).await;
                    }
                } else if let Some(req) = partial.get_mut(&stream_id) {
                    req.body.extend_from_slice(&chunk.data);
                    if chunk.end_body {
                        let req = partial.remove(&stream_id).unwrap();
                        handle_request(&shared, stream_id, req).await;
                    }
                }
            }
            Some(FrameBody::Reset(_)) => {
                shared
                    .subscribe_streams
                    .lock()
                    .unwrap()
                    .retain(|id| *id != stream_id);
                partial.remove(&stream_id);
            }
            _ => {}
        }
    }
}

async fn open_response(shared: &Arc<Shared>, stream_id: i64) {
    let frame = ServiceFrame::response(
        stream_id,
        ApplicationResponse {
            status: 200,
            headers: vec![],
            body: vec![],
            end_body: false,
        },
    );
    shared.send_frame(&frame).await;
}

async fn handle_control(shared: &Arc<Shared>, message: Value) {
    let Some(obj) = message.as_object() else {
        return;
    };
    // A register request.
    if obj.get("method").and_then(Value::as_str) == Some("link.register") {
        if let Some(id) = obj.get("id").and_then(Value::as_str) {
            let node_id = obj
                .get("params")
                .and_then(|p| p.get("node_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            *shared.node_id.lock().unwrap() = node_id;
            // Acknowledge: a message carrying the register id and no method.
            shared.send_control(&json!({"id": id, "ok": true})).await;
            shared.registered.send_replace(true);
        }
        return;
    }
    // A command result.
    if obj.get("method").and_then(Value::as_str) == Some("link.result") {
        if let Some(id) = obj.get("id").and_then(Value::as_str) {
            if let Some(tx) = shared.invokes.lock().unwrap().remove(id) {
                let _ = tx.send(message.clone());
            }
        }
    }
}

async fn handle_request(shared: &Arc<Shared>, stream_id: i64, req: ApplicationRequest) {
    if req.path == "/chat/stream" {
        let body = serde_json::to_vec(&shared.script.chat_ack).unwrap_or_default();
        respond(shared, stream_id, 200, body, true).await;
        // Flush any pending events onto open subscribe streams.
        flush_events(shared).await;
        return;
    }
    if req.path == "/chat/subscribe" {
        if !shared.script.subscribe_known {
            respond(shared, stream_id, 404, Vec::new(), true).await;
            return;
        }
        // Open the stream and remember it; events are pushed after a send.
        open_response(shared, stream_id).await;
        shared.subscribe_streams.lock().unwrap().push(stream_id);
        shared
            .pending_events
            .lock()
            .unwrap()
            .extend(shared.script.subscribe_events.iter().cloned());
        flush_events(shared).await;
        return;
    }
    if req.path.starts_with("/api/voice/tts-stream") {
        // Prefer the legacy fallback path in tests.
        respond(shared, stream_id, 404, Vec::new(), true).await;
        return;
    }
    if req.path.starts_with("/voice/tts-stream") {
        respond(shared, stream_id, 200, b"ID3audio".to_vec(), true).await;
        return;
    }
    // Default: echo a small JSON object.
    let body = serde_json::to_vec(&json!({"ok": true, "path": req.path})).unwrap_or_default();
    respond(shared, stream_id, 200, body, true).await;
}

async fn flush_events(shared: &Arc<Shared>) {
    let streams: Vec<i64> = shared.subscribe_streams.lock().unwrap().clone();
    if streams.is_empty() {
        return;
    }
    let events: Vec<Value> = std::mem::take(&mut *shared.pending_events.lock().unwrap());
    for event in events {
        let mut line = serde_json::to_vec(&event).unwrap_or_default();
        line.push(b'\n');
        for &stream_id in &streams {
            let frame = ServiceFrame::body_chunk(
                stream_id,
                BodyChunk {
                    data: line.clone(),
                    end_body: false,
                },
            );
            shared.send_frame(&frame).await;
        }
    }
}

async fn respond(shared: &Arc<Shared>, stream_id: i64, status: i32, body: Vec<u8>, end: bool) {
    let frame = ServiceFrame::response(
        stream_id,
        ApplicationResponse {
            status,
            headers: vec![Header::new("content-type", "application/json")],
            body,
            end_body: end,
        },
    );
    shared.send_frame(&frame).await;
}

async fn recv_binary(source: &mut SplitStream<ServerWs>) -> Option<Vec<u8>> {
    loop {
        match source.next().await {
            Some(Ok(Message::Binary(b))) => return Some(b),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return None,
        }
    }
}
