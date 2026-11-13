//! `goblinctl profile …`, `goblinctl pair …` and `goblinctl access …`: profiles, Bluetooth
//! pairing and what Muse may do on the device, through the running daemon.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Subcommand, ValueEnum};
use goblin_ipc::types::{
    ConsentGetParams, ConsentUpdate, FileScope, PairCancelParams, PairPhase, PairStartParams,
    PairUnpairParams, ProfileCreateParams, ProfileDeleteParams, ProfileListParams,
    ProfileRenameParams, ProfileSetSdkTokenParams, ProfileSwitchParams, SubscribeParams, Topic,
};
use goblin_ipc::Event;

use super::chat::{connect, print_json, TIMEOUT};
use crate::error::{CliError, Result};

/// Profile commands.
#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// List the profiles; the one being served is marked.
    List,
    /// Create an empty, unpaired profile.
    Create { name: String },
    /// Serve another profile (the daemon starts again as that profile).
    Switch { name: String },
    /// Rename a profile.
    Rename { name: String, new_name: String },
    /// Delete a profile's credentials and cached conversations (not its folder of files).
    Delete { name: String },
    /// Read a profile's SDK token from a file. The file is deleted once the token is stored.
    SdkToken { name: String, file: PathBuf },
}

/// Pairing commands.
#[derive(Debug, Subcommand)]
pub enum PairCmd {
    /// Open Bluetooth pairing and wait for the Muse app.
    Start {
        /// Profile to pair (default: the one being served).
        #[arg(long)]
        profile: Option<String>,
        /// Pair again a profile that is already paired.
        #[arg(long)]
        force: bool,
        /// Seconds the window stays open.
        #[arg(long, default_value_t = 300)]
        timeout: u32,
    },
    /// Close an open pairing window.
    Cancel,
    /// Forget a profile's pairing on this device.
    Unpair {
        /// Profile to unpair (default: the one being served).
        #[arg(long)]
        profile: Option<String>,
    },
}

/// How far the file commands may reach.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Reach {
    /// The profile's own folder.
    Home,
    /// Its folder and the storage card.
    Storage,
    /// Any path.
    Device,
}

/// Whether Muse may install what it builds.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Builds {
    /// No.
    Off,
    /// Each install waits for the owner on the device.
    Ask,
    /// Installs happen at once.
    Free,
}

/// Access commands (for the profile being served).
#[derive(Debug, Subcommand)]
pub enum AccessCmd {
    /// Show what Muse may do.
    Show,
    /// Change it; anything not named stays as it is.
    Set {
        /// Shell commands (full access to the device).
        #[arg(long)]
        shell: Option<bool>,
        /// Reading files.
        #[arg(long)]
        file_read: Option<bool>,
        /// Writing files.
        #[arg(long)]
        file_write: Option<bool>,
        /// The owner's own commands.
        #[arg(long)]
        commands: Option<bool>,
        /// Reach of the file commands.
        #[arg(long, value_enum)]
        files: Option<Reach>,
        /// Installing what Muse builds (tools, apps, games).
        #[arg(long, value_enum)]
        builds: Option<Builds>,
    },
}

/// `goblinctl profile …`.
pub fn profile(cmd: ProfileCmd, sock: Option<PathBuf>, json: bool) -> Result<()> {
    let mut c = connect(sock)?;
    match cmd {
        ProfileCmd::List => {
            let r = c.request(ProfileListParams {}, TIMEOUT)?;
            if json {
                return print_json(&r);
            }
            for p in &r.profiles {
                println!(
                    "{} {:<16} {}{}",
                    if p.name == r.active { "*" } else { " " },
                    p.name,
                    if p.paired { "paired" } else { "not paired" },
                    if p.has_sdk_token {
                        ""
                    } else {
                        ", no SDK token"
                    },
                );
            }
        }
        ProfileCmd::Create { name } => {
            c.request(ProfileCreateParams { name }, TIMEOUT)?;
        }
        ProfileCmd::Switch { name } => {
            c.request(ProfileSwitchParams { name }, TIMEOUT)?;
        }
        ProfileCmd::Rename { name, new_name } => {
            c.request(ProfileRenameParams { name, new_name }, TIMEOUT)?;
        }
        ProfileCmd::Delete { name } => {
            c.request(ProfileDeleteParams { name }, TIMEOUT)?;
        }
        ProfileCmd::SdkToken { name, file } => {
            c.request(
                ProfileSetSdkTokenParams {
                    name,
                    token_path: file.to_string_lossy().into_owned(),
                },
                TIMEOUT,
            )?;
        }
    }
    Ok(())
}

