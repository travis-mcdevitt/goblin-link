//! Typed errors for the client.
//!
//! Errors distinguish a rejected credential (needs re-pair or a VM re-fetch)
//! from a transient transport failure (worth retrying). No error value ever
//! contains a token, an Authorization header or message text.

use muse_proto::{ControlError, FramingError, NoiseError, TransportError};

/// An error talking to the Muse device API (`/fetch_vms`, `/device_token/refresh`).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The server answered with an HTTP status that ends the attempt. 401 means
    /// the device token or pairing was rejected and a retry will not fix it.
    #[error("device API returned HTTP {0}")]
    Http(u16),
    /// The response body was not the JSON shape the API documents.
    #[error("device API returned an unexpected response")]
    BadResponse,
    /// A transport-level failure (DNS, TLS, connection, timeout). Worth a retry.
    #[error("device API transport failure: {0}")]
    Transport(String),
    /// A request could not be built (bad URL, bad header value).
    #[error("device API request is invalid: {0}")]
    Request(String),
}

impl ApiError {
    /// Whether the error is the server rejecting our credential (HTTP 401).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ApiError::Http(401))
    }

    /// Whether a retry with the same inputs might succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, ApiError::Transport(_))
    }

    /// The HTTP status, when the attempt ended with one.
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Http(s) => Some(*s),
            _ => None,
        }
    }
}

/// An error on a live Noise control session.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The WebSocket upgrade was refused (401 re-fetch VMs, 403 forbidden).
    #[error("VM refused the connection: HTTP {0}")]
    UpgradeRejected(u16),
    /// The TLS or WebSocket connection could not be established.
    #[error("could not connect to the VM: {0}")]
    Connect(String),
    /// The Noise handshake failed.
    #[error("Noise handshake failed")]
    Handshake(#[from] NoiseError),
    /// The transport poisoned; the connection must be dropped and remade.
    #[error("transport failed")]
    Transport(#[from] TransportError),
    /// A framing error on the wire.
    #[error("framing failed")]
    Framing(#[from] FramingError),
    /// A control-message codec error.
    #[error("control message codec failed")]
    Control(#[from] ControlError),
    /// The session is not connected (closed, or never opened).
    #[error("the session is not connected")]
    NotConnected,
    /// A request exceeded a size, time or buffer bound.
    #[error("{0}")]
    Bounded(&'static str),
    /// A request timed out waiting for a reply.
    #[error("the request timed out")]
    Timeout,
    /// Muse refused an HTTP request carried by the session (non-2xx status).
    #[error("Muse request to {path} failed: HTTP {status}")]
    HttpStatus {
        /// The HTTP status.
        status: u16,
        /// The request path (never any query credential).
        path: String,
    },
    /// A caller argument was invalid (bad image, oversized note, bad app id).
    #[error("{0}")]
    Invalid(String),
    /// The session ended underneath an in-flight request.
    #[error("the session ended")]
    SessionEnded,
}

impl LinkError {
    /// The HTTP status for an `HttpStatus` error, if that is the variant.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            LinkError::HttpStatus { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Result alias for session operations.
pub type LinkResult<T> = Result<T, LinkError>;
