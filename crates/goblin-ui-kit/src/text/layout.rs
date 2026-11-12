//! Line layout: span styling, kerning, word wrapping and ellipsis truncation.

use super::{TextSystem, Weight};

/// Visual style of a run of text, mapped to a colour by the caller at draw time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanStyle {
    /// Normal body text.
    Normal,
    /// Bold (semibold weight).
    Strong,
    /// A hyperlink (drawn accented and underlined).
    Link,
    /// Inline monospace-style code (drawn on a tinted chip).
    Code,
    /// Dimmed secondary text.
    Muted,
}

impl SpanStyle {
    fn weight(self) -> Weight {
        match self {
            SpanStyle::Strong => Weight::SemiBold,
            SpanStyle::Code => Weight::Mono,
            _ => Weight::Regular,
        }
    }

    fn underline(self) -> bool {
        matches!(self, SpanStyle::Link)
    }
}

/// A run of text with one style.
#[derive(Clone, Copy, Debug)]
pub struct Span<'a> {
    /// The text.
    pub text: &'a str,
    /// Its style.
    pub style: SpanStyle,
}

impl<'a> Span<'a> {
    /// A normal-styled span.
    pub fn normal(text: &'a str) -> Self {
        Span {
            text,
            style: SpanStyle::Normal,
        }
    }
}

/// Paragraph-level style for a layout.
#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    /// Pixel size.
    pub size: f32,
    /// Base weight (overridden by strong spans).
    pub weight: Weight,
    /// Line height as a multiple of `size`.
    pub line_height: f32,
    /// Horizontal alignment within the wrap width.
    pub align: Align,
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// Left-aligned.
    Left,
    /// Centred.
    Center,
    /// Right-aligned.
    Right,
}

impl TextStyle {
    /// A left-aligned style at `size` px, 1.3 line height.
    pub fn new(size: f32) -> Self {
        TextStyle {
            size,
            weight: Weight::Regular,
            line_height: 1.3,
            align: Align::Left,
        }
    }

    /// Semibold variant.
    pub fn strong(mut self) -> Self {
        self.weight = Weight::SemiBold;
        self
    }

    /// Centred variant.
    pub fn center(mut self) -> Self {
        self.align = Align::Center;
        self
    }

    /// Sets the line height multiple.
    pub fn leading(mut self, m: f32) -> Self {
        self.line_height = m;
        self
    }
}

/// A positioned glyph in a [`Layout`].
#[derive(Clone, Copy, Debug)]
pub struct PGlyph {
    /// Font index.
    pub font: u16,
    /// Glyph id.
    pub gid: u16,
    /// Pen x relative to the layout origin.
    pub x: f32,
    /// Span style for colouring.
    pub style: SpanStyle,
}

/// One laid-out line.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    /// Baseline y relative to the layout origin.
    pub baseline: f32,
    /// Index of the first glyph.
    pub start: usize,
    /// One past the last glyph.
    pub end: usize,
    /// Line width in pixels.
    pub width: f32,
}

/// A decoration rectangle (underline or code background).
#[derive(Clone, Copy, Debug)]
pub struct Decoration {
    /// X relative to the layout origin.
    pub x: f32,
    /// Y relative to the layout origin.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// Style controlling how it is drawn.
    pub style: SpanStyle,
}

/// The result of laying out text.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// All glyphs in order.
    pub glyphs: Vec<PGlyph>,
    /// Lines indexing into `glyphs`.
    pub lines: Vec<Line>,
    /// Underlines and code chips.
    pub decorations: Vec<Decoration>,
    /// Widest line.
    pub width: f32,
    /// Total height (lines × line height).
    pub height: f32,
    /// Font size.
    pub size: f32,
    /// Size in quarter pixels (glyph cache key).
    pub size_q: u16,
    /// True when an ellipsis was added.
    pub truncated: bool,
}

/// A word plus the whitespace that precedes it, pre-shaped.
#[derive(Default)]
struct Word {
    glyphs: Vec<PGlyph>,
    advances: Vec<f32>,
    width: f32,
    lead_space: f32,
    hard_break_after: bool,
}

pub(super) fn size_q(size: f32) -> u16 {
    (size * 4.0).round().clamp(4.0, 4000.0) as u16
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF |      // kana
        0x3400..=0x4DBF |      // CJK ext A
        0x4E00..=0x9FFF |      // CJK
        0xF900..=0xFAFF |      // compat
        0xFF00..=0xFFEF) // fullwidth
}