/// `goblinctl pair …`.
pub fn pair(cmd: PairCmd, sock: Option<PathBuf>, json: bool) -> Result<()> {
    let mut c = connect(sock)?;
    match cmd {
        PairCmd::Cancel => {
            c.request(PairCancelParams {}, TIMEOUT)?;
        }
        PairCmd::Unpair { profile } => {
            c.request(PairUnpairParams { profile }, TIMEOUT)?;
        }
        PairCmd::Start {
            profile,
            force,
            timeout,
        } => {
            c.subscribe(
                SubscribeParams {
                    after_seq: None,
                    topics: Some(vec![Topic::Daemon]),
                },
                TIMEOUT,
            )?;
            // Bringing the radio up can take a while.
            let opened = c.request(
                PairStartParams {
                    profile,
                    timeout_s: Some(timeout),
                    force,
                },
                Duration::from_secs(40),
            )?;
            if json {
                print_json(&opened)?;
            } else {
                println!(
                    "In the Muse app: Settings > Devices > Add Device, then choose {}",
                    opened.ble_name
                );
            }
            let deadline = Instant::now() + Duration::from_secs(u64::from(timeout) + 60);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                let Some(frame) = c.next_event(left)? else {
                    break;
                };
                let Event::PairState(e) = frame.typed()? else {
                    continue;
                };
                if !json {
                    println!(
                        "{}{}",
                        e.state.as_str(),
                        e.detail
                            .as_deref()
                            .map(|d| format!(": {d}"))
                            .unwrap_or_default()
                    );
                }
                match e.state {
                    PairPhase::Done => return Ok(()),
                    PairPhase::Failed | PairPhase::Cancelled | PairPhase::Timeout => {
                        return Err(CliError::Daemon(format!(
                            "pairing ended: {}",
                            e.state.as_str()
                        )));
                    }
                    _ => {}
                }
            }
            return Err(CliError::Daemon("no word from the daemon".into()));
        }
    }
    Ok(())
}

/// `goblinctl access …`.
pub fn access(cmd: AccessCmd, sock: Option<PathBuf>, json: bool) -> Result<()> {
    let mut c = connect(sock)?;
    let consent = match cmd {
        AccessCmd::Show => c.request(ConsentGetParams {}, TIMEOUT)?,
        AccessCmd::Set {
            shell,
            file_read,
            file_write,
            commands,
            files,
            builds,
        } => c.request(
            ConsentUpdate {
                shell,
                file_read,
                file_write,
                drop_ins: commands,
                files: files.map(|r| match r {
                    Reach::Home => FileScope::Home,
                    Reach::Storage => FileScope::Storage,
                    Reach::Device => FileScope::Device,
                }),
                builds: builds.map(|b| match b {
                    Builds::Off => goblin_ipc::types::BuildAccess::Off,
                    Builds::Ask => goblin_ipc::types::BuildAccess::Ask,
                    Builds::Free => goblin_ipc::types::BuildAccess::Free,
                }),
            },
            TIMEOUT,
        )?,
    };
    if json {
        return print_json(&consent);
    }
    let on = |b: bool| if b { "on" } else { "off" };
    println!("shell        {}", on(consent.shell));
    println!("file read    {}", on(consent.file_read));
    println!("file write   {}", on(consent.file_write));
    println!("own commands {}", on(consent.drop_ins));
    println!(
        "files reach  {}",
        match consent.files {
            FileScope::Home => "the profile's own folder",
            FileScope::Storage => "its folder and the storage card",
            FileScope::Device => "any path",
        }
    );
    println!(
        "its builds   {}",
        match consent.builds {
            goblin_ipc::types::BuildAccess::Off => "not installed",
            goblin_ipc::types::BuildAccess::Ask => "installed when the owner approves",
            goblin_ipc::types::BuildAccess::Free => "installed freely",
        }
    );
    Ok(())
}
