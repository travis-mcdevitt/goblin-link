//! Straight-alpha sRGB colours and the packed pixel format used by [`crate::Canvas`].

use serde::de::{self, Deserialize, Deserializer};
use std::fmt;

/// An 8-bit straight-alpha sRGB colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha (255 = opaque).
    pub a: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    /// Opaque white.
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    /// Opaque black.
    pub const BLACK: Color = Color::rgb(0, 0, 0);

    /// Opaque colour from components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }

    /// Colour with alpha from components.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    /// Parses `#RGB`, `#RRGGBB` or `#RRGGBBAA`.
    pub fn from_hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        let nib = |c: u8| -> Option<u8> { (c as char).to_digit(16).map(|v| v as u8) };
        let b = h.as_bytes();
        match b.len() {
            3 => Some(Color::rgb(
                nib(b[0])? * 17,
                nib(b[1])? * 17,
                nib(b[2])? * 17,
            )),
            6 | 8 => {
                let byte = |i: usize| -> Option<u8> { Some(nib(b[i])? << 4 | nib(b[i + 1])?) };
                let a = if b.len() == 8 { byte(6)? } else { 255 };
                Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, a))
            }
            _ => None,
        }
    }

    /// Same colour with a new alpha.
    pub const fn with_alpha(self, a: u8) -> Self {
        Color { a, ..self }
    }

    /// Same colour with alpha multiplied by `f` (0..=1).
    pub fn fade(self, f: f32) -> Self {
        let a = (self.a as f32 * f.clamp(0.0, 1.0)).round() as u8;
        self.with_alpha(a)
    }

    /// Linear interpolation in sRGB space (`t = 0` → `self`).
    pub fn mix(self, other: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color::rgba(
            l(self.r, other.r),
            l(self.g, other.g),
            l(self.b, other.b),
            l(self.a, other.a),
        )
    }

    /// Relative luminance (WCAG definition), 0..=1.
    pub fn luminance(self) -> f32 {
        fn ch(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }

    /// WCAG contrast ratio between two opaque colours.
    pub fn contrast(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Packs into the canvas pixel format (bytes R, G, B, A in memory).
    pub const fn pack(self) -> u32 {
        u32::from_le_bytes([self.r, self.g, self.b, self.a])
    }

    /// Unpacks a canvas pixel.
    pub const fn unpack(p: u32) -> Self {
        let [r, g, b, a] = p.to_le_bytes();
        Color { r, g, b, a }
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02X}{:02X}{:02X}{:02X}",
            self.r, self.g, self.b, self.a
        )
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::from_hex(&s).ok_or_else(|| de::Error::custom(format!("bad colour {s:?}")))
    }
}

/// Blends opaque `src` (packed) over opaque `dst` with coverage `a` in 0..=256.
#[inline(always)]
pub(crate) fn blend_px(dst: u32, src: u32, a: u32) -> u32 {
    let ia = 256 - a;
    let rb = (((src & 0x00FF_00FF) * a + (dst & 0x00FF_00FF) * ia) >> 8) & 0x00FF_00FF;
    let g = (((src & 0x0000_FF00) * a + (dst & 0x0000_FF00) * ia) >> 8) & 0x0000_FF00;
    rb | g | 0xFF00_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(Color::from_hex("#0064D4"), Some(Color::rgb(0, 0x64, 0xD4)));
        assert_eq!(Color::from_hex("#fff"), Some(Color::WHITE));
        assert_eq!(
            Color::from_hex("#00000080"),
            Some(Color::rgba(0, 0, 0, 0x80))
        );
        assert_eq!(Color::from_hex("0064D4"), None);
        assert_eq!(Color::from_hex("#12345"), None);
    }

    #[test]
    fn blend_endpoints() {
        let w = Color::WHITE.pack();
        let b = Color::BLACK.pack();
        assert_eq!(blend_px(w, b, 256), b);
        assert_eq!(blend_px(w, b, 0), w);
        let half = Color::unpack(blend_px(w, b, 128));
        assert!((126..=129).contains(&half.r));
    }

    #[test]
    fn contrast_of_palette_text() {
        let text = Color::from_hex("#111112").unwrap();
        let page = Color::from_hex("#F3F4F5").unwrap();
        assert!(text.contrast(page) > 15.0);
    }
}
