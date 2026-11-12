//! The input abstraction: a device-independent [`Button`] enum, press/hold/repeat phases, and an
//! [`InputMap`] describing how one platform's physical controls map to buttons. The SDL shell
//! fills these in from controller events; the kit never touches SDL.

use serde::Deserialize;

/// A logical button. The Brick exposes all of these; other devices map a subset and leave the
/// rest unbound (see [`InputMap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    /// D-pad up.
    Up,
    /// D-pad down.
    Down,
    /// D-pad left.
    Left,
    /// D-pad right.
    Right,
    /// Primary action (Brick: A, the right face button).
    A,
    /// Back (Brick: B, the bottom face button).
    B,
    /// Composer / keyboard (Brick: X).
    X,
    /// Context menu (Brick: Y).
    Y,
    /// Previous tab (Brick: L1).
    L1,
    /// Next tab (Brick: R1).
    R1,
    /// Page up (Brick: L2).
    L2,
    /// Page down (Brick: R2).
    R2,
    /// Send (Brick: Start).
    Start,
    /// Notifications / Status (Brick: Select).
    Select,
    /// App menu (Brick: Menu).
    Menu,
    /// Dictation / push-to-talk (Brick: F1, the left pill button / L3).
    F1,
    /// Call / mute (Brick: F2, the right pill button / R3).
    F2,
}

impl Button {
    /// Every button, for the input-test screen.
    pub const ALL: &'static [Button] = &[
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
        Button::A,
        Button::B,
        Button::X,
        Button::Y,
        Button::L1,
        Button::R1,
        Button::L2,
        Button::R2,
        Button::Start,
        Button::Select,
        Button::Menu,
        Button::F1,
        Button::F2,
    ];

    /// True for the four directional buttons.
    pub fn is_dpad(self) -> bool {
        matches!(
            self,
            Button::Up | Button::Down | Button::Left | Button::Right
        )
    }

    /// Short label for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Button::Up => "Up",
            Button::Down => "Down",
            Button::Left => "Left",
            Button::Right => "Right",
            Button::A => "A",
            Button::B => "B",
            Button::X => "X",
            Button::Y => "Y",
            Button::L1 => "L1",
            Button::R1 => "R1",
            Button::L2 => "L2",
            Button::R2 => "R2",
            Button::Start => "Start",
            Button::Select => "Select",
            Button::Menu => "Menu",
            Button::F1 => "F1",
            Button::F2 => "F2",
        }
    }
}

/// The phase of a button event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The button went down.
    Press,
    /// The button came up; `held` is the total time it was down.
    Release {
        /// Whether the press lasted past the hold threshold.
        was_hold: bool,
    },
    /// The hold threshold elapsed while still down (fires once).
    Hold,
    /// An auto-repeat tick while held (navigation only).
    Repeat,
}

/// A button event delivered to a screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputEvent {
    /// Which button.
    pub button: Button,
    /// What happened.
    pub phase: Phase,
}

impl InputEvent {
    /// True for a `Press` or `Repeat` of `button` (the common "navigate/activate" test).
    pub fn pressed(&self, button: Button) -> bool {
        self.button == button && matches!(self.phase, Phase::Press | Phase::Repeat)
    }

    /// True for exactly a `Press` of `button`.
    pub fn just_pressed(&self, button: Button) -> bool {
        self.button == button && self.phase == Phase::Press
    }

    /// True for a `Release` of `button`.
    pub fn released(&self, button: Button) -> bool {
        self.button == button && matches!(self.phase, Phase::Release { .. })
    }

    /// True for a `Hold` of `button`.
    pub fn held(&self, button: Button) -> bool {
        self.button == button && self.phase == Phase::Hold
    }
}

/// Repeat/hold timing in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Delay before the first auto-repeat.
    pub repeat_delay: u32,
    /// Interval between repeats.
    pub repeat_interval: u32,
    /// Delay before a press counts as a hold.
    pub hold: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            repeat_delay: 360,
            repeat_interval: 90,
            hold: 450,
        }
    }
}

/// Tracks held buttons and emits `Hold`/`Repeat` events over time. The SDL shell feeds raw
/// down/up and `tick`; screens receive the resulting [`InputEvent`]s.
pub struct InputState {
    timing: Timing,
    held: Vec<HeldButton>,
}

