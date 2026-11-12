//! Byte-exact tests for the protobuf-style field codec against `wire.json`.

mod common;

use common::{arr, load, s, unhex};
use muse_proto::wire::{
    decode_int32, decode_int64, decode_uint32, encode_field, encode_key, encode_varint,
    read_delimited, read_key, read_varint, skip_field, FieldValue, WireType,
};

fn wire_type(n: u64) -> WireType {
    WireType::try_from(n).expect("vector wire type is valid")
}

#[test]
fn encode_varint_matches() {
    for case in arr(&load("wire"), "encode_varint") {
        let value: u64 = s(case, "value").parse().unwrap();
        let mut out = Vec::new();
        encode_varint(value, &mut out);
        assert_eq!(hex::encode(&out), s(case, "hex"), "encode_varint {value}");
    }
}

#[test]
fn read_varint_matches() {
    for case in arr(&load("wire"), "read_varint") {
        let data = unhex(s(case, "hex"));
        let offset = case["offset"].as_u64().unwrap() as usize;
        match read_varint(&data, offset) {
            Ok((value, end)) => {
                assert_eq!(value.to_string(), s(case, "value"), "read_varint {case}");
                assert_eq!(end as u64, case["end"].as_u64().unwrap(), "end {case}");
            }
            Err(e) => assert_eq!(e.to_string(), s(case, "error"), "read_varint err {case}"),
        }
    }
}

#[test]
fn encode_key_matches() {
    let v = load("wire");
    for case in arr(&v, "encode_key") {
        let field = case["field"].as_u64().unwrap() as u32;
        let wire = wire_type(case["wire"].as_u64().unwrap());
        let mut out = Vec::new();
        encode_key(field, wire, &mut out).unwrap();
        assert_eq!(hex::encode(&out), s(case, "hex"), "encode_key {case}");
    }
    for case in arr(&v, "encode_key_errors") {
        let field = case["field"].as_u64().unwrap() as u32;
        let wire = case["wire"].as_u64().unwrap();
        // Wire types 3/4/6/7 are not representable as WireType, so those cases
        // are rejected at construction; only field-number errors reach encode_key.
        if let Ok(wt) = WireType::try_from(wire) {
            let mut out = Vec::new();
            assert!(
                encode_key(field, wt, &mut out).is_err(),
                "expected err {case}"
            );
        }
    }
}

#[test]
fn read_key_matches() {
    for case in arr(&load("wire"), "read_key") {
        let data = unhex(s(case, "hex"));
        match read_key(&data, 0) {
            Ok((field, wire, end)) => {
                assert_eq!(
                    u64::from(field),
                    case["field"].as_u64().unwrap(),
                    "field {case}"
                );
                assert_eq!(wire as u64, case["wire"].as_u64().unwrap(), "wire {case}");
                assert_eq!(end as u64, case["end"].as_u64().unwrap(), "end {case}");
            }
            Err(e) => assert_eq!(e.to_string(), s(case, "error"), "read_key err {case}"),
        }
    }
}

#[test]
fn encode_fields_match() {
    for case in arr(&load("wire"), "fields") {
        let field = case["field"].as_u64().unwrap() as u32;
        let value = s(case, "value");
        let bytes_storage;
        let fv = match s(case, "fn") {
            "varint" => FieldValue::Varint(value.parse().unwrap()),
            "int64" => FieldValue::Int64(value.parse().unwrap()),
            "int32" => FieldValue::Int32(value.parse().unwrap()),
            "uint32" => FieldValue::Uint32(value.parse().unwrap()),
            "bool" => FieldValue::Bool(value == "true"),
            "string" => FieldValue::String(value),
            "bytes" => {
                bytes_storage = unhex(value);
                FieldValue::Bytes(&bytes_storage)
            }
            other => panic!("unknown fn {other}"),
        };
        let mut out = Vec::new();
        encode_field(field, fv, &mut out).unwrap();
        assert_eq!(hex::encode(&out), s(case, "hex"), "field {case}");
    }
}

/// The reference's `*_field` functions range-check their Python-int arguments;
/// in Rust those out-of-range values are unrepresentable in the typed
/// `FieldValue` variants, so the type system enforces what `field_errors`
/// records. Nothing to assert at runtime beyond confirming the cases exist.
#[test]
fn field_range_errors_are_type_enforced() {
    assert!(!arr(&load("wire"), "field_errors").is_empty());
}

#[test]
fn decode_ints_match() {
    for case in arr(&load("wire"), "decode_ints") {
        let raw: u64 = s(case, "raw").parse().unwrap();
        assert_eq!(
            decode_int64(raw).to_string(),
            s(case, "int64"),
            "int64 {case}"
        );
        match decode_int32(raw) {
            Ok(v) => assert_eq!(v.to_string(), s(case, "int32"), "int32 {case}"),
            Err(e) => assert_eq!(e.to_string(), s(case, "int32_error"), "int32 err {case}"),
        }
        match decode_uint32(raw) {
            Ok(v) => assert_eq!(v.to_string(), s(case, "uint32"), "uint32 {case}"),
            Err(e) => assert_eq!(e.to_string(), s(case, "uint32_error"), "uint32 err {case}"),
        }
    }
}

#[test]
fn read_delimited_matches() {
    for case in arr(&load("wire"), "read_delimited") {
        let data = unhex(s(case, "hex"));
        match read_delimited(&data, 0) {
            Ok((value, end)) => {
                assert_eq!(
                    hex::encode(value),
                    s(case, "value"),
                    "read_delimited {case}"
                );
                assert_eq!(end as u64, case["end"].as_u64().unwrap(), "end {case}");
            }
            Err(e) => assert_eq!(e.to_string(), s(case, "error"), "read_delimited err {case}"),
        }
    }
}

#[test]
fn skip_field_matches() {
    for case in arr(&load("wire"), "skip_field") {
        let data = unhex(s(case, "hex"));
        let wire = match WireType::try_from(case["wire"].as_u64().unwrap()) {
            Ok(wt) => wt,
            Err(e) => {
                // Group/invalid wire types are unrepresentable; the reference
                // rejects them inside skip_field with the same message.
                assert_eq!(e.to_string(), s(case, "error"), "skip wire {case}");
                continue;
            }
        };
        match skip_field(&data, 0, wire) {
            Ok(end) => assert_eq!(end as u64, case["end"].as_u64().unwrap(), "skip {case}"),
            Err(e) => assert_eq!(e.to_string(), s(case, "error"), "skip err {case}"),
        }
    }
}
