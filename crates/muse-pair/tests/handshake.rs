//! The handshake against a recording made with the SDK's own Python implementation
//! (`handshake-vector.json`: fixed device key, nonces and identity), then a whole setup
//! driven through the controller as the Muse app drives it.

use muse_pair::framing::ChunkAssembler;
use muse_pair::session::{DeviceIdentity, Entropy, PairingSession};
use muse_pair::setup::{Action, Network, SetupController};
use serde_json::{json, Value};

fn vector() -> Value {
    serde_json::from_str(include_str!("handshake-vector.json")).unwrap()
}

fn session(now_ms: std::sync::Arc<std::sync::atomic::AtomicU64>) -> PairingSession {
    let scalar: [u8; 32] = hex::decode(vector()["dev_scalar"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    PairingSession::new(
        DeviceIdentity {
            node_id: "aabbccddeeff".into(),
            device_id: "mg-aabbccddeeff".into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            firmware_version: "0.1.0".into(),
        },
        Some("sdk-test-token".into()),
        Entropy {
            now_ms: Box::new(move || now_ms.load(std::sync::atomic::Ordering::Relaxed)),
            secret_key: Box::new(move || scalar),
            nonce: Box::new(|| std::array::from_fn(|i| 0x40 + i as u8)),
        },
    )
}

struct Online(bool);
impl Network for Online {
    fn is_online(&self) -> bool {
        self.0
    }
    fn current_connection_entry(&self) -> Value {
        json!({"ssid": "home", "rssi": -50})
    }
}

fn clock() -> std::sync::Arc<std::sync::atomic::AtomicU64> {
    std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1000))
}

/// The messages in the `Send` actions, reassembled.
fn sent(actions: &[Action]) -> Vec<Value> {
    let mut out = Vec::new();
    for a in actions {
        if let Action::Send(packets) = a {
            let mut asm = ChunkAssembler::default();
            for p in packets {
                if let Some(m) = asm.feed(p) {
                    out.push(serde_json::from_slice(&m).unwrap_or_else(|_| {
                        Value::String(String::from_utf8_lossy(&m).into_owned())
                    }));
                }
            }
        }
    }
    out
}

fn write(c: &mut SetupController, message: &Value) -> Vec<Action> {
    let text = serde_json::to_string(message).unwrap();
    let mut out = Vec::new();
    for p in muse_pair::framing::encode_chunks(text.as_bytes(), 185).unwrap() {
        out.extend(c.on_write(&p));
    }
    out
}

#[test]
fn the_handshake_matches_the_sdk_byte_for_byte() {
    let v = vector();
    let mut s = session(clock());
    let ready = s.handle_hello(v["hello"].as_object().unwrap()).unwrap();
    assert_eq!(ready, v["ready"]);
    let first = s.decrypt(v["rec0"].as_object().unwrap()).unwrap();
    assert_eq!(first, r#"{"action":"pairing_client_finished"}"#);
    let generation = s.handle_client_finished(
        serde_json::from_str::<Value>(&first)
            .unwrap()
            .as_object()
            .unwrap(),
    );
    assert_ne!(generation, 0);
    // The confirmation record, with the SDK token inside, is the same ciphertext.
    assert_eq!(
        s.encrypt_status("pairing_confirmed", generation).unwrap(),
        v["status0"]
    );
}

#[test]
fn a_replayed_or_altered_record_drops_the_session() {
    let v = vector();
    let mut s = session(clock());
    s.handle_hello(v["hello"].as_object().unwrap()).unwrap();
    let mut bad = v["rec0"].as_object().unwrap().clone();
    bad.insert("counter".into(), json!("1"));
    assert!(s.decrypt(&bad).is_err());
    assert!(
        s.decrypt(v["rec0"].as_object().unwrap()).is_err(),
        "the session is gone"
    );

    let mut s = session(clock());
    s.handle_hello(v["hello"].as_object().unwrap()).unwrap();
    s.decrypt(v["rec0"].as_object().unwrap()).unwrap();
    assert!(s.decrypt(v["rec0"].as_object().unwrap()).is_err(), "replay");
}

#[test]
fn a_session_left_waiting_expires() {
    let v = vector();
    let now = clock();
    let mut s = session(now.clone());
    s.handle_hello(v["hello"].as_object().unwrap()).unwrap();
    now.fetch_add(61_000, std::sync::atomic::Ordering::Relaxed);
    assert!(s.decrypt(v["rec0"].as_object().unwrap()).is_err());
}

#[test]
fn setup_runs_from_hello_to_saved_credentials() {
    let v = vector();
    let mut c = SetupController::new(session(clock()), Box::new(Online(true)));

    // Before the handshake: device info is open, anything sensitive is refused.
    let info = sent(&write(&mut c, &json!({"action": "get_device_info"})));
    assert_eq!(info[0]["type"], "device_info");
    assert_eq!(info[0]["pairing_protocol"], 5);
    assert_eq!(info[0]["network_ready"], true);
    let refused = sent(&write(&mut c, &json!({"action": "provision_v2"})));
    assert_eq!(refused, vec![json!("error_encryption_required")]);

    let ready = sent(&write(&mut c, &v["hello"]));
    assert_eq!(ready[0], v["ready"]);
    // Plaintext commands are ignored once pairing has started.
    assert!(write(&mut c, &json!({"action": "wifi_scan"})).is_empty());

    let confirmed = write(&mut c, &v["rec0"]);
    assert_eq!(sent(&confirmed)[0], v["status0"]);
    assert!(confirmed.contains(&Action::Confirmed));
}

#[test]
fn garbage_and_unknown_commands_are_answered_not_obeyed() {
    let mut c = SetupController::new(session(clock()), Box::new(Online(false)));
    assert!(
        sent(&c.on_write(b"not json")).is_empty(),
        "not a plaintext status"
    );
    assert!(sent(&write(&mut c, &json!({"action": "reboot"}))).is_empty());
    assert!(sent(&write(&mut c, &json!([1, 2]))).is_empty());
    let bad_hello = json!({"action": "pairing_client_hello", "version": 4});
    assert_eq!(
        sent(&write(&mut c, &bad_hello)),
        vec![json!("error_pairing_invalid_hello")]
    );
}
