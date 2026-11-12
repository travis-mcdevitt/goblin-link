//! Async server: listener, per-connection protocol enforcement, handler dispatch and event bus.

mod bus;

use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

pub use bus::{EventBus, Line, Sink, DEFAULT_RING};

use crate::frame::{encode_line, FrameDecoder, RequestFrame, Response};
use crate::protocol::Method;
use crate::types::{ClientKind, ErrorCode, IpcError, SubscribeResult};

/// What the server knows about the connection a request came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// Connection number, unique per daemon run.
    pub id: u64,
    /// Declared client kind ([`ClientKind::Unknown`] until `hello` succeeded).
    pub kind: ClientKind,
    /// Declared client version (empty until `hello` succeeded).
    pub version: String,
}

/// Application logic behind the socket.
///
/// `hello` is passed to the handler (it supplies `run_id` via [`EventBus::run_id`], profile and
/// capabilities); the server enforces that it comes first. `subscribe` is answered by the server
/// itself and never reaches the handler. Requests of one connection run concurrently, so
/// replies may be out of order.
pub trait Handler: Send + Sync + 'static {
    /// Handles one request and returns the JSON result or a protocol error.
    fn handle(
        &self,
        client: ClientInfo,
        method: Method,
    ) -> impl Future<Output = Result<Value, IpcError>> + Send;

    /// Called once when a connection closes (after `hello`, `client.kind` is what it declared).
    fn disconnected(&self, _client: ClientInfo) {}
}

/// Serializes a typed result for returning from [`Handler::handle`].
pub fn reply<T: serde::Serialize>(result: &T) -> Result<Value, IpcError> {
    serde_json::to_value(result).map_err(|e| IpcError::new(ErrorCode::Internal, e.to_string()))
}

/// Tunables of the server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Outgoing lines buffered per connection before a client counts as too slow.
    pub queue_capacity: usize,
    /// Concurrent requests per connection; more get `busy`.
    pub max_in_flight: usize,
    /// How long a single write to a client may take before it is disconnected.
    pub write_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 256,
            max_in_flight: 64,
            write_timeout: Duration::from_secs(10),
        }
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A bound IPC server.
pub struct Server<H> {
    listener: UnixListener,
    _guard: SocketGuard,
    handler: Arc<H>,
    bus: Arc<EventBus>,
    config: ServerConfig,
}

fn claim_socket_path(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another daemon is serving this socket",
                ));
            }
            std::fs::remove_file(path)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "path exists and is not a socket",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl<H: Handler> Server<H> {
    /// Creates the socket at `path` with mode 0600 (and its parent directory with mode 0700 when
    /// missing). A stale socket from a dead daemon is replaced; a live one is an error. Must be
    /// called inside a tokio runtime.
    pub fn bind(
        path: &Path,
        handler: Arc<H>,
        bus: Arc<EventBus>,
        config: ServerConfig,
    ) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            crate::paths::prepare_runtime_dir(parent)?;
        }
        claim_socket_path(path)?;
        let listener = UnixListener::bind(path)?;
        let guard = SocketGuard(path.to_owned());
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            _guard: guard,
            handler,
            bus,
            config,
        })
    }

    /// Accepts clients until `shutdown` completes, then closes all connections and removes the
    /// socket file.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> io::Result<()> {
        let (stop_tx, stop_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut next_id = 1u64;
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                () = &mut shutdown => break Ok(()),
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let conn = Conn {
                            id: next_id,
                            handler: self.handler.clone(),
                            bus: self.bus.clone(),
                            config: self.config.clone(),
                            stop: stop_rx.clone(),
                        };
                        next_id += 1;
                        tasks.spawn(conn.serve(stream));
                    }
                    Err(e) if is_transient(&e) => {}
                    Err(e) => break Err(e),
                },
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            }
        };
        stop_tx.send_replace(true);
        while tasks.join_next().await.is_some() {}
        result
    }
}

fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

struct Conn<H> {
    id: u64,
    handler: Arc<H>,
    bus: Arc<EventBus>,
    config: ServerConfig,
    stop: watch::Receiver<bool>,
}

