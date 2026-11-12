//! Drawing a thing once and letting the GPU move it.
//!
//! The kit draws every pixel on the CPU. That is exact and portable, but a screen that only
//! scrolls redraws everything to move it a few pixels. A *sprite* is a piece of the screen
//! (a chat bubble, the header, the hint bar) drawn once into its own picture with real
//! transparency; after that a frame only says where each sprite goes, and the shell hands
//! the sprites to the GPU as textures.
//!
//! [`Painter::sprite`](crate::widgets::Painter::sprite) is the one entry point. With sprites
//! off (screenshots, tests, devices without a GPU) it simply draws, so there is a single
//! drawing code path and the pictures it makes are the same either way. With sprites on:
//!
//! - A sprite not seen before is drawn twice into a scratch canvas, over black and over
//!   white. Compositing is linear in what lies underneath, so the two results give each
//!   pixel's own colour and its transparency exactly, without the canvas having to carry an
//!   alpha channel.
//! - The frame's list then gets "this sprite, here, clipped so". The frame canvas itself is
//!   not written.
//!
//! Anything drawn outside a sprite while sprites are on lands on the frame canvas and marks
//! it written ([`Canvas::take_written`](crate::Canvas::take_written)); the shell then knows
//! the frame cannot be composed from sprites alone and draws it the old way.

use std::collections::HashMap;

use crate::canvas::Canvas;
use crate::geom::IRect;
use crate::Color;

/// Largest sprite, in pixels. Something bigger is drawn directly.
pub const MAX_SPRITE_PIXELS: i32 = 1024 * 3072;

/// One sprite placed in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Which sprite.
    pub key: u64,
    /// Where its top-left corner goes; its size is the sprite's own.
    pub x: i32,
    /// Top.
    pub y: i32,
    /// Only this part of the screen may be touched.
    pub clip: IRect,
}

/// A sprite's pixels, on their way to the GPU: straight (not premultiplied) alpha, in the
/// canvas's byte order (R, G, B, A).
#[derive(Clone, Debug)]
pub struct Pixels {
    /// Which sprite.
    pub key: u64,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    /// `w * h * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The sprites of one frame and what is known of those made before.
#[derive(Default)]
pub struct Sprites {
    /// Whether [`Painter::sprite`](crate::widgets::Painter::sprite) makes sprites this frame.
    pub enabled: bool,
    /// Mixed into every key: whatever changes how everything looks (theme, size).
    pub salt: u64,
    /// The frame, in drawing order.
    pub placed: Vec<Placed>,
    /// Sprites drawn this frame for the first time.
    pub fresh: Vec<Pixels>,
    /// The colour the frame starts from (the page), when the frame cleared to one.
    pub clear: Option<Color>,
    /// What each known sprite's drawing returned (a height, usually), and its size.
    known: HashMap<u64, (f32, i32, i32)>,
    /// The canvas sprites are drawn into.
    pub(crate) scratch: Option<Canvas>,
    /// Inside a sprite's drawing (a sprite within a sprite is just drawn).
    pub(crate) capturing: bool,
}

impl Sprites {
    /// Starts a frame.
    pub fn begin(&mut self, enabled: bool, salt: u64) {
        self.enabled = enabled;
        self.salt = salt;
        self.placed.clear();
        self.fresh.clear();
        self.clear = None;
        self.capturing = false;
    }

    /// The value and size remembered for a sprite.
    pub fn known(&self, key: u64) -> Option<(f32, i32, i32)> {
        self.known.get(&key).copied()
    }

    /// Remembers a sprite.
    pub(crate) fn remember(&mut self, key: u64, value: f32, w: i32, h: i32) {
        self.known.insert(key, (value, w, h));
    }

    /// Forgets sprites the shell no longer holds (it dropped their textures), so they are
    /// drawn again when next needed.
    pub fn forget(&mut self, keys: impl IntoIterator<Item = u64>) {
        for k in keys {
            self.known.remove(&k);
        }
    }

