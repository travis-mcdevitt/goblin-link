//! Widgets: a [`Painter`] that bundles the canvas, the [`crate::Ui`] context and the active
//! [`Theme`], plus the drawing routines for every component in the plan's screen map — top bar,
//! tab rail, hint bar, lists, cards, settings rows, chat bubbles and the agent panel, chips,
//! the on-screen keyboard, overlays and toasts.
//!
//! Each routine draws immediately into the canvas; focusable widgets also register their
//! rectangle with [`crate::FocusSet`] and draw the focus ring when focused. The focus ring is a
//! thick accent outline plus a soft outer glow so it is unmistakable from arm's length.

mod chat;
mod controls;
mod keyboard;
mod overlay;

pub use chat::{AgentPanel, Bubble, TurnStatus};
pub use controls::{
    CallControls, Chip, Header, Hint, HintBar, Hints, LinkState, ListRow, RowKind, SettingRow,
    TabItem,
};
pub use keyboard::{KeyAction, Keyboard, KeyboardLayer, KeyboardState};
pub use overlay::{ConfirmDialog, Toast, ToastKind, VoiceOverlay};

use crate::canvas::Canvas;
use crate::color::Color;
use crate::focus::FocusId;
use crate::geom::{IRect, Rect};
use crate::icons::{Icon, IconStyle};
use crate::text::{Align, Span, TextStyle};
use crate::theme::Theme;
use crate::Ui;

/// Bundles the mutable canvas, the UI context and the current theme for a frame.
pub struct Painter<'a> {
    /// The target canvas.
    pub canvas: &'a mut Canvas,
    /// Fonts, icons, focus and mascot.
    pub ui: &'a mut Ui,
    /// The active theme.
    pub theme: &'a Theme,
    /// Animation clock in milliseconds.
    pub now_ms: u64,
}

/// A sprite key from whatever the sprite's look depends on.
pub fn sprite_key(what: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    what.hash(&mut h);
    h.finish()
}

/// Text role, mapping to a size and weight from the theme metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    /// Screen/section titles.
    Title,
    /// Body text.
    Body,
    /// Emphasised body.
    BodyStrong,
    /// Captions / secondary.
    Caption,
    /// Small labels (hints, timestamps).
    Small,
}

impl<'a> Painter<'a> {
    /// Creates a painter.
    pub fn new(canvas: &'a mut Canvas, ui: &'a mut Ui, theme: &'a Theme, now_ms: u64) -> Self {
        Painter {
            canvas,
            ui,
            theme,
            now_ms,
        }
    }

    /// The theme metrics.
    pub fn m(&self) -> crate::theme::Metrics {
        self.theme.metrics
    }

    /// The paragraph style for a role.
    pub fn style(&self, role: TextRole) -> TextStyle {
        let m = self.theme.metrics;
        match role {
            TextRole::Title => TextStyle::new(m.title).strong(),
            TextRole::Body => TextStyle::new(m.body),
            TextRole::BodyStrong => TextStyle::new(m.body).strong(),
            TextRole::Caption => TextStyle::new(m.caption),
            TextRole::Small => TextStyle::new(m.small),
        }
    }

    /// Fills the whole canvas with the page background.
    pub fn clear_page(&mut self) {
        if self.ui.sprites.enabled && !self.ui.sprites.capturing {
            // The GPU starts the frame from this colour; the frame canvas is left alone.
            self.ui.sprites.clear = Some(self.theme.pal.page);
            return;
        }
        self.canvas.clear(self.theme.pal.page);
    }

    /// Runs `draw` with nothing written: for what it measures and returns.
    pub fn muted<T>(&mut self, draw: impl FnOnce(&mut Painter) -> T) -> T {
        let was = self.canvas.set_muted(true);
        let out = draw(self);
        self.canvas.set_muted(was);
        out
    }

