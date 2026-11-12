//! Noise XX handshake and transport cipher (port of `noise/noise_xx.py`).
//!
//! Protocol `Noise_XX_25519_AESGCM_SHA256`. Ported directly on
//! `x25519-dalek`, `aes-gcm`, `sha2` and `hmac` rather than via the `snow`
//! crate: the reference derives the handshake hash from a 32-byte protocol
//! name right-padded with zeros (the SHA-256 branch of the Noise spec takes
//! the name verbatim because it is <= 32 bytes, so this is byte-identical to
//! standard Noise), but it is wrapped in the project's own chunk framing and
//! `CipherState` poisoning semantics, which `snow` does not expose. A direct
//! port keeps those behaviours and is checked byte-for-byte against vectors
//! dumped from the Python code (see `tests/noise.rs`).
//!
//! The initiator is used in production. The responder is a faithful port of
//! the reference's test responder, so the whole handshake can be exercised in
//! process without a live VM.
//!
//! Key material is zeroized on drop where the types allow it, and no key or
//! shared secret is ever logged or included in an error.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::error::NoiseError;

type HmacSha256 = Hmac<Sha256>;

/// The Noise protocol name.
pub const PROTOCOL_NAME: &[u8] = b"Noise_XX_25519_AESGCM_SHA256";
const DH_KEY_LEN: usize = 32;
const AEAD_TAG_LEN: usize = 16;
/// Smallest valid handshake message 2 (`e`, encrypted `s`, encrypted payload).
pub const MIN_MSG2_LEN: usize = DH_KEY_LEN + (DH_KEY_LEN + AEAD_TAG_LEN) + AEAD_TAG_LEN;
/// Smallest valid handshake message 3 (encrypted `s`, encrypted payload).
pub const MIN_MSG3_LEN: usize = DH_KEY_LEN + AEAD_TAG_LEN + AEAD_TAG_LEN;
/// Largest nonce the cipher will use before refusing (matches the reference,
/// which caps at JavaScript's safe-integer limit).
pub const MAX_SAFE_NONCE: u64 = (1 << 53) - 1;

