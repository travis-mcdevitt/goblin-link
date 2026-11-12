//! Byte-exact encode and decode tests for service envelopes against
//! `envelope.json`.

mod common;

use common::{arr, load, materialize, s};
use muse_proto::envelope::*;
use serde_json::Value;

fn body_bytes(v: &Value) -> Vec<u8> {
    materialize(v).expect("inline body blob")
}

fn build_header(v: &Value) -> Header {
    Header::new(s(v, "key"), s(v, "value"))
}

fn build_headers(v: &Value) -> Vec<Header> {
    arr(v, "headers").iter().map(build_header).collect()
}

fn build_request(v: &Value) -> ApplicationRequest {
    ApplicationRequest {
        verb: s(v, "verb").to_owned(),
        path: s(v, "path").to_owned(),
        headers: build_headers(v),
        body: body_bytes(&v["body"]),
        end_body: v["end_body"].as_bool().unwrap(),
    }
}

fn build_response(v: &Value) -> ApplicationResponse {
    ApplicationResponse {
        status: v["status"].as_i64().unwrap() as i32,
        headers: build_headers(v),
        body: body_bytes(&v["body"]),
        end_body: v["end_body"].as_bool().unwrap(),
    }
}

fn reset_code(n: i64) -> ResetCode {
    match n {
        0 => ResetCode::Unspecified,
        1 => ResetCode::Cancelled,
        2 => ResetCode::Timeout,
        3 => ResetCode::ProtocolError,
        4 => ResetCode::RefusedStream,
        5 => ResetCode::InternalError,
        6 => ResetCode::ServiceUnavailable,
        other => panic!("unknown reset code {other}"),
    }
}

fn build_body_chunk(v: &Value) -> BodyChunk {
    BodyChunk {
        data: body_bytes(&v["data"]),
        end_body: v["end_body"].as_bool().unwrap(),
    }
}

fn build_reset(v: &Value) -> Reset {
    Reset {
        code: reset_code(v["code"].as_i64().unwrap()),
        reason: s(v, "reason").to_owned(),
    }
}

fn build_frame(v: &Value) -> ServiceFrame {
    let stream_id = v["stream_id"].as_i64().unwrap();
    let value = &v["value"];
    let body = match v["kind"].as_str() {
        None => None,
        Some("request") => Some(FrameBody::Request(build_request(value))),
        Some("response") => Some(FrameBody::Response(build_response(value))),
        Some("body_chunk") => Some(FrameBody::BodyChunk(build_body_chunk(value))),
        Some("reset") => Some(FrameBody::Reset(build_reset(value))),
        Some(other) => panic!("unknown kind {other}"),
    };
    ServiceFrame { stream_id, body }
}

fn service_type(n: i64) -> ServiceType {
    match n {
        0 => ServiceType::Daemon,
        1 => ServiceType::Sentinel,
        2 => ServiceType::Vault,
        3 => ServiceType::Authd,
        other => panic!("unknown service {other}"),
    }
}

#[test]
fn encode_headers_match() {
    for case in arr(&load("envelope"), "headers") {
        let header = build_header(case);
        assert_eq!(
            hex::encode(encode_header(&header)),
            s(case, "hex"),
            "{case}"
        );
    }
}

#[test]
fn encode_service_frames_match_and_round_trip() {
    for case in arr(&load("envelope"), "service_frames") {
        let frame = build_frame(&case["frame"]);
        let encoded = encode_service_frame(&frame);
        assert_eq!(
            hex::encode(&encoded),
            s(case, "hex"),
            "encode {}",
            s(case, "name")
        );
        // Canonical input decodes back to an identical frame.
        let decoded = decode_service_frame(&encoded).unwrap();
        assert_eq!(decoded, frame, "round trip {}", s(case, "name"));
        // And matches the dumper's own decode.
        assert_eq!(
            decoded,
            build_frame(&case["decoded"]),
            "decoded {}",
            s(case, "name")
        );
    }
}

#[test]
fn encode_service_requests_match() {
    for case in arr(&load("envelope"), "service_requests") {
        let req = ServiceRequest {
            service: service_type(case["service"].as_i64().unwrap()),
            payload: hex::decode(s(case, "payload")).unwrap(),
        };
        assert_eq!(
            hex::encode(encode_service_request(&req)),
            s(case, "hex"),
            "{case}"
        );
    }
}

