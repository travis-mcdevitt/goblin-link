//! The [`Platform`]: a loaded adapter exposing optional capabilities.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{
    AudioConfig, DisplayConfig, HookSpec, InputConfig, PathsConfig, PlatformConfig,
};
use crate::detect::{self, HostProbe, SystemProbe};
use crate::error::{PlatformError, Result};
use crate::hook::Hook;
use crate::led::Leds;
use crate::power::{self, BatteryStatus, DeviceHealth};

/// Kind of launcher entry registered with the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Shown in the games list.
    Game,
    /// Shown in the tools list.
    Tool,
}

impl EntryKind {
    fn arg(self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Tool => "tool",
        }
    }
}

/// Which optional capabilities this platform provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// Screen geometry is known.
    pub display: bool,
    /// Brightness can be read and set.
    pub brightness: bool,
    /// Button map is known.
    pub input: bool,
    /// Speaker volume and mute can be controlled.
    pub volume: bool,
    /// Microphone gain can be controlled.
    pub mic_gain: bool,
    /// Status lights exist.
    pub leds: bool,
    /// Vibration motor exists.
    pub rumble: bool,
    /// Battery readout exists.
    pub battery: bool,
    /// SoC temperature readout exists.
    pub temperature: bool,
    /// The device can be kept awake.
    pub stay_awake: bool,
    /// Start-on-boot can be controlled.
    pub autostart: bool,
    /// Launcher entries can be registered with the firmware.
    pub firmware_registration: bool,
}

/// A loaded platform adapter.
#[derive(Debug, Clone)]
pub struct Platform {
    config: PlatformConfig,
    dir: PathBuf,
    hook_env: Vec<(String, String)>,
}

/// Restores the previous microphone gain when dropped or when [`MicGainGuard::restore`] runs.
#[derive(Debug)]
pub struct MicGainGuard {
    hook: Hook,
    previous: i32,
    active: bool,
}

impl MicGainGuard {
    /// Gain before the change, in dB.
    pub fn previous(&self) -> i32 {
        self.previous
    }

    /// Leave the new gain in place; the previous value will not be restored.
    pub fn keep(mut self) {
        self.active = false;
    }

    /// Put the gain back now and report any failure.
    pub fn restore(mut self) -> Result<()> {
        self.active = false;
        self.hook
            .run(&["set", &self.previous.to_string()])
            .map(drop)
    }
}

impl Drop for MicGainGuard {
    fn drop(&mut self) {
        if self.active {
            // Nothing useful can be done with a failure during drop; `restore` reports it.
            let _ = self.hook.run(&["set", &self.previous.to_string()]);
        }
    }
}

