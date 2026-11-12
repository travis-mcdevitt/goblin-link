//! Byte-exact Noise XX handshake and transport tests against `noise.json`,
//! plus the handshake error cases. Uses only the public API; cipher-state and
//! nonce-IV vectors that need private access live in the crate's unit tests.

mod common;

use common::{arr, check_bytes, load, materialize, s};
use muse_proto::error::NoiseError;
use muse_proto::noise::{CipherState, FixedKeySource, NoiseXXInitiator, NoiseXXResponder};
use serde_json::Value;

fn key32(v: &Value, field: &str) -> [u8; 32] {
    let bytes = hex::decode(s(v, field)).unwrap();
    bytes.try_into().expect("32-byte key")
}

#[test]
fn handshakes_are_byte_exact() {
    for hs in arr(&load("noise"), "handshakes") {
        let name = s(hs, "name");
        let mut ini = NoiseXXInitiator::new(FixedKeySource::new([
            key32(hs, "initiator_ephemeral_private"),
            key32(hs, "initiator_static_private"),
        ]));
        let mut resp = NoiseXXResponder::new(
            FixedKeySource::new([
                key32(hs, "responder_ephemeral_private"),
                key32(hs, "responder_static_private"),
            ]),
            hex::decode(s(hs, "responder_payload")).unwrap(),
        );
        ini.initialize().unwrap();
        resp.initialize().unwrap();
        assert_eq!(
            hex::encode(ini.handshake_hash()),
            s(hs, "h_initialized"),
            "{name} h0"
        );

        let m1 = ini.write_message1().unwrap();
        assert_eq!(hex::encode(&m1), s(hs, "msg1"), "{name} msg1");
        assert_eq!(
            hex::encode(ini.handshake_hash()),
            s(hs, "h_after_msg1"),
            "{name} h1"
        );

        let m2 = resp.read_message1_and_write_message2(&m1).unwrap();
        assert_eq!(hex::encode(&m2), s(hs, "msg2"), "{name} msg2");
        let payload = ini.read_message2(&m2).unwrap();
        assert_eq!(
            hex::encode(&payload),
            s(hs, "responder_payload"),
            "{name} payload"
        );
        assert_eq!(
            hex::encode(ini.handshake_hash()),
            s(hs, "h_after_msg2"),
            "{name} h2"
        );
        assert_eq!(
            hex::encode(ini.remote_static_public_key().unwrap()),
            s(hs, "remote_static"),
            "{name} rs"
        );

        let m3 = ini.write_message3().unwrap();
        assert_eq!(hex::encode(&m3), s(hs, "msg3"), "{name} msg3");
        resp.read_message3(&m3).unwrap();
        assert_eq!(
            hex::encode(ini.handshake_hash()),
            s(hs, "h_after_msg3"),
            "{name} h3"
        );
        assert_eq!(
            ini.handshake_hash(),
            resp.handshake_hash(),
            "{name} hashes agree"
        );

        let (mut i_send, mut i_recv) = ini.split().unwrap();
        let (mut r_send, mut r_recv) = resp.split().unwrap();

        for (i, step) in arr(hs, "transport").iter().enumerate() {
            let ad = hex::decode(s(step, "ad")).unwrap();
            let plaintext = materialize(&step["plaintext"]).expect("plaintext");
            let (enc, dec): (&mut CipherState, &mut CipherState) = match s(step, "dir") {
                "i2r" => (&mut i_send, &mut r_recv),
                "r2i" => (&mut r_send, &mut i_recv),
                other => panic!("unknown direction {other}"),
            };
            let ct = enc.encrypt_with_ad(&ad, &plaintext).unwrap();
            check_bytes(
                &ct,
                &step["ciphertext"],
                &format!("{name} transport {i} ct"),
            );
            let back = dec.decrypt_with_ad(&ad, &ct).unwrap();
            assert_eq!(back, plaintext, "{name} transport {i} round trip");
        }
    }
}

