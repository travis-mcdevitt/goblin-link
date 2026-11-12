//! `goblinctl`: command-line front end for Goblin Link.
//!
//! Each command group lives in its own module under `cmd/` and exposes an `Args` type (a clap
//! `Subcommand`/`Args` derive) and a `run` function. To add a group, create the module, add a
//! variant to [`Command`] and one match arm in [`main`].

mod cmd;
mod error;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Goblin Link command-line tool.
#[derive(Debug, Parser)]
#[command(name = "goblinctl", version, about)]
struct Cli {
    /// Daemon socket (default: $GOBLIN_SOCKET, else <runtime dir>/active).
    #[arg(long, global = true)]
    socket: Option<std::path::PathBuf>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect and exercise the platform adapter (LEDs, rumble, battery, volume, ...).
    Platform(cmd::platform::Args),
    /// Show the daemon's link, profile and agent.
    Status,
    /// Chats through the daemon: list, history, create, send.
    #[command(subcommand)]
    Chat(cmd::chat::ChatCmd),
    /// Profiles (one per Muse account): list, create, switch, rename, delete.
    #[command(subcommand)]
    Profile(cmd::profile::ProfileCmd),
    /// Bluetooth pairing with the Muse app.
    #[command(subcommand)]
    Pair(cmd::profile::PairCmd),
    /// What Muse may do on this device for the profile being served.
    #[command(subcommand)]
    Access(cmd::profile::AccessCmd),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Platform(args)) => cmd::platform::run(args),
        Some(Command::Status) => cmd::chat::status(cli.socket, cli.json),
        Some(Command::Chat(c)) => cmd::chat::run(c, cli.socket, cli.json),
        Some(Command::Profile(c)) => cmd::profile::profile(c, cli.socket, cli.json),
        Some(Command::Pair(c)) => cmd::profile::pair(c, cli.socket, cli.json),
        Some(Command::Access(c)) => cmd::profile::access(c, cli.socket, cli.json),
        None => {
            println!(
                "goblinctl {} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH
            );
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("goblinctl: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
