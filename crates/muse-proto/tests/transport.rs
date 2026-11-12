//! Byte-exact tests for NoiseTransport request building and inbound decode
//! against `transport.json`.

mod common;

use common::{arr, check_bytes, load, materialize, s};
use muse_proto::envelope::{BodyChunk, Header, ResetCode, ServiceType};
use muse_proto::framing::NoiseFrameDecoder;
use muse_proto::noise::CipherState;
use muse_proto::transport::{
    decode_request_envelope, service_from_name, DecryptedFrame, NoiseTransport,
};
use serde_json::Value;

fn cipher_from_hex(h: &str) -> CipherState {
    let mut cs = CipherState::new();
    cs.initialize_key(&hex::decode(h).unwrap()).unwrap();
    cs
}

fn chunk_id(op: &Value) -> i64 {
    let bytes = hex::decode(op["rng_bytes"][0].as_str().unwrap()).unwrap();
    i64::from_le_bytes(bytes.try_into().unwrap())
}

fn service(op: &Value) -> ServiceType {
    service_from_name(s(op, "service")).unwrap()
}

fn headers(op: &Value) -> Vec<Header> {
    arr(op, "headers")
        .iter()
        .map(|h| Header::new(s(h, "key"), s(h, "value")))
        .collect()
}

fn body(op: &Value) -> Vec<u8> {
    match &op["body"] {
        Value::String(s) => hex::decode(s).unwrap(),
        v => materialize(v).expect("body blob/pattern"),
    }
}

#[test]
fn client_ops_are_byte_exact() {
    let v = load("transport");
    let keys = &v["keys"];
    let mut transport = NoiseTransport::new(
        cipher_from_hex(s(keys, "client_send")),
        cipher_from_hex(s(keys, "client_recv")),
    );
    // A server-side decrypt to confirm each op decodes to `server_sees`.
    let mut server_recv = cipher_from_hex(s(keys, "client_send"));

    for op in arr(&v, "client_ops") {
        let cid = chunk_id(op);
        let frames = match s(op, "op") {
            "http_request" => {
                let enc = transport
                    .encrypt_http_request(
                        s(op, "method"),
                        s(op, "path"),
                        &body(op),
                        service(op),
                        &headers(op),
                        cid,
                    )
                    .unwrap();
                assert_eq!(
                    enc.stream_id,
                    op["stream_id"].as_i64().unwrap(),
                    "stream id {op}"
                );
                enc.frames
            }
            "stream_request" => {
                let enc = transport
                    .start_stream_request(
                        s(op, "method"),
                        s(op, "path"),
                        service(op),
                        &headers(op),
                        cid,
                    )
                    .unwrap();
                assert_eq!(
                    enc.stream_id,
                    op["stream_id"].as_i64().unwrap(),
                    "stream id {op}"
                );
                enc.frames
            }
            "body_chunk" => transport
                .encrypt_body_chunk(
                    op["stream_id_in"].as_i64().unwrap(),
                    &body_data(op),
                    service(op),
                    op["end_body"].as_bool().unwrap(),
                    cid,
                )
                .unwrap(),
            "reset" => transport
                .encrypt_reset(
                    op["stream_id_in"].as_i64().unwrap(),
                    service(op),
                    s(op, "reason"),
                    reset_code(op["code"].as_i64().unwrap()),
                    cid,
                )
                .unwrap(),
            other => panic!("unknown op {other}"),
        };

        let expected = arr(op, "frames");
        assert_eq!(frames.len(), expected.len(), "frame count {op}");
        for (i, frame) in frames.iter().enumerate() {
            check_bytes(frame, &expected[i], &format!("{} frame {i}", s(op, "op")));
        }

        // Server view: decrypt + reassemble + decode the request envelope.
        let mut decoder = NoiseFrameDecoder::new();
        let mut plain = None;
        for frame in &frames {
            plain = decoder
                .decode(&server_recv.decrypt_with_ad(b"", frame).unwrap())
                .unwrap();
        }
        let request = muse_proto::envelope::decode_service_request(&plain.unwrap()).unwrap();
        let inner =
            decode_request_envelope(&muse_proto::envelope::encode_service_request(&request))
                .unwrap();
        let sees = &op["server_sees"];
        assert_eq!(
            request.service as i64,
            sees["service"].as_i64().unwrap(),
            "service {op}"
        );
        assert_eq!(
            inner.stream_id,
            sees["frame"]["stream_id"].as_i64().unwrap(),
            "inner stream {op}"
        );
    }
}

