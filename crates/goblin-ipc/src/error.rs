//! Errors of the client implementations.

use crate::frame::FrameError;
use crate::types::IpcError;

/// Failure of a client operation.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Socket I/O failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The daemon sent something that violates the framing rules.
    #[error("protocol error: {0}")]
    Frame(#[from] FrameError),
    /// A reply or event payload did not match its documented type.
    #[error("bad payload: {0}")]
    Payload(#[from] serde_json::Error),
    /// The daemon answered with an error.
    #[error("daemon error: {0}")]
    Daemon(#[from] IpcError),
    /// No reply arrived in time. The request may still have been executed.
    #[error("timed out waiting for a reply")]
    Timeout,
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
    /// The daemon speaks a different protocol version.
    #[error("unsupported protocol version {0}")]
    Protocol(u32),
}
