//! Device side of Goblin Link BLE pairing, protocol version 5 (port of `pairing.py`).
//!
//! Community mode only: `pairing_auth` is `"none"` and the policy is `confirm_app`, so a valid
//! client-finished record confirms the session without a physical button. The wire format,
//! transcript, key schedule and record encryption match the SDK, so the Muse apps pair with it
//! unchanged.
//!
//! Community pairing protects setup secrets from passive observers. It does not authenticate
//! the ECDH peer, so it cannot stop an active man-in-the-middle.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Pairing protocol version.
pub const PAIRING_VERSION: u64 = 5;
const PAIRING_MODEL: &str = "hatch_link";
const PAIRING_SUITE: &str = "p256-hkdf-sha256-aes-gcm-v1";
const POLICY_APP: &str = "confirm_app";
const AUTH_COMMUNITY: &str = "none";
const RECORD_LABEL: &str = "hatch-link ble setup v1";
const SESSION_ID_LABEL: &[u8] = b"hatch-link session id v1";

const CLIENT_FINISHED_TIMEOUT_MS: u64 = 60_000;
const CONFIRMED_TIMEOUT_MS: u64 = 120_000;
const PROVISIONING_TIMEOUT_MS: u64 = 120_000;

/// Wire status for a bad hello.
pub const ERROR_INVALID_HELLO: &str = "error_pairing_invalid_hello";
/// Wire status for a record that does not open.
pub const ERROR_DECRYPT: &str = "error_pairing_decrypt";

const P256_POINT_BYTES: usize = 65;
const NONCE_BYTES: usize = 16;
const SESSION_ID_BYTES: usize = 16;
const TAG_BYTES: usize = 16;
const MAX_B64_CHARS: usize = 4096;
const MAX_CIPHERTEXT_B64_CHARS: usize = 16384;
const TO_DEVICE: u8 = 0;
const FROM_DEVICE: u8 = 1;

/// A handshake step failed; the string is the wire status to report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PairingError(pub &'static str);

/// Where a pairing session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingState {
    /// No session.
    Idle,
    /// Keys agreed; waiting for the app's first record.
    WaitClientFinished,
    /// Confirmed by the app.
    Ready,
    /// Credentials received and being checked.
    Provisioning,
}

fn b64url() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// Unpadded base64url.
pub fn b64url_encode(data: &[u8]) -> String {
    b64url().encode(data)
}

/// Decodes unpadded base64url, rejecting anything the firmware rejects.
pub fn b64url_decode(value: Option<&Value>, max_chars: usize) -> Option<Vec<u8>> {
    let text = value?.as_str()?;
    if text.is_empty() || text.len() > max_chars || text.len() % 4 == 1 {
        return None;
    }
    if !text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    // The firmware's decoder does not insist on zero trailing bits; neither do we.
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    )
    .decode(text)
    .ok()
}

fn parse_counter(value: Option<&Value>) -> Option<u64> {
    let text = value?.as_str()?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok()
}

/// The fields of the canonical v5 transcript that vary.
struct Transcript<'a> {
    device_id: &'a str,
    node_id: &'a str,
    mac: &'a str,
    firmware_version: &'a str,
    mobile_pub: &'a str,
    device_pub: &'a str,
    mobile_nonce: &'a str,
    device_nonce: &'a str,
}

impl Transcript<'_> {
    /// The community-mode, confirm-in-app transcript; SHA-256 of it is the `transcript_hash`.
    fn text(&self) -> String {
        [
            format!("hatch-link-pairing-v{PAIRING_VERSION}"),
            format!("version={PAIRING_VERSION}"),
            "initiator_role=mobile".into(),
            "responder_role=link".into(),
            format!("device_id={}", self.device_id),
            format!("node_id={}", self.node_id),
            format!("mac={}", self.mac),
            format!("model={PAIRING_MODEL}"),
            format!("firmware_version={}", self.firmware_version),
            format!("selected_cipher_suite={PAIRING_SUITE}"),
            format!("pairing_auth={AUTH_COMMUNITY}"),
            "pairing_auth_epoch=0".into(),
            format!("pairing_policy={POLICY_APP}"),
            "confirm_timeout_seconds=0".into(),
            format!("mobile_pub={}", self.mobile_pub),
            format!("device_pub={}", self.device_pub),
            format!("mobile_nonce={}", self.mobile_nonce),
            format!("device_nonce={}", self.device_nonce),
        ]
        .join("\n")
    }
}

