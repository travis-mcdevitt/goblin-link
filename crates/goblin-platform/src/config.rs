//! The `platform.toml` schema, parsing and validation.
//!
//! Every section except `id` is optional. A missing section means the capability is
//! unavailable on that platform. Unknown keys are rejected so typos do not silently disable
//! a capability.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::Deserialize;

use crate::error::{PlatformError, Result};

/// Default hook time limit when a hook does not set `timeout_ms`.
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;

/// Parsed and validated `platform.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    /// Stable identifier, lowercase letters, digits and `-`. Must equal the directory name.
    pub id: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// How to recognise this platform.
    #[serde(default)]
    pub detect: DetectRules,
    /// Screen geometry and brightness.
    #[serde(default)]
    pub display: Option<DisplayConfig>,
    /// Filesystem locations.
    #[serde(default)]
    pub paths: PathsConfig,
    /// SDL runtime hints.
    #[serde(default)]
    pub sdl: Option<SdlConfig>,
    /// Gamepad layout.
    #[serde(default)]
    pub input: Option<InputConfig>,
    /// Sound in and out.
    #[serde(default)]
    pub audio: Option<AudioConfig>,
    /// Status lights.
    #[serde(default)]
    pub leds: Option<LedConfig>,
    /// Vibration motor.
    #[serde(default)]
    pub rumble: Option<RumbleConfig>,
    /// Battery, temperature and sleep control.
    #[serde(default)]
    pub power: Option<PowerConfig>,
    /// Start-on-boot control.
    #[serde(default)]
    pub autostart: Option<AutostartConfig>,
    /// Bluetooth radio control for pairing.
    #[serde(default)]
    pub bluetooth: Option<BluetoothConfig>,
    /// Registration of launcher entries with the firmware.
    #[serde(default)]
    pub firmware: Option<FirmwareConfig>,
    /// TLS trust store.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    /// Whether someone has run the app on this device and found it working. `false` for a
    /// platform written from documentation alone.
    #[serde(default = "default_true")]
    pub qualified: bool,
    /// The product's name, as its maker sells it ("TrimUI Brick").
    #[serde(default)]
    pub model: Option<String>,
    /// The firmware the owner sees.
    #[serde(default)]
    pub os: Option<OsConfig>,
    /// What the device has besides what the other sections say.
    #[serde(default)]
    pub hardware: Option<HardwareConfig>,
    /// What an owner of this firmware expects everywhere: how to change the volume, what
    /// closes an app, and so on. One plain sentence each, keyed by a short name.
    #[serde(default)]
    pub os_conventions: BTreeMap<String, String>,
    /// Where software lives on this firmware: pointers for an agent to follow, not an
    /// inventory.
    #[serde(default)]
    pub software: Option<SoftwareConfig>,
}

/// Where to look for what is installed (`[software]`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareConfig {
    /// The package database of the base system, if it has one, and what reads it.
    #[serde(default)]
    pub package_database: Option<String>,
    /// Folders of system libraries that are the same on every unit of this firmware. Their
    /// file names are reported as they are found.
    #[serde(default)]
    pub library_dirs: Vec<String>,
    /// Other places software is kept, with a few words on what is there.
    #[serde(default)]
    pub places: Vec<SoftwarePlace>,
}

/// One place software is kept.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwarePlace {
    /// The folder.
    pub path: String,
    /// What is found there.
    pub what: String,
}

/// The firmware (`[os]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsConfig {
    /// Its name ("NextUI").
    pub name: String,
    /// A file whose first line is the installed version.
    #[serde(default)]
    pub version_file: Option<String>,
    /// What it is built on ("Tina Linux").
    #[serde(default)]
    pub base: Option<String>,
    /// Where its source and documentation are, in full ("github.com/LoveRetro/NextUI").
    #[serde(default)]
    pub repository: Option<String>,
}

