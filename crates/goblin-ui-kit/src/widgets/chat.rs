//! Chat transcript widgets: the user bubble, the agent reply panel (rich text with a streaming
//! cursor), and per-turn status.

use super::{Painter, TextRole};
use crate::geom::Rect;
use crate::icons::Icon;
use crate::text::rich::{self, BlockKind, RichLayout};
use crate::text::{Span, SpanStyle, TextStyle};

/// Delivery state of a chat turn, drawn differently from "done" per the honest-states principle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnStatus {
    /// Queued locally, not yet sent.
    Unsent,
    /// Sent, awaiting acknowledgement.
    Sending,
    /// Muse acknowledged and is working.
    Working,
    /// Reply streaming in.
    Replying,
    /// Complete.
    Done,
    /// Failed to send or was rejected.
    Failed,
}

impl TurnStatus {
    /// Icon and label for a status line.
    pub fn line(self) -> Option<(Icon, &'static str)> {
        match self {
            TurnStatus::Unsent => Some((Icon::Clock, "Unsent")),
            TurnStatus::Sending => Some((Icon::ArrowUp, "Sending…")),
            TurnStatus::Working => Some((Icon::Sparkles, "Working…")),
            TurnStatus::Replying => Some((Icon::Sparkles, "Replying…")),
            TurnStatus::Done => None,
            TurnStatus::Failed => Some((Icon::Alert, "Not delivered")),
        }
    }
}

/// A user message bubble.
pub struct Bubble<'t> {
    /// Message text (plain).
    pub text: &'t str,
    /// Delivery status.
    pub status: TurnStatus,
}

/// An agent reply panel (rich text, optionally streaming).
pub struct AgentPanel<'t> {
    /// The reply Markdown (subset).
    pub markdown: &'t str,
    /// Whether the reply is still streaming (draws a caret).
    pub streaming: bool,
}

