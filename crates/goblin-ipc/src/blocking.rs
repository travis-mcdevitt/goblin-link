//! Blocking, std-only client.
//!
//! Two styles are supported. A CLI uses [`Client::request`], which blocks until the reply
//! arrives (events that arrive meanwhile are queued). A single-threaded UI loop calls
//! [`Client::send`] and then polls [`Client::try_recv`] once per frame; the socket is
//! non-blocking in that style and no threads are needed.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::frame::{encode_line, EventFrame, FrameDecoder, RequestFrame, Response, ServerFrame};
use crate::protocol::{Method, Request, PROTOCOL_VERSION};
use crate::resume::ResumeState;
use crate::types::{ClientKind, HelloParams, HelloResult, SubscribeParams, SubscribeResult, Topic};
use crate::ClientError;

/// Something received from the daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A reply to a request sent with [`Client::send`].
    Response(Response),
    /// A pushed event.
    Event(EventFrame),
}

/// A blocking connection to the daemon.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    out: Vec<u8>,
    queue: VecDeque<Incoming>,
    next_id: u64,
    hello: HelloResult,
    closed: bool,
}

impl Client {
    /// Connects and performs `hello`, waiting at most `timeout` for the reply. The socket is left
    /// in blocking mode; call [`set_nonblocking`](Self::set_nonblocking) for the polling style.
    pub fn connect(
        socket: &Path,
        kind: ClientKind,
        version: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let mut client = Self {
            stream: UnixStream::connect(socket)?,
            decoder: FrameDecoder::new(),
            out: Vec::new(),
            queue: VecDeque::new(),
            next_id: 1,
            hello: HelloResult {
                daemon_version: String::new(),
                protocol: 0,
                run_id: String::new(),
                profile: String::new(),
                capabilities: Default::default(),
            },
            closed: false,
        };
        let hello: HelloResult = client.request(
            HelloParams {
                client: kind,
                version: version.to_owned(),
            },
            timeout,
        )?;
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

    /// Switches between blocking and polling mode.
    pub fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), ClientError> {
        self.stream.set_nonblocking(nonblocking)?;
        Ok(())
    }

