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
    /// Lay the text out as it is written, for a reader who must see all of it: the spaces
    /// a line begins with after a line break are kept (its indentation), and a word wider
    /// than the room left for it is broken where the line ends, not run past the edge.
    /// Each line says how it joins the one before ([`Line::joint`]), so a reader can be
    /// shown which lines the text began itself and which the wrapping made.
    pub verbatim: bool,
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
            verbatim: false,
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

    /// Lays the text out as it is written (see [`TextStyle::verbatim`]).
    pub fn verbatim(mut self) -> Self {
        self.verbatim = true;
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

/// How a line follows the one before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Joint {
    /// It begins where the text has a line break (or is the first line).
    #[default]
    Break,
    /// The wrapping began it where the text has a space: the two lines are one line of the
    /// text, with a space between them.
    Space,
    /// The wrapping began it where the text has nothing: a word too long for the line, or
    /// characters that are not set apart by spaces, goes on with nothing between.
    Split,
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
    /// How it follows the line before it.
    pub joint: Joint,
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
    /// Laid out as written ([`TextStyle::verbatim`]), and still something of the text is
    /// not on the screen, where a reader would take it for absent: spaces at the end of a
    /// line or of the text, more than one space where a line was carried over, or
    /// indentation as wide as the line.
    pub lost: bool,
}

/// A word plus the whitespace that precedes it, pre-shaped.
#[derive(Default)]
struct Word {
    glyphs: Vec<PGlyph>,
    advances: Vec<f32>,
    width: f32,
    lead_space: f32,
    /// How many spaces (and tabs) `lead_space` is.
    spaces: u32,
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
                cur.spaces += 1;
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