impl Painter<'_> {
    /// Draws a right-aligned user bubble within `area` (full content width). Returns its height.
    pub fn user_bubble(&mut self, area: Rect, bubble: &Bubble) -> f32 {
        let m = self.theme.metrics;
        let pad = m.pad * 0.9;
        let max_bubble = area.w * 0.78;
        let inner = max_bubble - pad * 2.0;
        let style = TextStyle::new(m.body).leading(1.3);
        let spans = [Span::normal(bubble.text)];
        let l = self.ui.text.layout(&spans, style, Some(inner));
        let bw = (l.width + pad * 2.0).min(max_bubble);
        let bh = l.height + pad * 2.0;
        let rect = Rect::new(area.right() - bw, area.y, bw, bh);
        self.ui.last_bubble = rect;
        let fg = self.theme.on_user_bubble();
        self.canvas
            .fill_rrect(rect, m.radius_card, self.theme.user_bubble());
        self.ui.text.draw(
            self.canvas,
            &l,
            rect.x + pad,
            rect.y + pad + style.size * 0.0 + self.ascent(style.size) - style.size,
            &|_| fg,
        );
        // status line under the bubble (right aligned)
        if let Some((icon, text)) = bubble.status.line() {
            let col = if bubble.status == TurnStatus::Failed {
                self.theme.pal.danger
            } else {
                self.theme.pal.text_muted
            };
            let tw = self.measure(text, TextRole::Caption);
            let s = m.caption;
            let y = rect.bottom() + m.pad * 0.25;
            self.status_line(
                area.right() - tw - s - m.pad * 0.4,
                y,
                icon,
                text,
                col,
                tw + s + m.pad,
            );
            return bh + m.pad * 0.25 + m.caption * 1.3;
        }
        bh
    }

    fn ascent(&self, size: f32) -> f32 {
        self.ui.text.line_metrics(size).0
    }

    /// Draws the "Muse is working" bubble: three dots that brighten in turn (still with Reduce
    /// motion). Returns its height.
    pub fn typing_bubble(&mut self, area: Rect) -> f32 {
        let m = self.theme.metrics;
        let h = m.row * 0.8;
        let w = h * 1.9;
        let rect = Rect::new(area.x, area.y, w, h);
        self.canvas.fill_rrect(rect, h * 0.5, self.theme.pal.subtle);
        let r = h * 0.1;
        let step = if self.theme.reduce_motion {
            1
        } else {
            (self.now_ms / 320) % 3
        };
        for i in 0..3u64 {
            let cx = rect.cx() + (i as f32 - 1.0) * h * 0.32;
            let a = if i == step { 0.9 } else { 0.4 };
            self.canvas
                .fill_circle(cx, rect.cy(), r, self.theme.pal.text.fade(a));
        }
        h
    }

    /// Draws a left-aligned agent panel within `area`. Returns its height.
    pub fn agent_panel(&mut self, area: Rect, panel: &AgentPanel) -> f32 {
        let m = self.theme.metrics;
        let pad = m.pad;
        let max_panel = area.w * 0.9;
        let inner = max_panel - pad * 2.0;
        let blocks = rich::parse(panel.markdown);
        let rl = RichLayout::build(&mut self.ui.text, &blocks, inner, m.body);
        let ph = rl.height + pad * 2.0;
        // The bubble hugs its text, as in the Muse app; it never jumps narrower than a word.
        let pw = (rl.natural_width(inner).ceil() + pad * 2.0).clamp(m.row * 1.2, max_panel);
        let rect = Rect::new(area.x, area.y, pw, ph);
        self.ui.last_bubble = rect;
        self.canvas
            .fill_rrect(rect, m.radius_card, self.theme.pal.subtle);
        self.draw_rich(&rl, rect.x + pad, rect.y + pad, inner, panel.streaming);
        ph
    }

    /// Draws a rich layout (also used by the Feed and Library previews).
    pub fn draw_rich(&mut self, rl: &RichLayout, x: f32, y: f32, width: f32, streaming: bool) {
        let m = self.theme.metrics;
        let pal = self.theme.pal;
        let accent = pal.accent;
        let colorize = move |s: SpanStyle| match s {
            SpanStyle::Link => accent,
            SpanStyle::Muted => pal.text_muted,
            SpanStyle::Code => pal.text,
            _ => pal.text,
        };
        let asc = self.ascent(rl.size);
        let mut last_end = (x, y);
        for b in &rl.blocks {
            let by = y + b.y;
            match &b.kind {
                BlockKind::Rule => {
                    self.canvas.fill_rect(
                        Rect::new(x, by + rl.size * 0.4, width, m.hairline),
                        self.theme.pal.divider,
                    );
                }
                BlockKind::Code => {
                    let bg = Rect::new(x, by, width, b.height - rl.size * 0.2);
                    self.canvas
                        .fill_rrect(bg, m.radius_card * 0.5, self.theme.pal.subtle_strong);
                    let clip = self.canvas.push_clip(bg);
                    self.ui.text.draw(
                        self.canvas,
                        &b.layout,
                        x + b.text_x,
                        by + m.pad * 0.5 + asc - rl.size,
                        &colorize,
                    );
                    self.canvas.set_clip(clip);
                }
                BlockKind::Bullet(depth) => {
                    let bx = x + rl.size * (0.5 + *depth as f32);
                    self.canvas.fill_circle(
                        bx,
                        by + asc - rl.size * 0.28,
                        rl.size * 0.12,
                        self.theme.pal.text_muted,
                    );
                    self.ui.text.draw(
                        self.canvas,
                        &b.layout,
                        x + b.text_x,
                        by + asc - rl.size,
                        &colorize,
                    );
                    last_end = self.rich_line_end(&b.layout, x + b.text_x, by);
                }
                BlockKind::Numbered(n) => {
                    let label = format!("{n}.");
                    let style = TextStyle::new(rl.size);
                    let muted = self.theme.pal.text_muted;
                    let l = self.ui.text.line(&label, style, None);
                    self.ui.text.draw(
                        self.canvas,
                        &l,
                        x + b.text_x - l.width - rl.size * 0.2,
                        by + asc - rl.size,
                        &|_| muted,
                    );
                    self.ui.text.draw(
                        self.canvas,
                        &b.layout,
                        x + b.text_x,
                        by + asc - rl.size,
                        &colorize,
                    );
                    last_end = self.rich_line_end(&b.layout, x + b.text_x, by);
                }
                BlockKind::Quote => {
                    // A bar down the left edge marks quoted text.
                    self.canvas.fill_rrect(
                        Rect::new(x + rl.size * 0.2, by, rl.size * 0.16, b.height),
                        rl.size * 0.08,
                        self.theme.pal.text_muted.fade(0.6),
                    );
                    self.ui.text.draw(
                        self.canvas,
                        &b.layout,
                        x + b.text_x,
                        by + asc - rl.size,
                        &colorize,
                    );
                    last_end = self.rich_line_end(&b.layout, x + b.text_x, by);
                }
                BlockKind::Table => {
                    let tw = b.text_x.min(width);
                    let frame = Rect::new(x, by, tw, b.height);
                    self.canvas.stroke_rrect(
                        frame,
                        m.radius_card * 0.4,
                        m.hairline,
                        self.theme.pal.divider,
                    );
                    for ry in &b.rules {
                        self.canvas.fill_rect(
                            Rect::new(x, by + ry, tw, m.hairline),
                            self.theme.pal.divider,
                        );
                    }
                    let clip = self.canvas.push_clip(frame);
                    for cell in &b.cells {
                        self.ui.text.draw(
                            self.canvas,
                            &cell.layout,
                            x + cell.x,
                            by + cell.y,
                            &colorize,
                        );
                    }
                    self.canvas.set_clip(clip);
                    last_end = (x, by + b.height);
                }
                BlockKind::Heading | BlockKind::Paragraph => {
                    self.ui.text.draw(
                        self.canvas,
                        &b.layout,
                        x + b.text_x,
                        by + asc - rl.size,
                        &colorize,
                    );
                    last_end = self.rich_line_end(&b.layout, x + b.text_x, by);
                }
            }
        }
        if streaming {
            // Blinking caret after the last glyph.
            let on = (self.now_ms / 500) % 2 == 0;
            if on {
                let cw = (rl.size * 0.1).max(1.5);
                self.canvas.fill_rrect(
                    Rect::new(
                        last_end.0 + rl.size * 0.08,
                        last_end.1 + asc - rl.size * 0.95,
                        cw,
                        rl.size * 1.05,
                    ),
                    cw * 0.5,
                    self.theme.pal.accent,
                );
            }
        }
    }

    /// Computes the (x, y-top) just past the last glyph of a layout's final line.
    fn rich_line_end(&self, layout: &crate::text::Layout, x: f32, y_top: f32) -> (f32, f32) {
        match layout.lines.last() {
            Some(line) => {
                let top = y_top + (line.baseline - self.ascent(layout.size));
                (x + line.width, top)
            }
            None => (x, y_top),
        }
    }
}

