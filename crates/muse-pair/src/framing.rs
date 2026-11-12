//! Chunked messages over GATT writes and notifications (port of `ble_framing.py`).
//!
//! A message is split into packets of `0xFE, index, total` followed by a fragment. A packet
//! without that header is a whole message by itself.

/// First byte of a chunk.
pub const CHUNK_MAGIC: u8 = 0xFE;
const HEADER_BYTES: usize = 3;
/// Largest packet sent, whatever the MTU.
pub const MAX_PACKET_BYTES: usize = 160;
const MAX_CHUNKS: usize = 255;
/// Largest message accepted.
pub const MAX_MESSAGE_BYTES: usize = 8192;
/// Pause between the packets of one message, so the peer's stack keeps up (as the SDK does).
pub const CHUNK_STAGGER_MS: u64 = 50;

/// Splits `data` into framed packets that each fit one notification at `mtu`. `None` if the
/// message would need more than 255 chunks.
pub fn encode_chunks(data: &[u8], mtu: usize) -> Option<Vec<Vec<u8>>> {
    let notify_max = (if mtu > 3 { mtu - 3 } else { 20 }).min(MAX_PACKET_BYTES);
    let usable = notify_max.saturating_sub(HEADER_BYTES).max(1);
    let fragments: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(usable).collect()
    };
    if fragments.len() > MAX_CHUNKS {
        return None;
    }
    let total = fragments.len() as u8;
    Some(
        fragments
            .iter()
            .enumerate()
            .map(|(i, frag)| {
                let mut packet = vec![CHUNK_MAGIC, i as u8, total];
                packet.extend_from_slice(frag);
                packet
            })
            .collect(),
    )
}

/// Puts chunked writes back together, strictly in order. Index 0, or a change in the total,
/// starts a new message; a chunk out of order or a message too large discards what has been
/// collected.
#[derive(Debug, Default)]
pub struct ChunkAssembler {
    buf: Vec<u8>,
    total: u8,
    next: u8,
}

impl ChunkAssembler {
    /// Forgets a message in progress.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.total = 0;
        self.next = 0;
    }

    /// Adds one write; returns a message once it is complete.
    pub fn feed(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < HEADER_BYTES || packet[0] != CHUNK_MAGIC {
            return Some(packet.to_vec());
        }
        let (index, total, fragment) = (packet[1], packet[2], &packet[HEADER_BYTES..]);
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
        if self.buf.len() + fragment.len() > MAX_MESSAGE_BYTES {
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
    fn a_message_survives_chunking_at_any_mtu() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        for mtu in [23, 64, 185, 247, 517] {
            let packets = encode_chunks(&data, mtu).unwrap();
            assert!(packets
                .iter()
                .all(|p| p.len() <= (mtu - 3).min(MAX_PACKET_BYTES)));
            let mut a = ChunkAssembler::default();
            let mut out = None;
            for p in &packets {
                assert!(out.is_none());
                out = a.feed(p);
            }
            assert_eq!(out.as_deref(), Some(&data[..]), "mtu {mtu}");
        }
        // 20-byte packets carry 17 bytes each: 255 chunks hold 4335 bytes and no more.
        assert!(encode_chunks(&vec![0; 4335], 23).is_some());
        assert!(encode_chunks(&vec![0; 4336], 23).is_none());
        assert_eq!(
            encode_chunks(b"", 23).unwrap(),
            vec![vec![CHUNK_MAGIC, 0, 1]]
        );
    }

    #[test]
    fn disorder_discards_and_plain_packets_pass_through() {
        let mut a = ChunkAssembler::default();
        assert_eq!(
            a.feed(b"{}"),
            Some(b"{}".to_vec()),
            "no header: a whole message"
        );
        assert_eq!(a.feed(&[CHUNK_MAGIC, 0, 3, b'a']), None);
        assert_eq!(a.feed(&[CHUNK_MAGIC, 2, 3, b'c']), None, "skipped a chunk");
        assert_eq!(
            a.feed(&[CHUNK_MAGIC, 1, 3, b'b']),
            None,
            "nothing to continue"
        );
        // A fresh message starts at index 0.
        assert_eq!(a.feed(&[CHUNK_MAGIC, 0, 2, b'x']), None);
        assert_eq!(a.feed(&[CHUNK_MAGIC, 1, 2, b'y']), Some(b"xy".to_vec()));
        assert_eq!(a.feed(&[CHUNK_MAGIC, 0, 0]), None);
        // Too large in total.
        let big = [&[CHUNK_MAGIC, 0, 255][..], &[0u8; 150]].concat();
        assert_eq!(a.feed(&big), None);
        let mut dropped = false;
        for i in 1..255u8 {
            let p = [&[CHUNK_MAGIC, i, 255][..], &[0u8; 150]].concat();
            if a.feed(&p).is_none() && a.buf.is_empty() {
                dropped = true;
                break;
            }
        }
        assert!(dropped);
    }
}
