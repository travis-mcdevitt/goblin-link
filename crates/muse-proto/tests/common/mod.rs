//! Shared helpers for the vector-driven tests.
//!
//! Vectors are JSON dumped from the Python reference by
//! `tools/dump_vectors.py`. Byte strings are recorded either inline as
//! lowercase hex (`{"hex": "…"}`), by length and SHA-256 when large
//! (`{"len": N, "sha256": "…"}`), or as a pattern spec
//! (`{"pattern": "affine31", "len": N, "seed": S}`). These helpers load a
//! vector file and compare Rust output against any of those forms.

#![allow(dead_code)]

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Loads and parses a vector file by base name (e.g. `"wire"`).
pub fn load(name: &str) -> Value {
    let path = format!("{}/tests/vectors/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// Decodes a lowercase-hex string.
pub fn unhex(s: &str) -> Vec<u8> {
    hex::decode(s).unwrap_or_else(|e| panic!("bad hex {s:?}: {e}"))
}

/// The dumper's `affine31` filler: byte `i` is `(i*31 + seed*7 + 11) mod 256`.
pub fn pattern_bytes(len: usize, seed: u64) -> Vec<u8> {
    (0..len)
        .map(|i| {
            ((i as u64)
                .wrapping_mul(31)
                .wrapping_add(seed.wrapping_mul(7))
                .wrapping_add(11)
                & 0xFF) as u8
        })
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Materialises the exact bytes a vector describes, when it can (inline hex or
/// a pattern spec). Returns `None` for a length+hash summary, which can only
/// be checked with [`check_bytes`].
pub fn materialize(v: &Value) -> Option<Vec<u8>> {
    if let Some(h) = v.get("hex").and_then(Value::as_str) {
        return Some(unhex(h));
    }
    if let Some(spec) = v.get("pattern").and_then(Value::as_str) {
        assert_eq!(spec, "affine31", "unknown pattern spec");
        let len = v["len"].as_u64().expect("pattern len") as usize;
        let seed = v["seed"].as_u64().expect("pattern seed");
        return Some(pattern_bytes(len, seed));
    }
    if let Some(zeros) = v.get("zeros").and_then(Value::as_u64) {
        return Some(vec![0u8; zeros as usize]);
    }
    None
}

/// Asserts `actual` matches the bytes a vector describes, in any recorded form.
pub fn check_bytes(actual: &[u8], expected: &Value, ctx: &str) {
    if let Some(bytes) = materialize(expected) {
        assert_eq!(actual, bytes.as_slice(), "{ctx}: bytes differ");
        return;
    }
    let len = expected["len"].as_u64().expect("blob len") as usize;
    let sha = expected["sha256"].as_str().expect("blob sha256");
    assert_eq!(actual.len(), len, "{ctx}: length differs");
    assert_eq!(sha256_hex(actual), sha, "{ctx}: sha256 differs");
}

/// Convenience accessor: a required string field.
pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing string {key} in {v}"))
}

/// Convenience accessor: a required array field.
pub fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing array {key} in {v}"))
}