    /// Queues a request and tries to write it without blocking; returns its id. Remaining bytes
    /// are written by later calls to [`flush`](Self::flush) or [`try_recv`](Self::try_recv).
    pub fn send<P: Request>(&mut self, params: P) -> Result<u64, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let method: Method = params.into();
        self.out
            .extend_from_slice(&encode_line(&RequestFrame::new(id, &method)?)?);
        self.flush()?;
        Ok(id)
    }

    /// Writes as much queued output as the socket accepts without blocking. Returns `true` when
    /// everything has been written.
    pub fn flush(&mut self) -> Result<bool, ClientError> {
        while !self.out.is_empty() {
            match self.stream.write(&self.out) {
                Ok(0) => return Err(self.close()),
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.closed = true;
                    return Err(e.into());
                }
            }
        }
        Ok(true)
    }

    fn close(&mut self) -> ClientError {
        self.closed = true;
        ClientError::Closed
    }

    /// Returns the next reply or event if one is available, `None` otherwise. Intended for
    /// non-blocking mode: it flushes pending output and reads whatever the socket has.
    pub fn try_recv(&mut self) -> Result<Option<Incoming>, ClientError> {
        if let Some(item) = self.queue.pop_front() {
            return Ok(Some(item));
        }
        if self.closed {
            return Err(ClientError::Closed);
        }
        self.flush()?;
        self.fill(None)?;
        Ok(self.queue.pop_front())
    }

    /// Reads from the socket and decodes frames into the queue. With a deadline the socket must be
    /// in blocking mode; the call waits for at most one read.
    fn fill(&mut self, deadline: Option<Instant>) -> Result<(), ClientError> {
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ClientError::Timeout);
            }
            self.stream.set_read_timeout(Some(left))?;
        }
        let mut buf = [0u8; 16 * 1024];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(self.close()),
                Ok(n) => {
                    self.decoder.push(&buf[..n]);
                    self.decode_pending()?;
                    // A blocking read returns after one chunk; polling drains the socket.
                    if deadline.is_some() || n < buf.len() {
                        return Ok(());
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return if deadline.is_some() {
                        Err(ClientError::Timeout)
                    } else {
                        Ok(())
                    };
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.closed = true;
                    return Err(e.into());
                }
            }
        }
    }

    fn decode_pending(&mut self) -> Result<(), ClientError> {
        loop {
            let line = match self.decoder.next_line() {
                Ok(Some(l)) => l,
                Ok(None) => return Ok(()),
                Err(e) => {
                    self.closed = true;
                    return Err(e.into());
                }
            };
            match ServerFrame::decode(&line) {
                Ok(ServerFrame::Response(r)) => self.queue.push_back(Incoming::Response(r)),
                Ok(ServerFrame::Event(e)) => self.queue.push_back(Incoming::Event(e)),
                Err(e) => {
                    self.closed = true;
                    return Err(e.into());
                }
            }
        }
    }

    /// Sends a request and blocks until its reply arrives or `timeout` passes. Events and replies
    /// to other requests that arrive meanwhile are kept for [`try_recv`](Self::try_recv) /
    /// [`next_event`](Self::next_event). The socket is switched to blocking mode for the call and
    /// restored afterwards.
    pub fn request<P: Request>(
        &mut self,
        params: P,
        timeout: Duration,
    ) -> Result<P::Result, ClientError> {
        let deadline = Instant::now() + timeout;
        self.stream.set_nonblocking(false)?;
        let result = self.request_blocking(params, deadline);
        // Leave the socket blocking with no read timeout; callers who poll switch it themselves.
        let _ = self.stream.set_read_timeout(None);
        result
    }

    fn request_blocking<P: Request>(
        &mut self,
        params: P,
        deadline: Instant,
    ) -> Result<P::Result, ClientError> {
        let id = self.send_blocking(params, deadline)?;
        loop {
            if let Some(pos) = self
                .queue
                .iter()
                .position(|i| matches!(i, Incoming::Response(r) if r.id == id))
            {
                if let Some(Incoming::Response(r)) = self.queue.remove(pos) {
                    return Ok(serde_json::from_value(r.into_value()?)?);
                }
            }
            if self.closed {
                return Err(ClientError::Closed);
            }
            self.fill(Some(deadline))?;
        }
    }

    fn send_blocking<P: Request>(
        &mut self,
        params: P,
        deadline: Instant,
    ) -> Result<u64, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let method: Method = params.into();
        self.out
            .extend_from_slice(&encode_line(&RequestFrame::new(id, &method)?)?);
        let left = deadline.saturating_duration_since(Instant::now());
        self.stream
            .set_write_timeout(Some(left.max(Duration::from_millis(1))))?;
        let written = self.stream.write_all(&self.out);
        self.out.clear();
        match written {
            Ok(()) => Ok(id),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Err(ClientError::Timeout)
            }
            Err(e) => {
                self.closed = true;
                Err(e.into())
            }
        }
    }

    /// Starts event delivery; see the async client's `subscribe`.
    pub fn subscribe(
        &mut self,
        params: SubscribeParams,
        timeout: Duration,
    ) -> Result<SubscribeResult, ClientError> {
        self.request(params, timeout)
    }

    /// Subscribes resuming from `resume` (see [`ResumeState`]).
    pub fn subscribe_resuming(
        &mut self,
        resume: &ResumeState,
        topics: Option<Vec<Topic>>,
        timeout: Duration,
    ) -> Result<SubscribeResult, ClientError> {
        let after_seq = resume.after_seq_for(&self.hello.run_id);
        self.subscribe(SubscribeParams { after_seq, topics }, timeout)
    }

    /// Blocks up to `timeout` for the next event; `Ok(None)` on timeout. Replies that arrive
    /// meanwhile stay queued for [`try_recv`](Self::try_recv).
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<EventFrame>, ClientError> {
        let deadline = Instant::now() + timeout;
        self.stream.set_nonblocking(false)?;
        let outcome = loop {
            let pos = self
                .queue
                .iter()
                .position(|i| matches!(i, Incoming::Event(_)));
            if let Some(Incoming::Event(e)) = pos.and_then(|p| self.queue.remove(p)) {
                break Ok(Some(e));
            }
            if self.closed {
                break Err(ClientError::Closed);
            }
            match self.fill(Some(deadline)) {
                Ok(()) => {}
                Err(ClientError::Timeout) => break Ok(None),
                Err(e) => break Err(e),
            }
        };
        let _ = self.stream.set_read_timeout(None);
        outcome
    }
}
