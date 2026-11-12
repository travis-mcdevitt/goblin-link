//! LED control: colour, effect, duration, cycles, raw frames, and save/restore of the
//! firmware's own settings.
//!
//! Two drivers exist. The built-in `trimui-led-anim` driver talks to `/sys/class/led_anim`
//! directly. The `hook` driver delegates to a script with this contract:
//!
//! * `leds.sh set <zone> <rrggbb> <effect> <duration_ms> <cycles>` where `effect` is one of
//!   [`LedEffect::name`] and `cycles` is `-1` for endless;
//! * `leds.sh off <zone>`;
//! * `leds.sh brightness <n>` (the maximum the hardware may use, 0..=`max_brightness`);
//! * `leds.sh snapshot` prints an opaque token (one line of text);
//! * `leds.sh restore` receives that token on standard input.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::{LedConfig, LedDriver};
use crate::error::{PlatformError, Result};
use crate::hook::Hook;

/// An 8-bit RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// All channels off.
    pub const BLACK: Rgb = Rgb(0, 0, 0);

    /// Scale every channel by `numerator / 255`.
    pub fn scaled(self, numerator: u8) -> Rgb {
        let s = |c: u8| ((u16::from(c) * u16::from(numerator)) / 255) as u8;
        Rgb(s(self.0), s(self.1), s(self.2))
    }

    /// `RRGGBB` upper-case hex.
    pub fn hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.hex())
    }
}

impl FromStr for Rgb {
    type Err = PlatformError;

    /// Accepts `RRGGBB`, `#RRGGBB`, `0xRRGGBB` and a few colour names.
    fn from_str(s: &str) -> Result<Self> {
        let named = match s.to_ascii_lowercase().as_str() {
            "black" | "off" => Some(Rgb(0, 0, 0)),
            "white" => Some(Rgb(255, 255, 255)),
            "red" => Some(Rgb(255, 0, 0)),
            "green" => Some(Rgb(0, 255, 0)),
            "blue" => Some(Rgb(0, 0, 255)),
            "yellow" => Some(Rgb(255, 180, 0)),
            "purple" => Some(Rgb(160, 0, 255)),
            "cyan" => Some(Rgb(0, 255, 255)),
            "orange" => Some(Rgb(255, 90, 0)),
            _ => None,
        };
        if let Some(rgb) = named {
            return Ok(rgb);
        }
        let digits = s
            .trim_start_matches('#')
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let bad = || PlatformError::BadValue {
            source_name: "colour".into(),
            value: s.to_owned(),
        };
        if digits.len() != 6 || !digits.is_ascii() {
            return Err(bad());
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| bad());
        Ok(Rgb(channel(0)?, channel(2)?, channel(4)?))
    }
}

/// Animation applied to a zone. Numbering follows the TrimUI driver's `effect_names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedEffect {
    /// Zone off.
    Off,
    /// Linear fade up and down.
    Linear,
    /// Smooth "breathing".
    Breath,
    /// Sweeping highlight.
    Sniff,
    /// Constant colour.
    Static,
    /// Blink pattern 1.
    Blink1,
    /// Blink pattern 2.
    Blink2,
    /// Blink pattern 3.
    Blink3,
}

impl LedEffect {
    /// All effects in driver order.
    pub const ALL: [LedEffect; 8] = [
        Self::Off,
        Self::Linear,
        Self::Breath,
        Self::Sniff,
        Self::Static,
        Self::Blink1,
        Self::Blink2,
        Self::Blink3,
    ];

    /// The driver's numeric code.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Effect for a driver code.
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.get(usize::from(code)).copied()
    }

    /// Lowercase name used on command lines and in the hook contract.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Linear => "linear",
            Self::Breath => "breath",
            Self::Sniff => "sniff",
            Self::Static => "static",
            Self::Blink1 => "blink1",
            Self::Blink2 => "blink2",
            Self::Blink3 => "blink3",
        }
    }
}