    /// Draws a piece of the screen that can be drawn once and then moved: `draw` is given
    /// the rectangle to draw in and must draw the same thing wherever that is, depending on
    /// nothing but what `key` was made from. It returns a number (a height, usually) that is
    /// handed back on later frames without drawing.
    ///
    /// With sprites off this is `draw(self, rect)`. With them on, the piece is drawn once
    /// into its own picture (see [`crate::sprites`]) and the frame records where it goes.
    /// `effects` says `draw` also does things that are not pixels and must happen every
    /// frame (registering focus targets): it is then run each frame with nothing written.
    pub fn sprite(
        &mut self,
        key: u64,
        rect: Rect,
        effects: bool,
        mut draw: impl FnMut(&mut Painter, Rect) -> f32,
    ) -> f32 {
        if !self.ui.sprites.enabled || self.ui.sprites.capturing {
            return draw(self, rect);
        }
        let key = key ^ self.ui.sprites.salt.rotate_left(17);
        // Whole pixels, so the copy the GPU places is the picture drawn in place.
        let area = rect.round_out();
        if area.w <= 0 || area.h <= 0 || area.w * area.h > crate::sprites::MAX_SPRITE_PIXELS {
            return draw(self, rect);
        }
        let value = match self.ui.sprites.known(key) {
            Some((value, _, _)) => value,
            None => {
                // Drawn at the scratch canvas's corner, the same distance from a pixel
                // boundary as in place, over black and then over white.
                let moved = Rect::new(
                    rect.x - area.x as f32,
                    rect.y - area.y as f32,
                    rect.w,
                    rect.h,
                );
                let mut scratch = self
                    .ui
                    .sprites
                    .scratch
                    .take()
                    .unwrap_or_else(|| Canvas::new(1, 1));
                let focus_mark = self.ui.focus.len();
                let ring = (self.ui.ring.target, self.ui.ring.clip);
                let bubble = self.ui.last_bubble;
                self.ui.sprites.capturing = true;
                scratch.reset(area.w, area.h, Color::BLACK);
                std::mem::swap(self.canvas, &mut scratch);
                let value = draw(self, moved);
                let black: Vec<u32> = self.canvas.pixels().to_vec();
                self.canvas.reset(area.w, area.h, Color::WHITE);
                draw(self, moved);
                std::mem::swap(self.canvas, &mut scratch);
                self.ui.sprites.capturing = false;
                // What the drawing registered was for the scratch canvas, not the screen.
                self.ui.focus.truncate(focus_mark);
                (self.ui.ring.target, self.ui.ring.clip) = ring;
                self.ui.last_bubble = bubble;
                let rgba = crate::sprites::alpha_from_two(&black, scratch.pixels());
                self.ui.sprites.scratch = Some(scratch);
                self.ui.sprites.fresh.push(crate::sprites::Pixels {
                    key,
                    w: area.w as u32,
                    h: area.h as u32,
                    rgba,
                });
                self.ui.sprites.remember(key, value, area.w, area.h);
                value
            }
        };
        self.ui.sprites.placed.push(crate::sprites::Placed {
            key,
            x: area.x,
            y: area.y,
            clip: self.canvas.clip(),
        });
        if effects {
            self.muted(|p| draw(p, rect))
        } else {
            value
        }
    }

    /// Draws a single line clipped to `max_w`, truncating with an ellipsis. `y` is the top.
    /// Returns the drawn width.
    pub fn line(
        &mut self,
        text: &str,
        role: TextRole,
        x: f32,
        y: f32,
        max_w: f32,
        color: Color,
    ) -> f32 {
        let style = self.style(role);
        let l = self.ui.text.line(text, style, Some(max_w));
        // `TextSystem::draw` takes the layout's top; its first baseline is already one ascent down.
        self.ui.text.draw(self.canvas, &l, x, y, &|_| color);
        l.width
    }

    /// Draws a centred single line within `rect` (vertically and horizontally). Returns width.
    /// Vertical centring targets cap height so glyphs sit optically centred in buttons and pills.
    pub fn line_centered(&mut self, text: &str, role: TextRole, rect: Rect, color: Color) -> f32 {
        let style = self.style(role);
        let l = self.ui.text.line(text, style, Some(rect.w));
        let x = rect.x + (rect.w - l.width) * 0.5;
        let baseline = rect.cy() + style.size * 0.34;
        let first = l.lines.first().map(|ln| ln.baseline).unwrap_or(0.0);
        self.ui
            .text
            .draw(self.canvas, &l, x, baseline - first, &|_| color);
        l.width
    }

    /// Fades the bottom (or top) edge of a scrolling area into the page, as the cue that there is
    /// more that way.
    pub fn edge_fade(&mut self, area: Rect, bottom: bool) {
        let m = self.theme.metrics;
        let page = self.theme.pal.page;
        let h = m.pad * 2.4;
        let steps = (h / 2.0).ceil().max(4.0) as usize;
        let sh = h / steps as f32;
        for k in 0..steps {
            // 0 at the inner edge of the fade, 1 at the area's edge.
            let t = (k as f32 + 1.0) / steps as f32;
            let (y0, y1) = if bottom {
                let top = area.bottom() - h;
                (
                    (top + k as f32 * sh).round(),
                    (top + (k + 1) as f32 * sh).round(),
                )
            } else {
                let kk = (steps - 1 - k) as f32;
                (
                    (area.y + kk * sh).round(),
                    (area.y + (kk + 1.0) * sh).round(),
                )
            };
            if y1 > y0 {
                self.canvas.fill_rect(
                    Rect::new(area.x, y0, area.w, y1 - y0),
                    page.fade(0.92 * t * t),
                );
            }
        }
    }

