//! A small Markdown subset for agent replies: paragraphs, bullet and numbered lists, `**bold**`,
//! `` `code` ``, and `[label](url)` / bare URL links. This matches what the Muse app renders in
//! its agent panel (bullets, numbered lists, bold, links, code) without pulling in a full
//! CommonMark engine.

use super::layout::{Align, SpanStyle, TextStyle};
use super::{Layout, Span, TextSystem};

/// An owned styled run.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Text content.
    pub text: String,
    /// Visual style.
    pub style: SpanStyle,
    /// Link target when the run is a link.
    pub href: Option<String>,
}

/// A block of agent rich text.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A paragraph of inline runs.
    Paragraph(Vec<Run>),
    /// A bullet (`•`) list item at `depth` (0-based) with inline runs.
    Bullet { depth: u8, runs: Vec<Run> },
    /// A numbered list item (`n.`) with inline runs.
    Numbered { number: u32, runs: Vec<Run> },
    /// A fenced or indented code block (monospace-styled, not wrapped).
    Code(String),
    /// A heading (`#` = level 1).
    Heading { level: u8, runs: Vec<Run> },
    /// A block quote (`> text`), consecutive lines joined.
    Quote(Vec<Run>),
    /// A table: rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Run>>>),
    /// A horizontal rule.
    Rule,
}

/// Parses the Markdown subset into blocks.
pub fn parse(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<Run> = Vec::new();
    let normalized = src.replace('\r', "");
    let lines: Vec<&str> = normalized.lines().collect();
    let mut i = 0;

    let flush_para = |para: &mut Vec<Run>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(std::mem::take(para)));
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some(rest) = trimmed.strip_prefix("```") {
            flush_para(&mut para, &mut blocks);
            let _lang = rest;
            let mut code = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push_str(lines[i]);
                code.push('\n');
                i += 1;
            }
            i += 1; // closing fence
            while code.ends_with('\n') {
                code.pop();
            }
            blocks.push(Block::Code(code));
            continue;
        }

        if trimmed.is_empty() {
            flush_para(&mut para, &mut blocks);
            i += 1;
            continue;
        }

        if matches!(trimmed, "---" | "***" | "___") {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }

        if let Some(rest) = bullet_marker(trimmed) {
            flush_para(&mut para, &mut blocks);
            let depth = (indent / 2).min(4) as u8;
            blocks.push(Block::Bullet {
                depth,
                runs: parse_inline(rest),
            });
            i += 1;
            continue;
        }

        if let Some((num, rest)) = numbered_marker(trimmed) {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Numbered {
                number: num,
                runs: parse_inline(rest),
            });
            i += 1;
            continue;
        }

        // Heading.
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let rest = &trimmed[level..];
            if level <= 6 && (rest.starts_with(' ') || rest.is_empty()) {
                flush_para(&mut para, &mut blocks);
                let mut runs = parse_inline(rest.trim());
                for r in &mut runs {
                    if r.style == SpanStyle::Normal {
                        r.style = SpanStyle::Strong;
                    }
                }
                blocks.push(Block::Heading {
                    level: level as u8,
                    runs,
                });
                i += 1;
                continue;
            }
        }

        // Block quote: consecutive `>` lines form one quote.
        if trimmed.starts_with('>') {
            flush_para(&mut para, &mut blocks);
            let mut runs: Vec<Run> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let text = lines[i].trim_start()[1..].trim();
                if !runs.is_empty() && !text.is_empty() {
                    runs.push(Run {
                        text: " ".into(),
                        style: SpanStyle::Muted,
                        href: None,
                    });
                }
                for mut r in parse_inline(text) {
                    if r.style == SpanStyle::Normal {
                        r.style = SpanStyle::Muted;
                    }
                    runs.push(r);
                }
                i += 1;
            }
            blocks.push(Block::Quote(runs));
            continue;
        }

        // Table: a `|` row followed by a `|---|` separator row.
        if trimmed.starts_with('|') && i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
            flush_para(&mut para, &mut blocks);
            let mut rows = vec![table_cells(trimmed, true)];
            i += 2;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(table_cells(lines[i].trim(), false));
                i += 1;
            }
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            for r in &mut rows {
                r.resize(cols, Vec::new());
            }
            blocks.push(Block::Table(rows));
            continue;
        }

        // Continuation of a paragraph: join with a space.
        if !para.is_empty() {
            para.push(Run {
                text: " ".into(),
                style: SpanStyle::Normal,
                href: None,
            });
        }
        para.extend(parse_inline(trimmed));
        i += 1;
    }
    flush_para(&mut para, &mut blocks);
    blocks
}

