//! `goblinctl platform ...`: exercise the platform adapter.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use clap::{Args as ClapArgs, Subcommand};
use goblin_platform::detect::{self, HostProbe};
use goblin_platform::{
    Cycles, EntryKind, LedEffect, LedSetting, Leds, Platform, PresenceLeds, PresenceOptions,
    PresenceState, Rgb,
};

use crate::error::{CliError, Result};

/// Arguments of `goblinctl platform`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Directory containing `<id>/platform.toml` (default: auto-locate).
    #[arg(long, global = true, value_name = "DIR")]
    platforms_dir: Option<PathBuf>,
    /// Use this platform id instead of detecting one.
    #[arg(long, global = true, value_name = "ID")]
    platform: Option<String>,
    /// Print machine-readable JSON where the command has structured output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Sub,
}

#[derive(Debug, Subcommand)]
enum Sub {
    /// Show the detected platform and its capabilities.
    Info,
    /// LED control.
    #[command(subcommand)]
    Leds(LedsCmd),
    /// Vibrate the device briefly.
    Rumble {
        /// Pulse length in milliseconds.
        ms: u32,
        /// Strength in percent.
        #[arg(long, default_value_t = 60)]
        strength: u8,
    },
    /// Battery and supply state.
    Battery,
    /// Speaker volume and mute.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Microphone gain.
    #[command(subcommand)]
    MicGain(MicGainCmd),
    /// Uptime, load, memory, disk, temperature (the `device.health` fields).
    Health,
    /// Start-on-boot control.
    #[command(subcommand)]
    Autostart(AutostartCmd),
    /// Hold or release the device awake.
    StayAwake {
        /// `on` or `off`.
        state: OnOff,
    },
    /// Register a launcher entry with the firmware.
    #[command(subcommand)]
    Register(RegisterCmd),
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
enum LedsCmd {
    /// List zones, ceiling and locations.
    List,
    /// Read a zone's current setting.
    Read {
        /// Zone name.
        zone: String,
    },
    /// Set a zone (or `all`).
    Set {
        /// Zone name or `all`.
        zone: String,
        /// Colour: RRGGBB, #RRGGBB or a name.
        colour: String,
        /// Effect: off, linear, breath, sniff, static, blink1, blink2, blink3.
        #[arg(long, default_value = "static")]
        effect: String,
        /// Animation cycle length in milliseconds.
        #[arg(long, default_value_t = 1000)]
        duration_ms: u32,
        /// Number of cycles or `forever`.
        #[arg(long, default_value = "forever")]
        cycles: String,
        /// Hardware brightness ceiling to apply for the duration of the call.
        #[arg(long)]
        brightness: Option<u32>,
        /// Keep the setting for this many seconds, then restore the previous state.
        #[arg(long)]
        hold: Option<f64>,
    },
    /// Turn a zone (or `all`) off.
    Off {
        /// Zone name or `all`.
        zone: String,
    },
    /// Drive individual LEDs through the raw frame interface.
    Frame {
        /// Light only this LED index.
        #[arg(long, conflicts_with = "colours")]
        index: Option<usize>,
        /// Colour for `--index`.
        #[arg(long, default_value = "white")]
        colour: String,
        /// Comma-separated colour for every LED in order.
        #[arg(long)]
        colours: Option<String>,
        /// Hardware brightness ceiling to apply for the duration of the call.
        #[arg(long)]
        brightness: Option<u32>,
        /// Keep the frame for this many seconds, then restore the previous state.
        #[arg(long, default_value_t = 2.0)]
        hold: f64,
    },
    /// Show a presence state on the presence zones, then restore.
    Presence {
        /// connected, reconnecting, not-paired, error, working or attention.
        state: String,
        /// Seconds to hold before restoring.
        #[arg(long, default_value_t = 3.0)]
        hold: f64,
        /// Exercise the "never touch LEDs" option (nothing is written).
        #[arg(long)]
        never_touch: bool,
        /// Hardware brightness ceiling to apply for the duration of the call.
        #[arg(long)]
        brightness: Option<u32>,
    },
}

#[derive(Debug, Subcommand)]
enum VolumeCmd {
    /// Print the volume in percent.
    Get,
    /// Set the volume in percent.
    Set {
        /// Percent, 0-100 (limited by the platform ceiling).
        percent: u8,
    },
    /// Print whether the speaker is muted.
    Muted,
    /// Mute the speaker.
    Mute,
    /// Unmute the speaker.
    Unmute,
}

#[derive(Debug, Subcommand)]
enum MicGainCmd {
    /// Print the gain in dB.
    Get,
    /// Set the gain in dB.
    Set {
        /// Gain in dB.
        db: i32,
        /// Restore the previous gain after this many seconds instead of keeping the new one.
        #[arg(long)]
        hold: Option<f64>,
    },
}

#[derive(Debug, Subcommand)]
enum AutostartCmd {
    /// Print enabled or disabled.
    Status,
    /// Enable start on boot.
    Enable,
    /// Disable start on boot.
    Disable,
}

#[derive(Debug, Subcommand)]
enum RegisterCmd {
    /// Add an entry.
    Add {
        /// `game` or `tool`.
        kind: Kind,
        /// Entry name.
        name: String,
        /// Absolute path of the executable to run.
        command: String,
    },
    /// Remove an entry.
    Remove {
        /// `game` or `tool`.
        kind: Kind,
        /// Entry name.
        name: String,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Kind {
    Game,
    Tool,
}

impl From<Kind> for EntryKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Game => EntryKind::Game,
            Kind::Tool => EntryKind::Tool,
        }
    }
}

