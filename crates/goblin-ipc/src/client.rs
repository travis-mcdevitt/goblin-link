//! Async (tokio) client.
//!
//! [`Client::connect`] opens the socket and performs `hello`. Requests take `&self`, so many can
//! be in flight at once (join the futures); replies are matched to requests by id even when they
//! arrive out of order. Events are consumed with [`Client::next_event`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::frame::{encode_line, EventFrame, FrameDecoder, RequestFrame, Response, ServerFrame};
use crate::protocol::{Method, Request, PROTOCOL_VERSION};
use crate::resume::ResumeState;
use crate::types::{ClientKind, HelloParams, HelloResult, SubscribeParams, SubscribeResult, Topic};
use crate::ClientError;

/// Connection settings.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Declared client kind.
    pub kind: ClientKind,
    /// Declared client version.
    pub version: String,
    /// Default time to wait for a reply.
    pub timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            kind: ClientKind::Ctl,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            timeout: Duration::from_secs(10),
        }
    }
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Response>>>;

struct Shared {
    next_id: AtomicU64,
    pending: Pending,
    closed: AtomicBool,
}

impl Shared {
    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Response>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An async connection to the daemon.
pub struct Client {
    shared: Arc<Shared>,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<EventFrame>>,
    reader: JoinHandle<()>,
    hello: HelloResult,
    timeout: Duration,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Client {
    /// Connects to `socket` and performs `hello`.
    pub async fn connect(socket: &Path, options: ClientOptions) -> Result<Self, ClientError> {
        let stream = tokio::time::timeout(options.timeout, UnixStream::connect(socket))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let (rd, wr) = stream.into_split();
        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(rd, shared.clone(), events_tx));
        let mut client = Self {
            shared,
            writer: tokio::sync::Mutex::new(wr),
            events: tokio::sync::Mutex::new(events_rx),
            reader,
            hello: HelloResult {
                daemon_version: String::new(),
                protocol: 0,
                run_id: String::new(),
                profile: String::new(),
                capabilities: Default::default(),
            },
            timeout: options.timeout,
        };
        let hello: HelloResult = client
            .request(HelloParams {
                client: options.kind,
                version: options.version,
            })
            .await?;
        if hello.protocol != PROTOCOL_VERSION {
            return Err(ClientError::Protocol(hello.protocol));
        }
        client.hello = hello;
        Ok(client)
    }

    /// The daemon's answer to `hello`.
    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// Whether the connection has been closed (by either side).
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// Sends a typed request and waits for its typed result using the default timeout.
    pub async fn request<P: Request>(&self, params: P) -> Result<P::Result, ClientError> {
        self.request_timeout(params, self.timeout).await
    }

    /// Like [`request`](Self::request) with an explicit timeout. A timeout does not cancel the
    /// request on the daemon; treat it as "outcome unknown".
    pub async fn request_timeout<P: Request>(
        &self,
        params: P,
        timeout: Duration,
    ) -> Result<P::Result, ClientError> {
        let value = self.call(params.into(), timeout).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Sends an untyped method and returns the raw result.
    pub async fn call(
        &self,
        method: Method,
        timeout: Duration,
    ) -> Result<serde_json::Value, ClientError> {
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let line = encode_line(&RequestFrame::new(id, &method)?)?;
        let (tx, rx) = oneshot::channel();
        self.shared.pending().insert(id, tx);
        let outcome = self.send_and_wait(&line, rx, timeout).await;
        if outcome.is_err() {
            self.shared.pending().remove(&id);
        }
        Ok(outcome?.into_value()?)
    }

    async fn send_and_wait(
        &self,
        line: &[u8],
        rx: oneshot::Receiver<Response>,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        {
            let mut writer = self.writer.lock().await;
            tokio::time::timeout_at(deadline, writer.write_all(line))
                .await
                .map_err(|_| ClientError::Timeout)??;
        }
        match tokio::time::timeout_at(deadline, rx).await {
            Err(_) => Err(ClientError::Timeout),
            Ok(Err(_)) => Err(ClientError::Closed),
            Ok(Ok(resp)) => Ok(resp),
        }
    }

    /// Starts (or restarts, replacing the filters of) event delivery. Events then arrive through
    /// [`next_event`](Self::next_event). If the result says `gap`, re-fetch state with the
    /// `*.get`/`*.list` methods.
    pub async fn subscribe(&self, params: SubscribeParams) -> Result<SubscribeResult, ClientError> {
        self.request(params).await
    }

    /// Subscribes resuming from `resume`: `after_seq` is sent only when the daemon's `run_id` is
    /// the one `resume` last saw. Feed every received event to [`ResumeState::observe`] with
    /// [`hello`](Self::hello)`.run_id`.
    pub async fn subscribe_resuming(
        &self,
        resume: &ResumeState,
        topics: Option<Vec<Topic>>,
    ) -> Result<SubscribeResult, ClientError> {
        self.subscribe(SubscribeParams {
            after_seq: resume.after_seq_for(&self.hello.run_id),
            topics,
        })
        .await
    }

    /// Waits for the next event. Returns `None` once the connection is closed and all received
    /// events were consumed.
    pub async fn next_event(&self) -> Option<EventFrame> {
        self.events.lock().await.recv().await
    }
}

async fn read_loop(
    mut rd: tokio::net::unix::OwnedReadHalf,
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<EventFrame>,
) {
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 16 * 1024];
    'outer: loop {
        match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => decoder.push(&buf[..n]),
        }
        loop {
            let line = match decoder.next_line() {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(_) => break 'outer,
            };
            match ServerFrame::decode(&line) {
                Ok(ServerFrame::Response(r)) => {
                    let waiter = shared.pending().remove(&r.id);
                    if let Some(tx) = waiter {
                        let _ = tx.send(r);
                    }
                }
                Ok(ServerFrame::Event(e)) => {
                    let _ = events.send(e);
                }
                // Unparseable frame: the stream cannot be trusted any more.
                Err(_) => break 'outer,
            }
        }
    }
    shared.closed.store(true, Ordering::Release);
    shared.pending().clear();
}