/// Hardware facts no other section carries (`[hardware]`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareConfig {
    /// The screen takes touch.
    #[serde(default)]
    pub touch: bool,
    /// There is a keyboard.
    #[serde(default)]
    pub keyboard: bool,
    /// There is Wi-Fi.
    #[serde(default)]
    pub wifi: bool,
    /// There is Bluetooth.
    #[serde(default)]
    pub bluetooth: bool,
    /// It can drive an external display.
    #[serde(default)]
    pub video_out: bool,
    /// It has an accelerometer or gyroscope.
    #[serde(default)]
    pub motion_sensors: bool,
    /// It has a satellite positioning receiver.
    #[serde(default)]
    pub gps: bool,
    /// It has a camera.
    #[serde(default)]
    pub camera: bool,
    /// It has a headphone socket.
    #[serde(default)]
    pub headphone_jack: bool,
    /// The processor, as its maker names it.
    #[serde(default)]
    pub cpu: Option<String>,
    /// The graphics processor and the API it offers.
    #[serde(default)]
    pub gpu: Option<String>,
    /// What can be plugged into it: its USB host ports and what the system can drive.
    #[serde(default)]
    pub usb_host: Option<String>,
    /// What it can be plugged into: how it appears to a computer.
    #[serde(default)]
    pub usb_device: Option<String>,
}

/// A script reference: either a bare path or a table with a custom time limit.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum HookSpec {
    /// `hook = "hooks/x.sh"`.
    Path(String),
    /// `hook = { path = "hooks/x.sh", timeout_ms = 2000 }`.
    Detailed {
        /// Script path relative to the platform directory.
        path: String,
        /// Time limit in milliseconds.
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
}

impl HookSpec {
    /// Script path relative to the platform directory.
    pub fn path(&self) -> &str {
        match self {
            Self::Path(p) | Self::Detailed { path: p, .. } => p,
        }
    }

    /// Effective time limit in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        match self {
            Self::Detailed {
                timeout_ms: Some(t),
                ..
            } => *t,
            _ => DEFAULT_HOOK_TIMEOUT_MS,
        }
    }
}

/// Rules deciding whether a platform matches the running device. A platform matches when
/// **any** criterion holds, so one firmware can be recognised by environment or by hardware.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectRules {
    /// All listed variables must have exactly these values.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `/proc/device-tree/model` contains this text.
    #[serde(default)]
    pub device_tree_model_contains: Option<String>,
    /// A file holds this exact text as a string of its own, as `strings FILE | grep -x TEXT`
    /// would find it. How firmwares that share a platform name tell their devices apart.
    #[serde(default)]
    pub string_in_file: Option<StringInFile>,
    /// Match when nothing else does (desktop development).
    #[serde(default)]
    pub fallback: bool,
}

/// A text to look for in a file (`[detect.string_in_file]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StringInFile {
    /// The file.
    pub path: String,
    /// The whole string.
    pub text: String,
}

/// Screen geometry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// UI scale relative to the 1024x768 reference layout.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// Rotation in degrees: 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
    /// Brightness control script (`get`, `set <percent>`).
    #[serde(default)]
    pub brightness_hook: Option<HookSpec>,
}

fn default_scale() -> f32 {
    1.0
}

/// Filesystem locations. Environment variables `GOBLIN_DATA_DIR`, `GOBLIN_GAMES_DIR`,
/// `GOBLIN_TOOLS_DIR` and `GOBLIN_TMP_DIR` override these at run time.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsConfig {
    /// Where the gadget keeps profiles and caches.
    #[serde(default)]
    pub data: Option<String>,
    /// Game/ROM root.
    #[serde(default)]
    pub games: Option<String>,
    /// Firmware tool/launcher root.
    #[serde(default)]
    pub tools: Option<String>,
    /// Scratch space.
    #[serde(default = "default_tmp")]
    pub tmp: String,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data: None,
            games: None,
            tools: None,
            tmp: default_tmp(),
        }
    }
}

fn default_tmp() -> String {
    "/tmp".to_owned()
}

/// SDL runtime hints.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdlConfig {
    /// Directories to prepend to `LD_LIBRARY_PATH` before loading SDL.
    #[serde(default)]
    pub library_path: Vec<String>,
}

