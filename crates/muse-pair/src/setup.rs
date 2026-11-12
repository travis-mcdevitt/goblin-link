//! The commands the Muse app sends over Bluetooth while it sets a device up (port of
//! `ble_setup.py`).
//!
//! [`SetupController`] turns each complete message into [`Action`]s for whoever owns the
//! radio and the storage: packets to notify, a disconnect, or credentials to check and save.
//! It does no I/O of its own.

use serde_json::{json, Map, Value};

use crate::framing::{encode_chunks, ChunkAssembler};
use crate::session::{PairingError, PairingSession};

/// Seconds before the link is dropped after an error, so the status gets out first.
pub const DISCONNECT_AFTER_ERROR_MS: u64 = 300;

fn sensitive(action: &str) -> bool {
    matches!(
        action,
        "provision"
            | "provision_v2"
            | "wifi_scan"
            | "ota"
            | "device.ota"
            | "unpair"
            | "set_wifi"
            | "set_auth"
    )
}

fn plaintext_status(status: &str) -> bool {
    matches!(
        status,
        "error_encryption_required"
            | "error_pairing_invalid_hello"
            | "error_pairing_unavailable"
            | "error_pairing_decrypt"
    )
}

/// What the app hands over at the end of setup.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Device access token.
    pub access_token: String,
    /// Device refresh token.
    pub refresh_token: String,
    /// The account's user name.
    pub username: String,
    /// API root (v1).
    pub api_url: String,
    /// API root (v2).
    pub api_url_v2: String,
    /// Host of the Noise endpoint.
    pub noise_host: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Something for the owner of the radio and the storage to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Notify these packets, in order.
    Send(Vec<Vec<u8>>),
    /// Drop the link after this many milliseconds.
    Disconnect(u64),
    /// The app confirmed the pairing.
    Confirmed,
    /// Check these credentials and, if they are good, save them; then call
    /// [`SetupController::provision_finished`] with the generation.
    Provision(Credentials, u64),
}

/// The network as setup sees it. The operating system owns Wi-Fi: this only reports.
pub trait Network: Send {
    /// Whether the device is online.
    fn is_online(&self) -> bool;
    /// The current connection as a `wifi_scan_result` entry.
    fn current_connection_entry(&self) -> Value;
}

/// Handles one setup client at a time.
pub struct SetupController {
    pairing: PairingSession,
    network: Box<dyn Network>,
    assembler: ChunkAssembler,
    plaintext_blocked: bool,
    provisioning: bool,
    mtu: usize,
}

impl SetupController {
    /// A controller for `pairing`.
    pub fn new(pairing: PairingSession, network: Box<dyn Network>) -> Self {
        Self {
            pairing,
            network,
            assembler: ChunkAssembler::default(),
            plaintext_blocked: false,
            provisioning: false,
            mtu: crate::framing::MAX_PACKET_BYTES + 3,
        }
    }

    /// The ATT MTU the client negotiated.
    pub fn set_mtu(&mut self, mtu: usize) {
        self.mtu = mtu;
    }

    /// One GATT write from the app.
    pub fn on_write(&mut self, packet: &[u8]) -> Vec<Action> {
        match self.assembler.feed(packet) {
            Some(message) => self.handle_message(&message, false),
            None => Vec::new(),
        }
    }

    /// The app went away: the session goes with it.
    pub fn on_disconnect(&mut self) {
        self.assembler.reset();
        self.plaintext_blocked = false;
        self.mtu = crate::framing::MAX_PACKET_BYTES + 3;
        self.pairing.reset();
    }

    fn handle_message(&mut self, raw: &[u8], decrypted: bool) -> Vec<Action> {
        let command: Map<String, Value> = match serde_json::from_slice::<Value>(raw) {
            Ok(Value::Object(m)) => m,
            _ => {
                log::warn!("invalid command JSON ({} bytes)", raw.len());
                return self.status("error_invalid_command", 0);
            }
        };
        let action = command
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        log::info!(
            "RX action: {}{}",
            if action.is_empty() { "?" } else { &action },
            if decrypted { " (encrypted)" } else { "" }
        );
        let action = action.as_str();
        if !decrypted && action == "pairing_client_hello" {
            self.handle_hello(&command)
        } else if !decrypted && action == "pairing_encrypted" {
            self.handle_record(&command)
        } else if action == "get_device_info" {
            let info = self.device_info();
            self.send_json(&info)
        } else if !decrypted && self.plaintext_blocked {
            log::warn!("plaintext command ignored after pairing started: {action}");
            Vec::new()
        } else if !decrypted && sensitive(action) {
            self.status("error_encryption_required", 0)
        } else if decrypted && action == "pairing_client_finished" {
            self.handle_client_finished(&command)
        } else if decrypted && sensitive(action) && !self.pairing.confirmed() {
            self.status("error_pairing_confirm_required", 0)
        } else if decrypted && action == "wifi_scan" {
            let networks = if self.network.is_online() {
                vec![self.network.current_connection_entry()]
            } else {
                Vec::new()
            };
            self.send_encrypted(
                &json!({"type": "wifi_scan_result", "networks": networks}),
                0,
            )
        } else if decrypted && action == "provision_v2" {
            self.handle_provision(&command)
        } else {
            self.status("error_unknown_action", 0)
        }
    }

