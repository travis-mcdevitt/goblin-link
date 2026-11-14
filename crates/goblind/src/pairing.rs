//! Pairing a profile with the Muse app over Bluetooth, from inside the daemon.
//!
//! `pair.start` opens a setup window: the radio is brought up (the platform's hook), the
//! setup service is registered with BlueZ and advertised, and the Muse app's messages are
//! handed to [`muse_pair::setup::SetupController`]. When the app sends the device
//! credentials they are checked against the Muse API and saved into the profile; a pairing
//! of the profile being served restarts the daemon so it connects.
//!
//! The window closes on success, on `pair.cancel`, or when its time is up, and the adapter
//! is put back as it was.

use std::sync::Arc;
use std::time::Duration;

use goblin_ipc::types::{PairPhase, PairStateEvent};
use goblin_ipc::Event;
use goblin_store::profiles::{Pairing, Profile, Secret};
use muse_pair::ble::{BleEvent, BlePeripheral};
use muse_pair::session::{DeviceIdentity, Entropy, PairingSession};
use muse_pair::setup::{Action, Credentials, Network, SetupController};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use crate::daemon::DaemonState;

/// How long the window stays open unless the request says otherwise.
pub const DEFAULT_WINDOW_S: u32 = 600;
/// Time for the last status to reach the app before the link goes down.
const SHUTDOWN_DELAY: Duration = Duration::from_millis(1500);
const CURRENT_CONNECTION_LABEL: &str = "Use current connection";

/// An open setup window.
pub struct PairHandle {
    /// The profile being paired.
    pub profile: String,
    /// Wakes the window to close it.
    pub cancel: Arc<Notify>,
}

/// The current network. Connectivity is checked before opening the window and at
/// confirmation; failure closes pairing with an actionable error on the handheld.
struct ReportedNetwork {
    ssid: Option<String>,
}

impl Network for ReportedNetwork {
    fn is_online(&self) -> bool {
        true
    }
    fn current_connection_entry(&self) -> Value {
        // Marked open so the app skips the password field; whatever comes back is ignored
        // (the operating system owns Wi-Fi). The real name lets the app preselect it.
        json!({
            "ssid": self.ssid.as_deref().unwrap_or(CURRENT_CONNECTION_LABEL),
            "rssi": -40,
            "secure": false,
        })
    }
}

