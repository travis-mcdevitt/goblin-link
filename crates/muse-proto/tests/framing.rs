//! Tests for Noise transport framing against `framing.json`: single-frame
//! encode/decode, fragmentation, the random chunk id, and the reassembly
//! decoder's ordering, bounds, pending limit and TTL behaviour.

mod common;

use std::time::{Duration, Instant};

use common::{arr, check_bytes, load, materialize, s};
use muse_proto::framing::{
    decode_noise_frame, encode_noise_frame, encode_noise_frames, random_chunk_id,
    NoiseFrameDecoder, NoiseTransportFrame,
};
use rand_core::{impls, Error, RngCore};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn build_frame_from_fields(v: &Value) -> NoiseTransportFrame {
    NoiseTransportFrame {
        chunk_id: v["chunk_id"].as_i64().unwrap(),
        chunk_index: v["chunk_index"].as_u64().unwrap() as u32,
        total_chunks: v["total_chunks"].as_u64().unwrap() as u32,
        payload: hex::decode(s(v, "payload")).unwrap(),
    }
}

#[test]
fn encode_frame_matches() {
    for case in arr(&load("framing"), "encode_frame") {
        let frame = build_frame_from_fields(case);
        assert_eq!(
            hex::encode(encode_noise_frame(&frame)),
            s(case, "hex"),
            "{}",
            s(case, "name")
        );
    }
}

#[test]
fn decode_frame_matches() {
    for case in arr(&load("framing"), "decode_frame") {
        let data = hex::decode(s(case, "hex")).unwrap();
        match decode_noise_frame(&data) {
            Ok(frame) => {
                assert!(
                    case.get("error").is_none(),
                    "{}: expected error",
                    s(case, "name")
                );
                assert_eq!(
                    frame.chunk_id,
                    case["chunk_id"].as_i64().unwrap(),
                    "{}",
                    s(case, "name")
                );
                assert_eq!(
                    u64::from(frame.chunk_index),
                    case["chunk_index"].as_u64().unwrap()
                );
                assert_eq!(
                    u64::from(frame.total_chunks),
                    case["total_chunks"].as_u64().unwrap()
                );
                assert_eq!(hex::encode(&frame.payload), s(case, "payload"));
            }
            Err(e) => assert_eq!(e.to_string(), s(case, "error"), "{}", s(case, "name")),
        }
    }
}

#[test]
fn encode_frames_match() {
    for case in arr(&load("framing"), "encode_frames") {
        let payload = materialize(&case["payload"]).expect("pattern payload");
        let chunk_id = case["chunk_id"].as_i64().unwrap();
        let frames = encode_noise_frames(&payload, chunk_id).unwrap();
        assert_eq!(
            frames.len() as u64,
            case["count"].as_u64().unwrap(),
            "count {}",
            s(case, "name")
        );
        assert_eq!(
            sha(&frames.concat()),
            s(case, "all_frames_sha256"),
            "concat {}",
            s(case, "name")
        );
        for entry in arr(case, "frames") {
            let idx = entry["index"].as_u64().unwrap() as usize;
            let frame = &frames[idx];
            assert_eq!(
                frame.len() as u64,
                entry["len"].as_u64().unwrap(),
                "len {idx}"
            );
            assert_eq!(sha(frame), s(entry, "sha256"), "sha {idx}");
            let header = hex::decode(s(entry, "header_hex")).unwrap();
            assert!(frame.starts_with(&header), "header {idx}");
            if let Some(h) = entry.get("hex").and_then(Value::as_str) {
                assert_eq!(hex::encode(frame), h, "hex {idx}");
            }
        }
    }
}

#[test]
fn encode_frames_rejects_oversized() {
    let case = &load("framing")["encode_frames_too_large"];
    let payload = vec![0u8; case["len"].as_u64().unwrap() as usize];
    let err = encode_noise_frames(&payload, 1).unwrap_err();
    assert_eq!(err.to_string(), s(case, "error"));
}

/// An RNG that replays a fixed byte sequence, to reproduce the dumped
/// `random_chunk_id` draw.
struct FixedRng {
    bytes: Vec<u8>,
    pos: usize,
}

impl RngCore for FixedRng {
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for b in dest.iter_mut() {
            *b = self.bytes[self.pos];
            self.pos += 1;
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

#[test]
fn random_chunk_id_and_frames_match() {
    let case = &load("framing")["encode_frames_random_id"];
    let mut rng = FixedRng {
        bytes: hex::decode(s(case, "rng_bytes")).unwrap(),
        pos: 0,
    };
    let chunk_id = random_chunk_id(&mut rng);
    let payload = hex::decode(s(case, "payload")).unwrap();
    let frames = encode_noise_frames(&payload, chunk_id).unwrap();
    let expected: Vec<&str> = arr(case, "frames")
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    let got: Vec<String> = frames.iter().map(hex::encode).collect();
    assert_eq!(got, expected);
}

#[test]
fn decoder_scenarios_match() {
    for scenario in arr(&load("framing"), "decoder_scenarios") {
        let name = s(scenario, "name");
        let mut decoder = NoiseFrameDecoder::new();
        // A fixed base instant; the dumper's `advance` adds seconds cumulatively.
        let base = Instant::now();
        let mut elapsed = 0.0f64;
        for (i, step) in arr(scenario, "steps").iter().enumerate() {
            elapsed += step["advance"].as_f64().unwrap();
            let now = base + Duration::from_secs_f64(elapsed);
            let frame = frame_bytes(&step["frame"]);
            match decoder.decode_at(&frame, now) {
                Ok(result) => {
                    assert!(
                        step.get("error").is_none(),
                        "{name} step {i}: expected error"
                    );
                    match result {
                        None => assert!(
                            step["result"].is_null(),
                            "{name} step {i}: expected a message"
                        ),
                        Some(bytes) => {
                            check_bytes(&bytes, &step["result"], &format!("{name} step {i}"))
                        }
                    }
                }
                Err(e) => assert_eq!(e.to_string(), s(step, "error"), "{name} step {i}"),
            }
        }
    }
}

/// Reconstructs a step's frame bytes, whether inline or a `rebuild` spec.
fn frame_bytes(v: &Value) -> Vec<u8> {
    if let Some(bytes) = materialize(v) {
        return bytes;
    }
    let rebuild = &v["rebuild"];
    let frame = NoiseTransportFrame {
        chunk_id: rebuild["chunk_id"].as_i64().unwrap(),
        chunk_index: rebuild["chunk_index"].as_u64().unwrap() as u32,
        total_chunks: rebuild["total_chunks"].as_u64().unwrap() as u32,
        payload: materialize(&rebuild["payload"]).expect("rebuild payload"),
    };
    encode_noise_frame(&frame)
}
