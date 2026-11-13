//! `goblind`: the Goblin Link daemon.
//!
//! One tokio runtime hosts the device session (Noise control link, command
//! executor, chat engine) and the IPC server that `goblin-ui` and `goblinctl` talk
//! to. See `docs/daemon.md` for the architecture and what is deferred.

use goblind::{config, daemon, exec, service};

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use serde_json::json;

use goblin_ipc::server::{EventBus, Server, ServerConfig};
use goblin_store::chats::ChatCache;
use goblin_store::commands::{CommandLog, ConsentStore, DEFAULT_MAX_ENTRIES};
use goblin_store::inbox::{Inbox, DEFAULT_MAX_NOTICES};
use goblin_store::profiles::ProfileStore;
use goblin_store::settings::SettingsStore;
use goblin_store::Layout;
use muse_link::DeviceDescription;

use daemon::{build_state, cache_limits, DaemonHandler, Stores};
use exec::{Executor, RunAs};

/// The Goblin Link daemon.
#[derive(Debug, Parser)]
#[command(name = "goblind", version, about)]
struct Cli {
    /// Data directory (profiles, settings, caches). Defaults to the platform or XDG path.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Runtime directory for the IPC socket. Defaults to the platform or XDG path.
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    /// Active profile name. Defaults to the stored active profile or `personal`.
    #[arg(long)]
    profile: Option<String>,
    /// Account to run device commands as (only honoured when running as root).
    #[arg(long)]
    run_as: Option<String>,
    /// Directory of owner drop-in command definitions.
    #[arg(long)]
    commands_dir: Option<PathBuf>,
    /// Directory holding each profile's own folder (`<homes>/<profile>`), the only place
    /// the file commands reach at the narrowest setting. Defaults to a sibling `<data>.homes`.
    #[arg(long)]
    homes_dir: Option<PathBuf>,
    /// The removable storage (the SD card), which the "storage" file setting opens up.
    #[arg(long)]
    storage_root: Option<PathBuf>,
    /// Where what Muse builds is installed (`<built>/<profile>/...`). It should be on a
    /// filesystem with real permissions. Without it nothing can be installed.
    #[arg(long)]
    built_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("goblind: could not start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("goblind: {e}");
            ExitCode::FAILURE
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "goblin-link".into())
}

async fn run(cli: Cli) -> Result<(), String> {
    let platform = goblin_platform::Platform::detect().ok().map(Arc::new);
    let paths = config::resolve(cli.data_dir, cli.runtime_dir, platform.as_deref());
    std::fs::create_dir_all(&paths.data_dir).map_err(|e| format!("data dir: {e}"))?;

    let layout = Layout::new(&paths.data_dir);
    let profiles = ProfileStore::open(layout.clone()).map_err(|e| e.to_string())?;

    // Pick the active profile, creating it if necessary.
    let active = cli
        .profile
        .or_else(|| profiles.active())
        .unwrap_or_else(|| "personal".into());
    if !profiles.exists(&active).map_err(|e| e.to_string())? {
        profiles.create(&active).map_err(|e| e.to_string())?;
    }
    profiles.set_active(&active).map_err(|e| e.to_string())?;
    let profile = profiles.profile(&active).map_err(|e| e.to_string())?;

    let settings = SettingsStore::open(&layout.settings_file()).map_err(|e| e.to_string())?;
    // Each profile has its own consent; the old device-wide file is handed down once.
    profiles.migrate_consent().map_err(|e| e.to_string())?;
    let consent = ConsentStore::open(&profile.consent_file()).map_err(|e| e.to_string())?;

    // The profile's own folder, and what its file commands may never reach.
    let homes = match cli.homes_dir.clone() {
        Some(dir) => dir,
        None => default_homes(&paths.data_dir)?,
    };
    let home = homes.join(&active);
    std::fs::create_dir_all(&home).map_err(|e| format!("profile folder: {e}"))?;
    let mut denied = vec![paths.data_dir.clone()];
    denied.extend(cli.commands_dir.clone());
    // What is installed changes only through an install, never through a file command.
    denied.extend(cli.built_dir.clone());
    let homes_dir = homes.clone();
    let rules = exec::scope::FileRules {
        home,
        homes,
        storage: cli.storage_root.clone(),
        denied,
    };

    let stores = Stores {
        chats: ChatCache::open(&profile.chats_dir(), cache_limits()).map_err(|e| e.to_string())?,
        inbox: Inbox::open(&profile.notifications_file(), DEFAULT_MAX_NOTICES)
            .map_err(|e| e.to_string())?,
        commands: CommandLog::open(&profile.commands_file(), DEFAULT_MAX_ENTRIES)
            .map_err(|e| e.to_string())?,
    };

    // Resolve the run-as account and drop-in commands.
    let current = RunAs::current();
    let is_root = current.uid == 0;
    let run_as = choose_run_as(cli.run_as.as_deref(), current, RunAs::lookup)?;
    let drop_ins = match &cli.commands_dir {
        Some(dir) => exec::dropins::load(dir, is_root),
        None => Vec::new(),
    };
    let platform_for_health = platform.clone();
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let health_version = version.clone();
    let health: exec::HealthFn = Arc::new(move || {
        let mut health = match &platform_for_health {
            Some(p) => p.health(),
            None => goblin_platform::power::read_health(None),
        };
        health.version = health_version.clone();
        serde_json::to_value(health).unwrap_or(json!({}))
    });
    let executor = Arc::new(Executor::new(run_as, drop_ins, health).with_rules(rules));

    let identity = profile.identity().map_err(|e| e.to_string())?;
    let device = DeviceDescription {
        node_id: identity.node_id(),
        display_name: hostname(),
        version: version.clone(),
        commands: executor.specs(),
    };

    let run_id = muse_link::random_uuid();
    let bus = Arc::new(EventBus::new(run_id));
    let state = build_state(
        version,
        bus.clone(),
        profiles,
        platform,
        settings,
        consent,
        active.clone(),
        stores,
        executor,
        device,
    );

    state.set_homes_dir(homes_dir);
    // Anything given "for an hour" whose hour ended while the handheld was off or asleep
    // goes back now, before Muse is connected and could use it.
    state.revert_due();
    state.start_timers();
    if let Some(dir) = cli.built_dir.clone() {
        state.set_built_dir(dir);
    }

    // The IPC socket and the active-profile symlink.
    let socket = goblin_ipc::paths::socket_path(&paths.runtime_dir, &active)
        .map_err(|e| format!("socket path: {e}"))?;
    let handler = Arc::new(DaemonHandler::new(state.clone()));
    let server = Server::bind(&socket, handler, bus.clone(), ServerConfig::default())
        .map_err(|e| format!("another daemon may be running for this profile: {e}"))?;
    let _ = goblin_ipc::paths::set_active(&paths.runtime_dir, &active);
    log::info!("goblind serving profile '{active}' on {}", socket.display());

    // The device-session manager.
    let service = tokio::spawn(service::run(state.clone()));

    // Shut down on daemon.shutdown or a signal.
    let shutdown_state = state.clone();
    let shutdown = async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            () = shutdown_state.wait_shutdown() => {}
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    };

    let result = server.run(shutdown).await;
    state.request_shutdown();
    service.abort();
    // Hand the LEDs back to the firmware.
    state.release_presence();
    result.map_err(|e| e.to_string())?;
    if state.restart_requested() {
        // The profile was switched: start again from nothing as the same process, so no
        // state of the profile served until now (session, caches, access rules) carries
        // over to the next.
        return Err(restart());
    }
    Ok(())
}

