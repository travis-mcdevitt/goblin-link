//! "Pip", the gadget's original mascot avatar.
//!
//! Pip is a friendly rounded pebble with a single soft spark-tuft on top — deliberately unlike
//! Meta's Muse avatar (a cloaked figure), and drawn entirely from the canvas primitives so it
//! scales cleanly and tints to the active palette. Four states animate from the clock and an
//! optional audio level:
//!
//! - **Idle** — a slow vertical bob and periodic blink.
//! - **Listening** — the spark brightens and a ring pulses with the mic level.
//! - **Thinking** — the eyes narrow and the spark orbits in small steps.
//! - **Speaking** — the mouth opens and closes with the output level.

use crate::anim::{ease_in_out_sine, loop_phase};
use crate::canvas::Canvas;
use crate::color::Color;
use crate::geom::Rect;
use crate::theme::Theme;

/// What the agent is doing, which drives the mascot's animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MascotState {
    /// Connected and waiting.
    #[default]
    Idle,
    /// Microphone open (listening / dictation / your turn in a call).
    Listening,
    /// Muse is working on a reply.
    Thinking,
    /// Muse is speaking (reply TTS or its turn in a call).
    Speaking,
}

/// The mascot's animation state. Cheap to clone; one lives in [`crate::Ui`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Mascot {
    state: MascotState,
    level: f32,
}

impl Mascot {
    /// A new idle mascot.
    pub fn new() -> Self {
        Mascot::default()
    }

    /// Current state.
    pub fn state(&self) -> MascotState {
        self.state
    }

    /// Sets the animation state.
    pub fn set_state(&mut self, s: MascotState) {
        self.state = s;
    }

    /// Feeds an audio level (0..1) used by listening/speaking; decays toward it smoothly.
    pub fn set_level(&mut self, level: f32) {
        let l = level.clamp(0.0, 1.0);
        self.level += (l - self.level) * 0.4;
    }

    /// True while the state visibly animates (so the UI keeps requesting frames).
    pub fn is_animating(&self) -> bool {
        true // the idle bob and blink are always moving; callers throttle to ~30 fps when idle.
    }

    /// Draws the mascot within `bounds` (a square region) at time `now_ms`.
    pub fn draw(&self, canvas: &mut Canvas, theme: &Theme, bounds: Rect, now_ms: u64) {
        let size = bounds.w.min(bounds.h);
        if size < 6.0 {
            return;
        }
        let reduce = theme.reduce_motion;
        let t = now_ms as f32 / 1000.0;

        // Vertical bob.
        let bob = if reduce {
            0.0
        } else {
            match self.state {
                MascotState::Thinking => (t * 2.2).sin() * size * 0.012,
                _ => (t * 1.4).sin() * size * 0.03,
            }
        };
        let cx = bounds.cx();
        let cy = bounds.cy() + bob + size * 0.04;

        let accent = theme.pal.accent;
        let body = blend(theme.pal.surface, accent, 0.16);
        let body_edge = blend(body, accent, 0.25);

        // Listening ring pulse.
        if self.state == MascotState::Listening {
            let base = size * 0.46;
            let amp = if reduce {
                0.0
            } else {
                self.level * size * 0.12
            };
            let ph = ease_in_out_sine(loop_phase(now_ms, 1600));
            let r = base + amp + ph * size * 0.04;
            canvas.stroke_circle(cx, cy, r, (size * 0.02).max(1.5), accent.fade(0.35));
            canvas.stroke_circle(
                cx,
                cy,
                r + size * 0.06,
                (size * 0.015).max(1.0),
                accent.fade(0.16),
            );
        }

        // Body: a soft rounded pebble, slightly taller than wide.
        let bw = size * 0.62;
        let bh = size * 0.66;
        let body_rect = Rect::new(cx - bw * 0.5, cy - bh * 0.5, bw, bh);
        let radius = bw * 0.46;
        // subtle drop to ground it
        canvas.fill_rrect_feather(
            Rect::new(cx - bw * 0.42, cy + bh * 0.38, bw * 0.84, size * 0.07),
            size * 0.04,
            size * 0.05,
            Color::BLACK.fade(0.10),
        );
        canvas.fill_rrect(body_rect, radius, body);
        canvas.stroke_rrect(body_rect, radius, (size * 0.012).max(1.0), body_edge);

        // Cheek highlight.
        canvas.fill_ellipse(
            cx - bw * 0.16,
            cy - bh * 0.1,
            bw * 0.16,
            bh * 0.12,
            theme.pal.surface.fade(0.5),
        );

        // Spark tuft on top.
        self.draw_spark(canvas, theme, cx, cy - bh * 0.5, size, now_ms);

        // Face.
        self.draw_face(canvas, theme, cx, cy, size, now_ms);
    }

