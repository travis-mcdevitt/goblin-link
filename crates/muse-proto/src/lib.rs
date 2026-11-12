//! Wire formats for the Goblin Link protocol: the Noise XX handshake, the
//! transport framing and service envelopes that ride over it, the
//! length-prefixed JSON control messages, and the BLE pairing chunk framing.
//!
//! This crate is a faithful Rust port of the wire layer of the Python Muse
//! Gadget SDK (`gadgets/trimui-brick/pak/lib/musegadget`, from
//! `facebookincubator/muse-gadget-sdk`, Apache-2.0; see the `NOTICE` file). It
//! is pure: no I/O, no async, no sockets. An async client (`muse-link`) drives
//! it over a WebSocket; `muse-pair` drives the BLE framing over BlueZ.
//!
//! # Layers
//!
//! * [`noise`] — the `Noise_XX_25519_AESGCM_SHA256` handshake and the
//!   post-handshake [`noise::CipherState`] cipher.
//! * [`framing`] — fragmentation of plaintext into Noise transport frames and
//!   their reassembly ([`framing::NoiseFrameDecoder`]).
//! * [`envelope`] — the service envelopes and `ServiceFrame` oneof carried in
//!   each frame.
//! * [`transport`] — [`transport::NoiseTransport`], which ties the ciphers,
//!   framing and envelopes together into a multiplexed request/response
//!   transport.
//! * [`control`] — the length-prefixed JSON messages of the `/link-control`
//!   stream ([`control::encode_message`], [`control::MessageDecoder`]).
//! * [`ble`] — the GATT chunk framing used during pairing.
//! * [`wire`] — the shared protobuf-style field codec the envelopes use.
//!
//! # Safety properties
//!
//! No function panics on bytes taken from the network: decoders return typed
//! errors ([`error`]) and never index or slice unchecked. Cipher state is
//! poisoned on any failure so a half-advanced nonce is never reused, and key
//! material is zeroized on drop where the underlying types allow it. No key,
//! shared secret or token is ever logged or placed in an error value.
//!
//! See `docs/protocol-notes.md` for a prose description of the layers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ble;
pub mod control;
pub mod envelope;
pub mod error;
pub mod framing;
pub mod noise;
pub mod transport;
pub mod wire;

pub use envelope::{
    ApplicationRequest, ApplicationResponse, BodyChunk, FrameBody, Header, Reset, ResetCode,
    ServiceFrame, ServiceRequest, ServiceResponse, ServiceType,
};
pub use error::{BleError, ControlError, FramingError, NoiseError, TransportError, WireError};
pub use transport::{DecryptedFrame, EncryptedFrames, NoiseTransport};