/// The low-order X25519 points the reference rejects as peer public keys.
pub const LOW_ORDER_POINTS: [[u8; 32]; 7] = [
    [0; 32],
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ],
    [
        0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef,
        0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f,
        0x11, 0x57,
    ],
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    [
        0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    [
        0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
];

/// Builds the 12-byte GCM nonce: 4 zero bytes, then the counter big-endian.
fn build_nonce_iv(nonce: u64) -> [u8; 12] {
    let mut iv = [0u8; 12];
    iv[4..].copy_from_slice(&nonce.to_be_bytes());
    iv
}

/// AES-256-GCM cipher with a monotonic nonce, as Noise prescribes.
///
/// Before a key is set (`has_key() == false`) encrypt and decrypt pass data
/// through unchanged, matching the reference and the Noise spec. Once keyed,
/// any failure poisons the state so a half-advanced nonce can never be reused.
pub struct CipherState {
    cipher: Option<Aes256Gcm>,
    nonce: u64,
    poisoned: bool,
}

impl std::fmt::Debug for CipherState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CipherState")
            .field("keyed", &self.cipher.is_some())
            .field("nonce", &self.nonce)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl CipherState {
    /// An unkeyed cipher (pass-through).
    pub fn new() -> Self {
        Self {
            cipher: None,
            nonce: 0,
            poisoned: false,
        }
    }

    fn assert_alive(&self) -> Result<(), NoiseError> {
        if self.poisoned {
            Err(NoiseError::CipherPoisoned)
        } else {
            Ok(())
        }
    }

    /// Installs a 32-byte key and resets the nonce to 0.
    pub fn initialize_key(&mut self, key: &[u8]) -> Result<(), NoiseError> {
        self.assert_alive()?;
        if key.len() != 32 {
            return Err(NoiseError::BadKeyLength);
        }
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| NoiseError::BadKeyLength)?;
        self.cipher = Some(cipher);
        self.nonce = 0;
        Ok(())
    }

    /// Whether a key has been installed.
    pub fn has_key(&self) -> bool {
        self.cipher.is_some()
    }

    fn next_nonce(&mut self) -> Result<u64, NoiseError> {
        if self.nonce >= MAX_SAFE_NONCE {
            self.poisoned = true;
            return Err(NoiseError::NonceExhausted);
        }
        let current = self.nonce;
        self.nonce += 1;
        Ok(current)
    }

    /// Encrypts `plaintext` with associated data `ad`.
    pub fn encrypt_with_ad(&mut self, ad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        self.assert_alive()?;
        if self.cipher.is_none() {
            return Ok(plaintext.to_vec());
        }
        let nonce = self.next_nonce()?;
        let iv = build_nonce_iv(nonce);
        let cipher = self.cipher.as_ref().expect("cipher present");
        match cipher.encrypt(
            &iv.into(),
            Payload {
                msg: plaintext,
                aad: ad,
            },
        ) {
            Ok(ciphertext) => Ok(ciphertext),
            Err(_) => {
                self.poisoned = true;
                Err(NoiseError::EncryptFailed)
            }
        }
    }

    /// Decrypts `ciphertext` with associated data `ad`.
    pub fn decrypt_with_ad(&mut self, ad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        self.assert_alive()?;
        if self.cipher.is_none() {
            return Ok(ciphertext.to_vec());
        }
        let nonce = self.next_nonce()?;
        let iv = build_nonce_iv(nonce);
        let cipher = self.cipher.as_ref().expect("cipher present");
        let result = cipher.decrypt(
            &iv.into(),
            Payload {
                msg: ciphertext,
                aad: ad,
            },
        );
        match result {
            Ok(plaintext) => Ok(plaintext),
            Err(_) => {
                self.poisoned = true;
                Err(NoiseError::DecryptFailed)
            }
        }
    }
}

impl Default for CipherState {
    fn default() -> Self {
        Self::new()
    }
}

/// An X25519 key pair whose private half is zeroized on drop.
struct KeyPair {
    secret: StaticSecret,
    public: [u8; 32],
}

impl KeyPair {
    fn from_secret(secret: StaticSecret) -> Self {
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }
}

/// Supplies the ephemeral and static keys a handshake needs.
///
/// Production uses [`OsKeySource`]. Tests inject fixed keys so transcripts are
/// reproducible, exactly as the vector dumper monkeypatches the reference.
pub trait KeySource {
    /// Returns the next X25519 secret, as 32 raw bytes.
    fn next_secret(&mut self) -> [u8; 32];
}

/// Draws keys from the operating system CSPRNG.
#[derive(Debug, Default)]
pub struct OsKeySource;

impl KeySource for OsKeySource {
    fn next_secret(&mut self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut bytes);
        bytes
    }
}

/// A fixed list of secrets, consumed in order (for tests and the dumper).
#[derive(Debug, Default)]
pub struct FixedKeySource {
    secrets: std::collections::VecDeque<[u8; 32]>,
}

impl FixedKeySource {
    /// Builds a source from the given secrets.
    pub fn new(secrets: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self {
            secrets: secrets.into_iter().collect(),
        }
    }
}

impl KeySource for FixedKeySource {
    fn next_secret(&mut self) -> [u8; 32] {
        self.secrets.pop_front().expect("FixedKeySource exhausted")
    }
}

use rand_core::RngCore;