/// How long the D-pad is held before its repeat doubles in speed.
const ACCELERATE_AFTER_MS: u64 = 1500;

struct HeldButton {
    button: Button,
    down_at: u64,
    next_repeat: u64,
    hold_fired: bool,
}

impl InputState {
    /// New tracker with the given timing.
    pub fn new(timing: Timing) -> Self {
        InputState {
            timing,
            held: Vec::new(),
        }
    }

    /// Records a button down at time `now_ms`; returns the `Press` event.
    pub fn press(&mut self, button: Button, now_ms: u64) -> InputEvent {
        self.held.retain(|h| h.button != button);
        self.held.push(HeldButton {
            button,
            down_at: now_ms,
            next_repeat: now_ms + self.timing.repeat_delay as u64,
            hold_fired: false,
        });
        InputEvent {
            button,
            phase: Phase::Press,
        }
    }

    /// Records a button up at `now_ms`; returns the `Release` event if it was tracked.
    pub fn release(&mut self, button: Button, now_ms: u64) -> Option<InputEvent> {
        let idx = self.held.iter().position(|h| h.button == button)?;
        let h = self.held.remove(idx);
        let was_hold = now_ms.saturating_sub(h.down_at) >= self.timing.hold as u64;
        Some(InputEvent {
            button,
            phase: Phase::Release { was_hold },
        })
    }

    /// Emits `Hold` and `Repeat` events that have come due by `now_ms`.
    pub fn tick(&mut self, now_ms: u64, out: &mut Vec<InputEvent>) {
        for h in &mut self.held {
            if !h.hold_fired && now_ms.saturating_sub(h.down_at) >= self.timing.hold as u64 {
                h.hold_fired = true;
                out.push(InputEvent {
                    button: h.button,
                    phase: Phase::Hold,
                });
            }
            // The D-pad and the page triggers repeat while held.
            if h.button.is_dpad() || matches!(h.button, Button::L2 | Button::R2) {
                // At most one repeat per call: after a slow frame the steps that came due
                // meanwhile are dropped, not delivered in a burst (which moved several rows
                // at once and overshot).
                if now_ms >= h.next_repeat {
                    out.push(InputEvent {
                        button: h.button,
                        phase: Phase::Repeat,
                    });
                    // A long hold speeds up: after about a second and a half the D-pad
                    // repeats twice as fast, so a long list does not take forever. (Paging
                    // with L2/R2 keeps its pace.)
                    let held_ms = now_ms.saturating_sub(h.down_at);
                    let fast = h.button.is_dpad() && held_ms >= ACCELERATE_AFTER_MS;
                    let interval = self.timing.repeat_interval as u64;
                    let interval = if fast {
                        (interval / 2).max(1)
                    } else {
                        interval
                    };
                    // Keep the rhythm when on time; start afresh when a frame ran long.
                    h.next_repeat = (h.next_repeat + interval).max(now_ms + interval / 2);
                }
            }
        }
    }

    /// True while `button` is held.
    pub fn is_down(&self, button: Button) -> bool {
        self.held.iter().any(|h| h.button == button)
    }

    /// Clears all held state (e.g. on focus loss / window minimise).
    pub fn clear(&mut self) {
        self.held.clear();
    }
}

/// How a platform's SDL controller indices map to [`Button`]s. The kit only needs this struct's
/// shape; the real values come from `platforms/<id>/platform.toml`, which another crate owns.
/// Documented keys: `a,b,x,y,l1,r1,select,start,menu,f1,f2` (controller button indices),
/// `l2_axis,r2_axis` (trigger axes), `layout` (`"nintendo"` or `"xbox"` for hint glyphs).
#[derive(Clone, Debug, Deserialize)]
pub struct InputMap {
    /// SDL button index → logical button (as parsed from platform.toml).
    pub a: u8,
    /// B index.
    pub b: u8,
    /// X index.
    pub x: u8,
    /// Y index.
    pub y: u8,
    /// L1 index.
    pub l1: u8,
    /// R1 index.
    pub r1: u8,
    /// Select index.
    pub select: u8,
    /// Start index.
    pub start: u8,
    /// Menu/guide index.
    pub menu: u8,
    /// F1 (L3) index.
    pub f1: u8,
    /// F2 (R3) index.
    pub f2: u8,
    /// L2 trigger axis.
    pub l2_axis: u8,
    /// R2 trigger axis.
    pub r2_axis: u8,
    /// Button-glyph layout name.
    #[serde(default = "default_layout")]
    pub layout: String,
}

