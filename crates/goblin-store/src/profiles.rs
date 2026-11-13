//! Profiles and credentials, compatible with the Python SDK's state directory.
//!
//! Each profile directory is a valid `MUSEGADGET_STATE_DIR` for the Python SDK: `identity.json`
//! (`{"mac"}`), `pairing.json` (token record) and `sdk_token`. Field names and validation
//! mirror `config.py`, `identity.py` and `pair.py` of the SDK. Secrets are redacted in `Debug`
//! and `Display`.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fsutil::{
    self, create_private_dir, read_json, remove_file, write_atomic, write_json_atomic,
};
use crate::{Layout, StoreError};

const IDENTITY_FILE: &str = "identity.json";
const PAIRING_FILE: &str = "pairing.json";
const SDK_TOKEN_FILE: &str = "sdk_token";
const CONSENT_FILE: &str = "consent.json";
/// Left in the data directory once the device-wide consent has been copied into the profiles.
const CONSENT_MIGRATED: &str = "consent.json.migrated";
const MAX_NAME: usize = 32;

/// Checks a profile name: 1 to 32 ASCII letters, digits, `_` or `-`. This accepts every name the
/// existing Brick tools can create (they forbid `/` and `.`) and keeps the name usable as a
/// directory and socket file name.
pub fn validate_profile_name(name: &str) -> Result<(), StoreError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidName(name.to_owned()))
    }
}

/// A secret string whose `Debug` and `Display` never show the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself. Use only to put it on the wire or into a file.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// An SDK token as issued by gadgets.muse.ai: `mgst_` plus 43 base64url characters whose last
/// character is canonical.
#[derive(Clone, PartialEq, Eq)]
pub struct SdkToken(Secret);

impl SdkToken {
    /// Validates `text` (surrounding whitespace is ignored).
    pub fn parse(text: &str) -> Result<Self, StoreError> {
        let t = text.trim();
        let body = t.strip_prefix("mgst_").ok_or(StoreError::InvalidSdkToken)?;
        let valid = body.len() == 43
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && body
                .bytes()
                .last()
                .is_some_and(|b| b"AEIMQUYcgkosw048".contains(&b));
        if valid {
            Ok(Self(Secret::new(t)))
        } else {
            Err(StoreError::InvalidSdkToken)
        }
    }

    /// The part gadgets.muse.ai displays (first 12 characters).
    pub fn hint(&self) -> &str {
        &self.0.expose()[..12]
    }

    /// The token value.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl fmt::Debug for SdkToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SdkToken(<redacted>)")
    }
}

/// The stable device identity: a random locally administered MAC-shaped value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Lower-case colon-separated MAC.
    pub mac: String,
}

fn valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|p| {
            p.len() == 2
                && p.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

impl Identity {
    /// Builds an identity from random bytes, forcing a unicast, locally administered address.
    pub fn from_random(mut octets: [u8; 6]) -> Self {
        octets[0] = (octets[0] & 0xFC) | 0x02;
        let mac = octets
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":");
        Self { mac }
    }

    /// Last six hex digits of the MAC.
    pub fn suffix(&self) -> String {
        self.mac.replace(':', "")[6..].to_owned()
    }

    /// `homelink-<suffix>`.
    pub fn node_id(&self) -> String {
        format!("homelink-{}", self.suffix())
    }

    /// `hatch-link:<mac>`.
    pub fn device_id(&self) -> String {
        format!("hatch-link:{}", self.mac)
    }

    /// `MuseGadget<SUFFIX>` (no separator, upper case).
    pub fn ble_name(&self) -> String {
        format!("MuseGadget{}", self.suffix().to_uppercase())
    }
}

/// Saved device pairing (`pairing.json`). Unknown fields are kept in `extra` so that records
/// written by the Python SDK survive a rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct Pairing {
    /// Short-lived device access token.
    pub access_token: Secret,
    /// Refresh token.
    pub refresh_token: Secret,
    /// `token_type`, `"device"` as written by the SDK.
    pub token_type: Option<String>,
    /// Account user name.
    pub username: Option<String>,
    /// `api_url`.
    pub api_url: Option<String>,
    /// `api_url_v2` (empty string means the default Muse API).
    pub api_url_v2: Option<String>,
    /// `noise_host`.
    pub noise_host: Option<String>,
    /// Unix seconds when `access_token` was saved; 0 for older records.
    pub access_token_saved_at: u64,
    /// First 12 characters of the SDK token the pairing was made with.
    pub sdk_token_hint: Option<String>,
    /// Any other fields, preserved verbatim.
    pub extra: Map<String, Value>,
}

