//! Async client for a Muse VM, built on [`muse_proto`].
//!
//! * [`api`] — device-token rotation and leased-VM lookup over HTTPS.
//! * [`session`] — [`LinkSession`], one Noise control session: register,
//!   invoke dispatch to a [`CommandRunner`], chat send, chat subscription,
//!   generic requests and TTS streaming.
//! * [`turn`] — [`MuseTurn`], following one request through the chat event
//!   stream (a port of `muse_turn.py`).
//! * [`tls`] — the shared rustls config (Mozilla roots minus the reference's
//!   distrusted roots).
//!
//! The crate owns the WebSocket, the clock and the RNG; [`muse_proto`] owns the
//! bytes. It never logs tokens, Authorization headers or message bodies.
//!
//! With the `testsupport` feature, [`testsupport::SimVm`] provides a simulated
//! VM so the whole client is testable with no real network.
#![warn(missing_docs)]

pub mod api;
pub mod error;
mod http;
pub mod session;
pub mod tls;
pub mod turn;

#[cfg(feature = "testsupport")]
pub mod testsupport;

pub use api::{api_root, fetch_vms, refresh_device_token, user_agent, Tokens, Vm};
pub use error::{ApiError, LinkError, LinkResult};
pub use muse_proto::Header;
pub use session::{
    random_uuid, ChatSubscription, CommandRunner, DeviceDescription, DuplexStream, HttpStream,
    LinkSession, Outcome, SessionConfig, SideChat, IMAGE_MIME_TYPES, MAX_SEND_BYTES,
};
pub use turn::{
    acknowledged_message_ids, transcript_text, AgentStatus, IdKind, MuseTurn, ReplayScope, Reply,
    TaskFinished, TurnError, TurnEvent, DETAIL_REPLY_QUIET_S, EMPTY_REPLY_GRACE_S, REPLY_QUIET_S,
};