/// Run the `platform` command group.
pub fn run(args: Args) -> Result<()> {
    let platform = load_platform(&args)?;
    let json = args.json;
    match args.command {
        Sub::Info => info(&platform, json),
        Sub::Leds(cmd) => leds(&platform, cmd),
        Sub::Rumble { ms, strength } => {
            platform.rumble(ms, strength)?;
            println!("rumble {ms} ms at {strength}%");
            Ok(())
        }
        Sub::Battery => {
            let b = platform.battery()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&b)?);
            } else {
                println!(
                    "percent={} status={} charging={} external_power={} voltage_v={} temperature_c={}",
                    opt(b.percent),
                    b.status.as_deref().unwrap_or("?"),
                    b.charging,
                    opt(b.external_power),
                    opt(b.voltage_v),
                    opt(b.temperature_c)
                );
            }
            Ok(())
        }
        Sub::Volume(cmd) => volume(&platform, cmd),
        Sub::MicGain(cmd) => mic_gain(&platform, cmd),
        Sub::Health => {
            let h = platform.health();
            println!("{}", serde_json::to_string_pretty(&h)?);
            Ok(())
        }
        Sub::Autostart(cmd) => {
            match cmd {
                AutostartCmd::Status => {
                    let on = platform.autostart_enabled()?;
                    println!("{}", if on { "enabled" } else { "disabled" });
                }
                AutostartCmd::Enable => platform.set_autostart(true)?,
                AutostartCmd::Disable => platform.set_autostart(false)?,
            }
            Ok(())
        }
        Sub::StayAwake { state } => {
            platform.set_stay_awake(matches!(state, OnOff::On))?;
            Ok(())
        }
        Sub::Register(cmd) => {
            match cmd {
                RegisterCmd::Add {
                    kind,
                    name,
                    command,
                } => {
                    platform.register_entry(kind.into(), &name, &command)?;
                }
                RegisterCmd::Remove { kind, name } => {
                    platform.unregister_entry(kind.into(), &name)?
                }
            }
            Ok(())
        }
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "?".to_owned(), |v| v.to_string())
}

fn load_platform(args: &Args) -> Result<Platform> {
    let probe = HostProbe;
    let dir = match &args.platforms_dir {
        Some(d) => d.clone(),
        None => detect::locate_platforms_dir(&probe)?,
    };
    match &args.platform {
        Some(id) => Ok(Platform::load(&dir.join(id))?),
        None => Ok(Platform::detect_in(&dir, &probe)?),
    }
}