/// `(mobile_tx_key, mobile_rx_key, session_id)`: the first opens records the device receives,
/// the second seals records it sends.
fn derive_session_keys(
    ecdh_secret: &[u8],
    mobile_nonce: &[u8],
    device_nonce: &[u8],
    transcript_hash: &[u8],
) -> ([u8; 32], [u8; 32], [u8; SESSION_ID_BYTES]) {
    let salt = Sha256::new()
        .chain_update(mobile_nonce)
        .chain_update(device_nonce)
        .chain_update(transcript_hash)
        .finalize();
    let mut session_secret = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), ecdh_secret)
        .expand(RECORD_LABEL.as_bytes(), &mut session_secret)
        .expect("32 bytes is a valid HKDF length");
    let expand = |info: &[u8]| {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::from_prk(&session_secret)
            .expect("a 32-byte PRK")
            .expand(info, &mut out)
            .expect("32 bytes is a valid HKDF length");
        out
    };
    let digest = Sha256::new()
        .chain_update(SESSION_ID_LABEL)
        .chain_update(transcript_hash)
        .chain_update(ecdh_secret)
        .finalize();
    let mut session_id = [0u8; SESSION_ID_BYTES];
    session_id.copy_from_slice(&digest[..SESSION_ID_BYTES]);
    (
        expand(b"mobile->device"),
        expand(b"device->mobile"),
        session_id,
    )
}

fn record_nonce(direction: u8, counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = direction;
    n[4..].copy_from_slice(&counter.to_be_bytes());
    n
}

fn record_aad(session_id_b64: &str, direction: u8, counter: u64) -> Vec<u8> {
    let arrow = if direction == TO_DEVICE { "m2d" } else { "d2m" };
    format!("{RECORD_LABEL}|{session_id_b64}|{arrow}|{counter}").into_bytes()
}

/// Who the device says it is while pairing.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    /// `homelink-<suffix>`.
    pub node_id: String,
    /// `hatch-link:<mac>`.
    pub device_id: String,
    /// The identity's MAC-shaped address.
    pub mac: String,
    /// Version reported to the app.
    pub firmware_version: String,
}

/// Sources of time and randomness, replaceable in tests.
pub struct Entropy {
    /// Milliseconds on a clock that only goes forward.
    pub now_ms: Box<dyn Fn() -> u64 + Send>,
    /// A fresh P-256 secret scalar (32 bytes).
    pub secret_key: Box<dyn Fn() -> [u8; 32] + Send>,
    /// A fresh device nonce.
    pub nonce: Box<dyn Fn() -> [u8; NONCE_BYTES] + Send>,
}

impl Default for Entropy {
    fn default() -> Self {
        use rand_core::RngCore;
        let start = std::time::Instant::now();
        Self {
            now_ms: Box::new(move || start.elapsed().as_millis() as u64),
            secret_key: Box::new(|| {
                p256::SecretKey::random(&mut rand_core::OsRng)
                    .to_bytes()
                    .into()
            }),
            nonce: Box::new(|| {
                let mut n = [0u8; NONCE_BYTES];
                rand_core::OsRng.fill_bytes(&mut n);
                n
            }),
        }
    }
}