/// Gamepad layout: joystick button indices and axes. Indices are SDL joystick indices.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// Name of the controller mapping family, for example `gamepad-xbox360`.
    #[serde(default)]
    pub map: Option<String>,
    /// Face-button arrangement: `nintendo` (A right, B bottom) or `xbox`.
    #[serde(default)]
    pub layout: Option<String>,
    /// Button index for A.
    #[serde(default)]
    pub a: Option<u8>,
    /// Button index for B.
    #[serde(default)]
    pub b: Option<u8>,
    /// Button index for X.
    #[serde(default)]
    pub x: Option<u8>,
    /// Button index for Y.
    #[serde(default)]
    pub y: Option<u8>,
    /// Button index for L1.
    #[serde(default)]
    pub l1: Option<u8>,
    /// Button index for R1.
    #[serde(default)]
    pub r1: Option<u8>,
    /// Button index for Select.
    #[serde(default)]
    pub select: Option<u8>,
    /// Button index for Start.
    #[serde(default)]
    pub start: Option<u8>,
    /// Button index for Menu.
    #[serde(default)]
    pub menu: Option<u8>,
    /// Button index for the F1 function button.
    #[serde(default)]
    pub f1: Option<u8>,
    /// Button index for the F2 function button.
    #[serde(default)]
    pub f2: Option<u8>,
    /// Button index for hardware volume down.
    #[serde(default)]
    pub vol_down: Option<u8>,
    /// Button index for hardware volume up.
    #[serde(default)]
    pub vol_up: Option<u8>,
    /// Axis index of the left trigger.
    #[serde(default)]
    pub l2_axis: Option<u8>,
    /// Axis index of the right trigger.
    #[serde(default)]
    pub r2_axis: Option<u8>,
    /// Glyph (icon) name to draw for a logical button, for example `a = "btn-a-nintendo"`.
    #[serde(default)]
    pub glyphs: BTreeMap<String, String>,
}

/// Capture channel selection for a multi-channel microphone device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureUse {
    /// Use channel 0 only.
    Left,
    /// Use channel 1 only.
    Right,
    /// Average both channels.
    Mix,
}

/// Sound in and out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    /// ALSA playback device.
    #[serde(default)]
    pub playback: Option<String>,
    /// ALSA capture device.
    #[serde(default)]
    pub capture: Option<String>,
    /// Number of channels to open the capture device with (1 or 2).
    #[serde(default = "default_capture_channels")]
    pub capture_channels: u8,
    /// Which captured channel carries the built-in microphone.
    #[serde(default = "default_capture_use")]
    pub capture_use: CaptureUse,
    /// Output control script: `volume-get`, `volume-set <percent>`, `mute-get`, `mute-set 0|1`.
    #[serde(default)]
    pub output_hook: Option<HookSpec>,
    /// Microphone gain script: `get`, `set <db>`.
    #[serde(default)]
    pub mic_gain_hook: Option<HookSpec>,
    /// Inclusive legal range of the microphone gain, in dB.
    #[serde(default)]
    pub mic_gain_range: Option<[i32; 2]>,
}

fn default_true() -> bool {
    true
}
fn default_capture_channels() -> u8 {
    1
}
fn default_capture_use() -> CaptureUse {
    CaptureUse::Left
}

/// How LEDs are driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LedDriver {
    /// The built-in driver for the TrimUI `/sys/class/led_anim` interface.
    #[serde(rename = "trimui-led-anim")]
    TrimuiLedAnim,
    /// A hook script (`set`, `off`, `snapshot`, `restore`).
    #[serde(rename = "hook")]
    Hook,
}

/// Status lights.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedConfig {
    /// Which driver to use.
    pub driver: LedDriver,
    /// sysfs directory of the built-in driver.
    #[serde(default = "default_led_sysfs")]
    pub sysfs_path: String,
    /// Hook for `driver = "hook"`.
    #[serde(default)]
    pub hook: Option<HookSpec>,
    /// Addressable zones.
    pub zones: Vec<String>,
    /// Highest brightness the hardware accepts.
    pub max_brightness: u32,
    /// Zones used for background presence signalling (must be a subset of `zones`).
    #[serde(default)]
    pub presence_zones: Vec<String>,
    /// Zones that also show "working" and "attention", and otherwise keep the firmware's own
    /// setting (e.g. button backlights that face the user). Subset of `zones`.
    #[serde(default)]
    pub accent_zones: Vec<String>,
    /// Total LEDs addressable through the raw frame interface, when there is one.
    #[serde(default)]
    pub frame_leds: Option<u32>,
    /// Where each zone is on the device, for display in settings.
    #[serde(default)]
    pub locations: BTreeMap<String, String>,
}

fn default_led_sysfs() -> String {
    "/sys/class/led_anim".to_owned()
}

