//! Presence LEDs: background status signalling on the zones that are not tied to a button.
//!
//! States follow the status-light convention of the upstream SDK (plan section 8.4). The first
//! change saves the firmware's own LED settings; [`PresenceLeds::release`] (or dropping the
//! helper) puts them back. With `never_touch` set the helper does nothing at all.

use std::borrow::Borrow;

use crate::error::Result;
use crate::led::{Cycles, LedEffect, LedSetting, LedSnapshot, Leds, Rgb};

/// What the gadget wants to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    /// Linked to Muse and idle.
    Connected,
    /// Link lost, retrying.
    Reconnecting,
    /// No pairing on this device.
    NotPaired,
    /// Something is wrong.
    Error,
    /// Muse is working on a request.
    Working,
    /// A reply or notification is waiting.
    Attention,
}

impl PresenceState {
    /// All states, for listing.
    pub const ALL: [PresenceState; 6] = [
        Self::Connected,
        Self::Reconnecting,
        Self::NotPaired,
        Self::Error,
        Self::Working,
        Self::Attention,
    ];

    /// Lowercase name for command lines.
    pub fn name(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::NotPaired => "not-paired",
            Self::Error => "error",
            Self::Working => "working",
            Self::Attention => "attention",
        }
    }

    /// Parse a lowercase name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// What "connected" looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectedStyle {
    /// Lights off.
    Off,
    /// Dim theme colour.
    Dim,
}

/// User choices for presence signalling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceOptions {
    /// Never write to the LEDs.
    pub never_touch: bool,
    /// Theme colour used for working, attention and (dimmed) connected.
    pub theme: Rgb,
    /// Look of the connected state.
    pub connected: ConnectedStyle,
}

impl Default for PresenceOptions {
    fn default() -> Self {
        Self {
            never_touch: false,
            theme: Rgb(0, 120, 255),
            connected: ConnectedStyle::Off,
        }
    }
}

/// The setting a state maps to, or `None` for "lights off".
pub fn setting_for(state: PresenceState, options: &PresenceOptions) -> Option<LedSetting> {
    let forever = Cycles::Forever;
    let setting = |colour, effect, duration_ms| LedSetting {
        colour,
        effect,
        duration_ms,
        cycles: forever,
    };
    match state {
        PresenceState::Connected => match options.connected {
            ConnectedStyle::Off => None,
            ConnectedStyle::Dim => Some(setting(options.theme.scaled(40), LedEffect::Static, 1000)),
        },
        PresenceState::Reconnecting => Some(setting(Rgb(255, 180, 0), LedEffect::Blink1, 1000)),
        PresenceState::NotPaired => Some(setting(Rgb(160, 0, 255), LedEffect::Static, 1000)),
        PresenceState::Error => Some(setting(Rgb(255, 0, 0), LedEffect::Blink2, 800)),
        PresenceState::Working => Some(setting(options.theme, LedEffect::Breath, 3000)),
        PresenceState::Attention => Some(setting(options.theme, LedEffect::Blink1, 1200)),
    }
}

/// Drives the presence zones. `L` is `&Leds` for a short-lived helper or `Leds` for one that
/// lives as long as a daemon.
pub struct PresenceLeds<L: Borrow<Leds>> {
    leds: L,
    zones: Vec<String>,
    accent: Vec<String>,
    options: PresenceOptions,
    saved: Option<LedSnapshot>,
    current: Option<PresenceState>,
}

impl<L: Borrow<Leds>> PresenceLeds<L> {
    /// Create a helper for `zones` (normally the platform's presence zones).
    pub fn new(leds: L, zones: Vec<String>, options: PresenceOptions) -> Self {
        Self {
            leds,
            zones,
            accent: Vec::new(),
            options,
            saved: None,
            current: None,
        }
    }

    /// Also show working and attention on `zones`; in other states they get back the setting
    /// the firmware had (see [`crate::config::LedConfig::accent_zones`]).
    pub fn with_accent(mut self, zones: Vec<String>) -> Self {
        self.accent = zones;
        self
    }

    /// The LED controller.
    pub fn leds(&self) -> &Leds {
        self.leds.borrow()
    }

    /// Whether the firmware's settings have been saved (something was shown).
    pub fn active(&self) -> bool {
        self.saved.is_some()
    }

    /// The state last shown, if any.
    pub fn current(&self) -> Option<PresenceState> {
        self.current
    }

    /// Show `state` on every presence zone.
    pub fn set(&mut self, state: PresenceState) -> Result<()> {
        if self.options.never_touch || self.current == Some(state) {
            return Ok(());
        }
        if self.saved.is_none() {
            self.saved = Some(self.leds.borrow().snapshot()?);
        }
        let setting = setting_for(state, &self.options);
        for zone in &self.zones {
            match setting {
                Some(s) => self.leds.borrow().set(zone, s)?,
                None => self.leds.borrow().off(zone)?,
            }
        }
        let accent = matches!(state, PresenceState::Working | PresenceState::Attention);
        for zone in &self.accent {
            let s = if accent {
                setting
            } else {
                self.saved.as_ref().and_then(|snap| snap.zone(zone))
            };
            if let Some(s) = s {
                self.leds.borrow().set(zone, s)?;
            }
        }
        self.current = Some(state);
        Ok(())
    }

    /// Clear an attention state back to connected.
    pub fn clear_attention(&mut self) -> Result<()> {
        if self.current == Some(PresenceState::Attention) {
            self.set(PresenceState::Connected)?;
        }
        Ok(())
    }

