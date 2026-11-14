//! The device-session manager: token refresh, VM lookup, connect, and
//! reconnect with capped exponential backoff (port of `service.py`).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use goblin_ipc::types::LinkState;
use goblin_store::profiles::{Pairing, Profile, Secret};
use muse_link::{api, LinkError, LinkSession, Outcome, SessionConfig};

use crate::daemon::{DaemonRunner, DaemonState};

const DEFAULT_NOISE_HOST: &str = "hatch.metaaivm.com";
const BACKOFF_BASE_S: f64 = 2.0;
const BACKOFF_MAX_S: f64 = 60.0;
const AUTH_BACKOFF_MIN_S: f64 = 15.0;
const HEALTHY_SESSION_S: f64 = 30.0;
const HEALTHY_RECONNECT_S: f64 = 1.0;
const UNPAIRED_POLL_S: f64 = 30.0;
/// Longest wait between attempts while Muse's side answers with a server error.
const UNANSWERED_RETRY_S: f64 = 10.0;
const TOKEN_REFRESH_AGE_S: u64 = 3 * 3600;
const TOKEN_RETRY_S: f64 = 300.0;

/// Capped exponential backoff with a floor, as in the reference.
#[derive(Debug, Default)]
pub struct Backoff {
    failures: u32,
    floor: f64,
}

impl Backoff {
    /// The next delay (seconds), then advances the failure count.
    pub fn next_delay(&mut self) -> f64 {
        let delay =
            (BACKOFF_BASE_S * 2.0_f64.powi(self.failures.min(16) as i32)).min(BACKOFF_MAX_S);
        self.failures += 1;
        delay.max(self.floor)
    }

    /// Resets the backoff after a healthy session.
    pub fn reset(&mut self) {
        self.failures = 0;
        self.floor = 0.0;
    }