/// Shapes `spans` into a flat word list.
fn shape_words(ts: &mut TextSystem, spans: &[Span<'_>], size: f32, base: Weight) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut cur = Word::default();
    let mut prev: Option<(u16, u16)> = None;
    let sq = size_q(size);
    for span in spans {
        let weight = match span.style.weight() {
            Weight::SemiBold => Weight::SemiBold,
            Weight::Mono => Weight::Mono,
            _ => base,
        };
        for ch in span.text.chars() {
            if ch == '\n' {
                cur.hard_break_after = true;
                words.push(std::mem::take(&mut cur));
                prev = None;
                continue;
            }
            if ch == ' ' || ch == '\t' {
                if !cur.glyphs.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    prev = None;
                }
                let (f, g) = ts.resolve(' ', weight);
                cur.lead_space += ts
                    .glyph(super::GlyphKey {
                        font: f,
                        gid: g,
                        size_q: sq,
                    })
                    .advance;
                continue;
            }
            // Each CJK ideograph wraps on its own, so start a fresh word for it and after it.
            if is_cjk(ch) && !cur.glyphs.is_empty() {
                words.push(std::mem::take(&mut cur));
                prev = None;
            }
            let (font, gid) = ts.resolve(ch, weight);
            let adv = ts
                .glyph(super::GlyphKey {
                    font,
                    gid,
                    size_q: sq,
                })
                .advance;
            if let (Some((pf, pg)), Some(last)) = (prev, cur.advances.last_mut()) {
                if pf == font {
                    let k = ts.kern(font, pg, gid, size);
                    *last += k;
                    cur.width += k;
                }
            }
            cur.glyphs.push(PGlyph {
                font,
                gid,
                x: 0.0,
                style: span.style,
            });
            cur.advances.push(adv);
            cur.width += adv;
            prev = Some((font, gid));
            if is_cjk(ch) {
                words.push(std::mem::take(&mut cur));
                prev = None;
            }
        }
    }
    if !cur.glyphs.is_empty() || cur.lead_space > 0.0 {
        words.push(cur);
    }
    words
}

