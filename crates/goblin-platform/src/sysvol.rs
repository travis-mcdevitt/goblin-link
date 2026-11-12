//! The system volume as the firmware's launcher keeps it, so Muse follows the user's volume
//! keys instead of keeping a level of its own.
//!
//! NextUI (tg5040 and relatives) keeps its settings in the POSIX shared memory object
//! `/SharedSettings` (`/dev/shm/SharedSettings`), a struct of 32-bit ints (`SettingsV11` in
//! NextUI's `libmsettings`). Its `keymon` daemon applies the volume keys to the hardware and to
//! this struct whatever app is in front, so reading it is enough. Read-only: Muse never writes it.

use std::path::Path;

/// Where NextUI's shared settings live.
pub const NEXTUI_SHARED_SETTINGS: &str = "/dev/shm/SharedSettings";

/// `SettingsV11` field indices (32-bit little-endian ints).
const VERSION: usize = 0;
const HEADPHONES: usize = 3;
const SPEAKER: usize = 4;
const MUTE: usize = 5;
const TOGGLED_VOLUME: usize = 14;
const JACK: usize = 27;
const AUDIO_SINK: usize = 28;
/// `SETTINGS_DEFAULT_MUTE_NO_CHANGE`: the mute switch leaves the volume alone.
const MUTE_NO_CHANGE: i32 = -69;
/// NextUI's volume steps (0-20); the hardware gets `level * 5` percent.
const STEPS: i32 = 20;

/// The effective system volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemVolume {
    /// Effective level in percent (0-100, steps of 5).
    pub percent: u8,
    /// The mute switch is on.
    pub muted: bool,
    /// Output goes to headphones (or another sink) rather than the speaker.
    pub headphones: bool,
}

impl SystemVolume {
    /// Whether anything can be heard.
    pub fn audible(&self) -> bool {
        self.percent > 0
    }
}

/// Reads NextUI's volume, or `None` when this is not NextUI (or a layout it does not know).
pub fn read_nextui() -> Option<SystemVolume> {
    read_nextui_at(Path::new(NEXTUI_SHARED_SETTINGS))
}

/// [`read_nextui`] from another path (tests).
pub fn read_nextui_at(path: &Path) -> Option<SystemVolume> {
    let bytes = std::fs::read(path).ok()?;
    parse_nextui(&bytes)
}

/// Decodes `SettingsV11`, applying NextUI's `GetVolume` rules.
pub fn parse_nextui(bytes: &[u8]) -> Option<SystemVolume> {
    let int = |i: usize| -> Option<i32> {
        let b = bytes.get(i * 4..i * 4 + 4)?;
        Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if int(VERSION)? != 11 {
        return None;
    }
    let muted = int(MUTE)? != 0;
    let headphones = int(JACK)? != 0 || int(AUDIO_SINK)? != 0;
    let toggled = int(TOGGLED_VOLUME)?;
    let level = if muted && toggled != MUTE_NO_CHANGE {
        toggled
    } else if headphones {
        int(HEADPHONES)?
    } else {
        int(SPEAKER)?
    };
    Some(SystemVolume {
        percent: (level.clamp(0, STEPS) * 5) as u8,
        muted,
        headphones,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(fields: &[(usize, i32)]) -> Vec<u8> {
        let mut v = [0i32; 33];
        v[VERSION] = 11;
        v[TOGGLED_VOLUME] = MUTE_NO_CHANGE;
        for &(i, x) in fields {
            v[i] = x;
        }
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn speaker_level() {
        let s = parse_nextui(&settings(&[(SPEAKER, 8), (HEADPHONES, 4)])).unwrap();
        assert_eq!(s.percent, 40);
        assert!(!s.muted && !s.headphones && s.audible());
    }

    #[test]
    fn mute_switch_with_a_muted_level() {
        // As read from the Brick on 2026-11-12: speaker 1, mute on, muted level 0.
        let s = parse_nextui(&settings(&[(SPEAKER, 1), (MUTE, 1), (TOGGLED_VOLUME, 0)])).unwrap();
        assert_eq!(s.percent, 0);
        assert!(s.muted && !s.audible());
    }

    #[test]
    fn mute_switch_that_leaves_volume_alone() {
        let s = parse_nextui(&settings(&[(SPEAKER, 6), (MUTE, 1)])).unwrap();
        assert_eq!(s.percent, 30);
    }

    #[test]
    fn headphones_use_their_own_level() {
        let s = parse_nextui(&settings(&[(SPEAKER, 6), (HEADPHONES, 3), (JACK, 1)])).unwrap();
        assert_eq!(s.percent, 15);
        assert!(s.headphones);
    }

    #[test]
    fn other_layouts_are_ignored() {
        let mut b = settings(&[]);
        b[0] = 10;
        assert_eq!(parse_nextui(&b), None);
        assert_eq!(parse_nextui(&[1, 2]), None);
    }
}
