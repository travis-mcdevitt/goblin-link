//! `goblin-ui-kit`: a headless, SDL-free UI toolkit for the Goblin Link handheld client.
//!
//! Everything renders into an in-memory RGBA [`Canvas`], so the whole UI is testable without a
//! window (see the golden-image tests in `goblin-ui`). The crate provides:
//!
//! - a software [`Canvas`] with anti-aliased rounded rectangles, pills, circles, hairlines,
//!   focus rings, alpha blending and scaled image blitting ([`canvas`], [`raster`]);
//! - a pure-Rust [`text`] stack (fontdue) with a glyph cache, wrapping, a Markdown subset for
//!   agent replies, and font fallback;
//! - outline [`icons`] rasterised on demand from a bundled Lucide subset;
//! - [`theme`] tokens (light/dark, eight chat themes, a layout scale);
//! - a device-independent [`input`] model and directional [`focus`] navigation;
//! - [`widgets`] and an original animated [`mascot`];
//! - a [`Ui`] context bundling fonts, icons and per-frame scratch state.
//!
//! Fonts (Inter, OFL) and icons (Lucide, ISC) are bundled from `assets/`; see `NOTICE`.

pub mod anim;
pub mod canvas;
pub mod color;
pub mod focus;
pub mod geom;
pub mod icons;
pub mod image;
pub mod input;
pub mod mascot;
pub mod raster;
pub mod sprites;
pub mod svg;
pub mod text;
pub mod theme;
pub mod widgets;

pub use canvas::Canvas;
pub use color::Color;
pub use focus::{FocusId, FocusMemory, FocusSet};
pub use geom::{IRect, Rect};
pub use icons::{Icon, IconCache, IconStyle};
pub use image::{Image, Mask};
pub use input::{Button, InputEvent, InputMap, InputState, Phase, Timing};
pub use mascot::{Mascot, MascotState};
pub use text::{RichLayout, TextStyle, TextSystem};
pub use theme::{AvatarSize, ChatTheme, Mode, Palette, Theme, UiSize};

/// Inter Regular, bundled (OFL, see `assets/fonts/OFL-Inter.txt`).
pub const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
/// Inter SemiBold, bundled (OFL).
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");

/// DejaVu Sans Mono, bundled for code (see `assets/fonts/LICENSE-DejaVu.txt`).
pub const MONO: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSansMono.ttf");
/// Noto Emoji (monochrome), bundled as the fallback for emoji and symbols (OFL, see
/// `assets/fonts/OFL-NotoEmoji.txt`).
pub const EMOJI: &[u8] = include_bytes!("../../../assets/fonts/NotoEmoji.ttf");

/// The bundled fonts: Inter, the monospace face and the emoji fallback.
fn bundled_text() -> Result<TextSystem, text::FontError> {
    let mut ts = TextSystem::new(INTER_REGULAR, INTER_SEMIBOLD)?;
    ts.set_mono(MONO)?;
    ts.add_fallback(EMOJI)?;
    Ok(ts)
}

/// The focus ring glides from one item to the next instead of jumping. Widgets ask for it
/// with `Painter::focus_ring`; the app draws it once, last, with `Painter::draw_focus_ring`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusRing {
    /// Where the focused widget asked for the ring this frame: (rect, corner radius).
    pub target: Option<(Rect, f32)>,
    /// The clip in force where it was asked for, so the ring of an item scrolled partly out
    /// of its list is cut off with the item instead of being drawn over what lies beyond.
    pub clip: Option<IRect>,
    from: Option<(Rect, f32)>,
    to: Option<(Rect, f32)>,
    start_ms: u64,
    shown: Option<(Rect, f32)>,
}

/// How long the ring takes to reach a new item.
pub const RING_MS: u64 = 90;

impl FocusRing {
    /// Clears this frame's request (call before drawing a frame).
    pub fn begin(&mut self) {
        self.target = None;
        self.clip = None;
    }