    fn device_info(&mut self) -> Value {
        let mut info = Map::new();
        info.insert("type".into(), json!("device_info"));
        info.insert("node_id".into(), json!(self.pairing.node_id()));
        info.insert("version".into(), json!(self.pairing.firmware_version()));
        info.extend(self.pairing.device_info());
        info.insert("build_sha".into(), json!(""));
        info.insert("network_ready".into(), json!(self.network.is_online()));
        Value::Object(info)
    }

    fn handle_hello(&mut self, command: &Map<String, Value>) -> Vec<Action> {
        match self.pairing.handle_hello(command) {
            Ok(ready) => {
                self.plaintext_blocked = true;
                self.send_json(&ready)
            }
            Err(PairingError(status)) => self.status(status, 0),
        }
    }

    fn handle_record(&mut self, envelope: &Map<String, Value>) -> Vec<Action> {
        match self.pairing.decrypt(envelope) {
            Ok(plaintext) => self.handle_message(plaintext.as_bytes(), true),
            Err(PairingError(status)) => {
                let mut out = self.status(status, 0);
                out.push(Action::Disconnect(DISCONNECT_AFTER_ERROR_MS));
                out
            }
        }
    }

    fn handle_client_finished(&mut self, command: &Map<String, Value>) -> Vec<Action> {
        let generation = self.pairing.handle_client_finished(command);
        if generation == 0 {
            let mut out = self.status("error_pairing_decrypt", 0);
            out.push(Action::Disconnect(DISCONNECT_AFTER_ERROR_MS));
            return out;
        }
        log::info!("pairing confirmed (app consent)");
        let mut out = self.status("pairing_confirmed", generation);
        out.push(Action::Confirmed);
        out
    }

    fn handle_provision(&mut self, command: &Map<String, Value>) -> Vec<Action> {
        let text = |key: &str| {
            command
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let is_text = |key: &str| command.get(key).is_some_and(Value::is_string);
        if !is_text("ssid")
            || !is_text("password")
            || text("access_token").is_empty()
            || text("refresh_token").is_empty()
            || text("token_type") != "device"
        {
            return self.status("error_missing_credentials", 0);
        }
        if self.provisioning {
            return self.status("error_operation_in_progress", 0);
        }
        let generation = self.pairing.mark_provisioning();
        if generation == 0 {
            return self.status("error_pairing_confirm_required", 0);
        }
        self.provisioning = true;
        let credentials = Credentials {
            access_token: text("access_token"),
            refresh_token: text("refresh_token"),
            username: text("username"),
            api_url: text("api_url"),
            api_url_v2: text("api_url_v2"),
            noise_host: text("noise_host"),
        };
        // The operating system owns Wi-Fi: the device is either online already or it is not.
        let mut out = self.status("wifi_connecting", generation);
        if !self.network.is_online() {
            self.pairing.extend_provisioning(generation);
            out.extend(self.status("wifi_failed", generation));
            self.provisioning = false;
            return out;
        }
        out.extend(self.status("wifi_connected", generation));
        out.push(Action::Provision(credentials, generation));
        out
    }

    /// Whether the credentials of `generation` may be saved now: the session they came
    /// through is still the current one and still in time.
    pub fn may_commit(&mut self, generation: u64) -> bool {
        self.pairing.provisioning_valid(generation)
    }

    /// Reports how checking and saving the credentials went. `Ok` tells the app `auth_ok`;
    /// `Err` carries the status to send (`auth_failed`, `error_storage`), and the link is
    /// then dropped.
    pub fn provision_finished(
        &mut self,
        generation: u64,
        outcome: Result<(), &'static str>,
    ) -> Vec<Action> {
        self.provisioning = false;
        match outcome {
            Ok(()) => {
                log::info!("setup complete");
                self.status("auth_ok", generation)
            }
            Err(status) => {
                log::warn!("provisioning failed: {status}");
                let mut out = self.status(status, generation);
                out.push(Action::Disconnect(500));
                out
            }
        }
    }

    fn status(&mut self, status: &str, generation: u64) -> Vec<Action> {
        if let Some(envelope) = self.pairing.encrypt_status(status, generation) {
            log::info!("TX status (encrypted #{}): {status}", envelope["counter"]);
            return self.send_json(&envelope);
        }
        if generation != 0 || self.plaintext_blocked || !plaintext_status(status) {
            log::info!("TX status suppressed: {status}");
            return Vec::new();
        }
        log::info!("TX status: {status}");
        vec![Action::Send(vec![status.as_bytes().to_vec()])]
    }

    fn send_json(&mut self, value: &Value) -> Vec<Action> {
        let text = serde_json::to_string(value).unwrap_or_default();
        match encode_chunks(text.as_bytes(), self.mtu) {
            Some(packets) => vec![Action::Send(packets)],
            None => Vec::new(),
        }
    }

    fn send_encrypted(&mut self, value: &Value, generation: u64) -> Vec<Action> {
        let text = serde_json::to_string(value).unwrap_or_default();
        match self.pairing.encrypt_json(&text, generation) {
            Some(envelope) => self.send_json(&envelope),
            None => Vec::new(),
        }
    }
}
