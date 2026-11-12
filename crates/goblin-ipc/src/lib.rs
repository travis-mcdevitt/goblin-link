//! Goblin Link daemon/client IPC: protocol types, codec, and (behind features) clients and server.
//!
//! The contract is `docs/ipc.md`. Features: `client` (tokio async client), `blocking` (std-only
//! client with `try_recv`), `server` (tokio listener, handler trait and event bus).
#![warn(missing_docs)]

pub mod examples;
pub mod frame;
pub mod paths;
pub mod protocol;
pub mod resume;
pub mod types;

#[cfg(test)]
mod frame_tests;

#[cfg(any(feature = "client", feature = "blocking"))]
mod error;
#[cfg(any(feature = "client", feature = "blocking"))]
pub use error::ClientError;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "blocking")]
pub mod blocking;

#[cfg(feature = "server")]
pub mod server;

pub use frame::{EventFrame, FrameDecoder, FrameError, RequestFrame, Response, ServerFrame};
pub use protocol::{Event, Method, Request, PROTOCOL_VERSION};
pub use resume::ResumeState;
pub use types::{ErrorCode, IpcError, Topic};
