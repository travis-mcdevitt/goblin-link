//! Wire frames and the newline-delimited JSON codec.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{Event, Method};
use crate::types::{ErrorCode, IpcError};

/// Maximum length of one line in bytes, excluding the terminating newline (1 MiB).
pub const MAX_LINE: usize = 1 << 20;

/// Frame-level failures. All of them are fatal for the connection.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// A line exceeded [`MAX_LINE`] bytes.
    #[error("frame exceeds the {MAX_LINE} byte line limit")]
    TooLong,
    /// A line was not valid JSON for the expected frame shape.
    #[error("invalid frame: {0}")]
    Json(#[from] serde_json::Error),
    /// The JSON was valid but matched none of the three frame shapes.
    #[error("unrecognized frame shape")]
    Shape,
}

fn is_null(v: &Value) -> bool {
    v.is_null()
}

/// Client to daemon request: `{"id","method","params"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)] // fields are the wire keys named in the doc comment
pub struct RequestFrame {
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "is_null")]
    pub params: Value,
}

impl RequestFrame {
    /// Builds the frame for a typed method.
    pub fn new(id: u64, method: &Method) -> Result<Self, serde_json::Error> {
        Ok(Self {
            id,
            method: method.name().to_owned(),
            params: method.params_value()?,
        })
    }

    /// Parses the method name and params into a typed [`Method`].
    pub fn method(&self) -> Result<Method, IpcError> {
        Method::from_parts(&self.method, self.params.clone())
    }
}

/// Daemon to client reply: `{"id","ok":true,"result"}` or `{"id","ok":false,"error"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)] // fields are the wire keys named in the doc comment
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

impl Response {
    /// A success reply.
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// A failure reply.
    pub fn failure(id: u64, error: IpcError) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Converts the reply into its result value or error. A malformed reply (`ok` without a
    /// result, failure without an error) becomes an `internal` error.
    pub fn into_value(self) -> Result<Value, IpcError> {
        if self.ok {
            self.result
                .ok_or_else(|| IpcError::new(ErrorCode::Internal, "reply without result"))
        } else {
            Err(self
                .error
                .unwrap_or_else(|| IpcError::new(ErrorCode::Internal, "reply without error")))
        }
    }
}

/// Daemon to client event: `{"seq","event","data"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)] // fields are the wire keys named in the doc comment
pub struct EventFrame {
    pub seq: u64,
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

impl EventFrame {
    /// Builds the frame for a typed event.
    pub fn new(seq: u64, event: &Event) -> Result<Self, serde_json::Error> {
        Ok(Self {
            seq,
            event: event.name().to_owned(),
            data: event.data_value()?,
        })
    }

    /// Parses the payload into a typed [`Event`]; unknown event names become `Event::Unknown`.
    pub fn typed(&self) -> Result<Event, serde_json::Error> {
        Event::from_parts(&self.event, self.data.clone())
    }
}

/// Anything the daemon can send: a reply or an event.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerFrame {
    /// A reply to a request.
    Response(Response),
    /// A pushed event.
    Event(EventFrame),
}

impl ServerFrame {
    /// Decodes one line (without its newline).
    pub fn decode(line: &[u8]) -> Result<Self, FrameError> {
        let value: Value = serde_json::from_slice(line)?;
        let obj = value.as_object().ok_or(FrameError::Shape)?;
        if obj.contains_key("event") {
            Ok(Self::Event(serde_json::from_value(value)?))
        } else if obj.contains_key("ok") {
            Ok(Self::Response(serde_json::from_value(value)?))
        } else {
            Err(FrameError::Shape)
        }
    }
}

/// Serializes `frame` as one line terminated by `\n`. Fails with [`FrameError::TooLong`] when the
/// peer would reject the line.
pub fn encode_line<T: Serialize>(frame: &T) -> Result<Vec<u8>, FrameError> {
    let mut bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_LINE {
        return Err(FrameError::TooLong);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Incremental line decoder for arbitrary byte chunks.
///
/// Push whatever the socket returned, then call [`next_line`](Self::next_line) until it yields
/// `None`. Blank lines are skipped. Once a line is too long the decoder stays failed, because the
/// protocol requires closing the connection.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    start: usize,
    scanned: usize,
    failed: bool,
}

impl FrameDecoder {
    /// An empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, chunk: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
            self.scanned = 0;
        } else if self.start > MAX_LINE {
            self.buf.drain(..self.start);
            self.scanned -= self.start;
            self.start = 0;
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Returns the next complete line without its newline, `None` when more bytes are needed, or
    /// [`FrameError::TooLong`] when a line exceeds [`MAX_LINE`].
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if self.failed {
            return Err(FrameError::TooLong);
        }
        loop {
            let from = self.scanned.max(self.start);
            match self.buf[from..].iter().position(|&b| b == b'\n') {
                Some(off) => {
                    let end = from + off;
                    let len = end - self.start;
                    let line_start = self.start;
                    self.start = end + 1;
                    self.scanned = self.start;
                    if len > MAX_LINE {
                        self.failed = true;
                        return Err(FrameError::TooLong);
                    }
                    if len == 0 {
                        continue;
                    }
                    return Ok(Some(self.buf[line_start..end].to_vec()));
                }
                None => {
                    self.scanned = self.buf.len();
                    if self.buf.len() - self.start > MAX_LINE {
                        self.failed = true;
                        return Err(FrameError::TooLong);
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// Number of buffered bytes that do not yet form a complete line.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }
}
