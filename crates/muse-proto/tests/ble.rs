//! Tests for BLE chunk framing against `ble.json`.

mod common;

use common::{arr, load, materialize, s};
use muse_proto::ble::{encode_chunks, ChunkAssembler};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn mtu(v: &Value) -> u16 {
    v["mtu"].as_u64().unwrap() as u16
}

#[test]
fn encode_chunks_match() {
    for case in arr(&load("ble"), "encode") {
        let data = materialize(&case["data"]).expect("pattern data");
        let chunks = encode_chunks(&data, mtu(case)).unwrap();
        assert_eq!(
            chunks.len() as u64,
            case["count"].as_u64().unwrap(),
            "count {case}"
        );
        assert_eq!(
            sha(&chunks.concat()),
            s(case, "all_sha256"),
            "all_sha {case}"
        );
        if let Some(expected) = case["chunks"].as_array() {
            let got: Vec<String> = chunks.iter().map(hex::encode).collect();
            let want: Vec<&str> = expected.iter().map(|v| v.as_str().unwrap()).collect();
            assert_eq!(got, want, "chunks {case}");
        }
    }
}

#[test]
fn encode_errors_match() {
    for case in arr(&load("ble"), "encode_errors") {
        let data = materialize(&case["data"]).expect("pattern data");
        let err = encode_chunks(&data, mtu(case)).unwrap_err();
        let expected = s(case, "error");
        if expected.starts_with("message needs") {
            // TooManyChunks: message is ported verbatim.
            assert_eq!(err.to_string(), expected, "{case}");
        } else {
            // MTU 6: the reference raises Python's `range() arg 3 must not be
            // zero`; we report a clearer typed error for the same condition.
            assert!(err.to_string().contains("MTU"), "mtu-6 error {case}: {err}");
        }
    }
}

#[test]
fn reference_quirks_match() {
    // MTU 4 and 5 leave a negative usable size; the reference yields a single
    // empty chunk rather than erroring, and the port matches that.
    for case in arr(&load("ble"), "reference_quirks") {
        let chunks = encode_chunks(
            &vec![0u8; case["len"].as_u64().unwrap() as usize],
            mtu(case),
        )
        .unwrap();
        let got: Vec<String> = chunks.iter().map(hex::encode).collect();
        let want: Vec<&str> = case["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "{case}");
    }
}

#[test]
fn assembler_scenarios_match() {
    for scenario in arr(&load("ble"), "assembler_scenarios") {
        let name = s(scenario, "name");
        let mut asm = match scenario["max_bytes"].as_u64() {
            Some(m) => ChunkAssembler::with_max_bytes(m as usize),
            None => ChunkAssembler::new(),
        };
        for (i, step) in arr(scenario, "steps").iter().enumerate() {
            let packet = hex::decode(s(step, "packet")).unwrap();
            let result = asm.feed(&packet);
            match step["result"].as_str() {
                None => assert!(
                    result.is_none(),
                    "{name} step {i}: expected None, got {result:?}"
                ),
                Some(expected) => {
                    assert_eq!(
                        hex::encode(result.expect("a message")),
                        expected,
                        "{name} step {i}"
                    );
                }
            }
        }
    }
}
