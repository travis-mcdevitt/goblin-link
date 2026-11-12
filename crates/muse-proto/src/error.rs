//! Error types. Display strings match the Python reference's exception
//! messages wherever the same condition exists there, so logs and test
//! vectors line up.

use thiserror::Error;

/// Malformed protobuf-style data (the reference's `ProtoError`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WireError {
    /// The input ended inside a varint.
    #[error("truncated varint")]
    TruncatedVarint,
    /// A varint longer than 10 bytes, or one whose value exceeds 64 bits.
    #[error("malformed varint")]
    MalformedVarint,
    /// Field number 0, above `2^29-1`, or in the reserved range.
    #[error("invalid field number")]
    InvalidFieldNumber,
    /// Wire type 3, 4, 6 or 7.
    #[error("invalid wire type")]
    InvalidWireType,
    /// A length-delimited field runs past the end of the input.
    #[error("truncated delimited field")]
    TruncatedDelimited,
    /// A fixed64 field runs past the end of the input.
    #[error("truncated fixed64 field")]
    TruncatedFixed64,
    /// A fixed32 field runs past the end of the input.
    #[error("truncated fixed32 field")]
    TruncatedFixed32,
    /// An `int32` field carries a value outside `i32`.
    #[error("int32 value out of range")]
    Int32OutOfRange,
    /// A `uint32` field carries a value above `u32::MAX`.
    #[error("uint32 value out of range")]
    Uint32OutOfRange,
    /// A known field arrived with the wrong wire type; names the field.
    #[error("{0} wrong wire type")]
    WrongWireType(&'static str),
    /// A string field is not valid UTF-8.
    #[error("invalid utf-8 string")]
    InvalidUtf8,
    /// `ServiceRequest.service` is not a known [`crate::ServiceType`].
    #[error("unknown service type")]
    UnknownServiceType,
    /// `Reset.code` is not a known [`crate::ResetCode`].
    #[error("unknown reset code")]
    UnknownResetCode,
}

/// Handshake or cipher state machine violation (the reference's
/// `NoiseProtocolError`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NoiseError {
    /// A cipher operation failed earlier; the state is unusable.
    #[error("CipherState: poisoned after prior failure")]
    CipherPoisoned,
    /// The key passed to [`crate::noise::CipherState::initialize_key`] is not 32 bytes.
    #[error("CipherState: AES-GCM key must be 32 bytes")]
    BadKeyLength,
    /// The nonce reached `2^53 - 1`.
    #[error("CipherState: nonce exhausted")]
    NonceExhausted,
    /// Authentication failed (wrong key, tampered data or wrong order).
    #[error("CipherState: decrypt failed")]
    DecryptFailed,
    /// AES-GCM refused to encrypt (only possible for absurd lengths).
    #[error("CipherState: encrypt failed")]
    EncryptFailed,
    /// A handshake method was called out of order.
    #[error("NoiseXX: {method} called in wrong phase (expected {expected}, got {actual})")]
    WrongPhase {
        /// Method name, as in the reference.
        method: &'static str,
        /// Phase number the method requires.
        expected: u8,
        /// Phase number the handshake is in.
        actual: u8,
    },
    /// A handshake method was called after a failure or `destroy`.
    #[error("NoiseXX: {method} called on dead handshake")]
    DeadHandshake {
        /// Method name, as in the reference.
        method: &'static str,
    },
    /// Handshake message 1 is shorter than 32 bytes.
    #[error("NoiseXX: message 1 too short ({0} < 32)")]
    Message1TooShort(usize),
    /// Handshake message 2 is shorter than 96 bytes.
    #[error("NoiseXX: message 2 too short ({0} < 96)")]
    Message2TooShort(usize),
    /// Handshake message 3 is shorter than 64 bytes.
    #[error("NoiseXX: message 3 too short ({0} < 64)")]
    Message3TooShort(usize),
    /// A public key that is not 32 bytes.
    #[error("x25519: invalid public key length")]
    InvalidPublicKeyLength,
    /// The peer sent one of the known low-order points.
    #[error("x25519: rejected low-order public key")]
    LowOrderPublicKey,
    /// The Diffie-Hellman result was all zeros.
    #[error("x25519: DH produced all-zeros output")]
    ZeroSharedSecret,
    /// Internal invariant: a key the phase guarantees is missing.
    #[error("NoiseXX: missing {0} key")]
    MissingKey(&'static str),
}