const KNOWN_PAIRING_KEYS: [&str; 9] = [
    "access_token",
    "refresh_token",
    "token_type",
    "username",
    "api_url",
    "api_url_v2",
    "noise_host",
    "access_token_saved_at",
    "sdk_token_hint",
];

impl Pairing {
    /// A fresh record with the two tokens and the save time.
    pub fn new(access: Secret, refresh: Secret, saved_at: u64) -> Self {
        Self {
            access_token: access,
            refresh_token: refresh,
            token_type: Some("device".into()),
            username: None,
            api_url: None,
            api_url_v2: None,
            noise_host: None,
            access_token_saved_at: saved_at,
            sdk_token_hint: None,
            extra: Map::new(),
        }
    }

    /// Parses and validates a record like `config.pairing_problem` does: both tokens must be
    /// non-blank strings and the timestamp, when present, a finite non-negative number.
    pub fn from_value(value: &Value) -> Result<Self, StoreError> {
        let obj = value
            .as_object()
            .ok_or(StoreError::InvalidPairing("not an object"))?;
        let token = |key: &str| -> Result<Secret, StoreError> {
            match obj.get(key).and_then(Value::as_str) {
                Some(s) if !s.trim().is_empty() => Ok(Secret::new(s)),
                _ => Err(StoreError::InvalidPairing("lacks device tokens")),
            }
        };
        let access_token = token("access_token")?;
        let refresh_token = token("refresh_token")?;
        let saved_at = match obj.get("access_token_saved_at") {
            None => 0,
            Some(Value::Number(n)) => match n.as_u64() {
                Some(u) => u,
                None => match n.as_f64() {
                    Some(f) if f.is_finite() && f >= 0.0 => f as u64,
                    _ => return Err(StoreError::InvalidPairing("invalid token timestamp")),
                },
            },
            Some(_) => return Err(StoreError::InvalidPairing("invalid token timestamp")),
        };
        let text = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            access_token,
            refresh_token,
            token_type: text("token_type"),
            username: text("username"),
            api_url: text("api_url"),
            api_url_v2: text("api_url_v2"),
            noise_host: text("noise_host"),
            access_token_saved_at: saved_at,
            sdk_token_hint: text("sdk_token_hint"),
            extra: obj
                .iter()
                .filter(|(k, _)| !KNOWN_PAIRING_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        })
    }

    /// The JSON record, including preserved extra fields.
    pub fn to_value(&self) -> Value {
        let mut m = self.extra.clone();
        m.insert(
            "access_token".into(),
            Value::String(self.access_token.expose().into()),
        );
        m.insert(
            "refresh_token".into(),
            Value::String(self.refresh_token.expose().into()),
        );
        for (k, v) in [
            ("token_type", &self.token_type),
            ("username", &self.username),
            ("api_url", &self.api_url),
            ("api_url_v2", &self.api_url_v2),
            ("noise_host", &self.noise_host),
            ("sdk_token_hint", &self.sdk_token_hint),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), Value::String(v.clone()));
            }
        }
        m.insert(
            "access_token_saved_at".into(),
            Value::from(self.access_token_saved_at),
        );
        Value::Object(m)
    }
}

/// What `profile.list` needs to know about a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSummary {
    /// Profile name.
    pub name: String,
    /// A valid `pairing.json` exists.
    pub paired: bool,
    /// A valid `sdk_token` exists.
    pub has_sdk_token: bool,
}

/// All profiles under the data directory and the `active` pointer.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    layout: Layout,
}

impl ProfileStore {
    /// The data directory layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Opens the store, creating `profiles/` (mode 0700) if needed.
    pub fn open(layout: Layout) -> Result<Self, StoreError> {
        create_private_dir(&layout.profiles_dir())?;
        Ok(Self { layout })
    }

    fn dir_of(&self, name: &str) -> PathBuf {
        self.layout.profiles_dir().join(name)
    }