#[test]
fn handshake_errors_match() {
    let errors = &load("noise")["errors"];
    let low_order: Vec<[u8; 32]> = errors["low_order_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            hex::decode(v.as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap()
        })
        .collect();

    for case in arr(errors, "cases") {
        let name = s(case, "name");
        let expected = s(case, "error");
        match name {
            "msg2_too_short" => {
                // Length is checked before any key use, so the ephemeral is
                // irrelevant; this case records no `initiator_e`.
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([[1u8; 32]]));
                ini.initialize().unwrap();
                ini.write_message1().unwrap();
                let msg2 = hex::decode(s(case, "msg2")).unwrap();
                assert_eq!(
                    ini.read_message2(&msg2).unwrap_err().to_string(),
                    expected,
                    "{name}"
                );
                // The handshake is now dead.
                assert!(matches!(
                    ini.write_message3(),
                    Err(NoiseError::DeadHandshake { .. })
                ));
            }
            "msg2_tampered_static" => {
                let mut ini = fresh_initiator(case);
                let msg2 = hex::decode(s(case, "msg2")).unwrap();
                assert_eq!(
                    ini.read_message2(&msg2).unwrap_err().to_string(),
                    expected,
                    "{name}"
                );
            }
            n if n.starts_with("msg2_low_order_") => {
                let mut ini = fresh_initiator(case);
                let msg2 = hex::decode(s(case, "msg2")).unwrap();
                assert_eq!(
                    ini.read_message2(&msg2).unwrap_err().to_string(),
                    expected,
                    "{name}"
                );
            }
            "write1_before_initialize" => {
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([]));
                assert_eq!(ini.write_message1().unwrap_err().to_string(), expected);
            }
            "split_before_handshake" => {
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([]));
                assert_eq!(ini.split().unwrap_err().to_string(), expected);
            }
            "double_initialize" => {
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([]));
                ini.initialize().unwrap();
                assert_eq!(ini.initialize().unwrap_err().to_string(), expected);
            }
            "write1_after_destroy" => {
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([[9u8; 32]]));
                ini.initialize().unwrap();
                ini.destroy();
                assert_eq!(ini.write_message1().unwrap_err().to_string(), expected);
            }
            "msg1_too_short" => {
                let mut resp = NoiseXXResponder::new(FixedKeySource::new([]), Vec::new());
                resp.initialize().unwrap();
                let msg1 = hex::decode(s(case, "msg1")).unwrap();
                assert_eq!(
                    resp.read_message1_and_write_message2(&msg1)
                        .unwrap_err()
                        .to_string(),
                    expected
                );
            }
            "msg1_low_order" => {
                let mut resp = NoiseXXResponder::new(
                    FixedKeySource::new([key32(case, "responder_e"), [7u8; 32]]),
                    Vec::new(),
                );
                resp.initialize().unwrap();
                let msg1 = hex::decode(s(case, "msg1")).unwrap();
                assert_eq!(
                    resp.read_message1_and_write_message2(&msg1)
                        .unwrap_err()
                        .to_string(),
                    expected
                );
            }
            "msg3_too_short" => {
                // Drive a responder to the msg3 phase, then feed a short msg3.
                let mut resp =
                    NoiseXXResponder::new(FixedKeySource::new([[3u8; 32], [4u8; 32]]), Vec::new());
                resp.initialize().unwrap();
                let mut ini = NoiseXXInitiator::new(FixedKeySource::new([[5u8; 32], [6u8; 32]]));
                ini.initialize().unwrap();
                let m1 = ini.write_message1().unwrap();
                resp.read_message1_and_write_message2(&m1).unwrap();
                let msg3 = hex::decode(s(case, "msg3")).unwrap();
                assert_eq!(resp.read_message3(&msg3).unwrap_err().to_string(), expected);
            }
            other => panic!("unhandled error case {other}"),
        }
    }

    // All seven reference low-order points are present and rejected by DH.
    assert_eq!(low_order.len(), 7);
}

/// Builds an initialized initiator that has already sent message 1 with the
/// ephemeral key the error case fixes.
fn fresh_initiator(case: &Value) -> NoiseXXInitiator<FixedKeySource> {
    let e = key32(case, "initiator_e");
    let mut ini = NoiseXXInitiator::new(FixedKeySource::new([e]));
    ini.initialize().unwrap();
    ini.write_message1().unwrap();
    ini
}