impl<'a> Painter<'a> {
    /// A centred time label between groups of messages.
    pub fn time_divider(&mut self, area: Rect, label: &str) -> f32 {
        let m = self.theme.metrics;
        let h = m.caption * 1.9;
        let r = Rect::new(area.x, area.y, area.w, h);
        self.line_centered(label, TextRole::Caption, r, self.theme.pal.text_muted);
        h
    }

    /// An emoji reaction as a small pill on the lower right corner of the bubble drawn last,
    /// as the Muse app places it. Returns the height it adds below the bubble.
    pub fn reaction_pill(&mut self, emoji: &str) -> f32 {
        let m = self.theme.metrics;
        let bubble = self.ui.last_bubble;
        let h = m.caption * 1.7;
        let w = (self.measure(emoji, TextRole::Caption) + h * 0.8).max(h * 1.4);
        let x = (bubble.right() - w - m.pad * 0.5).max(bubble.x);
        let r = Rect::new(x, bubble.bottom() - h * 0.35, w, h);
        // A ring in the page colour separates it from the bubble it overlaps.
        self.canvas
            .fill_rrect(r.inset(-2.0, -2.0), h * 0.5 + 2.0, self.theme.pal.page);
        self.canvas
            .fill_rrect(r, h * 0.5, self.theme.pal.subtle_strong);
        self.line_centered(emoji, TextRole::Caption, r, self.theme.pal.text);
        h * 0.65
    }
}