    fn set_floor(&mut self, floor: f64) {
        self.floor = floor;
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Connects one session for `config`, registers, serves it until it ends or
/// shutdown is requested, and returns how it ended. Used by the production loop
/// and, with a simulated-VM config, by the integration tests.
pub async fn run_one_session(state: &Arc<DaemonState>, mut config: SessionConfig) -> Outcome {
    config.device = state.device();
    config.invoke_slots = Some(state.invoke_slots.clone());
    config.register_device = true;
    // Kept so a call can open its own voice session to the same VM.
    state.set_session_config(Some(config.clone()));

    state.set_link_state(LinkState::Connecting, None);
    let runner = Arc::new(DaemonRunner::new(state.clone()));
    let session = match LinkSession::connect(config, runner).await {
        Ok(s) => s,
        Err(LinkError::UpgradeRejected(401)) => {
            state.set_link_state(LinkState::AuthFailed, Some("VM rejected the token".into()));
            return Outcome::AuthRejected;
        }
        Err(LinkError::UpgradeRejected(status)) if status >= 500 => {
            // The gateway answered but the VM behind it did not (asleep, restarting): that
            // is Muse's side being away, not this device being refused.
            state.set_link_state(
                LinkState::Reconnecting,
                Some(format!("Muse is not answering (HTTP {status})")),
            );
            state
                .unanswered
                .store(true, std::sync::atomic::Ordering::Release);
            return Outcome::Closed;
        }
        Err(LinkError::UpgradeRejected(status)) => {
            state.set_link_state(
                LinkState::AuthFailed,
                Some(format!("VM refused: HTTP {status}")),
            );
            return Outcome::Forbidden;
        }
        Err(e) => {
            state.set_link_state(LinkState::Reconnecting, Some(e.to_string()));
            return Outcome::Closed;
        }
    };

    state.set_session(Some(session.clone()));
    state.set_link_state(LinkState::Registering, None);
    match session.wait_registered().await {
        Ok(()) => {
            state.set_link_state(LinkState::Online, None);
            state.send_owed_reply();
            let id_state = state.clone();
            tokio::spawn(async move { id_state.refresh_identity().await });
        }
        Err(e) => {
            state.set_session(None);
            state.set_link_state(LinkState::AuthFailed, Some(e.to_string()));
            return Outcome::Forbidden;
        }
    }

    let outcome = tokio::select! {
        o = session.wait_closed() => o,
        () = state.wait_shutdown() => {
            session.request_stop();
            session.wait_closed().await;
            Outcome::Stopped
        }
    };
    state.set_session(None);
    if outcome != Outcome::Stopped {
        state.set_link_state(LinkState::Offline, None);
    }
    outcome
}

/// The production reconnect loop. Fetches VMs with the device token and keeps a
/// session to the default VM, rotating tokens and backing off on failure.
///
/// This path reaches the real Muse device API and VM; it is exercised on-device
/// by the lead, not by the host test suite.
pub async fn run(state: Arc<DaemonState>) {
    let mut backoff = Backoff::default();
    let mut token_rejected = false;
    let mut unsaved: Option<Unsaved> = None;
    loop {
        if is_shutdown(&state) {
            return;
        }
        let name = state.active();
        let profile = match state.profiles.profile(&name) {
            Ok(p) => p,
            Err(_) => {
                sleep_or_stop(&state, UNPAIRED_POLL_S).await;
                continue;
            }
        };
        let Some(pairing) = current_pairing(&profile, &mut unsaved) else {
            state.set_link_state(LinkState::Unpaired, None);
            sleep_or_stop(&state, UNPAIRED_POLL_S).await;
            continue;
        };

        let pairing = match maybe_refresh(&state, &profile, pairing, false, &mut unsaved).await {
            Some(p) => p,
            None => {
                sleep_or_stop(&state, TOKEN_RETRY_S).await;
                continue;
            }
        };

        let api_root = api::api_root(pairing.api_url_v2.as_deref().unwrap_or(""));
        let access = pairing.access_token.expose().to_owned();
        let vms = match api::fetch_vms(&access, &api_root).await {
            Ok(v) => v,
            Err(e) if e.is_unauthorized() => {
                log::warn!("device token rejected by the API; refreshing");
                if maybe_refresh(&state, &profile, pairing.clone(), true, &mut unsaved)
                    .await
                    .is_none()
                {
                    sleep_or_stop(&state, TOKEN_RETRY_S).await;
                } else if token_rejected {
                    backoff.set_floor(AUTH_BACKOFF_MIN_S);
                    let d = backoff.next_delay();
                    sleep_or_stop(&state, d).await;
                }
                token_rejected = true;
                continue;
            }
            Err(e) => {
                let d = backoff.next_delay();
                log::info!("VM lookup failed ({e}); retrying in {d:.0}s");
                sleep_or_stop(&state, d).await;
                continue;
            }
        };
        token_rejected = false;
        let Some(vm) = vms.iter().find(|v| v.is_default).or_else(|| vms.first()) else {
            let d = backoff.next_delay();
            log::warn!("the account has no VM; retrying in {d:.0}s");
            sleep_or_stop(&state, d).await;
            continue;
        };
        state.set_vm(Some(vm.vm_name.clone()), token_age(&pairing));

        let config = SessionConfig::new(
            pairing
                .noise_host
                .clone()
                .unwrap_or_else(|| DEFAULT_NOISE_HOST.into()),
            if vm.vm_id.is_empty() {
                vm.vm_name.clone()
            } else {
                vm.vm_id.clone()
            },
            vm.vm_auth_token.clone(),
            state.device(),
        );
        let started = Instant::now();
        let outcome = run_one_session(&state, config).await;
        let lasted = started.elapsed().as_secs_f64();

        match outcome {
            Outcome::Stopped => return,
            Outcome::Unpaired => {
                let _ = profile.delete_pairing();
                state.set_link_state(LinkState::Unpaired, None);
                continue;
            }
            _ => {}
        }
        let healthy = lasted >= HEALTHY_SESSION_S;
        if healthy {
            backoff.reset();
        }
        if matches!(outcome, Outcome::AuthRejected | Outcome::Forbidden) {
            backoff.set_floor(AUTH_BACKOFF_MIN_S);
        }
        let delay = if healthy && outcome == Outcome::Closed {
            HEALTHY_RECONNECT_S
        } else {
            backoff.next_delay()
        };
        // A VM that is asleep comes back when its owner next talks to it from anywhere, and
        // tells them this device is offline until it has reconnected: keep the wait short.
        let delay = if state
            .unanswered
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            delay.min(UNANSWERED_RETRY_S)
        } else {
            delay
        };
        log::info!("reconnecting in {delay:.0}s");
        sleep_or_stop(&state, delay).await;
    }
}

fn token_age(pairing: &Pairing) -> Option<u64> {
    (pairing.access_token_saved_at > 0)
        .then(|| now_unix().saturating_sub(pairing.access_token_saved_at))
}

/// Tokens the API rotated that could not be saved. The refresh token they replaced is
/// already retired, so until they are saved these are the only ones that work.
struct Unsaved {
    /// The stored refresh token they replace.
    replaced: String,
    pairing: Pairing,
}

/// The pairing to use now: the stored one, or tokens that could not be saved yet in place
/// of the stored ones they replaced (and another try at saving them). A pairing changed
/// since, unpaired or paired again, wins over them.
fn current_pairing(profile: &Profile, unsaved: &mut Option<Unsaved>) -> Option<Pairing> {
    let stored = profile.pairing().ok().flatten();
    let Some(waiting) = unsaved.take() else {
        return stored;
    };
    let stored = stored?;
    if stored.refresh_token.expose() != waiting.replaced {
        return Some(stored);
    }
    match profile.save_pairing(&waiting.pairing) {
        Ok(()) => log::info!("rotated device token saved"),
        Err(e) => {
            log::warn!("still could not save the rotated device token: {e}");
            *unsaved = Some(Unsaved {
                replaced: waiting.replaced,
                pairing: waiting.pairing.clone(),
            });
        }
    }
    Some(waiting.pairing)
}

/// Saves tokens the API has just rotated, or keeps them in `unsaved` when they cannot be
/// saved: never falls back to the refresh token the API has retired.
fn rotated(
    profile: &Profile,
    pairing: &Pairing,
    access: String,
    refresh: String,
    now: u64,
    unsaved: &mut Option<Unsaved>,
) -> Pairing {
    match profile.rotate_tokens(
        Secret::new(access.clone()),
        Secret::new(refresh.clone()),
        now,
    ) {
        Ok(p) => {
            log::info!("device token rotated");
            *unsaved = None;
            p
        }
        Err(e) => {
            log::warn!("could not save the rotated device token ({e}); using it unsaved");
            let mut p = pairing.clone();
            p.access_token = Secret::new(access);
            p.refresh_token = Secret::new(refresh);
            p.access_token_saved_at = now;
            // What is stored is still what an earlier unsaved rotation replaced, if any.
            let replaced = unsaved
                .take()
                .map_or_else(|| pairing.refresh_token.expose().to_owned(), |u| u.replaced);
            *unsaved = Some(Unsaved {
                replaced,
                pairing: p.clone(),
            });
            p
        }
    }
}

async fn maybe_refresh(
    state: &Arc<DaemonState>,
    profile: &Profile,
    pairing: Pairing,
    force: bool,
    unsaved: &mut Option<Unsaved>,
) -> Option<Pairing> {
    if !force && pairing.access_token_saved_at > now_unix().saturating_sub(TOKEN_REFRESH_AGE_S) {
        return Some(pairing);
    }
    let identity = profile.identity().ok()?;
    let api_root = api::api_root(pairing.api_url_v2.as_deref().unwrap_or(""));
    let sdk_token = profile.sdk_token().ok().flatten();
    let sdk = sdk_token.as_ref().map(|t| t.expose());
    match api::refresh_device_token(
        pairing.refresh_token.expose(),
        &identity.node_id(),
        &api_root,
        sdk,
    )
    .await
    {
        Ok(tokens) => Some(rotated(
            profile,
            &pairing,
            tokens.access_token,
            tokens.refresh_token,
            now_unix(),
            unsaved,
        )),
        Err(e) if e.is_unauthorized() => {
            let _ = profile.delete_pairing();
            state.set_link_state(LinkState::Unpaired, Some("pairing revoked".into()));
            None
        }
        // Transient: keep using the current token unless this was a forced retry.
        Err(_) => {
            if force {
                None
            } else {
                Some(pairing)
            }
        }
    }
}

fn is_shutdown(state: &Arc<DaemonState>) -> bool {
    state.is_shutdown()
}

async fn sleep_or_stop(state: &Arc<DaemonState>, seconds: f64) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.0))) => {}
        () = state.wait_shutdown() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.next_delay(), 2.0);
        assert_eq!(b.next_delay(), 4.0);
        assert_eq!(b.next_delay(), 8.0);
        for _ in 0..10 {
            b.next_delay();
        }
        assert_eq!(b.next_delay(), 60.0);
        b.reset();
        assert_eq!(b.next_delay(), 2.0);
    }

    #[test]
    fn rotated_tokens_that_cannot_be_saved_are_used_until_they_are() {
        use goblin_store::{Layout, ProfileStore};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("work").unwrap();
        let mut stored = Pairing::from_value(&serde_json::json!({
            "access_token": "old-access", "refresh_token": "old-refresh",
            "access_token_saved_at": 1
        }))
        .unwrap();
        stored.access_token_saved_at = 1;
        profile.save_pairing(&stored).unwrap();

        // The card will not take a write: the new tokens are used, not the retired ones.
        let mode = |m| std::fs::set_permissions(profile.dir(), std::fs::Permissions::from_mode(m));
        mode(0o500).unwrap();
        let mut unsaved = None;
        let p = rotated(
            &profile,
            &stored,
            "a1".into(),
            "r1".into(),
            10,
            &mut unsaved,
        );
        assert_eq!(p.refresh_token.expose(), "r1");
        let now = current_pairing(&profile, &mut unsaved).unwrap();
        assert_eq!(now.refresh_token.expose(), "r1");
        // Rotated again before it could be saved: still replaces what is stored.
        rotated(&profile, &now, "a2".into(), "r2".into(), 20, &mut unsaved);
        assert_eq!(unsaved.as_ref().unwrap().replaced, "old-refresh");

        // Once it can, it is saved and nothing waits any more.
        mode(0o700).unwrap();
        let now = current_pairing(&profile, &mut unsaved).unwrap();
        assert_eq!(now.refresh_token.expose(), "r2");
        assert!(unsaved.is_none());
        let on_card = profile.pairing().unwrap().unwrap();
        assert_eq!(on_card.refresh_token.expose(), "r2");
        assert_eq!(on_card.access_token.expose(), "a2");
    }

    #[test]
    fn unsaved_tokens_give_way_to_a_pairing_changed_since() {
        use goblin_store::{Layout, ProfileStore};
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("work").unwrap();
        let fresh = Pairing::from_value(&serde_json::json!({
            "access_token": "new-access", "refresh_token": "paired-again"
        }))
        .unwrap();
        profile.save_pairing(&fresh).unwrap();
        let mut unsaved = Some(Unsaved {
            replaced: "old-refresh".into(),
            pairing: fresh.clone(),
        });
        unsaved.as_mut().unwrap().pairing.refresh_token = Secret::new("r1");
        let now = current_pairing(&profile, &mut unsaved).unwrap();
        assert_eq!(now.refresh_token.expose(), "paired-again");
        assert!(unsaved.is_none());
        profile.delete_pairing().unwrap();
        unsaved = Some(Unsaved {
            replaced: "paired-again".into(),
            pairing: fresh,
        });
        assert!(
            current_pairing(&profile, &mut unsaved).is_none(),
            "unpaired since"
        );
    }

    #[test]
    fn backoff_floor_applies() {
        let mut b = Backoff::default();
        b.set_floor(15.0);
        assert_eq!(b.next_delay(), 15.0); // 2.0 raised to the floor
    }
}