    // How the line being filled follows the one before it.
    let mut joint = Joint::Break;
    let finish_line = |out: &mut Layout,
                       line_start: &mut usize,
                       pen: &mut f32,
                       baseline: f32,
                       open: &mut Option<Open>,
                       joint: Joint| {
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
            joint,
        });
        out.width = out.width.max(width);
        *line_start = end;
        *pen = 0.0;
    };

    for word in &words {
        // Shown as written, spaces with nothing after them on their line (before a line
        // break, or at the end) are not seen, and they are not laid out: said, not drawn.
        let trailing = style.verbatim && word.glyphs.is_empty() && word.spaces > 0;
        out.lost |= trailing;
        let need = if line_first {
            word.width
        } else {
            word.lead_space + word.width
        };
        // A word that does not fit after what is on the line starts the next one. Laid
        // out as ever, one wider than a whole line stays where it is and runs over; shown
        // as written, it too starts a line, so that the space before it is where the line
        // was carried over and nowhere else, and it is broken from there.
        if !line_first && !trailing && pen + need > wrap && (word.width <= wrap || style.verbatim) {
            // wrap before this word
            finish_line(
                &mut out,
                &mut line_start,
                &mut pen,
                baseline,
                &mut open,
                joint,
            );
            baseline += line_adv;
            line_first = true;
            joint = if word.spaces > 0 {
                Joint::Space
            } else {
                Joint::Split
            };
            // One space is what the reader of a carried-over line takes to be there.
            out.lost |= style.verbatim && word.spaces > 1;
        }
        // Leading space collapses at the start of a wrapped line. Unwrapped text (code)
        // keeps its indentation, and so does text shown as written, on the lines it began
        // itself.
        if !trailing
            && (!line_first || max_width.is_none() || (style.verbatim && joint == Joint::Break))
        {
            pen += word.lead_space;
        }
        line_first = false;
        let broken = style.verbatim && pen + word.width > wrap;
        for (at, (g, adv)) in word.glyphs.iter().zip(&word.advances).enumerate() {
            // A word too wide for what is left of the line goes on where the line ends.
            // (A line always takes one glyph, however narrow it is.)
            if broken && pen > 0.0 && pen + adv > wrap {
                // Before its first glyph that is a line holding only the word's
                // indentation, which the reader cannot tell from an empty one.
                out.lost |= at == 0;
                finish_line(
                    &mut out,
                    &mut line_start,
                    &mut pen,
                    baseline,
                    &mut open,
                    joint,
                );
                baseline += line_adv;
                joint = Joint::Split;
            }
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
            finish_line(
                &mut out,
                &mut line_start,
                &mut pen,
                baseline,
                &mut open,
                joint,
            );
            baseline += line_adv;
            line_first = true;
            joint = Joint::Break;
        }
    }
    finish_line(
        &mut out,
        &mut line_start,
        &mut pen,
        baseline,
        &mut open,
        joint,
    );
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
    fn a_long_word_is_broken_where_asked() {
        let mut ts = test_text();
        let word = "supercalifragilisticexpialidocious";
        let spans = [Span::normal(word)];
        let style = TextStyle::new(24.0).verbatim();
        let l = ts.layout(&spans, style, Some(120.0));
        assert!(l.lines.len() >= 3, "{} lines", l.lines.len());
        for line in &l.lines {
            assert!(line.width <= 120.0, "line {} too wide", line.width);
        }
        // Every glyph is still there, in order, on some line.
        assert_eq!(l.glyphs.len(), word.chars().count());
        assert_eq!(
            l.lines.iter().map(|n| n.end - n.start).sum::<usize>(),
            l.glyphs.len()
        );
        // Words that fit are wrapped as ever.
        let plain = ts.layout(
            &[Span::normal("the quick brown fox")],
            TextStyle::new(24.0),
            Some(120.0),
        );
        let asked = ts.layout(&[Span::normal("the quick brown fox")], style, Some(120.0));
        assert_eq!(plain.lines.len(), asked.lines.len());
        // A wrap narrower than one glyph still ends.
        let narrow = ts.layout(&spans, style, Some(1.0));
        assert_eq!(narrow.lines.len(), word.chars().count());
    }

    #[test]
    fn text_shown_as_written_keeps_its_own_lines_apart_from_the_wrapped_ones() {
        let mut ts = test_text();
        let style = TextStyle::new(20.0).verbatim();
        let text =
            "if ready:\n    run the long job now\nelse:\n    supercalifragilisticexpialidocious";
        let l = ts.layout(&[Span::normal(text)], style, Some(150.0));
        let first_x = |line: &Line| l.glyphs[line.start].x;
        // The text's own four lines begin with a break; what the wrapping began says so,
        // and says whether a space or nothing lies between.
        let breaks: Vec<&Line> = l.lines.iter().filter(|n| n.joint == Joint::Break).collect();
        assert_eq!(breaks.len(), 4, "{:?}", l.lines);
        assert!(l.lines.iter().any(|n| n.joint == Joint::Space));
        assert!(l.lines.iter().any(|n| n.joint == Joint::Split));
        // An indented line starts indented; an unindented one and a wrapped one do not.
        assert_eq!(first_x(breaks[0]), 0.0);
        assert!(first_x(breaks[1]) > 10.0, "{}", first_x(breaks[1]));
        assert_eq!(first_x(breaks[2]), 0.0);
        assert!(first_x(breaks[3]) > 10.0);
        for line in l.lines.iter().filter(|n| n.joint != Joint::Break) {
            assert_eq!(first_x(line), 0.0);
        }
        for line in &l.lines {
            assert!(line.width <= 150.0, "line {} too wide", line.width);
        }
        // The long word is too wide for what its indentation leaves: it is broken there,
        // and each piece after the first is marked as going on with nothing between.
        let last_break = l
            .lines
            .iter()
            .rposition(|n| n.joint == Joint::Break)
            .unwrap();
        assert!(l.lines[last_break + 1..]
            .iter()
            .all(|n| n.joint == Joint::Split));
        assert!(l.lines.len() > last_break + 1);
        // Laid out as ever, the same text loses its indentation and says nothing of this
        // beyond where the wrapping fell.
        let plain = ts.layout(&[Span::normal(text)], TextStyle::new(20.0), Some(150.0));
        assert!(plain.lines.iter().all(|n| plain.glyphs[n.start].x == 0.0));
        // Text with no spaces between its characters wraps with nothing between.
        let cjk = ts.layout(
            &[Span::normal(
                "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}",
            )],
            style,
            Some(60.0),
        );
        assert!(cjk.lines.len() > 1);
        assert!(cjk.lines[1..].iter().all(|n| n.joint == Joint::Split));
    }

    /// The line of `l` that glyph number `glyph` is on.
    fn line_of(l: &Layout, glyph: usize) -> &Line {
        l.lines
            .iter()
            .find(|n| n.start <= glyph && glyph < n.end)
            .expect("on some line")
    }

    #[test]
    fn a_word_carried_over_after_a_space_says_so_even_when_it_is_then_broken() {
        let mut ts = test_text();
        let style = TextStyle::new(20.0).verbatim();
        // Two arguments: a folder, then a name too long for any line here.
        let text = format!("rm -rf ~/ {}", "A".repeat(60));
        let head = ts.measure("rm -rf ~/", style);
        let space = ts.measure("a b", style) - ts.measure("ab", style);
        assert!(space > 1.0);
        // From where not even the space fits after the folder, through where the space
        // fits and the name's first letter does not, to where a few letters fit as well.
        let mut wrap = head - 1.0;
        while wrap < head + space + 40.0 {
            let l = ts.layout(&[Span::normal(&text)], style, Some(wrap));
            // The seven glyphs of `rm -rf ~/` come first; the name starts at the eighth.
            let name = line_of(&l, 7);
            assert_eq!(name.start, 7, "at {wrap}: the name starts its own line");
            assert_eq!(name.joint, Joint::Space, "at {wrap}: after a space");
            // What follows of the name goes on with nothing between.
            let at = l.lines.iter().position(|n| n.start == 7).unwrap();
            assert!(l.lines.len() > at + 1, "at {wrap}: the name is broken");
            assert!(
                l.lines[at + 1..].iter().all(|n| n.joint == Joint::Split),
                "at {wrap}"
            );
            assert!(!l.lost, "at {wrap}");
            wrap += 0.5;
        }
    }

    #[test]
    fn text_shown_as_written_says_what_it_still_could_not_show() {
        let mut ts = test_text();
        let style = TextStyle::new(20.0).verbatim();
        // Where the first five words fit and nothing more does.
        let five = ts.measure("one two three four five", style) + 2.0;
        let mut lost =
            |text: &str, wrap: f32| ts.layout(&[Span::normal(text)], style, Some(wrap)).lost;
        // Ordinary text loses nothing: single spaces, indentation, lines of its own, a
        // wider gap inside a line, lines carried over at one space or inside a word.
        for whole in [
            "rm -rf build",
            "if x:\n    y()\n\nz()",
            "a  b   c",
            "one two three four five six seven eight nine ten",
            "supercalifragilisticexpialidocious",
            "",
        ] {
            assert!(!lost(whole, 120.0), "{whole:?}");
        }
        // In sh a backslash, a space and a line break is not a continued line, and a
        // backslash and a line break is. The space is not seen, so it is said.
        assert!(!lost("cmd --force \\\n  --dry-run", 400.0));
        assert!(lost("cmd --force \\ \n  --dry-run", 400.0));
        // The same at the end of the text, and on a line of nothing but spaces.
        assert!(lost("ls ", 400.0));
        assert!(lost("a\n   \nb", 400.0));
        // Several spaces where a line is carried over would read as one.
        assert!(lost("one two three four five     six seven", five));
        assert!(!lost("one two three four five six seven", five));
        assert!(!lost("one two three four five     six seven", 800.0));
        // Indentation as wide as the line leaves a line with nothing on it.
        let deep = format!("{}x", " ".repeat(80));
        assert!(lost(&deep, 120.0));
        assert!(!lost(&deep, 800.0));
        // Laid out as ever, none of this is asked.
        let plain = ts.layout(&[Span::normal("ls \n")], TextStyle::new(20.0), Some(400.0));
        assert!(!plain.lost);
    }

    #[test]
    fn says_what_a_character_puts_on_the_screen() {
        use crate::text::Drawn;
        let mut ui = crate::Ui::new().expect("ui");
        let ts = &mut ui.text;
        for seen in [
            'a', 'W', '-', '|', '\u{e9}', '\u{2014}', '\u{26a0}', '\u{2705}', '\u{2713}',
        ] {
            assert_eq!(ts.drawn(seen), Drawn::Glyph, "U+{:04X}", seen as u32);
        }
        // An accent is drawn on the letter before it and takes no room of its own.
        assert_eq!(ts.drawn('\u{301}'), Drawn::Mark);
        // No font has a glyph for a code point that is not a character at all, and the
        // bundled ones have none for Chinese: those would be boxes.
        for boxed in ['\u{378}', '\u{4e2d}', '\t', '\u{2800}'] {
            assert_eq!(ts.drawn(boxed), Drawn::Nothing, "U+{:04X}", boxed as u32);
        }
        // These have glyphs, and the glyphs are empty: a joiner, an emoji's selector, and
        // spaces of several kinds, the plain one among them.
        for empty in ['\u{34f}', '\u{fe0f}', '\u{200d}', '\u{a0}', '\u{2003}', ' '] {
            assert_eq!(ts.drawn(empty), Drawn::Nothing, "U+{:04X}", empty as u32);
        }
        // Asked again, the answer is the same.
        assert_eq!(ts.drawn('a'), Drawn::Glyph);
        assert_eq!(ts.drawn('\u{34f}'), Drawn::Nothing);
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