/// One device's pairing state.
///
/// Methods that advance the handshake return a nonzero *generation*. Deferred work
/// (checking the credentials) holds on to it and asks [`PairingSession::is_current`] before
/// acting, so work from an abandoned attempt cannot act on a newer one.
pub struct PairingSession {
    identity: DeviceIdentity,
    sdk_token: Option<String>,
    entropy: Entropy,
    generation: u64,
    state: PairingState,
    deadline_ms: u64,
    rx: Option<Aes256Gcm>,
    tx: Option<Aes256Gcm>,
    session_id_b64: String,
    rx_counter: u64,
    tx_counter: u64,
}

impl PairingSession {
    /// A session for `identity`. `sdk_token`, when there is one, is handed to the app once
    /// pairing is confirmed.
    pub fn new(identity: DeviceIdentity, sdk_token: Option<String>, entropy: Entropy) -> Self {
        let mut s = Self {
            identity,
            sdk_token,
            entropy,
            generation: 0,
            state: PairingState::Idle,
            deadline_ms: 0,
            rx: None,
            tx: None,
            session_id_b64: String::new(),
            rx_counter: 0,
            tx_counter: 0,
        };
        s.reset();
        s
    }

    /// Pairing fields for the `get_device_info` response.
    pub fn device_info(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("device_id".into(), json!(self.identity.device_id));
        m.insert("mac".into(), json!(self.identity.mac));
        m.insert("model".into(), json!(PAIRING_MODEL));
        m.insert("pairing_protocol".into(), json!(PAIRING_VERSION));
        m.insert("pairing_auth".into(), json!(AUTH_COMMUNITY));
        m.insert("pairing_auth_epoch".into(), json!(0));
        m.insert("pairing_policy".into(), json!(POLICY_APP));
        m
    }

    /// The node id.
    pub fn node_id(&self) -> &str {
        &self.identity.node_id
    }

    /// The version reported to the app.
    pub fn firmware_version(&self) -> &str {
        &self.identity.firmware_version
    }

    /// The current state, after expiring a session that ran out of time.
    pub fn state(&mut self) -> PairingState {
        self.expire();
        self.state
    }

    /// Whether the app has confirmed the session.
    pub fn confirmed(&mut self) -> bool {
        matches!(
            self.state(),
            PairingState::Ready | PairingState::Provisioning
        )
    }

    /// Whether `generation` is the session's current one.
    pub fn is_current(&self, generation: u64) -> bool {
        generation != 0 && generation == self.generation
    }

    /// Drops the session.
    pub fn reset(&mut self) {
        self.generation += 1;
        self.clear();
    }