fn x25519_dh(secret: &StaticSecret, public: &[u8]) -> Result<Zeroizing<[u8; 32]>, NoiseError> {
    if public.len() != DH_KEY_LEN {
        return Err(NoiseError::InvalidPublicKeyLength);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(public);
    for low in LOW_ORDER_POINTS.iter() {
        if bool::from(key.ct_eq(low)) {
            return Err(NoiseError::LowOrderPublicKey);
        }
    }
    let shared = secret.diffie_hellman(&PublicKey::from(key));
    let shared = Zeroizing::new(shared.to_bytes());
    if bool::from(shared.ct_eq(&[0u8; 32])) {
        return Err(NoiseError::ZeroSharedSecret);
    }
    Ok(shared)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// HKDF with two outputs, as Noise's `HKDF` uses it.
fn hkdf2(ck: &[u8], ikm: &[u8]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    let temp = Zeroizing::new(hmac_sha256(ck, ikm));
    let out1 = Zeroizing::new(hmac_sha256(temp.as_ref(), &[0x01]));
    let mut data2 = Zeroizing::new([0u8; 33]);
    data2[..32].copy_from_slice(out1.as_ref());
    data2[32] = 0x02;
    let out2 = Zeroizing::new(hmac_sha256(temp.as_ref(), data2.as_ref()));
    (out1, out2)
}

struct SymmetricState {
    ck: Zeroizing<[u8; 32]>,
    h: [u8; 32],
    cipher: CipherState,
}

impl SymmetricState {
    fn new() -> Self {
        Self {
            ck: Zeroizing::new([0u8; 32]),
            h: [0u8; 32],
            cipher: CipherState::new(),
        }
    }

    fn initialize(&mut self) {
        let mut padded = [0u8; 32];
        padded[..PROTOCOL_NAME.len()].copy_from_slice(PROTOCOL_NAME);
        self.h = padded;
        self.ck = Zeroizing::new(padded);
        self.mix_hash(b"");
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(self.h);
        hasher.update(data);
        self.h = hasher.finalize().into();
    }

    fn mix_key(&mut self, ikm: &[u8]) -> Result<(), NoiseError> {
        let (ck, temp_k) = hkdf2(self.ck.as_ref(), ikm);
        self.ck = ck;
        let mut cipher = CipherState::new();
        cipher.initialize_key(temp_k.as_ref())?;
        self.cipher = cipher;
        Ok(())
    }

    fn encrypt_and_hash(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let ciphertext = self.cipher.encrypt_with_ad(&self.h, plaintext)?;
        self.mix_hash(&ciphertext);
        Ok(ciphertext)
    }

    fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let plaintext = self.cipher.decrypt_with_ad(&self.h, ciphertext)?;
        self.mix_hash(ciphertext);
        Ok(plaintext)
    }

    fn split(&mut self) -> Result<(CipherState, CipherState), NoiseError> {
        let (temp_k1, temp_k2) = hkdf2(self.ck.as_ref(), b"");
        self.ck = Zeroizing::new([0u8; 32]);
        self.h = [0u8; 32];
        let mut c1 = CipherState::new();
        c1.initialize_key(temp_k1.as_ref())?;
        let mut c2 = CipherState::new();
        c2.initialize_key(temp_k2.as_ref())?;
        Ok((c1, c2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Created = 0,
    Initialized = 1,
    Msg1Sent = 2,
    Msg2Read = 3,
    Msg3Sent = 4,
    Split = 5,
    Dead = 6,
}

fn require_phase(actual: Phase, expected: Phase, method: &'static str) -> Result<(), NoiseError> {
    if actual == Phase::Dead {
        return Err(NoiseError::DeadHandshake { method });
    }
    if actual != expected {
        return Err(NoiseError::WrongPhase {
            method,
            expected: expected as u8,
            actual: actual as u8,
        });
    }
    Ok(())
}

/// The Noise XX initiator: the role the gadget plays against a Muse VM.
///
/// Drive it `initialize` -> `write_message1` -> `read_message2` ->
/// `write_message3` -> `split`. The resulting cipher pair feeds
/// [`crate::transport::NoiseTransport`]. Every method checks the phase and,
/// on any failure, marks the handshake dead so it cannot be reused.
pub struct NoiseXXInitiator<K: KeySource> {
    ss: SymmetricState,
    keys: K,
    e: Option<KeyPair>,
    s: Option<KeyPair>,
    re: Option<[u8; 32]>,
    rs: Option<[u8; 32]>,
    phase: Phase,
}

impl<K: KeySource> NoiseXXInitiator<K> {
    /// Creates an initiator drawing keys from `keys`.
    pub fn new(keys: K) -> Self {
        Self {
            ss: SymmetricState::new(),
            keys,
            e: None,
            s: None,
            re: None,
            rs: None,
            phase: Phase::Created,
        }
    }

    /// Mixes the protocol name into the handshake hash.
    pub fn initialize(&mut self) -> Result<(), NoiseError> {
        require_phase(self.phase, Phase::Created, "initialize")?;
        self.ss.initialize();
        self.phase = Phase::Initialized;
        Ok(())
    }

    /// Writes message 1 (`-> e`): the initiator's ephemeral public key.
    pub fn write_message1(&mut self) -> Result<Vec<u8>, NoiseError> {
        require_phase(self.phase, Phase::Initialized, "write_message1")?;
        let result = (|| {
            let e = KeyPair::from_secret(StaticSecret::from(self.keys.next_secret()));
            self.ss.mix_hash(&e.public);
            self.ss.encrypt_and_hash(b"")?;
            let public = e.public.to_vec();
            self.e = Some(e);
            Ok(public)
        })();
        if result.is_err() {
            self.phase = Phase::Dead;
        } else {
            self.phase = Phase::Msg1Sent;
        }
        result
    }

    /// Reads message 2 (`<- e, ee, s, es`) and returns the responder payload.
    pub fn read_message2(&mut self, msg: &[u8]) -> Result<Vec<u8>, NoiseError> {
        require_phase(self.phase, Phase::Msg1Sent, "read_message2")?;
        if msg.len() < MIN_MSG2_LEN {
            self.phase = Phase::Dead;
            return Err(NoiseError::Message2TooShort(msg.len()));
        }
        let result = (|| {
            let mut offset = 0;
            let mut re = [0u8; 32];
            re.copy_from_slice(&msg[offset..offset + DH_KEY_LEN]);
            self.ss.mix_hash(&re);
            self.re = Some(re);
            offset += DH_KEY_LEN;

            let e = self
                .e
                .as_ref()
                .ok_or(NoiseError::MissingKey("initiator ephemeral"))?;
            let ee = x25519_dh(&e.secret, &re)?;
            self.ss.mix_key(ee.as_ref())?;

            let enc_s = &msg[offset..offset + DH_KEY_LEN + AEAD_TAG_LEN];
            let rs_bytes = self.ss.decrypt_and_hash(enc_s)?;
            offset += DH_KEY_LEN + AEAD_TAG_LEN;
            let mut rs = [0u8; 32];
            if rs_bytes.len() != DH_KEY_LEN {
                return Err(NoiseError::InvalidPublicKeyLength);
            }
            rs.copy_from_slice(&rs_bytes);

            let es = x25519_dh(&e.secret, &rs)?;
            self.ss.mix_key(es.as_ref())?;
            self.rs = Some(rs);

            let payload = self.ss.decrypt_and_hash(&msg[offset..])?;
            Ok(payload)
        })();
        if result.is_err() {
            self.phase = Phase::Dead;
        } else {
            self.phase = Phase::Msg2Read;
        }
        result
    }

    /// Writes message 3 (`-> s, se`): the initiator's encrypted static key.
    pub fn write_message3(&mut self) -> Result<Vec<u8>, NoiseError> {
        require_phase(self.phase, Phase::Msg2Read, "write_message3")?;
        let result = (|| {
            let s = KeyPair::from_secret(StaticSecret::from(self.keys.next_secret()));
            let enc_s = self.ss.encrypt_and_hash(&s.public)?;
            let re = self
                .re
                .ok_or(NoiseError::MissingKey("responder ephemeral"))?;
            let se = x25519_dh(&s.secret, &re)?;
            self.ss.mix_key(se.as_ref())?;
            self.s = Some(s);
            let enc_payload = self.ss.encrypt_and_hash(b"")?;
            let mut out = enc_s;
            out.extend_from_slice(&enc_payload);
            Ok(out)
        })();
        if result.is_err() {
            self.phase = Phase::Dead;
        } else {
            self.phase = Phase::Msg3Sent;
        }
        result
    }

    /// Derives the transport cipher pair `(send, recv)` for the initiator.
    pub fn split(&mut self) -> Result<(CipherState, CipherState), NoiseError> {
        require_phase(self.phase, Phase::Msg3Sent, "split")?;
        self.phase = Phase::Split;
        let result = self.ss.split();
        self.e = None;
        self.s = None;
        self.re = None;
        self.rs = None;
        result
    }

    /// Discards all key material and marks the handshake dead.
    pub fn destroy(&mut self) {
        self.e = None;
        self.s = None;
        self.re = None;
        self.rs = None;
        self.phase = Phase::Dead;
    }

    /// The responder's static public key, once message 2 has been read.
    pub fn remote_static_public_key(&self) -> Option<[u8; 32]> {
        self.rs
    }

    /// The handshake hash so far (a transcript binding; safe to expose).
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.ss.h
    }
}

/// The Noise XX responder. A faithful port of the reference's in-process test
/// responder, so the full handshake can run without a live VM. Note `split`
/// returns `(send, recv)` from the responder's point of view, i.e. swapped
/// relative to the initiator.
pub struct NoiseXXResponder<K: KeySource> {
    ss: SymmetricState,
    keys: K,
    e: Option<KeyPair>,
    #[allow(dead_code)]
    s: Option<KeyPair>,
    re: Option<[u8; 32]>,
    payload: Vec<u8>,
    phase: Phase,
}

impl<K: KeySource> NoiseXXResponder<K> {
    /// Creates a responder that answers message 2 with `payload`.
    pub fn new(keys: K, payload: Vec<u8>) -> Self {
        Self {
            ss: SymmetricState::new(),
            keys,
            e: None,
            s: None,
            re: None,
            payload,
            phase: Phase::Created,
        }
    }

    /// Mixes the protocol name into the handshake hash.
    pub fn initialize(&mut self) -> Result<(), NoiseError> {
        require_phase(self.phase, Phase::Created, "initialize")?;
        self.ss.initialize();
        self.phase = Phase::Initialized;
        Ok(())
    }

    /// Reads message 1 and writes message 2.
    pub fn read_message1_and_write_message2(&mut self, msg1: &[u8]) -> Result<Vec<u8>, NoiseError> {
        require_phase(
            self.phase,
            Phase::Initialized,
            "read_message1_and_write_message2",
        )?;
        if msg1.len() < DH_KEY_LEN {
            self.phase = Phase::Dead;
            return Err(NoiseError::Message1TooShort(msg1.len()));
        }
        let result = (|| {
            let mut re = [0u8; 32];
            re.copy_from_slice(&msg1[..DH_KEY_LEN]);
            self.ss.mix_hash(&re);
            self.re = Some(re);
            self.ss.decrypt_and_hash(&msg1[DH_KEY_LEN..])?;

            let e = KeyPair::from_secret(StaticSecret::from(self.keys.next_secret()));
            self.ss.mix_hash(&e.public);
            let ee = x25519_dh(&e.secret, &re)?;
            self.ss.mix_key(ee.as_ref())?;
            let e_public = e.public;
            self.e = Some(e);

            let s = KeyPair::from_secret(StaticSecret::from(self.keys.next_secret()));
            let enc_s = self.ss.encrypt_and_hash(&s.public)?;
            let es = x25519_dh(&s.secret, &re)?;
            self.ss.mix_key(es.as_ref())?;
            self.s = Some(s);

            let payload = self.payload.clone();
            let enc_payload = self.ss.encrypt_and_hash(&payload)?;
            let mut out = Vec::with_capacity(32 + enc_s.len() + enc_payload.len());
            out.extend_from_slice(&e_public);
            out.extend_from_slice(&enc_s);
            out.extend_from_slice(&enc_payload);
            Ok(out)
        })();
        if result.is_err() {
            self.phase = Phase::Dead;
        } else {
            self.phase = Phase::Msg2Read;
        }
        result
    }

    /// Reads message 3.
    pub fn read_message3(&mut self, msg3: &[u8]) -> Result<(), NoiseError> {
        require_phase(self.phase, Phase::Msg2Read, "read_message3")?;
        if msg3.len() < MIN_MSG3_LEN {
            self.phase = Phase::Dead;
            return Err(NoiseError::Message3TooShort(msg3.len()));
        }
        let result = (|| {
            let offset = DH_KEY_LEN + AEAD_TAG_LEN;
            let rs_bytes = self.ss.decrypt_and_hash(&msg3[..offset])?;
            let mut rs = [0u8; 32];
            if rs_bytes.len() != DH_KEY_LEN {
                return Err(NoiseError::InvalidPublicKeyLength);
            }
            rs.copy_from_slice(&rs_bytes);
            let e = self
                .e
                .as_ref()
                .ok_or(NoiseError::MissingKey("responder ephemeral"))?;
            let se = x25519_dh(&e.secret, &rs)?;
            self.ss.mix_key(se.as_ref())?;
            self.ss.decrypt_and_hash(&msg3[offset..])?;
            Ok(())
        })();
        if result.is_err() {
            self.phase = Phase::Dead;
        } else {
            self.phase = Phase::Msg3Sent;
        }
        result
    }

    /// Derives the transport cipher pair `(send, recv)` for the responder.
    pub fn split(&mut self) -> Result<(CipherState, CipherState), NoiseError> {
        require_phase(self.phase, Phase::Msg3Sent, "split")?;
        self.phase = Phase::Split;
        let (c1, c2) = self.ss.split()?;
        self.e = None;
        self.s = None;
        self.re = None;
        Ok((c2, c1))
    }

    /// The handshake hash so far.
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.ss.h
    }
}

impl Drop for SymmetricState {
    fn drop(&mut self) {
        self.h.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(label: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(label.as_bytes());
        hasher.finalize().into()
    }

    #[test]
    fn full_handshake_and_transport() {
        let mut ini = NoiseXXInitiator::new(FixedKeySource::new([secret("ie"), secret("is")]));
        let mut resp = NoiseXXResponder::new(
            FixedKeySource::new([secret("re"), secret("rs")]),
            b"hi".to_vec(),
        );
        ini.initialize().unwrap();
        resp.initialize().unwrap();
        let m1 = ini.write_message1().unwrap();
        let m2 = resp.read_message1_and_write_message2(&m1).unwrap();
        assert_eq!(ini.read_message2(&m2).unwrap(), b"hi");
        let m3 = ini.write_message3().unwrap();
        resp.read_message3(&m3).unwrap();
        assert_eq!(ini.handshake_hash(), resp.handshake_hash());

        let (mut i_send, mut i_recv) = ini.split().unwrap();
        let (mut r_send, mut r_recv) = resp.split().unwrap();
        let ct = i_send.encrypt_with_ad(b"", b"ping").unwrap();
        assert_eq!(r_recv.decrypt_with_ad(b"", &ct).unwrap(), b"ping");
        let ct = r_send.encrypt_with_ad(b"", b"pong").unwrap();
        assert_eq!(i_recv.decrypt_with_ad(b"", &ct).unwrap(), b"pong");
    }

    #[test]
    fn wrong_phase_is_rejected() {
        let mut ini = NoiseXXInitiator::new(OsKeySource);
        assert!(matches!(
            ini.write_message1(),
            Err(NoiseError::WrongPhase { .. })
        ));
    }

    // Vector-driven checks that need access to private internals
    // (`build_nonce_iv`, the private nonce counter). The handshake, transport
    // transcript and error cases are in `tests/noise.rs` via the public API.

    fn vectors() -> serde_json::Value {
        let path = format!("{}/tests/vectors/noise.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn nonce_iv_matches_vectors() {
        let v = vectors();
        for case in v["nonce_iv"].as_array().unwrap() {
            let nonce: u64 = case["nonce"].as_str().unwrap().parse().unwrap();
            assert_eq!(
                hex::encode(build_nonce_iv(nonce)),
                case["hex"].as_str().unwrap()
            );
        }
        // The reference's `build_nonce_iv` range errors are type-enforced here:
        // a negative or >= 2^64 nonce is not a representable `u64`.
        assert!(!v["nonce_iv_errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn cipher_state_matches_vectors() {
        let v = vectors();
        let cs = &v["cipher_state"];
        let key = hex::decode(cs["key"].as_str().unwrap()).unwrap();

        // Unkeyed pass-through.
        let mut unkeyed = CipherState::new();
        let out = unkeyed
            .encrypt_with_ad(b"ad", b"plain passes through")
            .unwrap();
        assert_eq!(
            hex::encode(&out),
            cs["unkeyed_passthrough"].as_str().unwrap()
        );

        // Keyed encryption sequence (advancing nonce).
        let mut enc = CipherState::new();
        enc.initialize_key(&key).unwrap();
        for case in cs["encrypt"].as_array().unwrap() {
            let ad = hex::decode(case["ad"].as_str().unwrap()).unwrap();
            let pt = hex::decode(case["plaintext"].as_str().unwrap()).unwrap();
            let ct = enc.encrypt_with_ad(&ad, &pt).unwrap();
            assert_eq!(hex::encode(&ct), case["ciphertext"].as_str().unwrap());
        }

        // Bad key length.
        let mut bad = CipherState::new();
        assert_eq!(
            bad.initialize_key(b"short").unwrap_err().to_string(),
            cs["bad_key_error"].as_str().unwrap()
        );

        // Tampered decrypt poisons the state.
        let mut dec = CipherState::new();
        dec.initialize_key(&key).unwrap();
        let first = hex::decode(cs["encrypt"][0]["ciphertext"].as_str().unwrap()).unwrap();
        dec.decrypt_with_ad(b"", &first).unwrap();
        let mut tampered = hex::decode(cs["encrypt"][1]["ciphertext"].as_str().unwrap()).unwrap();
        tampered[0] ^= 1;
        assert_eq!(
            dec.decrypt_with_ad(b"", &tampered).unwrap_err().to_string(),
            cs["decrypt_tampered_error"].as_str().unwrap()
        );
        assert_eq!(
            dec.decrypt_with_ad(b"", &first).unwrap_err().to_string(),
            cs["decrypt_after_poison_error"].as_str().unwrap()
        );

        // Nonce exhaustion.
        let ex = &cs["nonce_exhaustion"];
        let mut cipher = CipherState::new();
        cipher.initialize_key(&key).unwrap();
        cipher.nonce = ex["start_nonce"].as_str().unwrap().parse().unwrap();
        let last = cipher.encrypt_with_ad(b"", b"last").unwrap();
        assert_eq!(hex::encode(&last), ex["last_ciphertext"].as_str().unwrap());
        assert_eq!(
            cipher
                .encrypt_with_ad(b"", b"one too many")
                .unwrap_err()
                .to_string(),
            ex["error"].as_str().unwrap()
        );
        assert_eq!(
            cipher
                .encrypt_with_ad(b"", b"after")
                .unwrap_err()
                .to_string(),
            ex["then_error"].as_str().unwrap()
        );
    }
}