    /// Forgets every sprite.
    pub fn forget_all(&mut self) {
        self.known.clear();
    }

    /// How many sprites are known.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    /// Whether no sprite is known.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// A sprite's own colour and transparency from the same drawing over black and over white.
///
/// For a pixel the drawing covers by `a` with colour `c`, over black it reads `c*a` and over
/// white `c*a + (1-a)`: the difference of the two is `1-a`, and the black one divided by
/// `a` is `c`.
pub(crate) fn alpha_from_two(black: &[u32], white: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(black.len() * 4);
    for (&b, &w) in black.iter().zip(white) {
        if b == w {
            // Fully covered: the pixel is what was drawn.
            out.extend_from_slice(&[b as u8, (b >> 8) as u8, (b >> 16) as u8, 255]);
            continue;
        }
        let ch = |v: u32, s: u32| (v >> s) & 0xFF;
        // The channel that moved most gives the steadiest reading of the transparency.
        let d = (0..3)
            .map(|i| ch(w, i * 8).saturating_sub(ch(b, i * 8)))
            .max()
            .unwrap_or(0);
        let a = 255 - d.min(255);
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let un = |v: u32| ((v * 255 + a / 2) / a).min(255) as u8;
        out.extend_from_slice(&[un(ch(b, 0)), un(ch(b, 8)), un(ch(b, 16)), a as u8]);
    }
    out
}

/// Composes a frame from its sprites on the CPU, the way the GPU does: the page colour, then
/// each placed sprite over it. For tests, which compare the result with the frame drawn
/// directly.
pub fn compose(
    w: u32,
    h: u32,
    page: Color,
    placed: &[Placed],
    pixels: &HashMap<u64, Pixels>,
) -> Vec<u32> {
    let mut out = vec![page.with_alpha(255).pack(); (w * h) as usize];
    for p in placed {
        let Some(s) = pixels.get(&p.key) else {
            continue;
        };
        let area = IRect::new(p.x, p.y, s.w as i32, s.h as i32)
            .intersect(&p.clip)
            .intersect(&IRect::new(0, 0, w as i32, h as i32));
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let i = (((y - p.y) as u32 * s.w + (x - p.x) as u32) * 4) as usize;
                let a = s.rgba[i + 3] as u32;
                if a == 0 {
                    continue;
                }
                let o = (y as u32 * w + x as u32) as usize;
                let d = out[o];
                let mix = |sc: u8, shift: u32| {
                    let dc = (d >> shift) & 0xFF;
                    ((sc as u32 * a + dc * (255 - a) + 127) / 255) << shift
                };
                out[o] = 0xFF00_0000
                    | mix(s.rgba[i], 0)
                    | mix(s.rgba[i + 1], 8)
                    | mix(s.rgba[i + 2], 16);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_drawings_give_back_colour_and_transparency() {
        // A pixel of colour (200, 100, 50) at 60% over black and over white.
        let over = |bg: u32| {
            let c = |v: u32| (v * 60 + bg * 40) / 100;
            0xFF00_0000 | c(200) | (c(100) << 8) | (c(50) << 16)
        };
        let rgba = alpha_from_two(
            &[over(0), 0xFF11_2233, 0xFF00_0000],
            &[over(255), 0xFF11_2233, 0xFFFF_FFFF],
        );
        let a = rgba[3] as i32;
        assert!((a - 153).abs() <= 1, "alpha {a}");
        assert!((rgba[0] as i32 - 200).abs() <= 2 && (rgba[1] as i32 - 100).abs() <= 2);
        assert_eq!(
            &rgba[4..8],
            &[0x33, 0x22, 0x11, 255],
            "untouched by the background: opaque"
        );
        assert_eq!(&rgba[8..12], &[0, 0, 0, 0], "nothing drawn: clear");
    }
}