fn info(platform: &Platform, json: bool) -> Result<()> {
    let caps = platform.capabilities();
    if json {
        println!("{}", serde_json::to_string_pretty(&caps)?);
        return Ok(());
    }
    let config = platform.config();
    println!(
        "platform: {} ({})",
        platform.id(),
        config.name.as_deref().unwrap_or("-")
    );
    println!("directory: {}", platform.dir().display());
    if let Ok(d) = platform.display() {
        println!(
            "display: {}x{} scale {} rotation {}",
            d.width, d.height, d.scale, d.rotation
        );
    }
    let paths = platform.paths();
    println!(
        "paths: data={} games={} tools={} tmp={}",
        paths.data.as_deref().unwrap_or("-"),
        paths.games.as_deref().unwrap_or("-"),
        paths.tools.as_deref().unwrap_or("-"),
        paths.tmp
    );
    if let Ok(i) = platform.input() {
        println!(
            "input: map={} layout={}",
            i.map.as_deref().unwrap_or("-"),
            i.layout.as_deref().unwrap_or("-")
        );
    }
    let yes = |b: bool| if b { "yes" } else { "unavailable" };
    println!("capabilities:");
    for (name, present) in [
        ("display", caps.display),
        ("brightness", caps.brightness),
        ("input", caps.input),
        ("volume", caps.volume),
        ("mic-gain", caps.mic_gain),
        ("leds", caps.leds),
        ("rumble", caps.rumble),
        ("battery", caps.battery),
        ("temperature", caps.temperature),
        ("stay-awake", caps.stay_awake),
        ("autostart", caps.autostart),
        ("firmware-registration", caps.firmware_registration),
    ] {
        println!("  {name:<22} {}", yes(present));
    }
    Ok(())
}

fn parse_cycles(text: &str) -> Result<Cycles> {
    if text == "forever" {
        return Ok(Cycles::Forever);
    }
    text.parse().map(Cycles::Count).map_err(|_| {
        CliError::Usage(format!(
            "cycles must be a number or 'forever', got {text:?}"
        ))
    })
}

fn zones_for(leds: &Leds, zone: &str) -> Result<Vec<String>> {
    if zone == "all" {
        Ok(leds.zones().to_vec())
    } else {
        Ok(vec![zone.to_owned()])
    }
}

fn hold_for(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|d| *d <= Duration::from_secs(120))
        .ok_or_else(|| CliError::Usage("hold must be between 0 and 120 seconds".into()))
}

/// Run `body` with `brightness` applied; afterwards restore the saved LED state (always, when
/// `restore` is set) and report the first error.
fn with_leds_restored(
    leds: &Leds,
    brightness: Option<u32>,
    restore: bool,
    body: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let snapshot = if restore {
        Some(leds.snapshot()?)
    } else {
        None
    };
    let result = brightness
        .map_or(Ok(()), |b| leds.set_max_brightness(b))
        .map_err(CliError::from)
        .and_then(|()| body());
    let restored = snapshot
        .map_or(Ok(()), |s| leds.restore(&s))
        .map_err(CliError::from);
    result.and(restored)
}