    /// Lays out wrapped multi-line text and draws it at `(x, y)` top-left. Returns the height.
    pub fn paragraph(
        &mut self,
        text: &str,
        role: TextRole,
        x: f32,
        y: f32,
        max_w: f32,
        color: Color,
    ) -> f32 {
        let style = self.style(role);
        let spans = [Span::normal(text)];
        let l = self.ui.text.layout(
            &spans,
            TextStyle {
                align: Align::Left,
                ..style
            },
            Some(max_w),
        );
        self.ui.text.draw(self.canvas, &l, x, y, &|_| color);
        l.height
    }

    /// The height [`Painter::paragraph`] would take, without drawing.
    pub fn paragraph_height(&mut self, text: &str, role: TextRole, max_w: f32) -> f32 {
        let style = self.style(role);
        let spans = [Span::normal(text)];
        self.ui
            .text
            .layout(
                &spans,
                TextStyle {
                    align: Align::Left,
                    ..style
                },
                Some(max_w),
            )
            .height
    }

    /// Lays out wrapped text centred in a column `max_w` wide whose left edge is `x`, and
    /// draws it from `y` down. Returns the height.
    pub fn paragraph_centered(
        &mut self,
        text: &str,
        role: TextRole,
        x: f32,
        y: f32,
        max_w: f32,
        color: Color,
    ) -> f32 {
        let style = self.style(role);
        let spans = [Span::normal(text)];
        let l = self.ui.text.layout(
            &spans,
            TextStyle {
                align: Align::Center,
                ..style
            },
            Some(max_w),
        );
        self.ui.text.draw(self.canvas, &l, x, y, &|_| color);
        l.height
    }

    /// Measures a plain line's width in the given role.
    pub fn measure(&mut self, text: &str, role: TextRole) -> f32 {
        let style = self.style(role);
        self.ui.text.measure(text, style)
    }

    /// Measures the height of a wrapped paragraph at `max_w` without drawing it.
    pub fn paragraph_measure(&mut self, text: &str, role: TextRole, max_w: f32) -> f32 {
        let style = self.style(role);
        let spans = [Span::normal(text)];
        self.ui
            .text
            .layout(
                &spans,
                TextStyle {
                    align: Align::Left,
                    ..style
                },
                Some(max_w),
            )
            .height
    }

    /// Draws an icon of `px` square at `(x, y)` tinted `color`.
    pub fn icon(&mut self, icon: Icon, px: f32, x: f32, y: f32, color: Color) {
        let size = px.round().max(4.0) as u16;
        let mask = self.ui.icons.get(icon, size, IconStyle::Outline);
        self.canvas
            .draw_mask(mask, x.round() as i32, y.round() as i32, color);
    }

    /// Draws a filled-variant icon (for active tabs).
    pub fn icon_filled(&mut self, icon: Icon, px: f32, x: f32, y: f32, color: Color) {
        let size = px.round().max(4.0) as u16;
        let mask = self.ui.icons.get(icon, size, IconStyle::Filled);
        self.canvas
            .draw_mask(mask, x.round() as i32, y.round() as i32, color);
    }

    /// Centres an icon within `rect`.
    pub fn icon_centered(&mut self, icon: Icon, px: f32, rect: Rect, color: Color) {
        let s = px.round().max(4.0);
        self.icon(icon, s, rect.cx() - s * 0.5, rect.cy() - s * 0.5, color);
    }

    /// Asks for the focus ring around `rect`. It is drawn once per frame by
    /// [`Painter::draw_focus_ring`], gliding there from the previously focused item.
    pub fn focus_ring(&mut self, rect: Rect, radius: f32) {
        self.ui.ring.target = Some((rect, radius));
        self.ui.ring.clip = Some(self.canvas.clip());
    }