/// Whether the Muse API host accepts a connection.
async fn api_reachable() -> bool {
    let host = muse_link::api::API_BASE
        .trim_start_matches("https://")
        .to_owned();
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect((host.as_str(), 443)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// The name of the Wi-Fi network in use, when `wpa_cli` can say.
async fn wifi_name() -> Option<String> {
    let ifaces = std::fs::read_dir("/sys/class/net").ok()?;
    for iface in ifaces.flatten() {
        if !iface.path().join("wireless").is_dir() {
            continue;
        }
        let out = tokio::process::Command::new("wpa_cli")
            .arg("-i")
            .arg(iface.file_name())
            .arg("status")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(ssid) = text.lines().find_map(|l| l.strip_prefix("ssid=")) {
            // Names with escapes are left to the label: guessing wrong would mislead.
            if !ssid.is_empty() && !ssid.contains('\\') {
                return Some(ssid.to_owned());
            }
        }
    }
    None
}

pub(crate) fn publish(
    state: &DaemonState,
    profile: &str,
    phase: PairPhase,
    detail: Option<String>,
) {
    let _ = state.bus.publish(&Event::PairState(PairStateEvent {
        profile: Some(profile.to_owned()),
        state: phase,
        detail,
    }));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs the platform's Bluetooth hook off the async threads. `Ok` when there is no hook.
async fn radio(state: &Arc<DaemonState>, args: Vec<String>) -> Result<(), String> {
    let Some(platform) = state.platform.clone().filter(|p| p.has_bluetooth_hook()) else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        platform.bluetooth(&args).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
}

/// Checks the device token with the Muse API and builds the record to save.
async fn verify(credentials: &Credentials, sdk_hint: Option<String>) -> Result<Pairing, ()> {
    let https = |url: &str| {
        if url.starts_with("https://") {
            url.to_owned()
        } else {
            String::new()
        }
    };
    let api_url = https(&credentials.api_url);
    let api_url_v2 = https(&credentials.api_url_v2);
    let root = muse_link::api::api_root(&api_url_v2);
    match muse_link::api::fetch_vms(&credentials.access_token, &root).await {
        Ok(vms) if !vms.is_empty() => {}
        Ok(_) => {
            log::warn!("device token check failed: the account has no VM");
            return Err(());
        }
        Err(e) => {
            log::warn!("device token check failed: {e}");
            return Err(());
        }
    }
    let saved_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut pairing = Pairing::new(
        Secret::new(credentials.access_token.clone()),
        Secret::new(credentials.refresh_token.clone()),
        saved_at,
    );
    pairing.username = Some(credentials.username.clone());
    pairing.api_url = Some(api_url);
    pairing.api_url_v2 = Some(api_url_v2);
    pairing.noise_host = Some(credentials.noise_host.clone());
    pairing.sdk_token_hint = sdk_hint;
    Ok(pairing)
}

/// The file an owner leaves on the card to give a profile its SDK token.
pub const SDK_TOKEN_DROP: &str = "sdk_token.txt";

/// How every "there is no token" message starts, so the app can recognise it.
pub const NO_SDK_TOKEN: &str = "No SDK token";

/// Gives `profile` the SDK token from `<homes>/sdk_token.txt`, removing that file. Fails,
/// with words for the owner, when there is no such file or it does not hold a token.
pub fn take_dropped_sdk_token(
    profile: &Profile,
    homes: Option<&std::path::Path>,
) -> Result<(), String> {
    let Some(homes) = homes else {
        return Err(format!(
            "{NO_SDK_TOKEN} for this profile. Get one at gadgets.muse.ai/settings/sdk-tokens and add it with: \
             goblinctl profile sdk-token <file>"
        ));
    };
    let folder = homes
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| homes.display().to_string());
    let file = homes.join(SDK_TOKEN_DROP);
    if !file.is_file() {
        return Err(format!(
            "{NO_SDK_TOKEN} for this profile. Get one at gadgets.muse.ai/settings/sdk-tokens \
             and save it as {SDK_TOKEN_DROP} in the {folder} folder on the SD card, then pair \
             again."
        ));
    }
    profile
        .import_sdk_token_file(&file)
        .map(|_| ())
        .map_err(|_| {
            format!(
                "{NO_SDK_TOKEN}. {SDK_TOKEN_DROP} in the {folder} folder is not a valid \
                 token: copy only the token from gadgets.muse.ai/settings/sdk-tokens."
            )
        })
}

/// What the caller needs to tell the owner once the window is opening.
pub struct Opening {
    /// The name to choose in the Muse app.
    pub ble_name: String,
    /// Whether the profile has an SDK token to hand to the app.
    pub has_sdk_token: bool,
}

/// Prepares a window for `profile` and returns what to show, plus the future that runs it.
pub fn prepare(
    state: Arc<DaemonState>,
    profile: Profile,
    timeout_s: u32,
    cancel: Arc<Notify>,
) -> Result<(Opening, impl std::future::Future<Output = ()> + Send), String> {
    let identity = profile.identity().map_err(|e| e.to_string())?;
    let sdk_token = profile.sdk_token().map_err(|e| e.to_string())?;
    let opening = Opening {
        ble_name: identity.ble_name(),
        has_sdk_token: sdk_token.is_some(),
    };
    let was_paired = profile.pairing().ok().flatten().is_some();
    let device = DeviceIdentity {
        node_id: identity.node_id(),
        device_id: identity.device_id(),
        mac: identity.mac.clone(),
        firmware_version: state.version.clone(),
    };
    let ble_name = opening.ble_name.clone();
    let run = async move {
        let name = profile.name().to_owned();
        let outcome =
            match network_ready(crate::tokenpage::local_ip(), api_reachable(), &cancel).await {
                Ok(true) => {
                    window(
                        &state,
                        &profile,
                        device,
                        sdk_token.as_ref().map(|t| t.expose().to_owned()),
                        sdk_token.as_ref().map(|t| t.hint().to_owned()),
                        &ble_name,
                        was_paired,
                        Duration::from_secs(u64::from(timeout_s)),
                        cancel,
                    )
                    .await
                }
                Ok(false) => Ok(Closed::Cancelled),
                Err(why) => Err(why.to_owned()),
            };
        state.pair_closed();
        let (phase, detail) = match outcome {
            Ok(Closed::Paired) => (PairPhase::Done, None),
            Ok(Closed::Cancelled) => (PairPhase::Cancelled, None),
            Ok(Closed::TimedOut) => (PairPhase::Timeout, None),
            Ok(Closed::Failed(why)) => (PairPhase::Failed, Some(why.into())),
            Err(why) => {
                log::warn!("pairing '{name}' failed: {why}");
                (PairPhase::Failed, Some(why))
            }
        };
        let paired = phase == PairPhase::Done;
        publish(&state, &name, phase, detail);
        if paired && name == state.active() {
            // The profile being served has credentials now: start again to connect with them.
            tokio::time::sleep(Duration::from_millis(300)).await;
            state.request_restart();
        }
    };
    Ok((opening, run))
}

enum Closed {
    Paired,
    Cancelled,
    TimedOut,
    Failed(&'static str),
}

const MUSE_UNREACHABLE: &str = "Can’t reach Muse. Check your connection and retry.";

async fn network_ready(
    ip: Option<std::net::IpAddr>,
    reachable: impl std::future::Future<Output = bool>,
    cancel: &Notify,
) -> Result<bool, &'static str> {
    if ip.is_none() {
        return Err(crate::tokenpage::NO_NETWORK);
    }
    tokio::select! {
        biased;
        () = cancel.notified() => Ok(false),
        online = reachable => if online { Ok(true) } else { Err(MUSE_UNREACHABLE) },
    }
}

#[allow(clippy::too_many_arguments)]
async fn window(
    state: &Arc<DaemonState>,
    profile: &Profile,
    device: DeviceIdentity,
    sdk_token: Option<String>,
    sdk_hint: Option<String>,
    ble_name: &str,
    was_paired: bool,
    timeout: Duration,
    cancel: Arc<Notify>,
) -> Result<Closed, String> {
    radio(state, vec!["up".into()]).await?;
    // Every way out of the window, failures too, stops the advertising the hook set up.
    let closed = open_window(
        state, profile, device, sdk_token, sdk_hint, ble_name, was_paired, timeout, cancel,
    )
    .await;
    if let Err(e) = radio(state, vec!["down".into()]).await {
        log::warn!("could not stop Bluetooth advertising: {e}");
    }
    closed
}

/// [`window`] with the adapter up.
#[allow(clippy::too_many_arguments)]
async fn open_window(
    state: &Arc<DaemonState>,
    profile: &Profile,
    device: DeviceIdentity,
    sdk_token: Option<String>,
    sdk_hint: Option<String>,
    ble_name: &str,
    was_paired: bool,
    timeout: Duration,
    cancel: Arc<Notify>,
) -> Result<Closed, String> {
    let name = profile.name();

    let ssid = tokio::select! {
        biased;
        () = cancel.notified() => return Ok(Closed::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(2), wifi_name()) => result.ok().flatten(),
    };
    let mut controller = SetupController::new(
        PairingSession::new(device, sdk_token, Entropy::default()),
        Box::new(ReportedNetwork { ssid }),
    );

    let (tx, mut events) = mpsc::channel(64);
    let ble = BlePeripheral::open(ble_name, was_paired, tx)
        .await
        .map_err(|e| e.to_string())?;
    // Some controllers need the advertising data set by hand (see the Brick's hook).
    let programmed = match muse_pair::ble::legacy_advertising_payload(ble_name, was_paired) {
        Some((adv, scan)) => radio(state, vec!["advertise".into(), hex(&adv), hex(&scan)]).await,
        None => Err("Bluetooth name too long".into()),
    };
    if let Err(why) = programmed {
        ble.close().await;
        return Err(why);
    }
    log::info!("pairing window open for '{name}' as {ble_name}");
    publish(
        state,
        name,
        PairPhase::Advertising,
        Some(ble_name.to_owned()),
    );

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut closed = Closed::TimedOut;
    'window: loop {
        let event = tokio::select! {
            () = &mut deadline => break,
            () = cancel.notified() => { closed = Closed::Cancelled; break }
            e = events.recv() => match e { Some(e) => e, None => break },
        };
        let mut actions = match event {
            BleEvent::Write { data, mtu } => {
                if let Some(mtu) = mtu {
                    controller.set_mtu(usize::from(mtu));
                }
                controller.on_write(&data)
            }
            BleEvent::Subscribed => {
                publish(state, name, PairPhase::Connected, None);
                Vec::new()
            }
            BleEvent::Disconnected => {
                log::info!("setup client disconnected; clearing the pairing session");
                controller.on_disconnect();
                publish(
                    state,
                    name,
                    PairPhase::Advertising,
                    Some(ble_name.to_owned()),
                );
                Vec::new()
            }
        };
        // Acting on one action can produce more (checking credentials ends in a status).
        while !actions.is_empty() {
            let mut next = Vec::new();
            for action in actions {
                match action {
                    Action::Send(packets) => ble.notify(&packets).await,
                    Action::Disconnect(ms) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        ble.disconnect().await;
                    }
                    Action::Confirmed => {
                        // Fresh, for the Wi-Fi step the app runs next.
                        match network_ready(crate::tokenpage::local_ip(), api_reachable(), &cancel)
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                closed = Closed::Cancelled;
                                break 'window;
                            }
                            Err(why) => {
                                closed = Closed::Failed(why);
                                break 'window;
                            }
                        }
                        publish(state, name, PairPhase::Confirming, None);
                    }
                    Action::Provision(credentials, generation) => {
                        publish(state, name, PairPhase::Registering, None);
                        let outcome = match verify(&credentials, sdk_hint.clone()).await {
                            Err(()) => Err("auth_failed"),
                            // Saved only if the session the credentials came through is
                            // still the current one.
                            Ok(pairing) if controller.may_commit(generation) => {
                                profile.save_pairing(&pairing).map_err(|e| {
                                    log::warn!("could not save the pairing: {e}");
                                    "error_storage"
                                })
                            }
                            Ok(_) => Err("error_storage"),
                        };
                        let paired = outcome.is_ok();
                        next.extend(controller.provision_finished(generation, outcome));
                        if paired {
                            for action in next.drain(..) {
                                if let Action::Send(packets) = action {
                                    ble.notify(&packets).await;
                                }
                            }
                            log::info!("profile '{name}' paired");
                            tokio::time::sleep(SHUTDOWN_DELAY).await;
                            closed = Closed::Paired;
                            break 'window;
                        }
                    }
                }
            }
            actions = next;
        }
    }
    log::info!(
        "pairing window for '{name}' closing: {}",
        match closed {
            Closed::Paired => "paired",
            Closed::Cancelled => "cancelled",
            Closed::TimedOut => "time is up",
            Closed::Failed(why) => why,
        }
    );
    // A client still connected would keep the adapter from advertising next time.
    ble.disconnect().await;
    ble.close().await;
    Ok(closed)
}