impl FromStr for LedEffect {
    type Err = PlatformError;

    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|e| e.name() == s.to_ascii_lowercase())
            .ok_or_else(|| PlatformError::BadValue {
                source_name: "led effect".into(),
                value: s.to_owned(),
            })
    }
}

/// How many animation cycles to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cycles {
    /// Until changed.
    Forever,
    /// A fixed number of cycles.
    Count(u32),
}

impl Cycles {
    fn driver_value(self) -> i64 {
        match self {
            Self::Forever => -1,
            Self::Count(n) => i64::from(n),
        }
    }

    fn from_driver_value(v: i64) -> Self {
        u32::try_from(v).map_or(Self::Forever, Self::Count)
    }
}

/// One zone's complete setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedSetting {
    /// Target colour.
    pub colour: Rgb,
    /// Animation.
    pub effect: LedEffect,
    /// Duration of one animation cycle in milliseconds.
    pub duration_ms: u32,
    /// Number of cycles.
    pub cycles: Cycles,
}

impl LedSetting {
    /// A constant colour.
    pub fn solid(colour: Rgb) -> Self {
        Self {
            colour,
            effect: LedEffect::Static,
            duration_ms: 1000,
            cycles: Cycles::Forever,
        }
    }
}

/// Everything needed to put the lights back the way the firmware left them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedSnapshot(SnapshotData);