fn body_data(op: &Value) -> Vec<u8> {
    match &op["data"] {
        Value::String(s) => hex::decode(s).unwrap(),
        v => materialize(v).expect("data"),
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
        other => panic!("code {other}"),
    }
}

#[test]
fn unknown_service_matches() {
    let v = load("transport");
    assert_eq!(
        service_from_name("nope").unwrap_err().to_string(),
        s(&v, "unknown_service_error")
    );
}

#[test]
fn server_scenarios_match() {
    let v = load("transport");
    let keys = &v["keys"];
    for sc in arr(&v, "server_scenarios") {
        let name = s(sc, "name");
        let mut transport = NoiseTransport::new(
            cipher_from_hex(s(keys, "client_send")),
            cipher_from_hex(s(keys, "client_recv")),
        );
        for (i, step) in arr(sc, "steps").iter().enumerate() {
            let ciphertext = materialize(&step["ciphertext"]).expect("ciphertext");
            match transport.decrypt_frame(&ciphertext) {
                Ok(result) => {
                    assert!(
                        step.get("error").is_none(),
                        "{name} step {i}: expected error"
                    );
                    compare_result(
                        result.as_ref(),
                        &step["result"],
                        &format!("{name} step {i}"),
                    );
                }
                Err(e) => assert_eq!(e.to_string(), s(step, "error"), "{name} step {i}"),
            }
        }
        // `alive_after`: can the transport still build a request?
        let alive = transport
            .encrypt_http_request("GET", "/probe", b"", ServiceType::Daemon, &[], 1)
            .is_ok();
        assert_eq!(
            alive,
            sc["alive_after"].as_bool().unwrap(),
            "{name} alive_after"
        );
    }
}

fn headers_json(headers: &[Header]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|h| serde_json::json!({"key": h.key, "value": h.value}))
            .collect(),
    )
}

/// Structural comparison that checks a (possibly large) body via SHA-256.
fn compare_result(frame: Option<&DecryptedFrame>, expected: &Value, ctx: &str) {
    match frame {
        None => assert!(expected.is_null(), "{ctx}: expected {expected}, got None"),
        Some(DecryptedFrame::Response {
            stream_id,
            response,
        }) => {
            assert_eq!(expected["kind"], "response", "{ctx}");
            assert_eq!(*stream_id, expected["stream_id"].as_i64().unwrap(), "{ctx}");
            let value = &expected["value"];
            assert_eq!(
                i64::from(response.status),
                value["status"].as_i64().unwrap(),
                "{ctx}"
            );
            assert_eq!(headers_json(&response.headers), value["headers"], "{ctx}");
            assert_eq!(
                response.end_body,
                value["end_body"].as_bool().unwrap(),
                "{ctx}"
            );
            check_bytes(&response.body, &value["body"], ctx);
        }
        Some(DecryptedFrame::BodyChunk { stream_id, chunk }) => {
            let BodyChunk { data, end_body } = chunk;
            assert_eq!(expected["kind"], "body_chunk", "{ctx}");
            assert_eq!(*stream_id, expected["stream_id"].as_i64().unwrap(), "{ctx}");
            assert_eq!(
                *end_body,
                expected["value"]["end_body"].as_bool().unwrap(),
                "{ctx}"
            );
            check_bytes(data, &expected["value"]["data"], ctx);
        }
        Some(DecryptedFrame::Reset { stream_id, reset }) => {
            assert_eq!(expected["kind"], "reset", "{ctx}");
            assert_eq!(*stream_id, expected["stream_id"].as_i64().unwrap(), "{ctx}");
            assert_eq!(
                reset.code as i64,
                expected["value"]["code"].as_i64().unwrap(),
                "{ctx}"
            );
            assert_eq!(reset.reason, s(&expected["value"], "reason"), "{ctx}");
        }
    }
}