/// Vibration motor.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RumbleConfig {
    /// Script invoked as `rumble.sh <milliseconds> <strength 0-100>`.
    pub hook: HookSpec,
    /// Longest pulse the platform allows.
    #[serde(default = "default_rumble_max_ms")]
    pub max_ms: u32,
}

fn default_rumble_max_ms() -> u32 {
    2_000
}

/// Battery, temperature and sleep control.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerConfig {
    /// `power_supply` directory of the battery.
    #[serde(default)]
    pub battery: Option<String>,
    /// `power_supply` directory of the external supply (has an `online` file).
    #[serde(default)]
    pub usb: Option<String>,
    /// File containing the SoC temperature in millidegrees Celsius.
    #[serde(default)]
    pub thermal_zone: Option<String>,
    /// Script invoked as `stay-awake.sh on|off`.
    #[serde(default)]
    pub stay_awake_hook: Option<HookSpec>,
}

/// Bluetooth radio control for pairing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BluetoothConfig {
    /// Script invoked as `bluetooth.sh up|advertise <adv-hex> <scan-hex>|down`.
    pub hook: HookSpec,
}

/// Start-on-boot control.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutostartConfig {
    /// Script invoked as `autostart.sh enable|disable|status`.
    pub hook: HookSpec,
}

/// Registration of launcher entries with the firmware.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareConfig {
    /// Script invoked as `register.sh add|remove game|tool <name> [<command>]`.
    pub register_hook: HookSpec,
}

/// TLS trust store.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// `embedded` or a path to a PEM bundle.
    pub ca_bundle: String,
}

impl PlatformConfig {
    /// Parse TOML text and validate it.
    pub fn parse(text: &str, origin: &Path) -> Result<Self> {
        let config: Self = toml::from_str(text).map_err(|e| PlatformError::Parse {
            path: origin.to_owned(),
            message: e.message().to_owned(),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Every hook script path declared by this configuration.
    pub fn hook_specs(&self) -> Vec<&HookSpec> {
        let mut hooks = Vec::new();
        hooks.extend(
            self.display
                .iter()
                .filter_map(|d| d.brightness_hook.as_ref()),
        );
        if let Some(a) = &self.audio {
            hooks.extend(a.output_hook.iter().chain(a.mic_gain_hook.iter()));
        }
        hooks.extend(self.leds.iter().filter_map(|l| l.hook.as_ref()));
        hooks.extend(self.rumble.iter().map(|r| &r.hook));
        hooks.extend(self.power.iter().filter_map(|p| p.stay_awake_hook.as_ref()));
        hooks.extend(self.autostart.iter().map(|a| &a.hook));
        hooks.extend(self.bluetooth.iter().map(|b| &b.hook));
        hooks.extend(self.firmware.iter().map(|f| &f.register_hook));
        hooks
    }

    /// Check every rule the type system cannot express.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(PlatformError::Invalid(m));
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return bad(format!(
                "id {:?} must be lowercase letters, digits and '-'",
                self.id
            ));
        }
        if let Some(d) = &self.display {
            if d.width == 0 || d.height == 0 {
                return bad("display width and height must be positive".into());
            }
            if !(d.scale.is_finite() && d.scale > 0.0) {
                return bad("display.scale must be positive".into());
            }
            if !matches!(d.rotation, 0 | 90 | 180 | 270) {
                return bad(format!(
                    "display.rotation {} is not 0/90/180/270",
                    d.rotation
                ));
            }
        }
        if let Some(i) = &self.input {
            validate_input(i)?;
        }
        if let Some(a) = &self.audio {
            if !(1..=2).contains(&a.capture_channels) {
                return bad("audio.capture_channels must be 1 or 2".into());
            }
            if a.capture_use == CaptureUse::Right && a.capture_channels < 2 {
                return bad("audio.capture_use = \"right\" needs capture_channels = 2".into());
            }
            if let Some([lo, hi]) = a.mic_gain_range {
                if lo > hi {
                    return bad("audio.mic_gain_range must be [min, max] with min <= max".into());
                }
            }
            if a.mic_gain_range.is_some() && a.mic_gain_hook.is_none() {
                return bad("audio.mic_gain_range needs audio.mic_gain_hook".into());
            }
        }
        if let Some(l) = &self.leds {
            validate_leds(l)?;
        }
        if let Some(r) = &self.rumble {
            if r.max_ms == 0 {
                return bad("rumble.max_ms must be positive".into());
            }
        }
        for hook in self.hook_specs() {
            validate_hook_path(hook.path())?;
            if hook.timeout_ms() == 0 {
                return bad(format!("hook {} has timeout_ms = 0", hook.path()));
            }
        }
        Ok(())
    }
}

fn validate_input(i: &InputConfig) -> Result<()> {
    let buttons = [
        ("a", i.a),
        ("b", i.b),
        ("x", i.x),
        ("y", i.y),
        ("l1", i.l1),
        ("r1", i.r1),
        ("select", i.select),
        ("start", i.start),
        ("menu", i.menu),
        ("f1", i.f1),
        ("f2", i.f2),
        ("vol_down", i.vol_down),
        ("vol_up", i.vol_up),
    ];
    let mut seen: BTreeMap<u8, &str> = BTreeMap::new();
    for (name, index) in buttons {
        if let Some(index) = index {
            if let Some(other) = seen.insert(index, name) {
                return Err(PlatformError::Invalid(format!(
                    "input buttons {other} and {name} share index {index}"
                )));
            }
        }
    }
    if let Some(layout) = &i.layout {
        if !matches!(layout.as_str(), "nintendo" | "xbox") {
            return Err(PlatformError::Invalid(format!(
                "input.layout {layout:?} must be \"nintendo\" or \"xbox\""
            )));
        }
    }
    if let (Some(l), Some(r)) = (i.l2_axis, i.r2_axis) {
        if l == r {
            return Err(PlatformError::Invalid(
                "input.l2_axis and input.r2_axis must differ".into(),
            ));
        }
    }
    Ok(())
}

fn validate_leds(l: &LedConfig) -> Result<()> {
    let bad = |m: String| Err(PlatformError::Invalid(m));
    if l.zones.is_empty() {
        return bad("leds.zones must not be empty".into());
    }
    let mut unique = BTreeSet::new();
    for zone in &l.zones {
        let valid = !zone.is_empty()
            && zone
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return bad(format!("leds zone {zone:?} must be [a-z0-9_]+"));
        }
        if !unique.insert(zone.as_str()) {
            return bad(format!("leds zone {zone:?} listed twice"));
        }
    }
    if l.max_brightness == 0 {
        return bad("leds.max_brightness must be positive".into());
    }
    for zone in l
        .presence_zones
        .iter()
        .chain(&l.accent_zones)
        .chain(l.locations.keys())
    {
        if !unique.contains(zone.as_str()) {
            return bad(format!("leds refers to unknown zone {zone:?}"));
        }
    }
    if l.driver == LedDriver::Hook && l.hook.is_none() {
        return bad("leds.driver = \"hook\" needs leds.hook".into());
    }
    Ok(())
}

