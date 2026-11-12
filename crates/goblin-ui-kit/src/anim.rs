//! Tiny animation helpers: easing, a spring-free timed tween, and a continuous phase clock for
//! idle/listening/thinking pulses. All are time-driven (milliseconds) so the UI can redraw only
//! when something is actually animating.

/// Ease-out cubic (fast start, gentle stop) — used for focus and page transitions.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Ease-in-out sine — used for looping pulses.
pub fn ease_in_out_sine(t: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos()
}

/// A value that animates toward a target over a fixed duration with easing. Reports whether it
/// is still moving so the caller knows to request another frame.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start_ms: u64,
    dur_ms: u32,
    value: f32,
}

impl Tween {
    /// A settled tween at `v`.
    pub fn new(v: f32) -> Self {
        Tween {
            from: v,
            to: v,
            start_ms: 0,
            dur_ms: 1,
            value: v,
        }
    }

    /// Current value.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Target value.
    pub fn target(&self) -> f32 {
        self.to
    }

    /// Starts animating toward `to` over `dur_ms` from the current value.
    pub fn set(&mut self, to: f32, dur_ms: u32, now_ms: u64) {
        if (to - self.to).abs() < f32::EPSILON && self.start_ms != 0 {
            return;
        }
        self.from = self.value;
        self.to = to;
        self.start_ms = now_ms.max(1);
        self.dur_ms = dur_ms.max(1);
    }

    /// Jumps immediately to `v`.
    pub fn snap(&mut self, v: f32) {
        self.from = v;
        self.to = v;
        self.value = v;
    }

    /// Advances to `now_ms`; returns true while still animating.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.start_ms == 0 || (self.value - self.to).abs() < 0.0005 {
            self.value = self.to;
            return false;
        }
        let elapsed = now_ms.saturating_sub(self.start_ms) as f32;
        let t = (elapsed / self.dur_ms as f32).clamp(0.0, 1.0);
        self.value = self.from + (self.to - self.from) * ease_out_cubic(t);
        if t >= 1.0 {
            self.value = self.to;
            false
        } else {
            true
        }
    }
}

/// A 0..1 saw/triangle phase for looping animations, derived purely from the clock.
pub fn loop_phase(now_ms: u64, period_ms: u32) -> f32 {
    (now_ms % period_ms.max(1) as u64) as f32 / period_ms.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tween_reaches_target() {
        let mut t = Tween::new(0.0);
        t.set(1.0, 100, 1000);
        assert!(t.tick(1050));
        assert!(t.value() > 0.0 && t.value() < 1.0);
        assert!(!t.tick(1200));
        assert_eq!(t.value(), 1.0);
    }

    #[test]
    fn easing_endpoints() {
        assert!(ease_out_cubic(0.0).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert!((ease_in_out_sine(0.5) - 0.5).abs() < 1e-6);
    }
}