    /// Starts a session from `pairing_client_hello`; returns `pairing_ready`.
    pub fn handle_hello(&mut self, message: &Map<String, Value>) -> Result<Value, PairingError> {
        let bad = PairingError(ERROR_INVALID_HELLO);
        // The version may arrive as 5 or 5.0, as JSON numbers do.
        let version_ok = message
            .get("version")
            .and_then(Value::as_f64)
            .is_some_and(|v| v == PAIRING_VERSION as f64);
        if !version_ok
            || message.get("pairing_auth").and_then(Value::as_str) != Some(AUTH_COMMUNITY)
            || message.get("pairing_policy").and_then(Value::as_str) != Some(POLICY_APP)
        {
            return Err(bad);
        }
        self.reset();
        let mobile_pub =
            b64url_decode(message.get("mobile_pub"), MAX_B64_CHARS).ok_or(bad.clone())?;
        let mobile_nonce =
            b64url_decode(message.get("mobile_nonce"), MAX_B64_CHARS).ok_or(bad.clone())?;
        if mobile_pub.len() != P256_POINT_BYTES
            || mobile_pub[0] != 0x04
            || mobile_nonce.len() != NONCE_BYTES
        {
            return Err(bad);
        }
        let peer = p256::PublicKey::from_sec1_bytes(&mobile_pub).map_err(|_| bad.clone())?;
        let secret =
            p256::SecretKey::from_slice(&(self.entropy.secret_key)()).map_err(|_| bad.clone())?;
        let device_pub = secret.public_key().to_encoded_point(false);
        let device_nonce = (self.entropy.nonce)();

        let mobile_pub_b64 = b64url_encode(&mobile_pub);
        let device_pub_b64 = b64url_encode(device_pub.as_bytes());
        let mobile_nonce_b64 = b64url_encode(&mobile_nonce);
        let device_nonce_b64 = b64url_encode(&device_nonce);
        let transcript = Transcript {
            device_id: &self.identity.device_id,
            node_id: &self.identity.node_id,
            mac: &self.identity.mac,
            firmware_version: &self.identity.firmware_version,
            mobile_pub: &mobile_pub_b64,
            device_pub: &device_pub_b64,
            mobile_nonce: &mobile_nonce_b64,
            device_nonce: &device_nonce_b64,
        }
        .text();
        let transcript_hash = Sha256::digest(transcript.as_bytes());
        let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
        let (rx_key, tx_key, session_id) = derive_session_keys(
            shared.raw_secret_bytes(),
            &mobile_nonce,
            &device_nonce,
            &transcript_hash,
        );
        self.rx = Some(Aes256Gcm::new_from_slice(&rx_key).expect("32-byte key"));
        self.tx = Some(Aes256Gcm::new_from_slice(&tx_key).expect("32-byte key"));
        self.session_id_b64 = b64url_encode(&session_id);
        self.rx_counter = 0;
        self.tx_counter = 0;
        self.state = PairingState::WaitClientFinished;
        self.deadline_ms = (self.entropy.now_ms)() + CLIENT_FINISHED_TIMEOUT_MS;
        Ok(json!({
            "type": "pairing_ready",
            "version": PAIRING_VERSION,
            "device_id": self.identity.device_id,
            "node_id": self.identity.node_id,
            "mac": self.identity.mac,
            "model": PAIRING_MODEL,
            "firmware_version": self.identity.firmware_version,
            "pairing_auth": AUTH_COMMUNITY,
            "pairing_auth_epoch": 0,
            "pairing_policy": POLICY_APP,
            "device_pub": device_pub_b64,
            "device_nonce": device_nonce_b64,
            "transcript_hash": b64url_encode(&transcript_hash),
            "session_id": self.session_id_b64,
        }))
    }

    /// Opens one app-to-device `pairing_encrypted` record. Any failure (wrong session, a
    /// skipped or replayed counter, a bad tag, expiry) drops the session, as the firmware does.
    pub fn decrypt(&mut self, envelope: &Map<String, Value>) -> Result<String, PairingError> {
        let opened = self.try_decrypt(envelope);
        match opened {
            Some(text) => {
                self.rx_counter += 1;
                Ok(text)
            }
            None => {
                self.reset();
                Err(PairingError(ERROR_DECRYPT))
            }
        }
    }

    fn try_decrypt(&mut self, envelope: &Map<String, Value>) -> Option<String> {
        if self.expire() || self.state == PairingState::Idle {
            return None;
        }
        if envelope.get("session_id").and_then(Value::as_str) != Some(&self.session_id_b64) {
            return None;
        }
        let counter = parse_counter(envelope.get("counter"))?;
        if counter != self.rx_counter {
            return None;
        }
        let mut sealed = b64url_decode(envelope.get("ciphertext"), MAX_CIPHERTEXT_B64_CHARS)?;
        let tag = b64url_decode(envelope.get("tag"), MAX_B64_CHARS)?;
        if tag.len() != TAG_BYTES {
            return None;
        }
        sealed.extend_from_slice(&tag);
        let plain = self
            .rx
            .as_ref()?
            .decrypt(
                Nonce::from_slice(&record_nonce(TO_DEVICE, counter)),
                Payload {
                    msg: &sealed,
                    aad: &record_aad(&self.session_id_b64, TO_DEVICE, counter),
                },
            )
            .ok()?;
        String::from_utf8(plain).ok()
    }