/// Lays out styled spans with optional wrapping.
pub(super) fn layout(
    ts: &mut TextSystem,
    spans: &[Span<'_>],
    style: TextStyle,
    max_width: Option<f32>,
) -> Layout {
    let words = shape_words(ts, spans, style.size, style.weight);
    let (ascent, descent) = ts.line_metrics(style.size);
    let line_adv = (style.size * style.line_height).max(ascent - descent);
    let mut out = Layout {
        size: style.size,
        size_q: size_q(style.size),
        ..Default::default()
    };
    let wrap = max_width.unwrap_or(f32::INFINITY);

    let mut pen = 0.0f32;
    let mut line_start = 0usize;
    let mut line_first = true;
    let mut baseline = ascent;

    struct Open {
        style: SpanStyle,
        x: f32,
    }
    let mut open: Option<Open> = None;
    // Pen position after the last glyph of the open decoration run.
    let mut open_end: f32 = 0.0;

    let finish_line = |out: &mut Layout,
                       line_start: &mut usize,
                       pen: &mut f32,
                       baseline: f32,
                       open: &mut Option<Open>| {
        if let Some(o) = open.take() {
            push_decoration(out, o.style, o.x, *pen, baseline, style.size);
        }
        let end = out.glyphs.len();
        let width = *pen;
        out.lines.push(Line {
            baseline,
            start: *line_start,
            end,
            width,
        });
        out.width = out.width.max(width);
        *line_start = end;
        *pen = 0.0;
    };

    for word in &words {
        let need = if line_first {
            word.width
        } else {
            word.lead_space + word.width
        };
        if !line_first && pen + need > wrap && word.width <= wrap {
            // wrap before this word
            finish_line(&mut out, &mut line_start, &mut pen, baseline, &mut open);
            baseline += line_adv;
            line_first = true;
        }
        // Leading space collapses at the start of a wrapped line; unwrapped text (code) keeps
        // its indentation.
        if !line_first || max_width.is_none() {
            pen += word.lead_space;
        }
        line_first = false;
        for (g, adv) in word.glyphs.iter().zip(&word.advances) {
            // underline / code runs
            match (open.as_ref().map(|o| o.style), g.style) {
                (Some(s), cur) if s == cur => {}
                (Some(_), _) => {
                    let o = open.take().unwrap();
                    // End at the run's last glyph, not after the space that follows it.
                    push_decoration(&mut out, o.style, o.x, open_end, baseline, style.size);
                    if g.style.underline() || g.style == SpanStyle::Code {
                        open = Some(Open {
                            style: g.style,
                            x: pen,
                        });
                    }
                }
                (None, cur) if cur.underline() || cur == SpanStyle::Code => {
                    open = Some(Open { style: cur, x: pen });
                }
                _ => {}
            }
            out.glyphs.push(PGlyph { x: pen, ..*g });
            pen += adv;
            if open.is_some() {
                open_end = pen;
            }
        }
        if word.hard_break_after {
            finish_line(&mut out, &mut line_start, &mut pen, baseline, &mut open);
            baseline += line_adv;
            line_first = true;
        }
    }
    finish_line(&mut out, &mut line_start, &mut pen, baseline, &mut open);
    out.height = out.lines.len() as f32 * line_adv;

    if style.align != Align::Left {
        let w = max_width.unwrap_or(out.width);
        for line in &out.lines {
            let shift = match style.align {
                Align::Center => (w - line.width) * 0.5,
                Align::Right => w - line.width,
                Align::Left => 0.0,
            };
            for g in &mut out.glyphs[line.start..line.end] {
                g.x += shift;
            }
            for d in &mut out.decorations {
                if d.y > line.baseline - style.size && d.y < line.baseline + style.size {
                    d.x += shift;
                }
            }
        }
    }
    out
}

fn push_decoration(
    out: &mut Layout,
    style: SpanStyle,
    x0: f32,
    pen: f32,
    baseline: f32,
    size: f32,
) {
    let w = pen - x0;
    if w <= 0.0 {
        return;
    }
    match style {
        SpanStyle::Link => out.decorations.push(Decoration {
            x: x0,
            y: baseline + size * 0.12,
            w,
            h: (size * 0.06).max(1.0),
            style,
        }),
        SpanStyle::Code => out.decorations.push(Decoration {
            x: x0 - size * 0.12,
            y: baseline - size * 0.78,
            w: w + size * 0.24,
            h: size * 1.02,
            style,
        }),
        _ => {}
    }
}

/// Lays out a single line, adding an ellipsis if it exceeds `max_width`.
pub(super) fn single_line(
    ts: &mut TextSystem,
    text: &str,
    style: TextStyle,
    max_width: Option<f32>,
) -> Layout {
    let flat = text.replace(['\n', '\t'], " ");
    let spans = [Span {
        text: &flat,
        style: SpanStyle::Normal,
    }];
    let mut l = layout_no_wrap(ts, &spans, style);
    if let Some(mw) = max_width {
        if l.width > mw {
            truncate(ts, &flat, style, mw, &mut l);
        }
    }
    l
}

fn layout_no_wrap(ts: &mut TextSystem, spans: &[Span<'_>], style: TextStyle) -> Layout {
    layout(ts, spans, TextStyle { ..style }, None)
}

fn truncate(ts: &mut TextSystem, text: &str, style: TextStyle, mw: f32, out: &mut Layout) {
    let ell = "…";
    let ell_w = {
        let l = layout_no_wrap(ts, &[Span::normal(ell)], style);
        l.width
    };
    let budget = (mw - ell_w).max(0.0);
    let mut cut = 0;
    let mut acc = 0.0;
    let q = out.size_q;
    let mut prev: Option<(u16, u16)> = None;
    for (i, ch) in text.char_indices() {
        let (f, g) = ts.resolve(ch, style.weight);
        let mut adv = ts
            .glyph(super::GlyphKey {
                font: f,
                gid: g,
                size_q: q,
            })
            .advance;
        if let Some((pf, pg)) = prev {
            if pf == f {
                adv += ts.kern(f, pg, g, style.size);
            }
        }
        if acc + adv > budget {
            break;
        }
        acc += adv;
        cut = i + ch.len_utf8();
        prev = Some((f, g));
    }
    let head = &text[..cut];
    let joined = format!("{head}{ell}");
    *out = layout_no_wrap(ts, &[Span::normal(&joined)], style);
    out.truncated = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::test_text;

    #[test]
    fn wraps_at_word_boundaries() {
        let mut ts = test_text();
        let spans = [Span::normal("the quick brown fox jumps over")];
        let l = ts.layout(&spans, TextStyle::new(24.0), Some(120.0));
        assert!(l.lines.len() >= 3);
        for line in &l.lines {
            assert!(line.width <= 120.0 + 1.0, "line {} too wide", line.width);
        }
    }

    #[test]
    fn single_long_word_does_not_loop() {
        let mut ts = test_text();
        let spans = [Span::normal("supercalifragilisticexpialidocious")];
        let l = ts.layout(&spans, TextStyle::new(24.0), Some(40.0));
        assert_eq!(l.lines.len(), 1);
    }

    #[test]
    fn ellipsis_truncation_fits() {
        let mut ts = test_text();
        let l = ts.line(
            "A fairly long status line that will not fit",
            TextStyle::new(20.0),
            Some(100.0),
        );
        assert!(l.truncated);
        assert!(l.width <= 101.0);
    }

    #[test]
    fn cjk_wraps_between_ideographs() {
        let mut ts = test_text();
        let spans = [Span::normal("日本語のテキストはこのように折り返されます")];
        let l = ts.layout(&spans, TextStyle::new(24.0), Some(120.0));
        assert!(l.lines.len() >= 2);
    }

    #[test]
    fn strong_spans_use_semibold() {
        let mut ts = test_text();
        let spans = [
            Span::normal("a "),
            Span {
                text: "bold",
                style: SpanStyle::Strong,
            },
        ];
        let l = ts.layout(&spans, TextStyle::new(24.0), None);
        let bold_glyph = l
            .glyphs
            .iter()
            .find(|g| g.style == SpanStyle::Strong)
            .unwrap();
        assert_eq!(bold_glyph.font, 1);
    }

    #[test]
    fn newlines_make_lines() {
        let mut ts = test_text();
        let spans = [Span::normal("one\ntwo\nthree")];
        let l = ts.layout(&spans, TextStyle::new(20.0), Some(500.0));
        assert_eq!(l.lines.len(), 3);
    }
}