    /// Restore the firmware's own LED settings and stop signalling.
    pub fn release(&mut self) -> Result<()> {
        self.current = None;
        match self.saved.take() {
            Some(snapshot) => self.leds.borrow().restore(&snapshot),
            None => Ok(()),
        }
    }
}

impl<L: Borrow<Leds>> Drop for PresenceLeds<L> {
    fn drop(&mut self) {
        // Nothing sensible can be done about a failure while unwinding or exiting.
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PlatformConfig;
    use std::path::Path;

    fn leds_in(dir: &Path) -> Leds {
        let text = format!(
            "id = \"t\"\n[leds]\ndriver = \"trimui-led-anim\"\nsysfs_path = {:?}\nzones = [\"f1\", \"m\", \"l\"]\nmax_brightness = 60\npresence_zones = [\"m\", \"l\"]\n",
            dir.to_string_lossy()
        );
        for zone in ["f1", "m", "l"] {
            for (k, v) in [
                ("effect", "4"),
                ("effect_rgb_hex", "FFFFFF "),
                ("effect_duration", "1000"),
                ("effect_cycles", "-1"),
            ] {
                let name = if k == "effect" {
                    format!("effect_{zone}")
                } else {
                    format!("{k}_{zone}")
                };
                std::fs::write(dir.join(name), v).expect("w");
            }
        }
        for f in ["max_scale", "max_scale_f1f2", "max_scale_lr"] {
            std::fs::write(dir.join(f), "5").expect("w");
        }
        let cfg = PlatformConfig::parse(&text, Path::new("t")).expect("parse");
        Leds::new(cfg.leds.as_ref().expect("leds"), None).expect("leds")
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name))
            .expect("read")
            .trim()
            .to_owned()
    }

    #[test]
    fn every_state_has_the_documented_look() {
        let o = PresenceOptions::default();
        assert_eq!(setting_for(PresenceState::Connected, &o), None);
        assert_eq!(
            setting_for(PresenceState::NotPaired, &o).expect("s").colour,
            Rgb(160, 0, 255)
        );
        assert_eq!(
            setting_for(PresenceState::Reconnecting, &o)
                .expect("s")
                .effect,
            LedEffect::Blink1
        );
        assert_eq!(
            setting_for(PresenceState::Error, &o).expect("s").colour,
            Rgb(255, 0, 0)
        );
        assert_eq!(
            setting_for(PresenceState::Working, &o).expect("s").effect,
            LedEffect::Breath
        );
        let dim = PresenceOptions {
            connected: ConnectedStyle::Dim,
            ..o
        };
        assert_eq!(
            setting_for(PresenceState::Connected, &dim)
                .expect("s")
                .effect,
            LedEffect::Static
        );
    }

    #[test]
    fn touches_only_presence_zones_and_restores() {
        let dir = tempfile::tempdir().expect("tempdir");
        let leds = leds_in(dir.path());
        {
            let mut p = PresenceLeds::new(
                &leds,
                vec!["m".into(), "l".into()],
                PresenceOptions::default(),
            );
            p.set(PresenceState::Error).expect("set");
            assert_eq!(read(dir.path(), "effect_rgb_hex_m"), "FF0000");
            assert_eq!(read(dir.path(), "effect_rgb_hex_f1"), "FFFFFF");
            p.set(PresenceState::Connected).expect("set");
            assert_eq!(read(dir.path(), "effect_l"), "0");
            p.set(PresenceState::Attention).expect("set");
            p.clear_attention().expect("clear");
            assert_eq!(p.current(), Some(PresenceState::Connected));
            p.release().expect("release");
        }
        assert_eq!(read(dir.path(), "effect_m"), "4");
        assert_eq!(read(dir.path(), "effect_rgb_hex_l"), "FFFFFF");
    }

    #[test]
    fn accent_zones_show_attention_then_get_firmware_setting_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let leds = leds_in(dir.path());
        let mut p = PresenceLeds::new(&leds, vec!["m".into()], PresenceOptions::default())
            .with_accent(vec!["f1".into()]);
        p.set(PresenceState::Connected).expect("set");
        assert_eq!(
            read(dir.path(), "effect_f1"),
            "4",
            "connected leaves the accent alone"
        );
        p.set(PresenceState::Attention).expect("set");
        assert_eq!(read(dir.path(), "effect_rgb_hex_f1"), "0078FF");
        assert_eq!(read(dir.path(), "effect_f1"), "5");
        p.set(PresenceState::Connected).expect("set");
        assert_eq!(read(dir.path(), "effect_rgb_hex_f1"), "FFFFFF");
        assert_eq!(read(dir.path(), "effect_f1"), "4");
        assert_eq!(read(dir.path(), "effect_m"), "0");
    }

    #[test]
    fn drop_restores() {
        let dir = tempfile::tempdir().expect("tempdir");
        let leds = leds_in(dir.path());
        {
            let mut p = PresenceLeds::new(&leds, vec!["m".into()], PresenceOptions::default());
            p.set(PresenceState::NotPaired).expect("set");
        }
        assert_eq!(read(dir.path(), "effect_rgb_hex_m"), "FFFFFF");
    }

    #[test]
    fn never_touch_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let leds = leds_in(dir.path());
        let options = PresenceOptions {
            never_touch: true,
            ..PresenceOptions::default()
        };
        let mut p = PresenceLeds::new(&leds, vec!["m".into()], options);
        p.set(PresenceState::Error).expect("set");
        assert_eq!(read(dir.path(), "effect_rgb_hex_m"), "FFFFFF");
        assert_eq!(p.current(), None);
    }

    #[test]
    fn state_names_round_trip() {
        for s in PresenceState::ALL {
            assert_eq!(PresenceState::from_name(s.name()), Some(s));
        }
    }
}
