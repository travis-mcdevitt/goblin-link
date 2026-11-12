//! Text: font loading with fallback, a glyph cache, line layout with wrapping, and a small
//! rich-text (Markdown subset) model for agent replies.
//!
//! Rasterisation uses `fontdue` (pure Rust, no shaping engine). Chat text is Latin-first; the
//! layout applies kerning, wraps at spaces and between CJK ideographs, and falls back through
//! any extra fonts in `fonts/fallback/` for characters Inter lacks (CJK, symbols, monochrome
//! emoji). Complex scripts that need shaping (Arabic joining, Indic reordering) are out of
//! scope for M1 and render unshaped.

mod layout;
pub mod rich;

pub use layout::{Align, Layout, Line, PGlyph, Span, SpanStyle, TextStyle};
pub use rich::{Block, RichLayout};

use crate::canvas::Canvas;
use crate::color::Color;
use crate::geom::Rect;
use crate::image::Mask;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::path::Path;

/// Font weight available in the bundled family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    /// Inter Regular.
    Regular,
    /// Inter SemiBold (titles, names, bold spans, buttons).
    SemiBold,
    /// The monospace face for code (Inter Regular when none is loaded).
    Mono,
}

/// A tiny FxHash-style hasher: glyph keys are small integers, SipHash is overkill on an A53.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(v as u64);
    }
    fn write_u16(&mut self, v: u16) {
        self.write_u64(v as u64);
    }
    fn write_u8(&mut self, v: u8) {
        self.write_u64(v as u64);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

/// HashMap with [`FxHasher`].
pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// Errors while loading fonts.
#[derive(Debug)]
pub enum FontError {
    /// A font file could not be read.
    Io(std::path::PathBuf, std::io::Error),
    /// A font file could not be parsed.
    Parse(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::Io(p, e) => write!(f, "font {}: {e}", p.display()),
            FontError::Parse(m) => write!(f, "font parse: {m}"),
        }
    }
}

impl std::error::Error for FontError {}

/// Glyph key: font index, glyph id and size in quarter pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Index into the font set.
    pub font: u16,
    /// Glyph id within that font.
    pub gid: u16,
    /// Pixel size × 4.
    pub size_q: u16,
}

/// A rasterised glyph.
#[derive(Clone, Debug)]
pub struct CachedGlyph {
    /// Coverage bitmap.
    pub mask: Mask,
    /// Bitmap left offset from the pen position.
    pub xmin: i32,
    /// Bitmap top offset above the baseline (positive up).
    pub top: i32,
    /// Horizontal advance in pixels.
    pub advance: f32,
}

/// Fonts, fallback resolution and the glyph cache.
pub struct TextSystem {
    fonts: Vec<fontdue::Font>,
    fallbacks: Vec<u16>,
    /// Index of the monospace font, when one is loaded.
    mono: Option<u16>,
    resolved: FxMap<(char, Weight), (u16, u16)>,
    glyphs: FxMap<GlyphKey, CachedGlyph>,
    cache_bytes: usize,
}

/// Cache budget before the glyph cache is flushed (about 1,500 body glyphs at scale 2).
const GLYPH_CACHE_BUDGET: usize = 3 * 1024 * 1024;

const REGULAR: u16 = 0;
const SEMIBOLD: u16 = 1;

impl TextSystem {
    /// Builds a text system from Inter Regular and SemiBold font data.
    pub fn new(regular: &[u8], semibold: &[u8]) -> Result<Self, FontError> {
        let load = |b: &[u8]| {
            fontdue::Font::from_bytes(b, fontdue::FontSettings::default())
                .map_err(|e| FontError::Parse(e.to_string()))
        };
        Ok(TextSystem {
            fonts: vec![load(regular)?, load(semibold)?],
            fallbacks: Vec::new(),
            mono: None,
            resolved: FxMap::default(),
            glyphs: FxMap::default(),
            cache_bytes: 0,
        })
    }