impl Platform {
    /// Load `<dir>/platform.toml` and check that every declared hook script exists.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join("platform.toml");
        let text = std::fs::read_to_string(&file).map_err(|e| PlatformError::io(&file, e))?;
        let config = PlatformConfig::parse(&text, &file)?;
        Self::from_config(config, dir)
    }

    /// Build a platform from an already parsed configuration.
    pub fn from_config(config: PlatformConfig, dir: &Path) -> Result<Self> {
        for hook in config.hook_specs() {
            let script = dir.join(hook.path());
            if !script.is_file() {
                return Err(PlatformError::Invalid(format!(
                    "hook script {} does not exist",
                    script.display()
                )));
            }
        }
        let hook_env = hook_environment(&config, dir, &HostProbe);
        Ok(Self {
            config,
            dir: dir.to_owned(),
            hook_env,
        })
    }

    /// Detect the platform for this device, searching the located platforms directory.
    pub fn detect() -> Result<Self> {
        let probe = HostProbe;
        let dir = detect::locate_platforms_dir(&probe)?;
        Self::detect_in(&dir, &probe)
    }

    /// Detect the platform among those under `platforms_dir` using `probe`.
    pub fn detect_in(platforms_dir: &Path, probe: &dyn SystemProbe) -> Result<Self> {
        let all = detect::load_all(platforms_dir)?;
        let configs: Vec<PlatformConfig> = all.iter().map(|(_, c)| c.clone()).collect();
        let chosen = detect::choose(&configs, probe)
            .ok_or_else(|| PlatformError::NoMatch(platforms_dir.to_owned()))?;
        let dir = all
            .iter()
            .find(|(_, c)| c.id == chosen.id)
            .map(|(d, _)| d.clone())
            .ok_or_else(|| PlatformError::NoMatch(platforms_dir.to_owned()))?;
        Self::from_config(chosen.clone(), &dir)
    }

    /// Platform id.
    pub fn id(&self) -> &str {
        &self.config.id
    }

    /// The parsed configuration.
    pub fn config(&self) -> &PlatformConfig {
        &self.config
    }

    /// Directory holding `platform.toml` and the hooks.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Filesystem locations.
    pub fn paths(&self) -> &PathsConfig {
        &self.config.paths
    }

    /// Screen geometry.
    pub fn display(&self) -> Result<&DisplayConfig> {
        self.config
            .display
            .as_ref()
            .ok_or(PlatformError::Unavailable("display"))
    }

    /// Button map.
    pub fn input(&self) -> Result<&InputConfig> {
        self.config
            .input
            .as_ref()
            .ok_or(PlatformError::Unavailable("input"))
    }

    /// Audio configuration.
    pub fn audio(&self) -> Result<&AudioConfig> {
        self.config
            .audio
            .as_ref()
            .ok_or(PlatformError::Unavailable("audio"))
    }

    fn hook(&self, spec: Option<&HookSpec>, capability: &'static str) -> Result<Hook> {
        spec.map(|s| Hook::new(s, &self.dir, self.hook_env.clone()))
            .ok_or(PlatformError::Unavailable(capability))
    }

    /// Which capabilities are present.
    pub fn capabilities(&self) -> Capabilities {
        let c = &self.config;
        let audio = c.audio.as_ref();
        let power = c.power.as_ref();
        Capabilities {
            display: c.display.is_some(),
            brightness: c
                .display
                .as_ref()
                .is_some_and(|d| d.brightness_hook.is_some()),
            input: c.input.is_some(),
            volume: audio.is_some_and(|a| a.output_hook.is_some()),
            mic_gain: audio.is_some_and(|a| a.mic_gain_hook.is_some()),
            leds: c.leds.is_some(),
            rumble: c.rumble.is_some(),
            battery: power.is_some_and(|p| p.battery.is_some()),
            temperature: power.is_some_and(|p| p.thermal_zone.is_some()),
            stay_awake: power.is_some_and(|p| p.stay_awake_hook.is_some()),
            autostart: c.autostart.is_some(),
            firmware_registration: c.firmware.is_some(),
        }
    }

    // ---- LEDs and rumble -------------------------------------------------------------------

    /// LED controller.
    pub fn leds(&self) -> Result<Leds> {
        let cfg = self
            .config
            .leds
            .as_ref()
            .ok_or(PlatformError::Unavailable("leds"))?;
        let hook = cfg
            .hook
            .as_ref()
            .map(|s| Hook::new(s, &self.dir, self.hook_env.clone()));
        Leds::new(cfg, hook)
    }

    /// Vibrate for `ms` milliseconds at `strength` percent (0..=100).
    pub fn rumble(&self, ms: u32, strength: u8) -> Result<()> {
        let cfg = self
            .config
            .rumble
            .as_ref()
            .ok_or(PlatformError::Unavailable("rumble"))?;
        if ms == 0 || ms > cfg.max_ms || strength > 100 {
            return Err(PlatformError::OutOfRange {
                what: "rumble",
                detail: format!("ms must be 1..={} and strength 0..=100", cfg.max_ms),
            });
        }
        self.hook(Some(&cfg.hook), "rumble")?
            .run(&[&ms.to_string(), &strength.to_string()])
            .map(drop)
    }

    // ---- Power ------------------------------------------------------------------------------

    /// Battery and supply state.
    pub fn battery(&self) -> Result<BatteryStatus> {
        power::read_battery(
            self.config
                .power
                .as_ref()
                .ok_or(PlatformError::Unavailable("battery"))?,
        )
    }

    /// SoC temperature in degrees Celsius.
    pub fn temperature_c(&self) -> Result<f64> {
        let zone = self
            .config
            .power
            .as_ref()
            .and_then(|p| p.thermal_zone.as_deref())
            .ok_or(PlatformError::Unavailable("temperature"))?;
        power::read_temperature_c(Path::new(zone))
    }

    /// Device health summary (uptime, load, memory, disk, temperature, model).
    pub fn health(&self) -> DeviceHealth {
        power::read_health(self.config.power.as_ref())
    }

    /// Hold (`true`) or release (`false`) the device awake.
    pub fn set_stay_awake(&self, on: bool) -> Result<()> {
        let spec = self
            .config
            .power
            .as_ref()
            .and_then(|p| p.stay_awake_hook.as_ref());
        self.hook(spec, "stay-awake")?
            .run(&[if on { "on" } else { "off" }])
            .map(drop)
    }

    // ---- Audio and display ------------------------------------------------------------------

    fn output_hook(&self) -> Result<Hook> {
        self.hook(self.audio()?.output_hook.as_ref(), "volume")
    }

    /// Speaker volume in percent.
    pub fn volume(&self) -> Result<u8> {
        parse_percent(&self.output_hook()?.run(&["volume-get"])?, "volume-get")
    }

    /// Set the speaker volume in percent, at most `audio.max_volume_percent`.
    pub fn set_volume(&self, percent: u8) -> Result<()> {
        let max = self.audio()?.max_volume_percent;
        if percent > max {
            return Err(PlatformError::OutOfRange {
                what: "volume",
                detail: format!("{percent}% exceeds the platform ceiling of {max}%"),
            });
        }
        self.output_hook()?
            .run(&["volume-set", &percent.to_string()])
            .map(drop)
    }

    /// Whether the speaker is muted.
    pub fn speaker_muted(&self) -> Result<bool> {
        match self.output_hook()?.run(&["mute-get"])?.as_str() {
            "1" => Ok(true),
            "0" => Ok(false),
            other => Err(PlatformError::BadValue {
                source_name: "mute-get".into(),
                value: other.to_owned(),
            }),
        }
    }

    /// Mute or unmute the speaker.
    pub fn set_speaker_muted(&self, muted: bool) -> Result<()> {
        self.output_hook()?
            .run(&["mute-set", if muted { "1" } else { "0" }])
            .map(drop)
    }

    fn mic_hook(&self) -> Result<(Hook, Option<[i32; 2]>)> {
        let audio = self.audio()?;
        Ok((
            self.hook(audio.mic_gain_hook.as_ref(), "mic gain")?,
            audio.mic_gain_range,
        ))
    }

    /// Microphone gain in dB.
    pub fn mic_gain(&self) -> Result<i32> {
        let (hook, _) = self.mic_hook()?;
        parse_int(&hook.run(&["get"])?, "mic gain get")
    }

    /// Set the microphone gain in dB. The returned guard restores the previous value.
    pub fn set_mic_gain(&self, db: i32) -> Result<MicGainGuard> {
        let (hook, range) = self.mic_hook()?;
        if let Some([lo, hi]) = range {
            if db < lo || db > hi {
                return Err(PlatformError::OutOfRange {
                    what: "mic gain",
                    detail: format!("{db} dB is outside {lo}..={hi}"),
                });
            }
        }
        let previous = parse_int(&hook.run(&["get"])?, "mic gain get")?;
        hook.run(&["set", &db.to_string()])?;
        Ok(MicGainGuard {
            hook,
            previous,
            active: true,
        })
    }

    /// Screen brightness in percent.
    pub fn brightness(&self) -> Result<u8> {
        let hook = self.hook(self.display()?.brightness_hook.as_ref(), "brightness")?;
        parse_percent(&hook.run(&["get"])?, "brightness get")
    }

    /// Set the screen brightness in percent.
    pub fn set_brightness(&self, percent: u8) -> Result<()> {
        if percent > 100 {
            return Err(PlatformError::OutOfRange {
                what: "brightness",
                detail: format!("{percent}% exceeds 100%"),
            });
        }
        let hook = self.hook(self.display()?.brightness_hook.as_ref(), "brightness")?;
        hook.run(&["set", &percent.to_string()]).map(drop)
    }

    // ---- Bluetooth ---------------------------------------------------------------------------

    /// Whether the platform brings the Bluetooth radio up itself (and may need the
    /// advertising data programmed by hand).
    pub fn has_bluetooth_hook(&self) -> bool {
        self.config.bluetooth.is_some()
    }

    /// Runs the Bluetooth hook: `["up"]`, `["advertise", adv_hex, scan_hex]` or `["down"]`.
    pub fn bluetooth(&self, args: &[&str]) -> Result<()> {
        self.hook(self.config.bluetooth.as_ref().map(|b| &b.hook), "bluetooth")?
            .run(args)
            .map(drop)
    }

    // ---- Autostart and firmware -------------------------------------------------------------

    fn autostart_hook(&self) -> Result<Hook> {
        self.hook(self.config.autostart.as_ref().map(|a| &a.hook), "autostart")
    }

    /// Whether the gadget starts on boot.
    pub fn autostart_enabled(&self) -> Result<bool> {
        match self.autostart_hook()?.run(&["status"])?.as_str() {
            "enabled" => Ok(true),
            "disabled" => Ok(false),
            other => Err(PlatformError::BadValue {
                source_name: "autostart status".into(),
                value: other.to_owned(),
            }),
        }
    }

    /// Enable or disable start on boot.
    pub fn set_autostart(&self, enabled: bool) -> Result<()> {
        self.autostart_hook()?
            .run(&[if enabled { "enable" } else { "disable" }])
            .map(drop)
    }

    /// Add a launcher entry that runs `command` (an absolute path to an executable).
    pub fn register_entry(&self, kind: EntryKind, name: &str, command: &str) -> Result<()> {
        check_entry_name(name)?;
        self.register_hook()?
            .run(&["add", kind.arg(), name, command])
            .map(drop)
    }

    /// Remove a launcher entry added by [`Platform::register_entry`].
    pub fn unregister_entry(&self, kind: EntryKind, name: &str) -> Result<()> {
        check_entry_name(name)?;
        self.register_hook()?
            .run(&["remove", kind.arg(), name])
            .map(drop)
    }

    fn register_hook(&self) -> Result<Hook> {
        self.hook(
            self.config.firmware.as_ref().map(|f| &f.register_hook),
            "firmware registration",
        )
    }
}

