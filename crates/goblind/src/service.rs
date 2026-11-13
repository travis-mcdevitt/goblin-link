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
        let Some(pairing) = profile.pairing().ok().flatten() else {
            state.set_link_state(LinkState::Unpaired, None);
            sleep_or_stop(&state, UNPAIRED_POLL_S).await;
            continue;
        };

        let pairing = match maybe_refresh(&state, &profile, pairing, false).await {
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
                if maybe_refresh(&state, &profile, pairing.clone(), true)
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

async fn maybe_refresh(
    state: &Arc<DaemonState>,
    profile: &Profile,
    pairing: Pairing,
    force: bool,
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
        Ok(tokens) => {
            match profile.rotate_tokens(
                Secret::new(tokens.access_token),
                Secret::new(tokens.refresh_token),
                now_unix(),
            ) {
                Ok(p) => {
                    log::info!("device token rotated");
                    Some(p)
                }
                Err(_) => Some(pairing),
            }
        }
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
    fn backoff_floor_applies() {
        let mut b = Backoff::default();
        b.set_floor(15.0);
        assert_eq!(b.next_delay(), 15.0); // 2.0 raised to the floor
    }
}