    fn draw_spark(
        &self,
        canvas: &mut Canvas,
        theme: &Theme,
        cx: f32,
        top: f32,
        size: f32,
        now_ms: u64,
    ) {
        let accent = theme.pal.accent;
        let bright = self.state == MascotState::Listening || self.state == MascotState::Speaking;
        let orbit = if self.state == MascotState::Thinking && !theme.reduce_motion {
            let p = loop_phase(now_ms, 1400);
            (p * std::f32::consts::TAU).sin() * size * 0.05
        } else {
            0.0
        };
        let sx = cx + orbit;
        let sy = top - size * 0.07;
        // stalk
        canvas.stroke_line(
            cx,
            top + size * 0.01,
            sx,
            sy + size * 0.03,
            (size * 0.016).max(1.0),
            blend(theme.pal.surface, accent, 0.3),
        );
        let glow = if bright { accent } else { accent.fade(0.7) };
        let r = size * (if bright { 0.075 } else { 0.06 });
        canvas.fill_circle(sx, sy, r + size * 0.03, accent.fade(0.18));
        canvas.fill_circle(sx, sy, r, glow);
        canvas.fill_circle(
            sx - r * 0.3,
            sy - r * 0.3,
            r * 0.4,
            theme.pal.on_accent.fade(0.8),
        );
    }

    fn draw_face(
        &self,
        canvas: &mut Canvas,
        theme: &Theme,
        cx: f32,
        cy: f32,
        size: f32,
        now_ms: u64,
    ) {
        let ink = blend(theme.pal.text, theme.pal.accent, 0.1);
        let eye_dx = size * 0.12;
        let eye_y = cy - size * 0.02;
        let eye_r = size * 0.045;

        // Blink: brief every few seconds (idle/listening/speaking); thinking keeps eyes narrowed.
        let blink = if theme.reduce_motion {
            0.0
        } else {
            let p = loop_phase(now_ms, 3400);
            if p > 0.96 {
                ((p - 0.96) / 0.04 * std::f32::consts::PI).sin()
            } else {
                0.0
            }
        };
        let narrow = if self.state == MascotState::Thinking {
            0.55
        } else {
            0.0
        };
        let open = (1.0 - blink.max(narrow)).clamp(0.08, 1.0);

        for side in [-1.0f32, 1.0] {
            let ex = cx + side * eye_dx;
            if open < 0.3 {
                // closed/narrow: a short line
                canvas.fill_rrect(
                    Rect::new(
                        ex - eye_r,
                        eye_y - eye_r * 0.35,
                        eye_r * 2.0,
                        (eye_r * 0.7 * open).max(1.2),
                    ),
                    eye_r * 0.35,
                    ink,
                );
            } else {
                canvas.fill_ellipse(ex, eye_y, eye_r, eye_r * open * 1.1, ink);
                // glint
                canvas.fill_circle(
                    ex + eye_r * 0.3,
                    eye_y - eye_r * 0.3,
                    eye_r * 0.3,
                    theme.pal.surface,
                );
            }
        }

        // Mouth.
        let my = cy + size * 0.11;
        let mw = size * 0.11;
        match self.state {
            MascotState::Speaking => {
                let openv = if theme.reduce_motion {
                    0.4
                } else {
                    0.25 + self.level * 0.9 + (now_ms as f32 / 90.0).sin().abs() * 0.1
                };
                let h = (size * 0.05 * openv).max(size * 0.02);
                canvas.fill_rrect(
                    Rect::new(cx - mw * 0.6, my - h * 0.5, mw * 1.2, h),
                    h * 0.5,
                    ink,
                );
            }
            MascotState::Listening => {
                // small 'o'
                canvas.fill_circle(cx, my, size * 0.028, ink);
            }
            _ => {
                // gentle smile arc
                let r = size * 0.1;
                canvas.stroke_arc(
                    cx,
                    my - r * 0.6,
                    r,
                    0.18 * std::f32::consts::PI,
                    0.82 * std::f32::consts::PI,
                    (size * 0.016).max(1.2),
                    ink,
                );
            }
        }
    }
}

fn blend(a: Color, b: Color, t: f32) -> Color {
    a.mix(b, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_without_panicking_in_all_states() {
        let theme = Theme::new(Mode::Light, 2.0);
        for st in [
            MascotState::Idle,
            MascotState::Listening,
            MascotState::Thinking,
            MascotState::Speaking,
        ] {
            let mut m = Mascot::new();
            m.set_state(st);
            m.set_level(0.6);
            let mut c = Canvas::new(120, 120);
            c.clear(theme.pal.page);
            m.draw(&mut c, &theme, Rect::new(10.0, 10.0, 100.0, 100.0), 1234);
            // Something was drawn near the centre.
            assert_ne!(c.get(60, 60), theme.pal.page);
        }
    }

    use crate::theme::Mode;
}