    /// Draws the focus ring where its glide has got to; returns true while it is still moving
    /// (the caller should draw another frame).
    pub fn draw_focus_ring(&mut self) -> bool {
        let (shown, moving) = self.ui.ring.advance(self.now_ms, self.theme.reduce_motion);
        if let Some((rect, radius)) = shown {
            let m = self.theme.metrics;
            let accent = self.theme.pal.accent;
            // Cut at the top and bottom of the list the item is in; the ring's own width is
            // let through at the sides, where lists run to the screen edge.
            let restore = self.ui.ring.clip.map(|c| {
                let side = (m.focus * 2.0 + 2.0).ceil() as i32;
                self.canvas
                    .set_clip(IRect::new(c.x - side, c.y, c.w + side * 2, c.h))
            });
            // The ring with room for its glow. Its picture depends on its size and on where
            // it sits within a pixel, not on where on the screen it is.
            let room = m.focus * 2.0 + 3.0;
            let key = sprite_key(&(
                0x51u8,
                rect.w.to_bits(),
                rect.h.to_bits(),
                radius.to_bits(),
                rect.x.fract().to_bits(),
                rect.y.fract().to_bits(),
            ));
            self.sprite(key, rect.inflate(room), false, |p, outer| {
                let rect = outer.inflate(-room);
                // Soft outer glow for arm's-length visibility.
                p.canvas.stroke_rrect(
                    rect.inflate(m.focus + 2.0),
                    radius + m.focus + 2.0,
                    m.focus,
                    accent.fade(0.25),
                );
                p.canvas
                    .stroke_rrect(rect.inflate(m.focus), radius + m.focus, m.focus, accent);
                0.0
            });
            if let Some(c) = restore {
                self.canvas.set_clip(c);
            }
        }
        moving
    }

    /// A hairline divider across `[x0, x1]` at `y`.
    pub fn divider(&mut self, x0: f32, x1: f32, y: f32) {
        self.canvas.fill_rect(
            Rect::new(x0, y, x1 - x0, self.theme.metrics.hairline),
            self.theme.pal.divider,
        );
    }

    /// A surface card with the standard radius and a faint border.
    pub fn card(&mut self, rect: Rect) {
        let m = self.theme.metrics;
        self.canvas
            .fill_rrect(rect, m.radius_card, self.theme.pal.surface);
        self.canvas
            .stroke_rrect(rect, m.radius_card, m.hairline, self.theme.pal.divider);
    }

    /// Draws a pill-shaped button. Registers focus when `id` is given and draws the ring when
    /// `focused`. Returns the button rect for hit-testing.
    pub fn button(&mut self, rect: Rect, label: &str, kind: ButtonKind, focused: bool) -> Rect {
        let m = self.theme.metrics;
        let radius = rect.h * 0.5;
        let (bg, fg) = match kind {
            ButtonKind::Primary => (self.theme.pal.accent, self.theme.pal.on_accent),
            ButtonKind::Secondary => (self.theme.pal.subtle, self.theme.pal.text),
            ButtonKind::Ghost => (Color::TRANSPARENT, self.theme.pal.text),
            // A fixed deep red: the palette's dark-mode danger is a text colour, too light for
            // white labels.
            ButtonKind::Danger => (Color::rgb(0xC6, 0x28, 0x28), Color::WHITE),
        };
        if bg.a > 0 {
            self.canvas.fill_rrect(rect, radius, bg);
        }
        if kind == ButtonKind::Ghost {
            self.canvas
                .stroke_rrect(rect, radius, m.hairline, self.theme.pal.divider);
        }
        self.line_centered(label, TextRole::BodyStrong, rect, fg);
        if focused {
            self.focus_ring(rect, radius);
        }
        rect
    }

    /// A circular icon button on a subtle fill.
    pub fn icon_button(&mut self, rect: Rect, icon: Icon, focused: bool, active: bool) {
        let r = rect.w.min(rect.h) * 0.5;
        let bg = if active {
            self.theme.pal.accent
        } else {
            self.theme.pal.subtle
        };
        let fg = if active {
            self.theme.pal.on_accent
        } else {
            self.theme.pal.text
        };
        self.canvas.fill_circle(rect.cx(), rect.cy(), r, bg);
        self.icon_centered(icon, r * 1.1, rect, fg);
        if focused {
            self.focus_ring(Rect::new(rect.cx() - r, rect.cy() - r, r * 2.0, r * 2.0), r);
        }
    }

    /// Registers a focusable rect; call after drawing the widget so `scroll_into_view` sees it.
    pub fn focusable(&mut self, id: FocusId, rect: Rect, group: u8) {
        self.ui.focus.add(id, rect, group);
    }
}

/// Visual kind of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Accent-filled primary action.
    Primary,
    /// Subtle-filled secondary action.
    Secondary,
    /// Outlined, transparent.
    Ghost,
    /// Destructive.
    Danger,
}