fn leds(platform: &Platform, cmd: LedsCmd) -> Result<()> {
    let leds = platform.leds()?;
    match cmd {
        LedsCmd::List => {
            let cfg = platform.config().leds.as_ref();
            println!("zones: {}", leds.zones().join(", "));
            println!("max brightness: {}", leds.max_brightness());
            if let Some(cfg) = cfg {
                for (zone, place) in &cfg.locations {
                    println!("  {zone}: {place}");
                }
                println!("presence zones: {}", cfg.presence_zones.join(", "));
            }
            Ok(())
        }
        LedsCmd::Read { zone } => {
            let s = leds.read(&zone)?;
            println!(
                "{zone}: colour={} effect={} duration_ms={} cycles={:?}",
                s.colour,
                s.effect.name(),
                s.duration_ms,
                s.cycles
            );
            Ok(())
        }
        LedsCmd::Set {
            zone,
            colour,
            effect,
            duration_ms,
            cycles,
            brightness,
            hold,
        } => {
            let setting = LedSetting {
                colour: colour.parse()?,
                effect: effect.parse::<LedEffect>()?,
                duration_ms,
                cycles: parse_cycles(&cycles)?,
            };
            let hold = hold.map(hold_for).transpose()?;
            let zones = zones_for(&leds, &zone)?;
            with_leds_restored(&leds, brightness, hold.is_some(), || {
                for z in &zones {
                    leds.set(z, setting)?;
                }
                if let Some(d) = hold {
                    sleep(d);
                }
                Ok(())
            })
        }
        LedsCmd::Off { zone } => {
            for z in zones_for(&leds, &zone)? {
                leds.off(&z)?;
            }
            Ok(())
        }
        LedsCmd::Frame {
            index,
            colour,
            colours,
            brightness,
            hold,
        } => {
            let count = platform
                .config()
                .leds
                .as_ref()
                .and_then(|l| l.frame_leds)
                .ok_or(goblin_platform::PlatformError::Unavailable("led frames"))?
                as usize;
            let frame: Vec<Rgb> = match (index, colours) {
                (_, Some(list)) => list
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?,
                (Some(i), None) if i < count => {
                    let mut f = vec![Rgb::BLACK; count];
                    f[i] = colour.parse()?;
                    f
                }
                (Some(i), None) => {
                    return Err(CliError::Usage(format!(
                        "index {i} out of range 0..{count}"
                    )))
                }
                (None, None) => return Err(CliError::Usage("give --index or --colours".into())),
            };
            let hold = hold_for(hold)?;
            with_leds_restored(&leds, brightness, true, || {
                leds.set_frame(&frame)?;
                sleep(hold);
                Ok(())
            })
        }
        LedsCmd::Presence {
            state,
            hold,
            never_touch,
            brightness,
        } => {
            let state = PresenceState::from_name(&state).ok_or_else(|| {
                let names: Vec<_> = PresenceState::ALL.iter().map(|s| s.name()).collect();
                CliError::Usage(format!(
                    "unknown state {state:?}; use one of {}",
                    names.join(", ")
                ))
            })?;
            let cfg = platform
                .config()
                .leds
                .as_ref()
                .ok_or(goblin_platform::PlatformError::Unavailable("leds"))?;
            let options = PresenceOptions {
                never_touch,
                ..PresenceOptions::default()
            };
            let hold = hold_for(hold)?;
            let before = leds.snapshot()?;
            let outcome = brightness
                .map_or(Ok(()), |b| leds.set_max_brightness(b))
                .map_err(CliError::from)
                .and_then(|()| {
                    let mut presence =
                        PresenceLeds::new(&leds, cfg.presence_zones.clone(), options);
                    presence.set(state)?;
                    sleep(hold);
                    presence.release().map_err(CliError::from)
                });
            // Brightness is not part of what PresenceLeds restores for the zones it touched
            // unless it was changed, so put the whole snapshot back to be certain.
            let restored = leds.restore(&before).map_err(CliError::from);
            outcome.and(restored)
        }
    }
}

fn volume(platform: &Platform, cmd: VolumeCmd) -> Result<()> {
    match cmd {
        VolumeCmd::Get => println!("{}", platform.volume()?),
        VolumeCmd::Set { percent } => platform.set_volume(percent)?,
        VolumeCmd::Muted => println!("{}", platform.speaker_muted()?),
        VolumeCmd::Mute => platform.set_speaker_muted(true)?,
        VolumeCmd::Unmute => platform.set_speaker_muted(false)?,
    }
    Ok(())
}

fn mic_gain(platform: &Platform, cmd: MicGainCmd) -> Result<()> {
    match cmd {
        MicGainCmd::Get => println!("{}", platform.mic_gain()?),
        MicGainCmd::Set { db, hold } => {
            let hold = hold.map(hold_for).transpose()?;
            let guard = platform.set_mic_gain(db)?;
            match hold {
                Some(d) => {
                    sleep(d);
                    guard.restore()?;
                }
                None => guard.keep(),
            }
        }
    }
    Ok(())
}