/// Entry names become file names on the firmware side, so keep them plain.
fn check_entry_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(PlatformError::OutOfRange {
            what: "entry name",
            detail: format!("{name:?} must be 1-64 of [A-Za-z0-9 ._-] and not start with '.'"),
        })
    }
}

fn parse_percent(text: &str, source: &str) -> Result<u8> {
    text.parse::<u8>()
        .ok()
        .filter(|v| *v <= 100)
        .ok_or_else(|| PlatformError::BadValue {
            source_name: source.to_owned(),
            value: text.to_owned(),
        })
}

fn parse_int(text: &str, source: &str) -> Result<i32> {
    text.parse().map_err(|_| PlatformError::BadValue {
        source_name: source.to_owned(),
        value: text.to_owned(),
    })
}

/// Environment passed to hooks: process variables win over `platform.toml` values.
fn hook_environment(
    config: &PlatformConfig,
    dir: &Path,
    probe: &dyn SystemProbe,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("GOBLIN_PLATFORM_ID".to_owned(), config.id.clone()),
        (
            "GOBLIN_PLATFORM_DIR".to_owned(),
            dir.to_string_lossy().into_owned(),
        ),
    ];
    let paths = &config.paths;
    let entries = [
        ("GOBLIN_DATA_DIR", paths.data.clone()),
        ("GOBLIN_GAMES_DIR", paths.games.clone()),
        ("GOBLIN_TOOLS_DIR", paths.tools.clone()),
        ("GOBLIN_TMP_DIR", Some(paths.tmp.clone())),
    ];
    for (name, configured) in entries {
        if let Some(value) = probe.env(name).or(configured) {
            env.push((name.to_owned(), value));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_platform(root: &Path, toml: &str, hooks: &[(&str, &str)]) -> PathBuf {
        let dir = root.join("t");
        std::fs::create_dir_all(dir.join("hooks")).expect("mkdir");
        std::fs::write(dir.join("platform.toml"), toml).expect("toml");
        for (name, body) in hooks {
            std::fs::write(dir.join("hooks").join(name), body).expect("hook");
        }
        dir
    }

    #[test]
    fn minimal_platform_reports_everything_unavailable() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = write_platform(root.path(), "id = \"t\"\n", &[]);
        let p = Platform::load(&dir).expect("load");
        let caps = p.capabilities();
        assert!(!caps.leds && !caps.rumble && !caps.battery && !caps.volume && !caps.autostart);
        assert!(p.leds().expect_err("leds").is_unavailable());
        assert!(p.rumble(10, 10).expect_err("rumble").is_unavailable());
        assert!(p.battery().expect_err("battery").is_unavailable());
        assert!(p.volume().expect_err("volume").is_unavailable());
        assert!(p.set_mic_gain(1).is_err());
        assert!(p
            .autostart_enabled()
            .expect_err("autostart")
            .is_unavailable());
        assert!(p.set_stay_awake(true).expect_err("awake").is_unavailable());
        assert!(p.brightness().expect_err("brightness").is_unavailable());
        assert!(p
            .register_entry(EntryKind::Tool, "x", "/bin/true")
            .expect_err("reg")
            .is_unavailable());
        assert!(!p.health().version.is_empty());
    }

    #[test]
    fn missing_hook_script_is_a_load_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = write_platform(
            root.path(),
            "id = \"t\"\n[rumble]\nhook = \"hooks/r.sh\"\n",
            &[],
        );
        let err = Platform::load(&dir).expect_err("must fail");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn rumble_validates_and_calls_hook() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = write_platform(
            root.path(),
            "id = \"t\"\n[paths]\ntmp = \"/nonexistent-tmp\"\n[rumble]\nhook = \"hooks/r.sh\"\nmax_ms = 500\n",
            &[("r.sh", "echo \"$1 $2\" > \"$GOBLIN_PLATFORM_DIR/out\"\n")],
        );
        let p = Platform::load(&dir).expect("load");
        assert!(p.rumble(0, 50).is_err());
        assert!(p.rumble(501, 50).is_err());
        assert!(p.rumble(100, 101).is_err());
        p.rumble(120, 60).expect("rumble");
        assert_eq!(
            std::fs::read_to_string(dir.join("out"))
                .expect("out")
                .trim(),
            "120 60"
        );
    }

    const AUDIO_TOML: &str = "id = \"t\"\n[audio]\noutput_hook = \"hooks/out.sh\"\nmax_volume_percent = 20\nmic_gain_hook = \"hooks/mic.sh\"\nmic_gain_range = [0, 31]\n";
    const OUT_SH: &str = "S=\"$GOBLIN_PLATFORM_DIR/state\"\ncase \"$1\" in\nvolume-get) cat \"$S.vol\" ;;\nvolume-set) echo \"$2\" > \"$S.vol\" ;;\nmute-get) cat \"$S.mute\" ;;\nmute-set) echo \"$2\" > \"$S.mute\" ;;\n*) exit 2 ;;\nesac\n";
    const MIC_SH: &str = "S=\"$GOBLIN_PLATFORM_DIR/gain\"\ncase \"$1\" in\nget) cat \"$S\" ;;\nset) echo \"$2\" > \"$S\" ;;\n*) exit 2 ;;\nesac\n";

    fn audio_platform(root: &Path) -> (PathBuf, Platform) {
        let dir = write_platform(root, AUDIO_TOML, &[("out.sh", OUT_SH), ("mic.sh", MIC_SH)]);
        std::fs::write(dir.join("state.vol"), "10\n").expect("w");
        std::fs::write(dir.join("state.mute"), "1\n").expect("w");
        std::fs::write(dir.join("gain"), "18\n").expect("w");
        let p = Platform::load(&dir).expect("load");
        (dir, p)
    }

    #[test]
    fn volume_ceiling_and_mute() {
        let root = tempfile::tempdir().expect("tempdir");
        let (_dir, p) = audio_platform(root.path());
        assert_eq!(p.volume().expect("vol"), 10);
        assert!(p.set_volume(21).is_err());
        p.set_volume(20).expect("set");
        assert_eq!(p.volume().expect("vol"), 20);
        assert!(p.speaker_muted().expect("mute"));
        p.set_speaker_muted(false).expect("unmute");
        assert!(!p.speaker_muted().expect("mute"));
    }

    #[test]
    fn mic_gain_guard_restores_on_drop_and_explicitly() {
        let root = tempfile::tempdir().expect("tempdir");
        let (dir, p) = audio_platform(root.path());
        let gain = || {
            std::fs::read_to_string(dir.join("gain"))
                .expect("gain")
                .trim()
                .to_owned()
        };
        assert!(p.set_mic_gain(32).is_err());
        {
            let guard = p.set_mic_gain(12).expect("set");
            assert_eq!(guard.previous(), 18);
            assert_eq!(gain(), "12");
        }
        assert_eq!(gain(), "18");
        let guard = p.set_mic_gain(5).expect("set");
        guard.restore().expect("restore");
        assert_eq!(gain(), "18");
        assert_eq!(p.mic_gain().expect("gain"), 18);
    }

    #[test]
    fn autostart_and_registration_contracts() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = write_platform(
            root.path(),
            "id = \"t\"\n[autostart]\nhook = \"hooks/a.sh\"\n[firmware]\nregister_hook = \"hooks/f.sh\"\n",
            &[
                ("a.sh", "S=\"$GOBLIN_PLATFORM_DIR/auto\"\ncase \"$1\" in\nenable) echo enabled > \"$S\" ;;\ndisable) echo disabled > \"$S\" ;;\nstatus) cat \"$S\" ;;\nesac\n"),
                ("f.sh", "echo \"$@\" > \"$GOBLIN_PLATFORM_DIR/reg\"\n"),
            ],
        );
        std::fs::write(dir.join("auto"), "disabled\n").expect("w");
        let p = Platform::load(&dir).expect("load");
        assert!(!p.autostart_enabled().expect("status"));
        p.set_autostart(true).expect("enable");
        assert!(p.autostart_enabled().expect("status"));
        p.register_entry(EntryKind::Tool, "Muse", "/opt/muse/bin/x")
            .expect("reg");
        assert_eq!(
            std::fs::read_to_string(dir.join("reg"))
                .expect("reg")
                .trim(),
            "add tool Muse /opt/muse/bin/x"
        );
        assert!(p.register_entry(EntryKind::Game, "../evil", "/x").is_err());
        assert!(p.register_entry(EntryKind::Game, ".hidden", "/x").is_err());
    }

    #[test]
    fn detect_in_picks_matching_platform() {
        let root = tempfile::tempdir().expect("tempdir");
        for (id, detect) in [("a", "env = { X = \"1\" }"), ("b", "fallback = true")] {
            let d = root.path().join(id);
            std::fs::create_dir(&d).expect("mkdir");
            std::fs::write(
                d.join("platform.toml"),
                format!("id = \"{id}\"\n[detect]\n{detect}\n"),
            )
            .expect("w");
        }
        struct P(Option<&'static str>);
        impl SystemProbe for P {
            fn env(&self, name: &str) -> Option<String> {
                (name == "X").then(|| self.0.map(str::to_owned)).flatten()
            }
            fn device_tree_model(&self) -> Option<String> {
                None
            }
        }
        assert_eq!(
            Platform::detect_in(root.path(), &P(Some("1")))
                .expect("a")
                .id(),
            "a"
        );
        assert_eq!(
            Platform::detect_in(root.path(), &P(None)).expect("b").id(),
            "b"
        );
    }

    #[test]
    fn environment_prefers_process_values() {
        struct P;
        impl SystemProbe for P {
            fn env(&self, name: &str) -> Option<String> {
                (name == "GOBLIN_TOOLS_DIR").then(|| "/override".to_owned())
            }
            fn device_tree_model(&self) -> Option<String> {
                None
            }
        }
        let cfg = PlatformConfig::parse(
            "id = \"t\"\n[paths]\ntools = \"/cfg\"\ndata = \"/d\"\n",
            Path::new("t"),
        )
        .expect("p");
        let env = hook_environment(&cfg, Path::new("/p"), &P);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("GOBLIN_TOOLS_DIR"), Some("/override"));
        assert_eq!(get("GOBLIN_DATA_DIR"), Some("/d"));
        assert_eq!(get("GOBLIN_TMP_DIR"), Some("/tmp"));
        assert_eq!(get("GOBLIN_GAMES_DIR"), None);
    }
}