    /// Where to draw the ring now, and whether it is still moving. `instant` skips the glide.
    pub fn advance(&mut self, now_ms: u64, instant: bool) -> (Option<(Rect, f32)>, bool) {
        let Some(target) = self.target else {
            self.from = None;
            self.to = None;
            self.shown = None;
            return (None, false);
        };
        let same = |a: &(Rect, f32), b: &(Rect, f32)| {
            (a.0.x - b.0.x).abs() < 0.5
                && (a.0.y - b.0.y).abs() < 0.5
                && (a.0.w - b.0.w).abs() < 0.5
                && (a.0.h - b.0.h).abs() < 0.5
        };
        if self.to.map_or(true, |t| !same(&t, &target)) {
            // A new destination: start from wherever the ring is now.
            self.from = if instant { None } else { self.shown };
            self.to = Some(target);
            self.start_ms = now_ms;
        }
        let Some(from) = self.from else {
            self.shown = Some(target);
            return (Some(target), false);
        };
        let t = (now_ms.saturating_sub(self.start_ms) as f32 / RING_MS as f32).clamp(0.0, 1.0);
        if t >= 1.0 {
            self.from = None;
            self.shown = Some(target);
            return (Some(target), false);
        }
        let e = anim::ease_out_cubic(t);
        let mix = |a: f32, b: f32| a + (b - a) * e;
        let r = Rect::new(
            mix(from.0.x, target.0.x),
            mix(from.0.y, target.0.y),
            mix(from.0.w, target.0.w),
            mix(from.0.h, target.0.h),
        );
        let shown = (r, mix(from.1, target.1));
        self.shown = Some(shown);
        (Some(shown), true)
    }
}

/// Shared UI context: the font system, icon cache and a reusable focus set. One per process.
pub struct Ui {
    /// Text layout and rasterisation.
    pub text: TextSystem,
    /// Icon mask cache.
    pub icons: IconCache,
    /// Focus set, reused each frame.
    pub focus: FocusSet,
    /// Per-screen focus memory.
    pub memory: FocusMemory,
    /// The animated mascot avatar.
    pub mascot: Mascot,
    /// The focus ring's glide between items.
    pub ring: FocusRing,
    /// Where the chat bubble drawn last sits (for what hangs off it, such as a reaction).
    pub last_bubble: Rect,
    /// Pieces of the screen drawn once and moved by the GPU (off unless the shell asks).
    pub sprites: sprites::Sprites,
}

impl Ui {
    /// Builds the context from the bundled fonts.
    pub fn new() -> Result<Self, text::FontError> {
        Ok(Ui {
            text: bundled_text()?,
            icons: IconCache::default(),
            focus: FocusSet::default(),
            memory: FocusMemory::default(),
            mascot: Mascot::new(),
            ring: FocusRing::default(),
            last_bubble: Rect::new(0.0, 0.0, 0.0, 0.0),
            sprites: sprites::Sprites::default(),
        })
    }

    /// Builds the context, loading fonts (and any fallbacks) from `font_dir` instead of the
    /// bundled copies. Falls back to bundled fonts if the directory is unusable.
    pub fn with_font_dir(font_dir: &std::path::Path) -> Result<Self, text::FontError> {
        let text = TextSystem::load_dir(font_dir).or_else(|_| bundled_text())?;
        Ok(Ui {
            text,
            icons: IconCache::default(),
            focus: FocusSet::default(),
            memory: FocusMemory::default(),
            mascot: Mascot::new(),
            ring: FocusRing::default(),
            last_bubble: Rect::new(0.0, 0.0, 0.0, 0.0),
            sprites: sprites::Sprites::default(),
        })
    }
}

/// Computes a sensible layout scale for a physical resolution (2.0 at 1024×768 reference).
pub fn scale_for(_width: u32, height: u32) -> f32 {
    // Scale so the vertical density matches the 1024×768 reference; clamp to sane bounds.
    (height as f32 / 384.0).clamp(1.1, 2.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_builds_from_bundled_fonts() {
        let ui = Ui::new().expect("ui context");
        assert!(ui.text.cached_glyphs() == 0);
    }

    #[test]
    fn scale_matches_targets() {
        assert!((scale_for(1024, 768) - 2.0).abs() < 0.01);
        assert!((scale_for(640, 480) - 1.25).abs() < 0.01);
    }
}