impl<H: Handler> Conn<H> {
    async fn serve(mut self, stream: UnixStream) {
        let (mut rd, wr) = stream.into_split();
        let (tx, rx) = mpsc::channel::<Line>(self.config.queue_capacity.max(2));
        let (kill_tx, mut kill_rx) = watch::channel(false);
        let sink = Sink {
            tx,
            kill: Arc::new(kill_tx),
        };
        let writer = tokio::spawn(write_loop(
            wr,
            rx,
            sink.kill.subscribe(),
            sink.kill.clone(),
            self.config.write_timeout,
        ));

        let mut info = ClientInfo {
            id: self.id,
            kind: ClientKind::Unknown,
            version: String::new(),
        };
        let mut greeted = false;
        let in_flight = Arc::new(AtomicUsize::new(0));
        let mut decoder = FrameDecoder::new();
        let mut buf = vec![0u8; 16 * 1024];

        'conn: loop {
            let n = tokio::select! {
                r = rd.read(&mut buf) => match r {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                },
                _ = kill_rx.changed() => break,
                _ = self.stop.changed() => break,
            };
            decoder.push(&buf[..n]);
            loop {
                let line = match decoder.next_line() {
                    Ok(Some(l)) => l,
                    Ok(None) => break,
                    Err(_) => break 'conn,
                };
                // A frame without a usable id cannot be answered; close the connection.
                let Ok(req) = serde_json::from_slice::<RequestFrame>(&line) else {
                    break 'conn;
                };
                let id = req.id;
                let method = match req.method() {
                    Ok(m) => m,
                    Err(e) => {
                        send_response(&sink, Response::failure(id, e)).await;
                        continue;
                    }
                };
                if !greeted && !matches!(method, Method::Hello(_)) {
                    let e = IpcError::new(ErrorCode::BadRequest, "hello must be the first request");
                    send_response(&sink, Response::failure(id, e)).await;
                    continue;
                }
                match method {
                    Method::Hello(params) => {
                        if greeted {
                            let e = IpcError::new(ErrorCode::BadRequest, "already greeted");
                            send_response(&sink, Response::failure(id, e)).await;
                            continue;
                        }
                        let call = self
                            .handler
                            .handle(info.clone(), Method::Hello(params.clone()));
                        let outcome = tokio::select! {
                            r = call => r,
                            _ = kill_rx.changed() => break 'conn,
                            _ = self.stop.changed() => break 'conn,
                        };
                        if outcome.is_ok() {
                            greeted = true;
                            info.kind = params.client;
                            info.version = params.version;
                        }
                        send_response(&sink, to_response(id, outcome)).await;
                    }
                    Method::Subscribe(p) => {
                        self.bus
                            .attach(self.id, sink.clone(), p.after_seq, p.topics, |r| {
                                line_for(Response::success(id, subscribe_value(r)))
                            });
                    }
                    other => {
                        if in_flight.fetch_add(1, Ordering::AcqRel) >= self.config.max_in_flight {
                            in_flight.fetch_sub(1, Ordering::AcqRel);
                            let e = IpcError::new(ErrorCode::Busy, "too many requests in flight");
                            send_response(&sink, Response::failure(id, e)).await;
                            continue;
                        }
                        let handler = self.handler.clone();
                        let sink = sink.clone();
                        let counter = in_flight.clone();
                        let client = info.clone();
                        tokio::spawn(async move {
                            let outcome = handler.handle(client, other).await;
                            counter.fetch_sub(1, Ordering::AcqRel);
                            send_response(&sink, to_response(id, outcome)).await;
                        });
                    }
                }
            }
        }
        self.bus.detach(self.id);
        self.handler.disconnected(info.clone());
        // Wake the writer so it drops the socket; queued lines are not flushed on purpose.
        sink.kill.send_replace(true);
        let _ = writer.await;
    }
}

fn subscribe_value(r: SubscribeResult) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

fn to_response(id: u64, outcome: Result<Value, IpcError>) -> Response {
    match outcome {
        Ok(v) => Response::success(id, v),
        Err(e) => Response::failure(id, e),
    }
}

/// Encodes a response; one that is too large becomes an `internal` error reply.
fn line_for(resp: Response) -> Line {
    let id = resp.id;
    match encode_line(&resp) {
        Ok(l) => l.into(),
        Err(_) => {
            let e = IpcError::new(ErrorCode::Internal, "reply too large");
            let fallback = Response::failure(id, e);
            encode_line(&fallback).unwrap_or_default().into()
        }
    }
}

async fn send_response(sink: &Sink, resp: Response) {
    let line = line_for(resp);
    let mut kill = sink.kill.subscribe();
    tokio::select! {
        _ = sink.tx.send(line) => {}
        _ = kill.wait_for(|k| *k) => {}
    }
}

async fn write_loop(
    mut wr: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Line>,
    mut killed: watch::Receiver<bool>,
    kill: Arc<watch::Sender<bool>>,
    write_timeout: Duration,
) {
    loop {
        let line = tokio::select! {
            l = rx.recv() => match l { Some(l) => l, None => break },
            _ = killed.wait_for(|k| *k) => break,
        };
        match tokio::time::timeout(write_timeout, wr.write_all(&line)).await {
            Ok(Ok(())) => {}
            _ => {
                kill.send_replace(true);
                break;
            }
        }
    }
    let _ = wr.shutdown().await;
}