/// Noise transport frame fragmentation and reassembly errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FramingError {
    /// The frame did not parse.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The decoder failed before and refuses further input.
    #[error("NoiseFrameDecoder: poisoned after prior failure")]
    Poisoned,
    /// `totalChunks` outside `1..=256`.
    #[error("invalid totalChunks: {0}")]
    InvalidTotalChunks(u32),
    /// `chunkIndex` not below `totalChunks`.
    #[error("chunkIndex {index} out of range [0, {total})")]
    ChunkIndexOutOfRange {
        /// Received index.
        index: u32,
        /// Declared total.
        total: u32,
    },
    /// One chunk's payload is larger than [`crate::framing::MAX_CHUNK_PAYLOAD`].
    #[error("payload too large for noise frame ({0} bytes)")]
    ChunkPayloadTooLarge(usize),
    /// More than [`crate::framing::MAX_PENDING_ASSEMBLIES`] messages in flight.
    #[error("too many pending noise frame assemblies")]
    TooManyPending,
    /// A chunk disagrees with earlier chunks about `totalChunks`.
    #[error("inconsistent totalChunks for chunkId: expected {expected}, got {got}")]
    InconsistentTotal {
        /// Total from the first chunk.
        expected: u32,
        /// Total from this chunk.
        got: u32,
    },
    /// The same chunk index arrived twice.
    #[error("duplicate chunkIndex {0}")]
    DuplicateChunkIndex(u32),
    /// The reassembled message exceeded [`crate::framing::MAX_ASSEMBLY_BYTES`].
    #[error("assembly exceeded byte budget")]
    AssemblyTooLarge,
    /// An outgoing message needs more than 256 chunks.
    #[error("payload too large for noise framing ({len} bytes, {chunks} chunks > 256)")]
    MessageTooLarge {
        /// Message length.
        len: usize,
        /// Chunks it would need.
        chunks: usize,
    },
}

/// Errors from [`crate::transport::NoiseTransport`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportError {
    /// The transport failed before; its cipher state must not be reused.
    #[error("NoiseTransport: dead after prior failure")]
    Dead,
    /// Encryption or decryption failed.
    #[error(transparent)]
    Noise(#[from] NoiseError),
    /// Fragmentation or reassembly failed.
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// The reassembled envelope did not parse.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The server sent a `ServiceResponse` with no payload.
    #[error("empty ServiceResponse payload")]
    EmptyPayload,
    /// The server sent a request frame, which the client never accepts.
    #[error("NoiseTransport: unexpected request frame from server")]
    UnexpectedRequest,
    /// A service name that is not `daemon`, `sentinel`, `vault` or `authd`.
    #[error("unknown noise service: {0}")]
    UnknownService(String),
    /// The stream id counter reached `i64::MAX`.
    #[error("NoiseTransport: stream ids exhausted")]
    StreamIdsExhausted,
}

/// Errors from the length-prefixed JSON control stream.
#[derive(Debug, Error)]
pub enum ControlError {
    /// An inbound length prefix above [`crate::control::MAX_INBOUND_MESSAGE`].
    /// The stream cannot be resynchronised; close it.
    #[error("inbound message too large: {0}")]
    InboundTooLarge(u32),
    /// The value could not be serialised as JSON.
    #[error("cannot encode control message: {0}")]
    Encode(#[from] serde_json::Error),
    /// The encoded message does not fit a `u32` length prefix.
    #[error("control message too large to encode ({0} bytes)")]
    OutboundTooLarge(usize),
}

/// Errors from BLE chunk encoding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BleError {
    /// The message needs more than 255 chunks at this MTU.
    #[error("message needs {0} chunks (max 255)")]
    TooManyChunks(usize),
    /// An ATT MTU of 4 to 6 leaves no room for a chunk payload.
    #[error("ATT MTU {0} leaves no room for a chunk payload")]
    MtuTooSmall(u16),
}
