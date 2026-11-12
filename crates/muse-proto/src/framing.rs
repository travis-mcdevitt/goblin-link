//! Noise transport framing: fragmentation and reassembly (port of
//! `noise/framing.py`).
//!
//! Each Noise ciphertext carries one encoded [`NoiseTransportFrame`]. A
//! plaintext message (an encoded `ServiceRequest` or `ServiceResponse`) is
//! cut into at most [`MAX_TOTAL_CHUNKS`] chunks of at most
//! [`MAX_CHUNK_PAYLOAD`] bytes that share a random 64-bit `chunk_id`. With
//! these limits an encrypted frame never exceeds 65,535 bytes (the Noise
//! message limit).
//!
//! The decoder enforces the reference's bounds: at most
//! [`MAX_PENDING_ASSEMBLIES`] partially received messages, at most
//! [`MAX_ASSEMBLY_BYTES`] per message, and partial messages older than
//! [`ASSEMBLY_TTL`] (measured from their first chunk) are dropped. Chunks may
//! arrive in any order. Any error poisons the decoder.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rand_core::RngCore;

use crate::error::{FramingError, WireError};
use crate::wire::{
    decode_int64, decode_uint32, delimited_field, int64_field, uint32_field, FieldCursor,
};

/// Largest payload in one chunk.
pub const MAX_CHUNK_PAYLOAD: usize = 65489;
/// Most partially received messages the decoder holds at once.
pub const MAX_PENDING_ASSEMBLIES: usize = 16;
/// Most chunks one message may be split into.
pub const MAX_TOTAL_CHUNKS: u32 = 256;
/// Most bytes one reassembled message may hold.
///
/// Note: `MAX_TOTAL_CHUNKS * MAX_CHUNK_PAYLOAD` is 16,765,184, just below
/// this budget, so with valid chunks the check can never fire. It is kept
/// because the reference has it.
pub const MAX_ASSEMBLY_BYTES: usize = 16 * 1024 * 1024;
/// Age after which a partial message is discarded.
pub const ASSEMBLY_TTL: Duration = Duration::from_secs(60);

/// One chunk of a framed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoiseTransportFrame {
    /// Shared by all chunks of one message.
    pub chunk_id: i64,
    /// Position of this chunk, `0..total_chunks`.
    pub chunk_index: u32,
    /// Number of chunks in the message. Decodes as 1 when absent.
    pub total_chunks: u32,
    /// Chunk bytes.
    pub payload: Vec<u8>,
}

impl Default for NoiseTransportFrame {
    fn default() -> Self {
        Self {
            chunk_id: 0,
            chunk_index: 0,
            total_chunks: 1,
            payload: Vec::new(),
        }
    }
}

/// Encodes one frame. Zero-valued fields are omitted, including a
/// `total_chunks` of 0 (which then decodes as 1).
pub fn encode_noise_frame(frame: &NoiseTransportFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.payload.len() + 32);
    if frame.chunk_id != 0 {
        int64_field(1, frame.chunk_id, &mut out);
    }
    if frame.chunk_index != 0 {
        uint32_field(2, frame.chunk_index, &mut out);
    }
    if frame.total_chunks != 0 {
        uint32_field(3, frame.total_chunks, &mut out);
    }
    if !frame.payload.is_empty() {
        delimited_field(4, &frame.payload, &mut out);
    }
    out
}

/// Decodes one frame without range checks (see [`NoiseFrameDecoder`]).
pub fn decode_noise_frame(data: &[u8]) -> Result<NoiseTransportFrame, WireError> {
    let mut frame = NoiseTransportFrame::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                frame.chunk_id =
                    decode_int64(cur.take_varint(wire, "NoiseTransportFrame.chunk_id")?);
            }
            2 => {
                let raw = cur.take_varint(wire, "NoiseTransportFrame.chunk_index")?;
                frame.chunk_index = decode_uint32(raw)?;
            }
            3 => {
                let raw = cur.take_varint(wire, "NoiseTransportFrame.total_chunks")?;
                frame.total_chunks = decode_uint32(raw)?;
            }
            4 => {
                frame.payload = cur
                    .take_delimited(wire, "NoiseTransportFrame.payload")?
                    .to_vec();
            }
            _ => cur.skip(wire)?,
        }
    }
    Ok(frame)
}

/// Draws a chunk id the way this crate does everywhere: 8 random bytes read
/// as a little-endian `i64` (the reference uses `secrets.randbits(64)`).
pub fn random_chunk_id<R: RngCore + ?Sized>(rng: &mut R) -> i64 {
    let mut bytes = [0u8; 8];
    rng.fill_bytes(&mut bytes);
    i64::from_le_bytes(bytes)
}