/// Whether `line` is a table separator row such as `|---|:--:|`.
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Splits a `| a | b |` row into parsed cells; header cells are strong.
fn table_cells(row: &str, header: bool) -> Vec<Vec<Run>> {
    let inner = row.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .map(|cell| {
            let mut runs = parse_inline(cell.trim());
            if header {
                for r in &mut runs {
                    if r.style == SpanStyle::Normal {
                        r.style = SpanStyle::Strong;
                    }
                }
            }
            runs
        })
        .collect()
}

/// For `*text*` or `_text_` starting at `i`, the emphasised text and the index after it.
/// There is no italic face, so the caller shows the text plainly without the markers.
fn emphasis(s: &str, i: usize) -> Option<(&str, usize)> {
    let b = s.as_bytes();
    let m = b[i];
    if m != b'*' && m != b'_' {
        return None;
    }
    let next = *b.get(i + 1)?;
    if next == m || next.is_ascii_whitespace() {
        return None;
    }
    // `_` only counts at a word boundary, so snake_case names are left alone.
    if m == b'_' && i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == m && !b[j - 1].is_ascii_whitespace() {
            let after = b.get(j + 1).copied();
            let boundary = after.map_or(true, |c| !c.is_ascii_alphanumeric() && c != m);
            if m == b'*' || boundary {
                return Some((&s[i + 1..j], j + 1));
            }
        }
        j += 1;
    }
    None
}

fn bullet_marker(s: &str) -> Option<&str> {
    for m in ["- ", "* ", "• ", "+ "] {
        if let Some(r) = s.strip_prefix(m) {
            return Some(r);
        }
    }
    None
}

fn numbered_marker(s: &str) -> Option<(u32, &str)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 9 {
        return None;
    }
    let rest = &s[digits.len()..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((digits.parse().ok()?, rest))
}