impl LedSnapshot {
    /// The recorded setting of one zone (built-in driver only).
    pub fn zone(&self, name: &str) -> Option<LedSetting> {
        match &self.0 {
            SnapshotData::Sysfs { zones, .. } => {
                zones.iter().find(|(z, _)| z == name).map(|(_, s)| *s)
            }
            SnapshotData::Hook(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotData {
    Sysfs {
        zones: Vec<(String, LedSetting)>,
        scales: Vec<(&'static str, u32)>,
    },
    Hook(String),
}

#[derive(Debug)]
enum Backend {
    Sysfs(PathBuf),
    Hook(Hook),
}

/// LED controller for one platform.
#[derive(Debug)]
pub struct Leds {
    zones: Vec<String>,
    max_brightness: u32,
    frame_leds: Option<u32>,
    backend: Backend,
}

/// Brightness-ceiling files of the TrimUI driver (`max_scale` covers `m`, `max_scale_f1f2` the
/// face buttons, `max_scale_lr` the side zones); all three are always set together.
const SCALE_FILES: [&str; 3] = ["max_scale", "max_scale_f1f2", "max_scale_lr"];

impl Leds {
    /// Build a controller from configuration. `hook` must be supplied for the hook driver.
    pub fn new(config: &LedConfig, hook: Option<Hook>) -> Result<Self> {
        let backend = match (config.driver, hook) {
            (LedDriver::TrimuiLedAnim, _) => Backend::Sysfs(PathBuf::from(&config.sysfs_path)),
            (LedDriver::Hook, Some(h)) => Backend::Hook(h),
            (LedDriver::Hook, None) => return Err(PlatformError::Unavailable("leds hook")),
        };
        Ok(Self {
            zones: config.zones.clone(),
            max_brightness: config.max_brightness,
            frame_leds: config.frame_leds,
            backend,
        })
    }

    /// Declared zones.
    pub fn zones(&self) -> &[String] {
        &self.zones
    }

    /// Highest brightness the hardware accepts.
    pub fn max_brightness(&self) -> u32 {
        self.max_brightness
    }

    fn check_zone(&self, zone: &str) -> Result<()> {
        if self.zones.iter().any(|z| z == zone) {
            Ok(())
        } else {
            Err(PlatformError::OutOfRange {
                what: "led zone",
                detail: format!("{zone:?} is not one of {:?}", self.zones),
            })
        }
    }

    /// Apply `setting` to `zone`.
    pub fn set(&self, zone: &str, setting: LedSetting) -> Result<()> {
        self.check_zone(zone)?;
        match &self.backend {
            Backend::Sysfs(dir) => {
                // The `effect_<zone>` file starts the animation, so it is written last.
                write_sysfs(
                    dir,
                    &format!("effect_rgb_hex_{zone}"),
                    &format!("{} ", setting.colour.hex()),
                )?;
                write_sysfs(
                    dir,
                    &format!("effect_duration_{zone}"),
                    &setting.duration_ms.to_string(),
                )?;
                write_sysfs(
                    dir,
                    &format!("effect_cycles_{zone}"),
                    &setting.cycles.driver_value().to_string(),
                )?;
                write_sysfs(
                    dir,
                    &format!("effect_{zone}"),
                    &setting.effect.code().to_string(),
                )
            }
            Backend::Hook(hook) => hook
                .run(&[
                    "set",
                    zone,
                    &setting.colour.hex(),
                    setting.effect.name(),
                    &setting.duration_ms.to_string(),
                    &setting.cycles.driver_value().to_string(),
                ])
                .map(drop),
        }
    }

    /// Turn `zone` off.
    pub fn off(&self, zone: &str) -> Result<()> {
        self.check_zone(zone)?;
        match &self.backend {
            Backend::Sysfs(dir) => write_sysfs(dir, &format!("effect_{zone}"), "0"),
            Backend::Hook(hook) => hook.run(&["off", zone]).map(drop),
        }
    }

    /// Raise or lower the hardware brightness ceiling (applies to every zone).
    pub fn set_max_brightness(&self, value: u32) -> Result<()> {
        if value > self.max_brightness {
            return Err(PlatformError::OutOfRange {
                what: "led brightness",
                detail: format!("{value} exceeds {}", self.max_brightness),
            });
        }
        match &self.backend {
            Backend::Sysfs(dir) => SCALE_FILES
                .iter()
                .try_for_each(|f| write_sysfs(dir, f, &value.to_string())),
            Backend::Hook(hook) => hook.run(&["brightness", &value.to_string()]).map(drop),
        }
    }

    /// Set every LED individually through the raw frame interface (built-in driver only).
    pub fn set_frame(&self, colours: &[Rgb]) -> Result<()> {
        let Backend::Sysfs(dir) = &self.backend else {
            return Err(PlatformError::Unavailable("led frames"));
        };
        let count = self
            .frame_leds
            .ok_or(PlatformError::Unavailable("led frames"))?;
        if colours.len() != count as usize {
            return Err(PlatformError::OutOfRange {
                what: "led frame",
                detail: format!("need {count} colours, got {}", colours.len()),
            });
        }
        let text: String = colours.iter().map(|c| format!("{} ", c.hex())).collect();
        write_sysfs(dir, "frame_hex", &text)
    }

    /// Record the current state so [`Leds::restore`] can put it back.
    pub fn snapshot(&self) -> Result<LedSnapshot> {
        match &self.backend {
            Backend::Sysfs(dir) => {
                let mut zones = Vec::new();
                for zone in &self.zones {
                    zones.push((zone.clone(), read_zone(dir, zone)?));
                }
                let mut scales = Vec::new();
                for file in SCALE_FILES {
                    scales.push((file, parse_num(&read_sysfs(dir, file)?, file)?));
                }
                Ok(LedSnapshot(SnapshotData::Sysfs { zones, scales }))
            }
            Backend::Hook(hook) => Ok(LedSnapshot(SnapshotData::Hook(hook.run(&["snapshot"])?))),
        }
    }

    /// Put the lights back as recorded by [`Leds::snapshot`]. Attempts every zone and reports
    /// the first failure.
    pub fn restore(&self, snapshot: &LedSnapshot) -> Result<()> {
        match (&self.backend, &snapshot.0) {
            (Backend::Sysfs(dir), SnapshotData::Sysfs { zones, scales }) => {
                let mut first_error = None;
                for (file, value) in scales {
                    keep_first(&mut first_error, write_sysfs(dir, file, &value.to_string()));
                }
                for (zone, setting) in zones {
                    keep_first(&mut first_error, self.set(zone, *setting));
                }
                first_error.map_or(Ok(()), Err)
            }
            (Backend::Hook(hook), SnapshotData::Hook(token)) => {
                hook.run_with_input(&["restore"], Some(token)).map(drop)
            }
            _ => Err(PlatformError::Invalid(
                "snapshot is from a different LED driver".into(),
            )),
        }
    }

    /// Read one zone's current setting (built-in driver only).
    pub fn read(&self, zone: &str) -> Result<LedSetting> {
        self.check_zone(zone)?;
        match &self.backend {
            Backend::Sysfs(dir) => read_zone(dir, zone),
            Backend::Hook(_) => Err(PlatformError::Unavailable("led readback")),
        }
    }
}

fn keep_first(slot: &mut Option<PlatformError>, result: Result<()>) {
    if let Err(e) = result {
        slot.get_or_insert(e);
    }
}

fn read_zone(dir: &Path, zone: &str) -> Result<LedSetting> {
    let code: u8 = parse_num(&read_sysfs(dir, &format!("effect_{zone}"))?, "effect")?;
    let colour = read_sysfs(dir, &format!("effect_rgb_hex_{zone}"))?.parse()?;
    Ok(LedSetting {
        colour,
        effect: LedEffect::from_code(code).ok_or_else(|| PlatformError::BadValue {
            source_name: format!("effect_{zone}"),
            value: code.to_string(),
        })?,
        duration_ms: parse_num(
            &read_sysfs(dir, &format!("effect_duration_{zone}"))?,
            "duration",
        )?,
        cycles: Cycles::from_driver_value(parse_num(
            &read_sysfs(dir, &format!("effect_cycles_{zone}"))?,
            "cycles",
        )?),
    })
}

fn parse_num<T: FromStr>(text: &str, what: &str) -> Result<T> {
    text.trim().parse().map_err(|_| PlatformError::BadValue {
        source_name: what.to_owned(),
        value: text.to_owned(),
    })
}

fn read_sysfs(dir: &Path, name: &str) -> Result<String> {
    let path = dir.join(name);
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_owned())
        .map_err(|e| PlatformError::io(path, e))
}

fn write_sysfs(dir: &Path, name: &str, value: &str) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, value).map_err(|e| PlatformError::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PlatformConfig;

    fn fake_sysfs() -> (tempfile::TempDir, Leds) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_text = format!(
            "id = \"t\"\n[leds]\ndriver = \"trimui-led-anim\"\nsysfs_path = {:?}\nzones = [\"f1\", \"m\"]\nmax_brightness = 60\nframe_leds = 3\n",
            dir.path().to_string_lossy()
        );
        let cfg = PlatformConfig::parse(&cfg_text, Path::new("t")).expect("parse");
        for zone in ["f1", "m"] {
            std::fs::write(dir.path().join(format!("effect_{zone}")), "4\n").expect("w");
            std::fs::write(
                dir.path().join(format!("effect_rgb_hex_{zone}")),
                "FFFFFF \n",
            )
            .expect("w");
            std::fs::write(dir.path().join(format!("effect_duration_{zone}")), "1000\n")
                .expect("w");
            std::fs::write(dir.path().join(format!("effect_cycles_{zone}")), "-1\n").expect("w");
        }
        for f in SCALE_FILES {
            std::fs::write(dir.path().join(f), "5\n").expect("w");
        }
        std::fs::write(dir.path().join("frame_hex"), "").expect("w");
        let leds = Leds::new(cfg.leds.as_ref().expect("leds"), None).expect("leds");
        (dir, leds)
    }

    fn file(dir: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(name))
            .expect("read")
            .trim()
            .to_owned()
    }

    #[test]
    fn colour_parsing() {
        assert_eq!("#ff8000".parse::<Rgb>().expect("hex"), Rgb(255, 128, 0));
        assert_eq!("0x0000FF".parse::<Rgb>().expect("hex"), Rgb(0, 0, 255));
        assert_eq!("purple".parse::<Rgb>().expect("name"), Rgb(160, 0, 255));
        assert!("12345".parse::<Rgb>().is_err());
        assert!("gggggg".parse::<Rgb>().is_err());
        assert!("ééé".parse::<Rgb>().is_err());
    }

    #[test]
    fn effect_codes_round_trip() {
        for e in LedEffect::ALL {
            assert_eq!(LedEffect::from_code(e.code()), Some(e));
            assert_eq!(e.name().parse::<LedEffect>().expect("name"), e);
        }
        assert_eq!(LedEffect::from_code(8), None);
    }

    #[test]
    fn set_writes_colour_before_trigger_and_reads_back() {
        let (dir, leds) = fake_sysfs();
        let setting = LedSetting {
            colour: Rgb(255, 0, 0),
            effect: LedEffect::Breath,
            duration_ms: 2500,
            cycles: Cycles::Count(3),
        };
        leds.set("f1", setting).expect("set");
        assert_eq!(file(&dir, "effect_rgb_hex_f1"), "FF0000");
        assert_eq!(file(&dir, "effect_f1"), "2");
        assert_eq!(file(&dir, "effect_cycles_f1"), "3");
        assert_eq!(leds.read("f1").expect("read"), setting);
    }

    #[test]
    fn unknown_zone_is_rejected() {
        let (_d, leds) = fake_sysfs();
        assert!(leds.set("zz", LedSetting::solid(Rgb::BLACK)).is_err());
        assert!(leds.off("../x").is_err());
    }

    #[test]
    fn snapshot_and_restore_round_trip() {
        let (dir, leds) = fake_sysfs();
        let snap = leds.snapshot().expect("snapshot");
        leds.set("f1", LedSetting::solid(Rgb(1, 2, 3)))
            .expect("set");
        leds.off("m").expect("off");
        leds.set_max_brightness(60).expect("brightness");
        leds.restore(&snap).expect("restore");
        assert_eq!(file(&dir, "effect_rgb_hex_f1"), "FFFFFF");
        assert_eq!(file(&dir, "effect_m"), "4");
        assert_eq!(file(&dir, "max_scale_f1f2"), "5");
        assert_eq!(leds.snapshot().expect("again"), snap);
    }

    #[test]
    fn brightness_limit_enforced() {
        let (_d, leds) = fake_sysfs();
        assert!(leds.set_max_brightness(61).is_err());
    }

    #[test]
    fn frame_requires_exact_length() {
        let (dir, leds) = fake_sysfs();
        assert!(leds.set_frame(&[Rgb::BLACK]).is_err());
        leds.set_frame(&[Rgb(1, 2, 3), Rgb::BLACK, Rgb(255, 255, 255)])
            .expect("frame");
        assert_eq!(file(&dir, "frame_hex"), "010203 000000 FFFFFF");
    }

    #[test]
    fn hook_driver_uses_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("leds.sh"),
            "echo \"$@\" >> \"$GOBLIN_TMP_DIR/log\"\n[ \"$1\" = snapshot ] && echo tok\n[ \"$1\" = restore ] && cat >> \"$GOBLIN_TMP_DIR/log\"\nexit 0\n",
        )
        .expect("write");
        let spec = crate::config::HookSpec::Path("leds.sh".into());
        let hook = Hook::new(
            &spec,
            dir.path(),
            vec![(
                "GOBLIN_TMP_DIR".into(),
                dir.path().to_string_lossy().into_owned(),
            )],
        );
        let cfg = PlatformConfig::parse(
            "id = \"t\"\n[leds]\ndriver = \"hook\"\nhook = \"leds.sh\"\nzones = [\"m\"]\nmax_brightness = 10\n",
            Path::new("t"),
        )
        .expect("parse");
        let leds = Leds::new(cfg.leds.as_ref().expect("leds"), Some(hook)).expect("leds");
        leds.set("m", LedSetting::solid(Rgb(255, 0, 0)))
            .expect("set");
        let snap = leds.snapshot().expect("snap");
        leds.restore(&snap).expect("restore");
        let log = std::fs::read_to_string(dir.path().join("log")).expect("log");
        assert!(log.contains("set m FF0000 static 1000 -1"), "{log}");
        assert!(log.contains("restore"), "{log}");
        assert!(log.contains("tok"), "{log}");
    }
}