/// Replaces this process with a fresh daemon (same arguments, less any `--profile`, so
/// the stored active profile is served). Only returns if that fails.
fn restart() -> String {
    use std::os::unix::process::CommandExt;
    let mut args = std::env::args_os().skip(1).peekable();
    let mut kept = Vec::new();
    while let Some(a) = args.next() {
        if a == "--profile" {
            args.next();
        } else if !a.to_string_lossy().starts_with("--profile=") {
            kept.push(a);
        }
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "goblind".into());
    log::info!("restarting to serve the switched profile");
    let e = std::process::Command::new(exe).args(kept).exec();
    format!("could not restart after the profile switch: {e}")
}

fn default_homes(data: &std::path::Path) -> Result<PathBuf, String> {
    let mut name = data
        .file_name()
        .ok_or("set --homes-dir when the data directory has no name")?
        .to_os_string();
    name.push(".homes");
    Ok(data.with_file_name(name))
}

fn choose_run_as(
    name: Option<&str>,
    current: RunAs,
    lookup: impl FnOnce(&str) -> Option<RunAs>,
) -> Result<RunAs, String> {
    match (name, current.uid == 0) {
        (Some(name), true) => lookup(name).ok_or_else(|| format!("unknown run-as account: {name}")),
        _ => Ok(current),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(uid: u32) -> RunAs {
        RunAs {
            name: "current".into(),
            uid,
            gid: uid,
            home: "/".into(),
        }
    }

    #[test]
    fn default_home_is_outside_the_denied_data_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let homes = default_homes(&data).unwrap();
        let home = homes.join("personal");
        std::fs::create_dir_all(&home).unwrap();
        let rules = exec::scope::FileRules {
            home: home.clone(),
            homes,
            storage: None,
            denied: vec![data],
        };
        assert!(rules
            .check(&home.join("note.txt"), goblin_ipc::types::FileScope::Home)
            .is_ok());
    }

    #[test]
    fn run_as_uses_the_requested_account() {
        let chosen = choose_run_as(Some("worker"), account(0), |name| {
            assert_eq!(name, "worker");
            Some(account(123))
        })
        .unwrap();
        assert_eq!(chosen.uid, 123);
    }

    #[test]
    fn run_as_rejects_an_unknown_account_as_root() {
        let error = choose_run_as(Some("missing-worker"), account(0), |_| None).unwrap_err();
        assert!(error.contains("missing-worker"));
    }

    #[test]
    fn run_as_keeps_current_without_a_flag_or_without_root() {
        assert_eq!(
            choose_run_as(None, account(0), |_| panic!("no lookup"))
                .unwrap()
                .uid,
            0
        );
        assert_eq!(
            choose_run_as(Some("worker"), account(123), |_| panic!("no lookup"))
                .unwrap()
                .uid,
            123
        );
    }
}
