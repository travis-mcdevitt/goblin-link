//! Tests for length-prefixed JSON control messages against `control.json`.
//!
//! `encode_message` is checked byte-for-byte against CPython's compact
//! `json.dumps`; `MessageDecoder` is checked by re-encoding each decoded
//! object and comparing. Cases the dumper flags `rust_deviation` (CPython
//! behaviours serde_json does not share — see the crate docs) are skipped.

mod common;

use common::{arr, load, s};
use muse_proto::control::MessageDecoder;
use muse_proto::control::{encode_message, MAX_INBOUND_MESSAGE};
use serde_json::Value;

#[test]
fn encode_matches_cpython() {
    for case in arr(&load("control"), "encode") {
        if case["rust_deviation"].as_bool().unwrap() {
            continue;
        }
        let out = encode_message(&case["value"]).unwrap();
        assert_eq!(hex::encode(&out), s(case, "hex"), "{}", s(case, "name"));
    }
}

#[test]
fn constants_match() {
    assert_eq!(
        u64::from(MAX_INBOUND_MESSAGE),
        load("control")["constants"]["MAX_INBOUND_MESSAGE"]
            .as_u64()
            .unwrap()
    );
}

#[test]
fn decoder_scenarios_match() {
    for scenario in arr(&load("control"), "decoder_scenarios") {
        if scenario["rust_deviation"].as_bool().unwrap() {
            continue;
        }
        let name = s(scenario, "name");
        let mut decoder = MessageDecoder::new();
        for (i, step) in arr(scenario, "steps").iter().enumerate() {
            let feed = hex::decode(s(step, "feed")).unwrap();
            match decoder.feed(&feed) {
                Ok(messages) => {
                    assert!(
                        step.get("error").is_none(),
                        "{name} step {i}: expected error"
                    );
                    let got: Vec<String> = messages
                        .iter()
                        .map(|m| hex::encode(encode_message(m).unwrap()))
                        .collect();
                    let expected: Vec<&str> = step["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap())
                        .collect();
                    assert_eq!(got, expected, "{name} step {i}");
                }
                Err(e) => assert_eq!(e.to_string(), s(step, "error"), "{name} step {i}"),
            }
        }
    }
}

/// Spot-checks of the documented CPython divergences, so the deviations are
/// exercised rather than merely skipped: serde_json rejects these.
#[test]
fn documented_deviations_are_rejected() {
    for raw in [
        &b"{\"n\":NaN}"[..],
        b"{\"n\":Infinity}",
        b"{\"s\":\"\\ud800\"}",
        b"\xef\xbb\xbf{\"a\":1}",
    ] {
        let mut dec = MessageDecoder::new();
        let mut feed = (raw.len() as u32).to_le_bytes().to_vec();
        feed.extend_from_slice(raw);
        // Dropped silently (not an object / not parseable), so no messages.
        assert_eq!(dec.feed(&feed).unwrap(), Vec::<Value>::new());
    }
}