    /// Names of all profile directories, sorted.
    pub fn names(&self) -> Result<Vec<String>, StoreError> {
        let dir = self.layout.profiles_dir();
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| StoreError::io(&dir, e))? {
            let entry = entry.map_err(|e| StoreError::io(&dir, e))?;
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if let (true, Some(name)) = (is_dir, entry.file_name().to_str()) {
                if validate_profile_name(name).is_ok() {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Summaries of all profiles, sorted by name.
    pub fn list(&self) -> Result<Vec<ProfileSummary>, StoreError> {
        let mut out = Vec::new();
        for name in self.names()? {
            out.push(self.profile(&name)?.summary());
        }
        Ok(out)
    }

    /// Whether a profile exists. Names that differ only in case count as the same profile
    /// because the SD card is FAT.
    pub fn exists(&self, name: &str) -> Result<bool, StoreError> {
        Ok(self.names()?.iter().any(|n| n.eq_ignore_ascii_case(name)))
    }

    /// Creates an empty, unpaired profile.
    pub fn create(&self, name: &str) -> Result<Profile, StoreError> {
        validate_profile_name(name)?;
        if self.exists(name)? {
            return Err(StoreError::AlreadyExists(name.to_owned()));
        }
        create_private_dir(&self.dir_of(name))?;
        let profile = self.profile(name)?;
        // A new profile starts with the least: files in its own folder, nothing else.
        write_json_atomic(
            &profile.consent_file(),
            &goblin_ipc::types::Consent::for_new_profile(),
            0o644,
        )?;
        Ok(profile)
    }

    /// Gives every profile its own consent file. Consent used to be one file for the whole
    /// device: the first time this runs, each profile without a file of its own gets what
    /// the device had (all on, if it was never changed), so nothing Muse could do before
    /// is taken away or added. After that a profile found without a file (made by another
    /// tool) gets what a new profile gets.
    pub fn migrate_consent(&self) -> Result<(), StoreError> {
        use goblin_ipc::types::Consent;
        let device = self.layout.consent_file();
        let marker = self.layout.root().join(CONSENT_MIGRATED);
        let first_time = !marker.exists();
        let inherited = if first_time {
            read_json::<Consent>(&device)
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            Consent::for_new_profile()
        };
        for name in self.names()? {
            let file = self.profile(&name)?.consent_file();
            if !file.exists() {
                write_json_atomic(&file, &inherited, 0o644)?;
            }
        }
        if first_time {
            if device.exists() {
                fs::rename(&device, &marker).map_err(|e| StoreError::io(&marker, e))?;
            } else {
                write_atomic(&marker, b"{}\n", 0o644)?;
            }
        }
        Ok(())
    }

    /// The handle for an existing profile (exact name).
    pub fn profile(&self, name: &str) -> Result<Profile, StoreError> {
        validate_profile_name(name)?;
        let dir = self.dir_of(name);
        if !dir.is_dir() {
            return Err(StoreError::NotFound(format!("profile {name}")));
        }
        Ok(Profile {
            name: name.to_owned(),
            dir,
        })
    }

    /// Renames a profile, keeping `active` pointing at it if it was active.
    pub fn rename(&self, old: &str, new: &str) -> Result<(), StoreError> {
        validate_profile_name(new)?;
        let from = self.profile(old)?;
        let clash = self
            .names()?
            .iter()
            .any(|n| n.eq_ignore_ascii_case(new) && n != old);
        if clash || (new == old) {
            return Err(StoreError::AlreadyExists(new.to_owned()));
        }
        let was_active = self.active().as_deref() == Some(old);
        let to = self.dir_of(new);
        fs::rename(&from.dir, &to).map_err(|e| StoreError::io(&to, e))?;
        if was_active {
            if let Err(error) = self.set_active(new) {
                // Callers restore related files on failure, so keep the profile's old name.
                fs::rename(&to, &from.dir).map_err(|rollback| {
                    StoreError::io(
                        &from.dir,
                        std::io::Error::other(format!(
                            "{error}; restoring profile failed: {rollback}"
                        )),
                    )
                })?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Deletes a profile and everything in it. The active profile cannot be deleted.
    pub fn delete(&self, name: &str) -> Result<(), StoreError> {
        let p = self.profile(name)?;
        if self.active().as_deref() == Some(name) {
            return Err(StoreError::ActiveProfile(name.to_owned()));
        }
        fs::remove_dir_all(&p.dir).map_err(|e| StoreError::io(&p.dir, e))
    }

    /// The active profile: the name in the `active` file if it is valid and exists.
    pub fn active(&self) -> Option<String> {
        let text = fs::read_to_string(self.layout.active_file()).ok()?;
        let name = text.trim();
        (validate_profile_name(name).is_ok() && self.dir_of(name).is_dir()).then(|| name.to_owned())
    }

    /// Makes `name` the active profile.
    pub fn set_active(&self, name: &str) -> Result<(), StoreError> {
        self.profile(name)?;
        write_atomic(
            &self.layout.active_file(),
            format!("{name}\n").as_bytes(),
            0o644,
        )
    }
}

/// One profile directory.
#[derive(Debug, Clone)]
pub struct Profile {
    name: String,
    dir: PathBuf,
}

impl Profile {
    /// The profile name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The directory (usable as the Python SDK's state dir).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<profile>/chats`.
    pub fn chats_dir(&self) -> PathBuf {
        self.dir.join("chats")
    }

    /// `<profile>/notifications.json`.
    pub fn notifications_file(&self) -> PathBuf {
        self.dir.join("notifications.json")
    }

    /// `<profile>/commands.json`.
    pub fn commands_file(&self) -> PathBuf {
        self.dir.join("commands.json")
    }

    /// `<profile>/consent.json`: what Muse may do on the device for this profile.
    pub fn consent_file(&self) -> PathBuf {
        self.dir.join(CONSENT_FILE)
    }

    /// Loads the identity, creating it on first use. A missing or malformed file gets a fresh
    /// identity (a malformed one is kept as `identity.json.corrupt`); an unreadable file is an
    /// error so that an identity is never replaced just because it cannot be read.
    pub fn identity(&self) -> Result<Identity, StoreError> {
        let path = self.dir.join(IDENTITY_FILE);
        match read_json::<Value>(&path) {
            Ok(Some(v)) => {
                if let Some(mac) = v
                    .get("mac")
                    .and_then(Value::as_str)
                    .filter(|m| valid_mac(m))
                {
                    return Ok(Identity {
                        mac: mac.to_owned(),
                    });
                }
                fsutil::quarantine(&path);
            }
            Ok(None) => {}
            Err(StoreError::Corrupt { .. }) => fsutil::quarantine(&path),
            Err(e) => return Err(e),
        }
        let identity = Identity::from_random(random_bytes(&path)?);
        write_json_atomic(&path, &serde_json::json!({ "mac": identity.mac }), 0o600)?;
        Ok(identity)
    }

    /// Loads and validates `pairing.json`; `Ok(None)` when absent.
    pub fn pairing(&self) -> Result<Option<Pairing>, StoreError> {
        let path = self.dir.join(PAIRING_FILE);
        match read_json::<Value>(&path)? {
            Some(v) => Pairing::from_value(&v).map(Some),
            None => Ok(None),
        }
    }

    /// Saves the pairing record atomically with mode 0600.
    pub fn save_pairing(&self, pairing: &Pairing) -> Result<(), StoreError> {
        write_json_atomic(&self.dir.join(PAIRING_FILE), &pairing.to_value(), 0o600)
    }

    /// Replaces the token pair after a refresh, keeping every other field (like `service.py`).
    pub fn rotate_tokens(
        &self,
        access: Secret,
        refresh: Secret,
        now: u64,
    ) -> Result<Pairing, StoreError> {
        let mut p = self
            .pairing()?
            .ok_or_else(|| StoreError::NotFound("pairing".into()))?;
        p.access_token = access;
        p.refresh_token = refresh;
        p.access_token_saved_at = now;
        self.save_pairing(&p)?;
        Ok(p)
    }

    /// Removes the pairing (unpair or revocation).
    pub fn delete_pairing(&self) -> Result<(), StoreError> {
        remove_file(&self.dir.join(PAIRING_FILE))
    }

    /// The SDK token if the file exists; an invalid token is an error.
    pub fn sdk_token(&self) -> Result<Option<SdkToken>, StoreError> {
        let path = self.dir.join(SDK_TOKEN_FILE);
        match fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Ok(None),
            Ok(text) => SdkToken::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io(path, e)),
        }
    }

    /// Saves the SDK token (mode 0600).
    pub fn save_sdk_token(&self, token: &SdkToken) -> Result<(), StoreError> {
        write_atomic(
            &self.dir.join(SDK_TOKEN_FILE),
            format!("{}\n", token.expose()).as_bytes(),
            0o600,
        )
    }

    /// Reads a token from `source`, validates and saves it, then deletes `source`. The source
    /// is left alone when the token is invalid.
    pub fn import_sdk_token_file(&self, source: &Path) -> Result<SdkToken, StoreError> {
        let text = fs::read_to_string(source).map_err(|e| StoreError::io(source, e))?;
        let token = SdkToken::parse(&text)?;
        self.save_sdk_token(&token)?;
        remove_file(source)?;
        Ok(token)
    }

    /// Removes the SDK token.
    pub fn delete_sdk_token(&self) -> Result<(), StoreError> {
        remove_file(&self.dir.join(SDK_TOKEN_FILE))
    }

    /// Summary for `profile.list`. Unreadable files count as absent.
    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            name: self.name.clone(),
            paired: matches!(self.pairing(), Ok(Some(_))),
            has_sdk_token: matches!(self.sdk_token(), Ok(Some(_))),
        }
    }
}

fn random_bytes(path: &Path) -> Result<[u8; 6], StoreError> {
    let mut buf = [0u8; 6];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| StoreError::io(path, e))?;
    Ok(buf)
}