    /// Loads `Inter-Regular.ttf` and `Inter-SemiBold.ttf` from `dir`, plus every `.ttf`/`.otf`
    /// in `dir/fallback/` (sorted by file name) as fallbacks.
    pub fn load_dir(dir: &Path) -> Result<Self, FontError> {
        let read = |p: std::path::PathBuf| std::fs::read(&p).map_err(|e| FontError::Io(p, e));
        let mut ts = TextSystem::new(
            &read(dir.join("Inter-Regular.ttf"))?,
            &read(dir.join("Inter-SemiBold.ttf"))?,
        )?;
        if let Ok(rd) = std::fs::read_dir(dir.join("fallback")) {
            let mut files: Vec<_> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf")
                    })
                })
                .collect();
            files.sort();
            for f in files {
                ts.add_fallback(&read(f)?)?;
            }
        }
        Ok(ts)
    }

    /// Sets the monospace font used for code.
    pub fn set_mono(&mut self, data: &[u8]) -> Result<(), FontError> {
        let f = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .map_err(|e| FontError::Parse(e.to_string()))?;
        self.mono = Some(self.fonts.len() as u16);
        self.fonts.push(f);
        self.resolved.clear();
        Ok(())
    }

    /// Whether any loaded font has a glyph for `ch`.
    pub fn has_glyph(&mut self, ch: char) -> bool {
        self.resolve(ch, Weight::Regular).1 != 0
    }

    /// Adds a fallback font consulted for characters missing from Inter.
    pub fn add_fallback(&mut self, data: &[u8]) -> Result<(), FontError> {
        let f = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .map_err(|e| FontError::Parse(e.to_string()))?;
        self.fallbacks.push(self.fonts.len() as u16);
        self.fonts.push(f);
        self.resolved.clear();
        Ok(())
    }

    /// Finds the font and glyph for a character, walking the fallback chain.
    pub fn resolve(&mut self, ch: char, w: Weight) -> (u16, u16) {
        if let Some(&r) = self.resolved.get(&(ch, w)) {
            return r;
        }
        let primary = match w {
            Weight::Regular => REGULAR,
            Weight::SemiBold => SEMIBOLD,
            Weight::Mono => self.mono.unwrap_or(REGULAR),
        };
        let mut found = (primary, 0);
        for f in std::iter::once(primary).chain(self.fallbacks.iter().copied()) {
            let gid = self.fonts[f as usize].lookup_glyph_index(ch);
            if gid != 0 {
                found = (f, gid);
                break;
            }
        }
        self.resolved.insert((ch, w), found);
        found
    }

    /// Ascent and descent (descent negative) of the primary font at `size` px.
    pub fn line_metrics(&self, size: f32) -> (f32, f32) {
        self.fonts[REGULAR as usize]
            .horizontal_line_metrics(size)
            .map(|m| (m.ascent, m.descent))
            .unwrap_or((size * 0.97, -size * 0.24))
    }

    /// Kerning adjustment between two glyphs of the same font.
    pub fn kern(&self, font: u16, a: u16, b: u16, size: f32) -> f32 {
        self.fonts[font as usize]
            .horizontal_kern_indexed(a, b, size)
            .unwrap_or(0.0)
    }

    /// Returns (rasterising on first use) a cached glyph.
    pub fn glyph(&mut self, key: GlyphKey) -> &CachedGlyph {
        if !self.glyphs.contains_key(&key) && self.cache_bytes > GLYPH_CACHE_BUDGET {
            self.glyphs.clear();
            self.cache_bytes = 0;
        }
        let fonts = &self.fonts;
        let bytes = &mut self.cache_bytes;
        self.glyphs.entry(key).or_insert_with(|| {
            let px = key.size_q as f32 / 4.0;
            let (m, bitmap) = fonts[key.font as usize].rasterize_indexed(key.gid, px);
            *bytes += bitmap.len() + 64;
            CachedGlyph {
                mask: Mask {
                    w: m.width as u32,
                    h: m.height as u32,
                    data: bitmap,
                },
                xmin: m.xmin,
                top: m.height as i32 + m.ymin,
                advance: m.advance_width,
            }
        })
    }

    /// Number of cached glyphs (diagnostics).
    pub fn cached_glyphs(&self) -> usize {
        self.glyphs.len()
    }

    /// Lays out styled spans; wraps at `max_width` when given.
    pub fn layout(
        &mut self,
        spans: &[Span<'_>],
        style: TextStyle,
        max_width: Option<f32>,
    ) -> Layout {
        layout::layout(self, spans, style, max_width)
    }

    /// Lays out a single plain line, truncating with an ellipsis to fit `max_width`.
    pub fn line(&mut self, text: &str, style: TextStyle, max_width: Option<f32>) -> Layout {
        layout::single_line(self, text, style, max_width)
    }

    /// Width of a plain single line.
    pub fn measure(&mut self, text: &str, style: TextStyle) -> f32 {
        self.line(text, style, None).width
    }

    /// Draws a layout with its top-left at `(x, y)`; `color` maps span styles to colours.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        l: &Layout,
        x: f32,
        y: f32,
        color: &dyn Fn(SpanStyle) -> Color,
    ) {
        let clip = canvas.clip();
        let (ct, cb) = (clip.y as f32, clip.bottom() as f32);
        for d in &l.decorations {
            let r = Rect::new(x + d.x, y + d.y, d.w, d.h);
            if r.bottom() >= ct && r.y <= cb {
                match d.style {
                    SpanStyle::Code => {
                        canvas.fill_rrect(r, d.h * 0.25, color(SpanStyle::Code).fade(0.14))
                    }
                    s => canvas.fill_rect(r, color(s)),
                }
            }
        }
        for line in &l.lines {
            let top = y + line.baseline - l.size * 1.1;
            let bottom = y + line.baseline + l.size * 0.4;
            if bottom < ct || top > cb {
                continue;
            }
            for g in &l.glyphs[line.start..line.end] {
                let key = GlyphKey {
                    font: g.font,
                    gid: g.gid,
                    size_q: l.size_q,
                };
                let cg = self.glyph(key);
                if cg.mask.w == 0 {
                    continue;
                }
                let gx = (x + g.x).round() as i32 + cg.xmin;
                let gy = (y + line.baseline).round() as i32 - cg.top;
                canvas.draw_mask(&cg.mask, gx, gy, color(g.style));
            }
        }
    }

    /// Convenience: lays out and draws one plain line, returning its width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: TextStyle,
        x: f32,
        y: f32,
        max_width: Option<f32>,
        c: Color,
    ) -> f32 {
        let l = self.line(text, style, max_width);
        self.draw(canvas, &l, x, y, &|_| c);
        l.width
    }
}

#[cfg(test)]
pub(crate) fn test_text() -> TextSystem {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    TextSystem::load_dir(&dir).expect("bundled fonts")
}