#[test]
fn encode_service_responses_match() {
    for case in arr(&load("envelope"), "service_responses") {
        let resp = ServiceResponse {
            payload: hex::decode(s(case, "payload")).unwrap(),
        };
        assert_eq!(
            hex::encode(encode_service_response(&resp)),
            s(case, "hex"),
            "{case}"
        );
    }
}

#[test]
fn decode_cases_match() {
    let v = load("envelope");
    let decode = &v["decode"];
    run_decode(decode, "service_frame", |d| {
        decode_service_frame(d).map(|f| frame_value(&f))
    });
    run_decode(decode, "service_request", |d| {
        decode_service_request(d).map(|r| {
            serde_json::json!({"service": r.service as i64, "payload": hex::encode(&r.payload)})
        })
    });
    run_decode(decode, "service_response", |d| {
        decode_service_response(d).map(|r| serde_json::json!({"payload": hex::encode(&r.payload)}))
    });
    run_decode(decode, "header", |d| {
        decode_header(d).map(|h| serde_json::json!({"key": h.key, "value": h.value}))
    });
    run_decode(decode, "application_request", |d| {
        decode_application_request(d).map(|r| request_value(&r))
    });
    run_decode(decode, "application_response", |d| {
        decode_application_response(d).map(|r| response_value(&r))
    });
    run_decode(decode, "body_chunk", |d| {
        decode_body_chunk(d).map(|c| body_chunk_value(&c))
    });
    run_decode(decode, "reset", |d| {
        decode_reset(d).map(|r| reset_value(&r))
    });
}

fn run_decode(
    decode: &Value,
    key: &str,
    f: impl Fn(&[u8]) -> Result<Value, muse_proto::WireError>,
) {
    for case in arr(decode, key) {
        let data = hex::decode(s(case, "hex")).unwrap();
        let name = s(case, "name");
        match f(&data) {
            Ok(actual) => {
                assert!(
                    case.get("error").is_none(),
                    "{key}/{name}: expected error, got {actual}"
                );
                assert_eq!(actual, case["value"], "{key}/{name} decoded value");
            }
            Err(e) => {
                assert_eq!(e.to_string(), s(case, "error"), "{key}/{name} error");
            }
        }
    }
}

// The decoded-value JSON mirrors the dumper's representation exactly.

fn header_value(h: &Header) -> Value {
    serde_json::json!({"key": h.key, "value": h.value})
}

fn blob_value(bytes: &[u8]) -> Value {
    // Mirrors the dumper's `blob()` for the small bodies in decode vectors.
    serde_json::json!({ "hex": hex::encode(bytes) })
}

fn request_value(r: &ApplicationRequest) -> Value {
    serde_json::json!({
        "verb": r.verb,
        "path": r.path,
        "headers": r.headers.iter().map(header_value).collect::<Vec<_>>(),
        "body": blob_value(&r.body),
        "end_body": r.end_body,
    })
}

fn response_value(r: &ApplicationResponse) -> Value {
    serde_json::json!({
        "status": r.status,
        "headers": r.headers.iter().map(header_value).collect::<Vec<_>>(),
        "body": blob_value(&r.body),
        "end_body": r.end_body,
    })
}

fn body_chunk_value(c: &BodyChunk) -> Value {
    serde_json::json!({"data": blob_value(&c.data), "end_body": c.end_body})
}

fn reset_value(r: &Reset) -> Value {
    serde_json::json!({"code": r.code as i64, "reason": r.reason})
}

fn frame_value(f: &ServiceFrame) -> Value {
    let (kind, value) = match &f.body {
        None => (Value::Null, Value::Null),
        Some(FrameBody::Request(r)) => (Value::from("request"), request_value(r)),
        Some(FrameBody::Response(r)) => (Value::from("response"), response_value(r)),
        Some(FrameBody::BodyChunk(c)) => (Value::from("body_chunk"), body_chunk_value(c)),
        Some(FrameBody::Reset(r)) => (Value::from("reset"), reset_value(r)),
    };
    serde_json::json!({"stream_id": f.stream_id, "kind": kind, "value": value})
}