/// Splits `data` into encoded frames sharing `chunk_id`.
///
/// Empty input still yields one frame (with `total_chunks = 1`).
pub fn encode_noise_frames(data: &[u8], chunk_id: i64) -> Result<Vec<Vec<u8>>, FramingError> {
    let chunks = data.len().div_ceil(MAX_CHUNK_PAYLOAD).max(1);
    let total = u32::try_from(chunks)
        .ok()
        .filter(|total| *total <= MAX_TOTAL_CHUNKS)
        .ok_or(FramingError::MessageTooLarge {
            len: data.len(),
            chunks,
        })?;
    if data.is_empty() {
        return Ok(vec![encode_noise_frame(&NoiseTransportFrame {
            chunk_id,
            chunk_index: 0,
            total_chunks: 1,
            payload: Vec::new(),
        })]);
    }
    Ok(data
        .chunks(MAX_CHUNK_PAYLOAD)
        .zip(0u32..)
        .map(|(chunk, index)| {
            encode_noise_frame(&NoiseTransportFrame {
                chunk_id,
                chunk_index: index,
                total_chunks: total,
                payload: chunk.to_vec(),
            })
        })
        .collect())
}

#[derive(Debug)]
struct Assembly {
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    total_bytes: usize,
    created_at: Instant,
}

/// Reassembles chunked messages; one per receive direction.
#[derive(Debug, Default)]
pub struct NoiseFrameDecoder {
    pending: HashMap<i64, Assembly>,
    poisoned: bool,
}

impl NoiseFrameDecoder {
    /// A decoder with nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one decrypted frame; returns the message once all its chunks
    /// have arrived. Uses [`Instant::now`] for the TTL.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, FramingError> {
        self.decode_at(frame, Instant::now())
    }

    /// [`NoiseFrameDecoder::decode`] with an explicit clock reading.
    pub fn decode_at(
        &mut self,
        frame: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>, FramingError> {
        if self.poisoned {
            return Err(FramingError::Poisoned);
        }
        let result = self.decode_inner(frame, now);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Number of partially received messages.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn decode_inner(
        &mut self,
        frame: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>, FramingError> {
        let frame = decode_noise_frame(frame)?;
        let total = frame.total_chunks;
        if !(1..=MAX_TOTAL_CHUNKS).contains(&total) {
            return Err(FramingError::InvalidTotalChunks(total));
        }
        if frame.chunk_index >= total {
            return Err(FramingError::ChunkIndexOutOfRange {
                index: frame.chunk_index,
                total,
            });
        }
        if frame.payload.len() > MAX_CHUNK_PAYLOAD {
            return Err(FramingError::ChunkPayloadTooLarge(frame.payload.len()));
        }

        self.pending.retain(|_, assembly| {
            now.saturating_duration_since(assembly.created_at) <= ASSEMBLY_TTL
        });

        if !self.pending.contains_key(&frame.chunk_id) {
            if self.pending.len() >= MAX_PENDING_ASSEMBLIES {
                return Err(FramingError::TooManyPending);
            }
            self.pending.insert(
                frame.chunk_id,
                Assembly {
                    chunks: vec![None; total as usize],
                    received: 0,
                    total_bytes: 0,
                    created_at: now,
                },
            );
        }
        let Some(assembly) = self.pending.get_mut(&frame.chunk_id) else {
            return Ok(None);
        };

        let expected = assembly.chunks.len();
        if expected != total as usize {
            self.pending.remove(&frame.chunk_id);
            return Err(FramingError::InconsistentTotal {
                expected: u32::try_from(expected).unwrap_or(u32::MAX),
                got: total,
            });
        }
        let Some(slot) = assembly.chunks.get_mut(frame.chunk_index as usize) else {
            return Ok(None);
        };
        if slot.is_some() {
            self.pending.remove(&frame.chunk_id);
            return Err(FramingError::DuplicateChunkIndex(frame.chunk_index));
        }
        assembly.total_bytes += frame.payload.len();
        if assembly.total_bytes > MAX_ASSEMBLY_BYTES {
            self.pending.remove(&frame.chunk_id);
            return Err(FramingError::AssemblyTooLarge);
        }
        *slot = Some(frame.payload);
        assembly.received += 1;
        if assembly.received < expected {
            return Ok(None);
        }

        let Some(done) = self.pending.remove(&frame.chunk_id) else {
            return Ok(None);
        };
        let mut message = Vec::with_capacity(done.total_bytes);
        for chunk in done.chunks.into_iter().flatten() {
            message.extend_from_slice(&chunk);
        }
        Ok(Some(message))
    }
}
