//! Chunked framing for BLE GATT writes and notifications (port of
//! `ble_framing.py`).
//!
//! Used only during pairing, when messages travel over a GATT characteristic
//! whose packets are capped near the ATT MTU. Every chunk is
//! `[0xFE, index, total]` followed by a fragment. A packet that is shorter
//! than the 3-byte header, or whose first byte is not `0xFE`, is a complete,
//! unchunked message.
//!
//! [`ChunkAssembler`] reassembles strictly in order: index 0 (or a change in
//! `total`) starts a new message, and any gap, repeat, overflow or oversize
//! discards what has been collected so far. This is deliberately simpler and
//! stricter than the Noise [`crate::framing`] decoder.

use crate::error::BleError;

/// Marks a chunked packet.
pub const CHUNK_MAGIC: u8 = 0xFE;
/// Header length: magic, index, total.
pub const HEADER_BYTES: usize = 3;
/// Hard cap on a single notification, regardless of MTU.
pub const MAX_PACKET_BYTES: usize = 160;
/// Most chunks one message may use (`total` is a single byte).
pub const MAX_CHUNKS: usize = 255;
/// Default reassembly budget.
pub const MAX_MESSAGE_BYTES: usize = 8192;
/// ATT MTU assumed when none has been negotiated.
pub const DEFAULT_ATT_MTU: u16 = 23;

/// Splits `data` into framed packets that each fit one notification at `mtu`.
///
/// Returns [`BleError::TooManyChunks`] if more than [`MAX_CHUNKS`] are needed,
/// and [`BleError::MtuTooSmall`] for an MTU of exactly 6 (which would leave a
/// zero-length payload; the reference raises here too). Empty input yields one
/// empty chunk, and — matching the reference — an MTU of 4 or 5 also yields a
/// single empty chunk rather than an error.
pub fn encode_chunks(data: &[u8], mtu: u16) -> Result<Vec<Vec<u8>>, BleError> {
    let notify_max: i32 = if mtu > 3 {
        i32::from(mtu - 3).min(MAX_PACKET_BYTES as i32)
    } else {
        20
    };
    let usable = notify_max - HEADER_BYTES as i32;

    let fragments: Vec<&[u8]> = if usable <= 0 {
        if usable == 0 {
            // The reference's `range(0, len, 0)` raises; surface it as an error.
            return Err(BleError::MtuTooSmall(mtu));
        }
        // Negative step yields an empty range, then the reference's `or [b""]`.
        vec![&[]]
    } else {
        let usable = usable as usize;
        if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(usable).collect()
        }
    };

    if fragments.len() > MAX_CHUNKS {
        return Err(BleError::TooManyChunks(fragments.len()));
    }
    let total = fragments.len() as u8;
    Ok(fragments
        .into_iter()
        .zip(0u8..)
        .map(|(fragment, index)| {
            let mut packet = Vec::with_capacity(HEADER_BYTES + fragment.len());
            packet.extend_from_slice(&[CHUNK_MAGIC, index, total]);
            packet.extend_from_slice(fragment);
            packet
        })
        .collect())
}

/// Reassembles chunked GATT writes, strictly in order.
#[derive(Debug)]
pub struct ChunkAssembler {
    max_bytes: usize,
    buf: Vec<u8>,
    total: u8,
    next: u8,
}

impl Default for ChunkAssembler {
    fn default() -> Self {
        Self::with_max_bytes(MAX_MESSAGE_BYTES)
    }
}

impl ChunkAssembler {
    /// An assembler with the default [`MAX_MESSAGE_BYTES`] budget.
    pub fn new() -> Self {
        Self::default()
    }

    /// An assembler with a custom per-message byte budget.
    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            buf: Vec::new(),
            total: 0,
            next: 0,
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.total = 0;
        self.next = 0;
    }

    /// Adds one packet; returns a complete message once one is available.
    ///
    /// A packet shorter than the header or not starting with [`CHUNK_MAGIC`]
    /// is returned whole as an unchunked message. Otherwise chunks are
    /// collected in order; a gap, a repeat, an index past `total`, or exceeding
    /// the byte budget discards the partial message and returns `None`.
    pub fn feed(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < HEADER_BYTES || packet[0] != CHUNK_MAGIC {
            return Some(packet.to_vec());
        }
        let index = packet[1];
        let total = packet[2];
        let fragment = &packet[HEADER_BYTES..];

        if total == 0 {
            self.reset();
            return None;
        }
        if index == 0 || total != self.total {
            self.reset();
            self.total = total;
        }
        if index != self.next || index >= self.total {
            self.reset();
            return None;
        }
        if self.buf.len() + fragment.len() > self.max_bytes {
            self.reset();
            return None;
        }
        self.buf.extend_from_slice(fragment);
        self.next = index + 1;
        if self.next < self.total {
            return None;
        }
        let message = std::mem::take(&mut self.buf);
        self.reset();
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_at_small_mtu() {
        let data: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let chunks = encode_chunks(&data, 23).unwrap();
        let mut asm = ChunkAssembler::new();
        let mut done = None;
        for chunk in &chunks {
            done = asm.feed(chunk);
        }
        assert_eq!(done.unwrap(), data);
    }

    #[test]
    fn unchunked_packet_passes_through() {
        let mut asm = ChunkAssembler::new();
        assert_eq!(asm.feed(b"{\"a\":1}"), Some(b"{\"a\":1}".to_vec()));
    }

    #[test]
    fn mtu_six_is_rejected() {
        assert_eq!(encode_chunks(b"abc", 6), Err(BleError::MtuTooSmall(6)));
    }
}