fn default_layout() -> String {
    "nintendo".into()
}

impl Default for InputMap {
    /// The TrimUI Brick mapping, as observed on the device.
    fn default() -> Self {
        InputMap {
            b: 0,
            a: 1,
            y: 2,
            x: 3,
            l1: 4,
            r1: 5,
            select: 6,
            start: 7,
            menu: 8,
            f1: 9,
            f2: 10,
            l2_axis: 2,
            r2_axis: 5,
            layout: "nintendo".into(),
        }
    }
}

impl InputMap {
    /// Maps an SDL controller/joystick button index to a logical button.
    pub fn button(&self, index: u8) -> Option<Button> {
        Some(match index {
            i if i == self.a => Button::A,
            i if i == self.b => Button::B,
            i if i == self.x => Button::X,
            i if i == self.y => Button::Y,
            i if i == self.l1 => Button::L1,
            i if i == self.r1 => Button::R1,
            i if i == self.select => Button::Select,
            i if i == self.start => Button::Start,
            i if i == self.menu => Button::Menu,
            i if i == self.f1 => Button::F1,
            i if i == self.f2 => Button::F2,
            _ => return None,
        })
    }

    /// Maps a trigger axis index to L2/R2.
    pub fn axis(&self, index: u8) -> Option<Button> {
        if index == self.l2_axis {
            Some(Button::L2)
        } else if index == self.r2_axis {
            Some(Button::R2)
        } else {
            None
        }
    }

    /// True when hint glyphs should use the Nintendo A/B/X/Y positions.
    pub fn nintendo_layout(&self) -> bool {
        self.layout.eq_ignore_ascii_case("nintendo")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brick_map_matches_platform_table() {
        let m = InputMap::default();
        assert_eq!(m.button(0), Some(Button::B));
        assert_eq!(m.button(1), Some(Button::A));
        assert_eq!(m.button(2), Some(Button::Y));
        assert_eq!(m.button(3), Some(Button::X));
        assert_eq!(m.button(9), Some(Button::F1));
        assert_eq!(m.button(10), Some(Button::F2));
        assert_eq!(m.button(99), None);
        assert_eq!(m.axis(2), Some(Button::L2));
        assert!(m.nintendo_layout());
    }

    #[test]
    fn hold_then_release_reports_hold() {
        let mut s = InputState::new(Timing::default());
        s.press(Button::F1, 0);
        let mut out = Vec::new();
        s.tick(500, &mut out);
        assert!(out.iter().any(|e| e.held(Button::F1)));
        let rel = s.release(Button::F1, 600).unwrap();
        assert_eq!(rel.phase, Phase::Release { was_hold: true });
    }

    #[test]
    fn dpad_repeats_but_face_buttons_dont() {
        let mut s = InputState::new(Timing::default());
        s.press(Button::Down, 0);
        s.press(Button::A, 0);
        let mut out = Vec::new();
        s.tick(1000, &mut out);
        assert!(out
            .iter()
            .any(|e| e.button == Button::Down && e.phase == Phase::Repeat));
        assert!(!out
            .iter()
            .any(|e| e.button == Button::A && e.phase == Phase::Repeat));
    }

    #[test]
    fn dpad_repeat_speeds_up_on_a_long_hold() {
        let count = |from: u64, to: u64, button: Button| {
            let mut s = InputState::new(Timing::default());
            s.press(button, 0);
            let mut out = Vec::new();
            s.tick(from, &mut out);
            out.clear();
            s.tick(to, &mut out);
            out.iter().filter(|e| e.phase == Phase::Repeat).count()
        };
        let early = count(400, 1300, Button::Down);
        let late = count(2000, 2900, Button::Down);
        assert!(
            late >= early * 2 - 2,
            "about twice as many: {early} then {late}"
        );
        assert_eq!(count(2000, 2900, Button::R2), count(400, 1300, Button::R2));
    }

    #[test]
    fn quick_tap_is_not_a_hold() {
        let mut s = InputState::new(Timing::default());
        s.press(Button::F1, 0);
        let rel = s.release(Button::F1, 100).unwrap();
        assert_eq!(rel.phase, Phase::Release { was_hold: false });
    }
}