#[cfg(test)]
mod window_tests {
    use super::*;

    #[tokio::test]
    async fn network_gate_reports_the_right_failure_and_can_retry_or_cancel() {
        let cancel = Notify::new();
        let ip = Some("192.168.1.5".parse().unwrap());
        assert_eq!(
            network_ready(None, std::future::pending(), &cancel).await,
            Err(crate::tokenpage::NO_NETWORK)
        );
        assert_eq!(
            network_ready(ip, async { false }, &cancel).await,
            Err(MUSE_UNREACHABLE)
        );
        assert_eq!(network_ready(ip, async { true }, &cancel).await, Ok(true));
        cancel.notify_one();
        assert_eq!(
            network_ready(ip, std::future::pending(), &cancel).await,
            Ok(false)
        );
        assert_eq!(network_ready(ip, async { true }, &cancel).await, Ok(true));
    }

    /// A platform whose Bluetooth hook only writes down what it was asked to do.
    fn logging_platform(dir: &std::path::Path) -> Arc<goblin_platform::Platform> {
        use std::os::unix::fs::PermissionsExt;
        let hooks = dir.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let script = hooks.join("bluetooth.sh");
        let log = dir.join("bluetooth.log");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$1\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            dir.join("platform.toml"),
            "id = \"test\"\n[bluetooth]\nhook = \"hooks/bluetooth.sh\"\n",
        )
        .unwrap();
        Arc::new(goblin_platform::Platform::load(dir).unwrap())
    }

    #[tokio::test]
    async fn a_window_that_cannot_open_puts_the_radio_back_down() {
        let platform_dir = tempfile::tempdir().unwrap();
        let (_tmp, state) = crate::daemon::test_state(Some(logging_platform(platform_dir.path())));
        let profile = state.profiles.profile("test").unwrap();
        let device = DeviceIdentity {
            node_id: "node".into(),
            device_id: "device".into(),
            mac: "00:11:22:33:44:55".into(),
            firmware_version: "0".into(),
        };
        // No system bus where the tests run, so the Bluetooth peripheral cannot open.
        let opened = window(
            &state,
            &profile,
            device,
            None,
            None,
            "Goblin test",
            false,
            Duration::from_secs(1),
            Arc::new(Notify::new()),
        )
        .await;
        assert!(opened.is_err(), "the peripheral opened after all");
        let log = std::fs::read_to_string(platform_dir.path().join("bluetooth.log")).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), ["up", "down"]);
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;
    use goblin_store::{Layout, ProfileStore};

    const TOKEN: &str = "mgst_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPA";

    #[test]
    fn a_dropped_token_file_is_taken_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("work").unwrap();
        let homes = dir.path().join("GoblinLink");
        std::fs::create_dir_all(&homes).unwrap();

        let why = take_dropped_sdk_token(&profile, Some(&homes)).unwrap_err();
        assert!(why.starts_with(NO_SDK_TOKEN), "{why}");
        assert!(
            why.contains("sdk_token.txt") && why.contains("GoblinLink"),
            "{why}"
        );

        let file = homes.join(SDK_TOKEN_DROP);
        std::fs::write(&file, "not a token\n").unwrap();
        let why = take_dropped_sdk_token(&profile, Some(&homes)).unwrap_err();
        assert!(
            why.starts_with(NO_SDK_TOKEN) && why.contains("valid"),
            "{why}"
        );
        assert!(file.exists(), "a bad file is left for the owner to fix");
        assert!(
            !why.contains("not a token"),
            "the file's text is not echoed"
        );

        std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
        take_dropped_sdk_token(&profile, Some(&homes)).unwrap();
        assert!(!file.exists());
        assert!(profile.sdk_token().unwrap().is_some());
    }
}