/// Parses inline styling into runs.
pub fn parse_inline(s: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    let mut text = String::new();
    let push_text = |text: &mut String, runs: &mut Vec<Run>| {
        if !text.is_empty() {
            runs.push(Run {
                text: std::mem::take(text),
                style: SpanStyle::Normal,
                href: None,
            });
        }
    };
    while i < b.len() {
        // Code span.
        if b[i] == b'`' {
            if let Some(end) = s[i + 1..].find('`') {
                push_text(&mut text, &mut runs);
                runs.push(Run {
                    text: s[i + 1..i + 1 + end].to_string(),
                    style: SpanStyle::Code,
                    href: None,
                });
                i += end + 2;
                continue;
            }
        }
        // Bold.
        if s[i..].starts_with("**") {
            if let Some(end) = s[i + 2..].find("**") {
                push_text(&mut text, &mut runs);
                runs.push(Run {
                    text: s[i + 2..i + 2 + end].to_string(),
                    style: SpanStyle::Strong,
                    href: None,
                });
                i += end + 4;
                continue;
            }
        }
        // Emphasis: shown as plain text without its markers.
        if let Some((inner, next)) = emphasis(s, i) {
            text.push_str(inner);
            i = next;
            continue;
        }
        // Link [label](url).
        if b[i] == b'[' {
            if let Some(close) = s[i..].find("](") {
                let after = i + close + 2;
                if let Some(paren) = s[after..].find(')') {
                    push_text(&mut text, &mut runs);
                    let label = &s[i + 1..i + close];
                    let url = &s[after..after + paren];
                    runs.push(Run {
                        text: label.to_string(),
                        style: SpanStyle::Link,
                        href: Some(url.to_string()),
                    });
                    i = after + paren + 1;
                    continue;
                }
            }
        }
        // Bare URL.
        if s[i..].starts_with("http://") || s[i..].starts_with("https://") {
            let end = s[i..]
                .find(|c: char| c.is_whitespace() || c == ')' || c == ']')
                .map(|e| i + e)
                .unwrap_or(s.len());
            push_text(&mut text, &mut runs);
            let url = &s[i..end];
            runs.push(Run {
                text: url.to_string(),
                style: SpanStyle::Link,
                href: Some(url.to_string()),
            });
            i = end;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        text.push(ch);
        i += ch.len_utf8();
    }
    push_text(&mut text, &mut runs);
    runs
}

/// A laid-out block ready to draw, with its vertical extent.
pub struct LaidBlock {
    /// The source block kind (for the marker and background).
    pub kind: BlockKind,
    /// The inline layout (empty for rules).
    pub layout: Layout,
    /// X offset of the text from the block's left edge.
    pub text_x: f32,
    /// Y offset of this block's top within the rich layout.
    pub y: f32,
    /// Total height including spacing below.
    pub height: f32,
    /// Hyperlink targets, one per link run, in order.
    pub links: Vec<String>,
    /// Table cells (empty for other blocks), positioned within the block.
    pub cells: Vec<TableCell>,
    /// Y offsets (within the block) of the lines under each table row but the last.
    pub rules: Vec<f32>,
}

/// One laid-out table cell.
pub struct TableCell {
    /// X offset within the block.
    pub x: f32,
    /// Y offset within the block.
    pub y: f32,
    /// The cell's text.
    pub layout: Layout,
}

/// Lightweight tag describing how to decorate a laid block.
#[derive(Clone, Debug)]
pub enum BlockKind {
    /// Paragraph.
    Paragraph,
    /// Bullet with a marker at `depth`.
    Bullet(u8),
    /// Numbered item.
    Numbered(u32),
    /// Code block.
    Code,
    /// Horizontal rule.
    Rule,
    /// Heading.
    Heading,
    /// Block quote (drawn with a bar at its left).
    Quote,
    /// Table (drawn from `cells` and `rules`).
    Table,
}

/// A fully laid-out rich-text body.
pub struct RichLayout {
    /// The blocks in order.
    pub blocks: Vec<LaidBlock>,
    /// Total height.
    pub height: f32,
    /// Body font size.
    pub size: f32,
}

impl RichLayout {
    /// The width the content actually needs within `width`: the widest line, or all of
    /// `width` when a block (code, rule, table) spans it.
    pub fn natural_width(&self, width: f32) -> f32 {
        let mut w: f32 = 0.0;
        for b in &self.blocks {
            match b.kind {
                BlockKind::Code | BlockKind::Rule | BlockKind::Table => return width,
                _ => w = w.max(b.text_x + b.layout.width),
            }
        }
        w.min(width)
    }

    /// Lays out agent rich text within `width` at the given body `size`.
    pub fn build(ts: &mut TextSystem, blocks: &[Block], width: f32, size: f32) -> RichLayout {
        let body = TextStyle::new(size).leading(1.34);
        let para_gap = size * 0.55;
        let list_gap = size * 0.3;
        let marker_w = size * 1.5;
        let mut y = 0.0;
        let mut out = Vec::new();

        for (idx, block) in blocks.iter().enumerate() {
            let first = idx == 0;
            match block {
                Block::Rule => {
                    if !first {
                        y += para_gap;
                    }
                    out.push(LaidBlock {
                        kind: BlockKind::Rule,
                        layout: Layout::default(),
                        text_x: 0.0,
                        y,
                        height: size,
                        links: Vec::new(),
                        cells: Vec::new(),
                        rules: Vec::new(),
                    });
                    y += size;
                }
                Block::Code(code) => {
                    if !first {
                        y += para_gap;
                    }
                    let pad = size * 0.5;
                    let style = TextStyle::new(size * 0.92).leading(1.3);
                    let spans = [Span {
                        text: code,
                        style: SpanStyle::Code,
                    }];
                    // Code blocks don't wrap; a line wider than the panel is clipped at its edge.
                    let mut layout = ts.layout(&spans, style, None);
                    // The block has one background; drop the per-run chips of inline code.
                    layout.decorations.clear();
                    let h = layout.height + pad * 2.0;
                    out.push(LaidBlock {
                        kind: BlockKind::Code,
                        layout,
                        text_x: pad,
                        y,
                        height: h,
                        links: Vec::new(),
                        cells: Vec::new(),
                        rules: Vec::new(),
                    });
                    y += h;
                }
                Block::Paragraph(runs)
                | Block::Bullet { runs, .. }
                | Block::Numbered { runs, .. } => {
                    let (text_x, kind, wrap) = match block {
                        Block::Bullet { depth, .. } => {
                            let x = marker_w + *depth as f32 * size;
                            (x, BlockKind::Bullet(*depth), width - x)
                        }
                        Block::Numbered { number, .. } => {
                            (marker_w, BlockKind::Numbered(*number), width - marker_w)
                        }
                        _ => (0.0, BlockKind::Paragraph, width),
                    };
                    if !first {
                        y += if matches!(kind, BlockKind::Paragraph) {
                            para_gap
                        } else {
                            list_gap
                        };
                    }
                    let spans: Vec<Span> = runs
                        .iter()
                        .map(|r| Span {
                            text: &r.text,
                            style: r.style,
                        })
                        .collect();
                    let links: Vec<String> = runs.iter().filter_map(|r| r.href.clone()).collect();
                    let layout = ts.layout(
                        &spans,
                        TextStyle {
                            align: Align::Left,
                            ..body
                        },
                        Some(wrap),
                    );
                    let h = layout.height.max(size * body.line_height);
                    out.push(LaidBlock {
                        kind,
                        layout,
                        text_x,
                        y,
                        height: h,
                        links,
                        cells: Vec::new(),
                        rules: Vec::new(),
                    });
                    y += h;
                }
                Block::Heading { level, runs } => {
                    if !first {
                        y += para_gap * 1.5;
                    }
                    let scale = match level {
                        1 => 1.3,
                        2 => 1.16,
                        _ => 1.06,
                    };
                    let style = TextStyle::new((size * scale).round()).leading(1.25);
                    let spans: Vec<Span> = runs
                        .iter()
                        .map(|r| Span {
                            text: &r.text,
                            style: r.style,
                        })
                        .collect();
                    let layout = ts.layout(&spans, style, Some(width));
                    let h = layout.height.max(style.size * style.line_height);
                    out.push(LaidBlock {
                        kind: BlockKind::Heading,
                        layout,
                        text_x: 0.0,
                        y,
                        height: h,
                        links: runs.iter().filter_map(|r| r.href.clone()).collect(),
                        cells: Vec::new(),
                        rules: Vec::new(),
                    });
                    y += h;
                }
                Block::Quote(runs) => {
                    if !first {
                        y += para_gap;
                    }
                    let text_x = size * 0.9;
                    let spans: Vec<Span> = runs
                        .iter()
                        .map(|r| Span {
                            text: &r.text,
                            style: r.style,
                        })
                        .collect();
                    let layout = ts.layout(&spans, body, Some(width - text_x));
                    let h = layout.height.max(size * body.line_height);
                    out.push(LaidBlock {
                        kind: BlockKind::Quote,
                        layout,
                        text_x,
                        y,
                        height: h,
                        links: runs.iter().filter_map(|r| r.href.clone()).collect(),
                        cells: Vec::new(),
                        rules: Vec::new(),
                    });
                    y += h;
                }
                Block::Table(rows) => {
                    if !first {
                        y += para_gap;
                    }
                    let cols = rows.first().map_or(0, Vec::len).max(1);
                    let cell_pad = size * 0.45;
                    let cell_style = TextStyle::new((size * 0.94).round()).leading(1.25);
                    let spans_of = |runs: &Vec<Run>| -> Vec<(String, SpanStyle)> {
                        runs.iter().map(|r| (r.text.clone(), r.style)).collect()
                    };
                    // Natural (unwrapped) width per column, then fit the columns to the panel.
                    let mut natural = vec![size * 1.5; cols];
                    for row in rows {
                        for (c, cell) in row.iter().enumerate() {
                            let owned = spans_of(cell);
                            let spans: Vec<Span> = owned
                                .iter()
                                .map(|(t, s)| Span { text: t, style: *s })
                                .collect();
                            let w = ts.layout(&spans, cell_style, None).width + cell_pad * 2.0;
                            natural[c] = natural[c].max(w);
                        }
                    }
                    let total: f32 = natural.iter().sum();
                    let widths: Vec<f32> = if total <= width {
                        natural
                    } else {
                        // Shrink in proportion, but keep every column wide enough for a word.
                        let min_w = (width / cols as f32 * 0.5).min(size * 4.0);
                        let scaled: Vec<f32> = natural
                            .iter()
                            .map(|w| (w * width / total).max(min_w))
                            .collect();
                        let st: f32 = scaled.iter().sum();
                        scaled.iter().map(|w| w * width / st).collect()
                    };
                    let mut cells = Vec::new();
                    let mut rules = Vec::new();
                    let mut ry = 0.0;
                    for (ri, row) in rows.iter().enumerate() {
                        let mut cx = 0.0;
                        let mut row_h: f32 = 0.0;
                        for (c, cell) in row.iter().enumerate() {
                            let owned = spans_of(cell);
                            let spans: Vec<Span> = owned
                                .iter()
                                .map(|(t, s)| Span { text: t, style: *s })
                                .collect();
                            let layout =
                                ts.layout(&spans, cell_style, Some(widths[c] - cell_pad * 2.0));
                            row_h = row_h.max(layout.height);
                            cells.push(TableCell {
                                x: cx + cell_pad,
                                y: ry + cell_pad * 0.6,
                                layout,
                            });
                            cx += widths[c];
                        }
                        ry += row_h.max(cell_style.size * cell_style.line_height) + cell_pad * 1.2;
                        if ri + 1 < rows.len() {
                            rules.push(ry);
                        }
                    }
                    out.push(LaidBlock {
                        kind: BlockKind::Table,
                        layout: Layout::default(),
                        // For a table this is its width (the frame hugs the columns).
                        text_x: widths.iter().sum(),
                        y,
                        height: ry,
                        links: Vec::new(),
                        cells,
                        rules,
                    });
                    y += ry;
                }
            }
        }
        RichLayout {
            blocks: out,
            height: y,
            size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_markdown() {
        let md = "Here is **bold** and `code` and a [link](https://x.com).\n\n- one\n- two\n\n1. first\n2. second";
        let b = parse(md);
        assert!(matches!(b[0], Block::Paragraph(_)));
        assert!(matches!(b[1], Block::Bullet { .. }));
        assert!(matches!(b[2], Block::Bullet { .. }));
        assert!(matches!(b[3], Block::Numbered { number: 1, .. }));
        if let Block::Paragraph(runs) = &b[0] {
            assert!(runs
                .iter()
                .any(|r| r.style == SpanStyle::Strong && r.text == "bold"));
            assert!(runs
                .iter()
                .any(|r| r.style == SpanStyle::Code && r.text == "code"));
            let link = runs.iter().find(|r| r.style == SpanStyle::Link).unwrap();
            assert_eq!(link.href.as_deref(), Some("https://x.com"));
        } else {
            panic!();
        }
    }

    #[test]
    fn headings_quotes_and_tables() {
        let md = "# Title\n\n> quoted **bit**\n> second line\n\n| Item | Qty |\n|---|:-:|\n| Paddle | 1 |\n| Bricks | 48 |\n\nafter";
        let b = parse(md);
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        if let Block::Quote(runs) = &b[1] {
            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(text, "quoted bit second line");
        } else {
            panic!("expected a quote, got {:?}", b[1]);
        }
        if let Block::Table(rows) = &b[2] {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].len(), 2);
            assert_eq!(
                rows[0][0][0].style,
                SpanStyle::Strong,
                "header cells are strong"
            );
            assert_eq!(rows[2][1][0].text, "48");
        } else {
            panic!("expected a table, got {:?}", b[2]);
        }
        assert!(matches!(&b[3], Block::Paragraph(_)));
        // A pipe line without a separator row is ordinary text.
        assert!(matches!(&parse("| not a table |")[0], Block::Paragraph(_)));
        // "#hashtag" is not a heading.
        assert!(matches!(&parse("#hashtag")[0], Block::Paragraph(_)));
    }

    #[test]
    fn emphasis_markers_are_dropped_but_names_are_kept() {
        let text = |s: &str| -> String { parse_inline(s).iter().map(|r| r.text.clone()).collect() };
        assert_eq!(text("an *italic* word"), "an italic word");
        assert_eq!(text("an _italic_ word"), "an italic word");
        assert_eq!(text("a_snake_case_name stays"), "a_snake_case_name stays");
        assert_eq!(text("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(text("**bold** stays bold"), "bold stays bold");
    }

    #[test]
    fn bundled_fonts_cover_code_and_emoji() {
        let mut ui = crate::Ui::new().expect("ui");
        assert!(ui.text.has_glyph('✅'));
        assert!(ui.text.has_glyph('👍'));
        // Monospace: 'i' and 'W' advance equally in code.
        let style = TextStyle::new(20.0);
        let w = |ui: &mut crate::Ui, t: &str| {
            ui.text
                .layout(
                    &[Span {
                        text: t,
                        style: SpanStyle::Code,
                    }],
                    style,
                    None,
                )
                .width
        };
        assert!((w(&mut ui, "iiii") - w(&mut ui, "WWWW")).abs() < 0.5);
    }

    #[test]
    fn parses_code_fence() {
        let md = "before\n```rust\nlet x = 1;\nlet y = 2;\n```\nafter";
        let b = parse(md);
        assert_eq!(b.len(), 3);
        assert_eq!(b[1], Block::Code("let x = 1;\nlet y = 2;".into()));
    }

    #[test]
    fn bare_url_becomes_link() {
        let runs = parse_inline("see https://muse.ai/x now");
        let link = runs.iter().find(|r| r.style == SpanStyle::Link).unwrap();
        assert_eq!(link.text, "https://muse.ai/x");
    }

    #[test]
    fn soft_wrapped_paragraph_joins() {
        let b = parse("line one\nline two");
        assert_eq!(b.len(), 1);
        if let Block::Paragraph(runs) = &b[0] {
            let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(joined, "line one line two");
        } else {
            panic!();
        }
    }

    #[test]
    fn rich_layout_has_positive_height() {
        let mut ts = crate::text::test_text();
        let blocks = parse("**Title**\n\n- alpha\n- beta\n\nA closing paragraph that is long enough to wrap across a couple of lines in the panel.");
        let rl = RichLayout::build(&mut ts, &blocks, 300.0, 22.0);
        assert!(rl.height > 40.0);
        assert_eq!(rl.blocks.len(), 4);
    }
}