/// Hook paths must stay inside the platform directory.
fn validate_hook_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    let escapes = p.is_absolute()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        || path.is_empty();
    if escapes {
        return Err(PlatformError::Invalid(format!(
            "hook path {path:?} must be relative to the platform directory and contain no '..'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PlatformConfig> {
        PlatformConfig::parse(text, Path::new("test.toml"))
    }

    fn invalid(text: &str, needle: &str) {
        let err = parse(text).expect_err("must be rejected").to_string();
        assert!(err.contains(needle), "expected {needle:?} in {err:?}");
    }

    #[test]
    fn minimal_file_gets_defaults() {
        let c = parse("id = \"x\"\n").expect("parse");
        assert_eq!(c.paths.tmp, "/tmp");
        assert!(c.leds.is_none() && c.audio.is_none() && c.rumble.is_none());
        assert!(c.hook_specs().is_empty());
    }

    #[test]
    fn shipped_platforms_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms");
        let all = crate::detect::load_all(&root).expect("load shipped platforms");
        let ids: Vec<_> = all.iter().map(|(_, c)| c.id.as_str()).collect();
        assert!(
            ids.contains(&"tg5040-brick-nextui") && ids.contains(&"generic-sdl"),
            "{ids:?}"
        );
        for (dir, config) in &all {
            for hook in config.hook_specs() {
                assert!(dir.join(hook.path()).is_file(), "{} missing", hook.path());
            }
        }
    }

    #[test]
    fn hook_spec_forms_and_timeouts() {
        let c = parse(
            "id = \"x\"\n[rumble]\nhook = { path = \"hooks/r.sh\", timeout_ms = 900 }\n[autostart]\nhook = \"hooks/a.sh\"\n",
        )
        .expect("parse");
        assert_eq!(c.rumble.as_ref().expect("r").hook.timeout_ms(), 900);
        assert_eq!(
            c.autostart.as_ref().expect("a").hook.timeout_ms(),
            DEFAULT_HOOK_TIMEOUT_MS
        );
        assert_eq!(c.hook_specs().len(), 2);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse("id = \"x\"\nbogus = 1\n").is_err());
        assert!(parse(
            "id = \"x\"\n[leds]\ndriver = \"hook\"\nhook = \"h.sh\"\nzones = [\"a\"]\nmax_brightness = 1\ntypo = 2\n"
        )
        .is_err());
    }

    #[test]
    fn rejects_bad_ids_and_display() {
        invalid("id = \"Bad_Id\"\n", "id");
        invalid("id = \"\"\n", "id");
        invalid("id = \"x\"\n[display]\nwidth = 0\nheight = 1\n", "positive");
        invalid(
            "id = \"x\"\n[display]\nwidth = 1\nheight = 1\nscale = 0.0\n",
            "scale",
        );
        invalid(
            "id = \"x\"\n[display]\nwidth = 1\nheight = 1\nrotation = 45\n",
            "rotation",
        );
    }

    #[test]
    fn rejects_hook_paths_that_escape() {
        invalid("id = \"x\"\n[autostart]\nhook = \"/bin/sh\"\n", "relative");
        invalid("id = \"x\"\n[autostart]\nhook = \"../x.sh\"\n", "relative");
        invalid(
            "id = \"x\"\n[autostart]\nhook = \"hooks/../../x.sh\"\n",
            "relative",
        );
        invalid(
            "id = \"x\"\n[autostart]\nhook = { path = \"a.sh\", timeout_ms = 0 }\n",
            "timeout",
        );
    }

    #[test]
    fn rejects_duplicate_button_indices_and_bad_layout() {
        invalid("id = \"x\"\n[input]\na = 1\nb = 1\n", "share index");
        invalid("id = \"x\"\n[input]\nlayout = \"sega\"\n", "layout");
        invalid("id = \"x\"\n[input]\nl2_axis = 2\nr2_axis = 2\n", "differ");
        assert!(parse("id = \"x\"\n[input]\na = 300\n").is_err());
    }

    #[test]
    fn rejects_bad_audio() {
        invalid(
            "id = \"x\"\n[audio]\ncapture_channels = 3\n",
            "capture_channels",
        );
        invalid("id = \"x\"\n[audio]\ncapture_use = \"right\"\n", "right");
        invalid(
            "id = \"x\"\n[audio]\nmic_gain_range = [5, 1]\nmic_gain_hook = \"h.sh\"\n",
            "min <= max",
        );
        invalid(
            "id = \"x\"\n[audio]\nmic_gain_range = [0, 1]\n",
            "mic_gain_hook",
        );
    }

    #[test]
    fn rejects_bad_leds() {
        let head = "id = \"x\"\n[leds]\ndriver = \"trimui-led-anim\"\nmax_brightness = 60\n";
        invalid(&format!("{head}zones = []\n"), "empty");
        invalid(&format!("{head}zones = [\"a\", \"a\"]\n"), "twice");
        invalid(&format!("{head}zones = [\"A\"]\n"), "[a-z0-9_]");
        invalid(
            &format!("{head}zones = [\"a\"]\npresence_zones = [\"b\"]\n"),
            "unknown zone",
        );
        invalid(
            "id = \"x\"\n[leds]\ndriver = \"trimui-led-anim\"\nmax_brightness = 0\nzones = [\"a\"]\n",
            "max_brightness",
        );
        invalid(
            "id = \"x\"\n[leds]\ndriver = \"hook\"\nmax_brightness = 1\nzones = [\"a\"]\n",
            "needs leds.hook",
        );
        assert!(parse(
            "id = \"x\"\n[leds]\ndriver = \"magic\"\nmax_brightness = 1\nzones = [\"a\"]\n"
        )
        .is_err());
    }

    #[test]
    fn syntax_errors_name_the_file() {
        let err = parse("id = ").expect_err("syntax").to_string();
        assert!(err.contains("test.toml"), "{err}");
    }
}