    /// Confirms the session after the first opened record, which must be exactly
    /// `{"action": "pairing_client_finished"}`. Under `confirm_app` the app has already
    /// collected consent. Returns the new generation, or 0 (after dropping the session).
    pub fn handle_client_finished(&mut self, command: &Map<String, Value>) -> u64 {
        let exact = command.len() == 1
            && command.get("action").and_then(Value::as_str) == Some("pairing_client_finished");
        let ok = exact
            && !self.expire()
            && self.state == PairingState::WaitClientFinished
            && self.rx_counter == 1;
        if !ok {
            self.reset();
            return 0;
        }
        self.generation += 1;
        self.state = PairingState::Ready;
        self.deadline_ms = (self.entropy.now_ms)() + CONFIRMED_TIMEOUT_MS;
        self.generation
    }

    /// Enters provisioning from a confirmed session; returns its generation, or 0.
    pub fn mark_provisioning(&mut self) -> u64 {
        if !self.expire() && self.state == PairingState::Ready {
            self.generation += 1;
            self.state = PairingState::Provisioning;
            self.deadline_ms = (self.entropy.now_ms)() + PROVISIONING_TIMEOUT_MS;
        }
        if self.state == PairingState::Provisioning {
            self.generation
        } else {
            0
        }
    }

    /// Whether `generation` is a provisioning still in time (the moment to save credentials).
    pub fn provisioning_valid(&mut self, generation: u64) -> bool {
        generation != 0
            && generation == self.generation
            && !self.expire()
            && self.state == PairingState::Provisioning
    }

    /// Gives a valid provisioning more time.
    pub fn extend_provisioning(&mut self, generation: u64) -> bool {
        let valid = self.provisioning_valid(generation);
        if valid {
            self.deadline_ms = (self.entropy.now_ms)() + PROVISIONING_TIMEOUT_MS;
        }
        valid
    }

    /// Seals a device-to-app record. `None` when there is no session, or when a nonzero
    /// `generation` no longer matches (the work it belongs to is stale).
    pub fn encrypt_json(&mut self, plaintext: &str, generation: u64) -> Option<Value> {
        if (generation != 0 && generation != self.generation)
            || self.expire()
            || self.state == PairingState::Idle
        {
            return None;
        }
        let counter = self.tx_counter;
        let sealed = self
            .tx
            .as_ref()?
            .encrypt(
                Nonce::from_slice(&record_nonce(FROM_DEVICE, counter)),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: &record_aad(&self.session_id_b64, FROM_DEVICE, counter),
                },
            )
            .ok()?;
        self.tx_counter += 1;
        let (ciphertext, tag) = sealed.split_at(sealed.len() - TAG_BYTES);
        Some(json!({
            "type": "pairing_encrypted",
            "session_id": self.session_id_b64,
            "counter": counter.to_string(),
            "ciphertext": b64url_encode(ciphertext),
            "tag": b64url_encode(tag),
        }))
    }

    /// Seals a status record. `pairing_confirmed` carries the SDK token when there is one.
    pub fn encrypt_status(&mut self, status: &str, generation: u64) -> Option<Value> {
        let mut message = Map::new();
        message.insert("type".into(), json!("status"));
        message.insert("status".into(), json!(status));
        if let (Some(token), "pairing_confirmed") = (&self.sdk_token, status) {
            message.insert("sdk_token".into(), json!(token));
        }
        let plaintext = serde_json::to_string(&message).ok()?;
        self.encrypt_json(&plaintext, generation)
    }

    fn clear(&mut self) {
        self.state = PairingState::Idle;
        self.deadline_ms = 0;
        self.rx = None;
        self.tx = None;
        self.session_id_b64.clear();
        self.rx_counter = 0;
        self.tx_counter = 0;
    }

    fn expire(&mut self) -> bool {
        if self.state == PairingState::Idle || (self.entropy.now_ms)() <= self.deadline_ms {
            return false;
        }
        self.clear();
        true
    }
}
