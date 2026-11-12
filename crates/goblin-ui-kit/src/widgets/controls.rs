//! Top bar, tab rail, hint bar with controller glyphs, list rows, settings rows and chips.

use super::{Painter, TextRole};
use crate::color::Color;
use crate::geom::Rect;
use crate::icons::Icon;
use crate::input::Button;
use crate::text::TextStyle;

/// A tab in the rail.
#[derive(Clone, Copy, Debug)]
pub struct TabItem {
    /// Its icon.
    pub icon: Icon,
    /// Its label.
    pub label: &'static str,
}

/// Link/connection state shown in the top bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Connected and healthy.
    Connected,
    /// Reconnecting.
    Reconnecting,
    /// Not paired / signed out.
    NotPaired,
    /// Error.
    Error,
}

impl LinkState {
    fn icon(self) -> Icon {
        match self {
            LinkState::Connected => Icon::Wifi,
            LinkState::Reconnecting => Icon::Refresh,
            LinkState::NotPaired => Icon::WifiOff,
            LinkState::Error => Icon::Alert,
        }
    }

    fn color(self, p: &Painter) -> Color {
        match self {
            LinkState::Connected => p.theme.pal.ok,
            LinkState::Reconnecting => p.theme.pal.warn,
            LinkState::NotPaired => p.theme.pal.text_muted,
            LinkState::Error => p.theme.pal.danger,
        }
    }
}

/// The call's controls, each named by the button that works it.
#[derive(Clone, Copy, Debug)]
pub struct CallControls {
    /// The microphone is muted.
    pub muted: bool,
    /// Muse is speaking, so it can be interrupted.
    pub speaking: bool,
}

/// The floating header: there is no title bar. Tabs (or a side chat's title) float top-left, the
/// status pill top-right, and on Chat the avatar sits centred at the top with the conversation
/// fading out beneath it.
pub struct Header<'t> {
    /// A side chat's title, shown as "‹ title" in place of the tabs (B returns to the main chat).
    pub back_title: Option<&'t str>,
    /// A screen's name, shown top-left on screens that are not tabs.
    pub title: Option<&'t str>,
    /// Tabs shown as a segmented control (empty on screens that are not tabs).
    pub tabs: &'t [TabItem],
    /// Selected tab.
    pub selected: usize,
    /// Avatar diameter in pixels; 0 draws none.
    pub avatar_px: f32,
    /// The agent's own avatar picture (the current frame); the drawn mascot when `None`.
    pub avatar: Option<&'t crate::Image>,
    /// The agent's name, shown in a pill overlapping the avatar's lower edge.
    pub name: &'t str,
    /// Short state shown in that pill instead of the name ("Working…", "Reconnecting");
    /// empty when all is well.
    pub state_caption: &'t str,
    /// A call is running: its controls are shown under the status pill.
    pub call: Option<CallControls>,
    /// Fade the content under the header band (screens whose content scrolls beneath it).
    pub fade: bool,
    /// Link state.
    pub link: LinkState,
    /// Battery 0..100.
    pub battery: u8,
    /// Whether the battery is charging.
    pub charging: bool,
    /// Clock text, e.g. "7:58".
    pub clock: &'t str,
}

impl Painter<'_> {
    /// Height of the row holding the floating controls.
    pub fn header_row(&self) -> f32 {
        self.theme.metrics.topbar
    }

    /// Height of the band the header occupies (controls row, or the avatar and its name pill
    /// when taller).
    pub fn header_band(&self, header: &Header) -> f32 {
        let m = self.theme.metrics;
        let mut h = self.header_row();
        if header.avatar_px > 0.0 {
            let lines = if header.state_caption.is_empty() {
                1.1
            } else {
                2.3
            };
            h = h.max(header.avatar_px + m.pad * 0.4 + m.caption * lines);
        }
        h
    }

    /// Draws the floating header at the top of `bounds`.
    pub fn header(&mut self, bounds: Rect, header: &Header) {
        let m = self.theme.metrics;
        let pad = m.pad;
        let row = self.header_row();
        let band = self.header_band(header);
        let page = self.theme.pal.page;
        if header.fade {
            // As in the Muse app, the conversation stays visible behind the header: a scrim that
            // is strongest at the top edge and gone by the bottom of the band.
            // Two-pixel steps, so the shade has no visible bands over a bubble.
            let steps = (((band + pad) / 2.0).ceil() as usize).max(8);
            let fh = (band + pad) / steps as f32;
            for k in 0..steps {
                let t = k as f32 / (steps - 1) as f32;
                // Nearly opaque behind the controls and the avatar, so passing text does not
                // tangle with them; it clears over the last third.
                let a = if t < 0.72 {
                    0.96
                } else {
                    0.96 * (1.0 - (t - 0.72) / 0.28)
                };
                // Whole-pixel strips that neither overlap nor leave gaps.
                let y0 = (bounds.y + k as f32 * fh).round();
                let y1 = (bounds.y + (k + 1) as f32 * fh).round();
                if y1 > y0 {
                    self.canvas
                        .fill_rect(Rect::new(bounds.x, y0, bounds.w, y1 - y0), page.fade(a));
                }
            }
        }
        let cy = bounds.y + row * 0.5;
        let h = row - pad * 0.7;

        // Right: status pill (link, battery, clock).
        let clock_w = self.measure(header.clock, TextRole::Caption);
        let batt_h = m.caption * 0.95;
        let batt_w = batt_h * 1.9;
        let ls = m.caption * 1.15;
        let pill_w = pad * 0.9 + ls + pad * 0.6 + batt_w + pad * 0.6 + clock_w + pad * 0.9;
        let pill = Rect::new(bounds.right() - pad * 0.6 - pill_w, cy - h * 0.5, pill_w, h);
        self.canvas
            .fill_rrect(pill, h * 0.5, self.theme.pal.surface);
        self.canvas
            .stroke_rrect(pill, h * 0.5, m.hairline, self.theme.pal.divider);
        let mut x = pill.x + pad * 0.9;
        self.icon(
            header.link.icon(),
            ls,
            x,
            cy - ls * 0.5,
            header.link.color(self),
        );
        x += ls + pad * 0.6;
        self.battery_glyph(
            Rect::new(x, cy - batt_h * 0.5, batt_w, batt_h),
            header.battery,
            header.charging,
        );
        x += batt_w + pad * 0.6;
        self.line(
            header.clock,
            TextRole::Caption,
            x,
            cy - m.caption * 0.65,
            clock_w + 2.0,
            self.theme.pal.text,
        );

        // A call's controls stack under the status pill: hang up, mute, and interrupt while
        // Muse is speaking. Each shows the button that works it.
        if let Some(call) = header.call {
            let rh = row * 0.92;
            let kh = rh * 0.6;
            let ic = m.body * 1.05;
            let mut rows: Vec<(Button, Icon, Color)> = vec![
                (Button::F2, Icon::PhoneOff, self.theme.pal.danger),
                (
                    Button::F1,
                    if call.muted { Icon::MicOff } else { Icon::Mic },
                    if call.muted {
                        self.theme.pal.warn
                    } else {
                        self.theme.pal.text
                    },
                ),
            ];
            if call.speaking {
                rows.push((Button::B, Icon::Volume, self.theme.pal.text));
            }
            let kw = self.keycap_width(Button::F2, kh);
            let sw = pad * 0.7 + kw + pad * 0.5 + ic + pad * 0.7;
            let stack = Rect::new(
                bounds.right() - pad * 0.6 - sw,
                pill.bottom() + pad * 0.5,
                sw,
                rh * rows.len() as f32 + pad * 0.3,
            );
            self.canvas
                .fill_rrect(stack, m.radius_card * 0.7, self.theme.pal.surface);
            self.canvas.stroke_rrect(
                stack,
                m.radius_card * 0.7,
                m.hairline,
                self.theme.pal.divider,
            );
            for (i, (button, icon, color)) in rows.iter().enumerate() {
                let cy = stack.y + pad * 0.15 + rh * (i as f32 + 0.5);
                let w = self.keycap_width(*button, kh);
                self.keycap(*button, stack.x + pad * 0.7 + (kw - w) * 0.5, cy, kh);
                self.icon(
                    *icon,
                    ic,
                    stack.right() - pad * 0.7 - ic,
                    cy - ic * 0.5,
                    *color,
                );
            }
        }

        // Left: a side chat's title, or the tabs flanked by L1 / R1.
        let left = bounds.x + pad * 0.6;
        if let Some(title) = header.back_title {
            let cs = m.caption * 1.1;
            let max_w = (bounds.w * 0.5 - header.avatar_px * 0.5 - pad * 3.0 - cs).max(pad * 4.0);
            let tw = self.measure(title, TextRole::Caption).min(max_w);
            let w = pad * 0.7 + cs + pad * 0.3 + tw + pad;
            let r = Rect::new(left, cy - h * 0.5, w, h);
            self.canvas.fill_rrect(r, h * 0.5, self.theme.pal.surface);
            self.canvas
                .stroke_rrect(r, h * 0.5, m.hairline, self.theme.pal.divider);
            self.icon(
                Icon::ChevronLeft,
                cs,
                r.x + pad * 0.7,
                cy - cs * 0.5,
                self.theme.pal.text_muted,
            );
            self.line(
                title,
                TextRole::Caption,
                r.x + pad + cs,
                cy - m.caption * 0.65,
                tw,
                self.theme.pal.text,
            );
        } else if let Some(title) = header.title {
            self.line(
                title,
                TextRole::Title,
                left + pad * 0.4,
                cy - m.title * 0.62,
                bounds.w * 0.6,
                self.theme.pal.text,
            );
        } else if !header.tabs.is_empty() {
            let isz = m.caption * 1.2;
            let labels_all = header.tabs.len() <= 3;
            let widths: Vec<f32> = header
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let label = labels_all || i == header.selected;
                    let lw = if label {
                        self.measure(t.label, TextRole::Caption) + pad * 0.4
                    } else {
                        0.0
                    };
                    isz + lw + pad * 1.3
                })
                .collect();
            let kh = h * 0.72;
            let kw = self.keycap_width(Button::R1, kh);
            let mut x = left;
            self.keycap(Button::L1, x, cy, kh);
            x += kw + pad * 0.4;
            let track = Rect::new(x, cy - h * 0.5, widths.iter().sum(), h);
            self.canvas
                .fill_rrect(track, h * 0.5, self.theme.pal.surface);
            self.canvas
                .stroke_rrect(track, h * 0.5, m.hairline, self.theme.pal.divider);
            for (i, t) in header.tabs.iter().enumerate() {
                let cell = Rect::new(x, track.y, widths[i], h);
                let active = i == header.selected;
                let label = labels_all || active;
                let color = if active {
                    self.theme.pal.on_accent
                } else {
                    self.theme.pal.text_muted
                };
                if active {
                    self.canvas.fill_rrect(cell, h * 0.5, self.theme.pal.accent);
                }
                let ix = cell.x + pad * 0.65;
                if active {
                    self.icon_filled(t.icon, isz, ix, cy - isz * 0.5, color);
                } else {
                    self.icon(t.icon, isz, ix, cy - isz * 0.5, color);
                }
                if label {
                    self.line(
                        t.label,
                        TextRole::Caption,
                        ix + isz + pad * 0.4,
                        cy - m.caption * 0.65,
                        cell.w,
                        color,
                    );
                }
                x += widths[i];
            }
            self.keycap(Button::R1, x + pad * 0.4, cy, kh);
        }

        // Centre: the avatar in a disc, with a pill over its lower edge holding the agent's name
        // (or its state when there is something to say).
        if header.avatar_px > 0.0 {
            let av = header.avatar_px;
            let av_rect = Rect::new(bounds.cx() - av * 0.5, bounds.y + pad * 0.3, av, av);
            self.canvas
                .fill_circle(av_rect.cx(), av_rect.cy(), av * 0.5, self.theme.pal.surface);
            match header.avatar {
                // The agent's own avatar fills the disc.
                Some(img) => self.canvas.draw_image(img, av_rect, av * 0.5, 1.0),
                None => {
                    let mascot = self.ui.mascot;
                    mascot.draw(
                        self.canvas,
                        self.theme,
                        av_rect.inset(av * 0.1, av * 0.1),
                        self.now_ms,
                    );
                }
            }
            // The pill holds the name; while there is something to say it gains a second line
            // (as the Muse app does: "is working", "Researching facts", "is responding").
            if !header.name.is_empty() {
                let two = !header.state_caption.is_empty();
                let nw = self.measure(header.name, TextRole::Caption);
                let sw = if two {
                    self.measure(header.state_caption, TextRole::Small)
                } else {
                    0.0
                };
                let lw = nw.max(sw).min(bounds.w * 0.42);
                let line = m.caption * 1.45;
                let ph = if two { line * 1.95 } else { m.caption * 1.7 };
                let pw = lw + pad * 1.6;
                let pill = Rect::new(
                    bounds.cx() - pw * 0.5,
                    av_rect.bottom() - m.caption * 0.75,
                    pw,
                    ph,
                );
                let radius = if two { m.caption * 0.9 } else { ph * 0.5 };
                self.canvas.fill_rrect(pill, radius, self.theme.pal.surface);
                self.canvas
                    .stroke_rrect(pill, radius, m.hairline, self.theme.pal.divider);
                if two {
                    let top = Rect::new(pill.x, pill.y + line * 0.08, pill.w, line);
                    let bottom = Rect::new(pill.x, pill.y + line * 0.9, pill.w, line);
                    self.line_centered(header.name, TextRole::Caption, top, self.theme.pal.text);
                    self.line_centered(
                        header.state_caption,
                        TextRole::Small,
                        bottom,
                        self.theme.pal.text_muted,
                    );
                } else {
                    self.line_centered(header.name, TextRole::Caption, pill, self.theme.pal.text);
                }
            }
        }
    }

    fn battery_glyph(&mut self, rect: Rect, pct: u8, charging: bool) {
        let m = self.theme.metrics;
        let body = Rect::new(rect.x, rect.y, rect.w - m.hairline * 3.0, rect.h);
        let col = if pct <= 12 {
            self.theme.pal.danger
        } else if pct <= 25 {
            self.theme.pal.warn
        } else {
            self.theme.pal.text_muted
        };
        self.canvas
            .stroke_rrect(body, m.small * 0.4, m.hairline.max(1.0), col);
        // nub
        let nub_h = rect.h * 0.4;
        self.canvas.fill_rrect(
            Rect::new(
                body.right() + m.hairline,
                rect.cy() - nub_h * 0.5,
                m.hairline * 2.5,
                nub_h,
            ),
            m.hairline,
            col,
        );
        let inner = body.inset(m.hairline * 1.5, m.hairline * 1.5);
        let fill_w = (inner.w * pct as f32 / 100.0).max(0.0);
        self.canvas.fill_rrect(
            Rect::new(inner.x, inner.y, fill_w, inner.h),
            m.small * 0.2,
            col,
        );
        if charging {
            self.icon_centered(Icon::BatteryCharging, rect.h, rect, self.theme.pal.ok);
        }
    }

    /// Draws the hint bar at the bottom: keycap + label pairs, most important first. Hints that
    /// do not fit are left out rather than squeezed.
    pub fn hint_bar(&mut self, rect: Rect, hints: &Hints) {
        let m = self.theme.metrics;
        self.canvas.fill_rect(rect, self.theme.pal.page);
        self.divider(rect.x, rect.right(), rect.y);
        let gap = m.pad * 1.2;
        let kh = rect.h * 0.66;
        let mut x = rect.x + m.pad;
        for hint in hints.items() {
            let kw = self.keycap_width(hint.button, kh);
            let lw = self.measure(hint.label, TextRole::Caption);
            if x + kw + m.pad * 0.4 + lw > rect.right() - m.pad * 0.5 {
                break;
            }
            self.keycap(hint.button, x, rect.cy(), kh);
            if hint.label.is_empty() {
                // A paired key (L2 before R2): the next keycap follows closely.
                x += kw + m.pad * 0.3;
                continue;
            }
            x += kw + m.pad * 0.4;
            self.line(
                hint.label,
                TextRole::Caption,
                x,
                rect.cy() - m.caption * 0.65,
                lw + 2.0,
                self.theme.pal.text_muted,
            );
            x += lw + gap;
        }
    }

    /// The text on a button's keycap.
    fn keycap_label(button: Button) -> &'static str {
        match button {
            Button::A => "A",
            Button::B => "B",
            Button::X => "X",
            Button::Y => "Y",
            Button::L1 => "L1",
            Button::L2 => "L2",
            Button::R1 => "R1",
            Button::R2 => "R2",
            Button::F1 => "F1",
            Button::F2 => "F2",
            Button::Start => "Start",
            Button::Select => "Select",
            Button::Menu => "Menu",
            Button::Up | Button::Down => "▲▼",
            Button::Left | Button::Right => "◀▶",
        }
    }

    /// Width of a keycap of height `h`: a circle for one letter, a rounded box otherwise.
    pub fn keycap_width(&mut self, button: Button, h: f32) -> f32 {
        let label = Self::keycap_label(button);
        if label.chars().count() <= 1 {
            return h;
        }
        let style = TextStyle::new(h * 0.6).strong();
        (self.ui.text.measure(label, style) + h * 0.55).max(h)
    }

    /// Draws a button keycap with its left edge at `x`, centred on `cy`; returns its width. Every
    /// button uses the same style (subtle fill, strong label).
    pub fn keycap(&mut self, button: Button, x: f32, cy: f32, h: f32) -> f32 {
        let label = Self::keycap_label(button);
        let w = self.keycap_width(button, h);
        let rect = Rect::new(x, cy - h * 0.5, w, h);
        let radius = if w <= h + 0.5 { h * 0.5 } else { h * 0.32 };
        // A clear cap in both modes: a tint of the text colour with a firmer outline.
        let ink = self.theme.pal.text;
        self.canvas.fill_rrect(rect, radius, ink.fade(0.10));
        self.canvas
            .stroke_rrect(rect, radius, self.theme.metrics.hairline, ink.fade(0.38));
        let style = TextStyle::new(h * 0.6).strong();
        let l = self.ui.text.line(label, style, None);
        // Centre on cap height (as `line_centered` does), so letters sit mid-cap.
        let baseline = cy + style.size * 0.34;
        let first = l.lines.first().map(|ln| ln.baseline).unwrap_or(0.0);
        self.ui.text.draw(
            self.canvas,
            &l,
            (rect.cx() - l.width * 0.5).round(),
            baseline - first,
            &|_| ink,
        );
        w
    }

    /// Draws a single controller button keycap centred in `rect`.
    pub fn glyph(&mut self, button: Button, rect: Rect) {
        let h = rect.w.min(rect.h);
        let w = self.keycap_width(button, h);
        self.keycap(button, rect.cx() - w * 0.5, rect.cy(), h);
    }

    /// Draws a chip (quick reply / filter). Returns its rect.
    pub fn chip(&mut self, rect: Rect, label: &str, selected: bool, focused: bool) -> Rect {
        let r = rect.h * 0.5;
        let (bg, fg) = if selected {
            (self.theme.pal.accent, self.theme.pal.on_accent)
        } else {
            (self.theme.pal.subtle, self.theme.pal.text)
        };
        self.canvas.fill_rrect(rect, r, bg);
        self.line_centered(label, TextRole::Caption, rect, fg);
        if focused {
            self.focus_ring(rect, r);
        }
        rect
    }

    /// Draws a list row with an optional leading icon, a title and optional subtitle, and an
    /// optional trailing chevron. Returns nothing; register focus separately.
    pub fn list_row(&mut self, rect: Rect, row: &ListRow, focused: bool) {
        let m = self.theme.metrics;
        if focused {
            self.canvas
                .fill_rrect(rect, m.radius_card * 0.7, self.theme.pal.subtle);
        }
        let pad = m.pad;
        let mut x = rect.x + pad;
        if let Some(icon) = row.icon {
            let s = m.body * 1.2;
            self.icon(
                icon,
                s,
                x,
                rect.cy() - s * 0.5,
                row.icon_color.unwrap_or(self.theme.pal.text_muted),
            );
            x += s + pad * 0.8;
        }
        let right_pad = if row.chevron { m.body * 1.6 } else { pad };
        let text_w = rect.right() - x - right_pad;
        if row.subtitle.is_empty() {
            self.line(
                row.title,
                TextRole::Body,
                x,
                rect.cy() - m.body * 0.65,
                text_w,
                self.theme.pal.text,
            );
        } else {
            self.line(
                row.title,
                TextRole::BodyStrong,
                x,
                rect.y + rect.h * 0.2,
                text_w,
                self.theme.pal.text,
            );
            self.line(
                row.subtitle,
                TextRole::Caption,
                x,
                rect.y + rect.h * 0.55,
                text_w,
                self.theme.pal.text_muted,
            );
        }
        if row.chevron {
            let s = m.body * 1.1;
            self.icon(
                Icon::ChevronRight,
                s,
                rect.right() - pad - s,
                rect.cy() - s * 0.5,
                self.theme.pal.text_muted,
            );
        }
        if focused {
            self.focus_ring(rect, m.radius_card * 0.7);
        }
    }

    /// Draws a settings row (section header, toggle, choice, value or action).
    pub fn setting_row(&mut self, rect: Rect, row: &SettingRow, focused: bool) {
        let m = self.theme.metrics;
        if row.kind == RowKind::Section {
            self.line(
                row.label,
                TextRole::Caption,
                rect.x,
                rect.cy() - m.caption * 0.6,
                rect.w,
                self.theme.pal.text_muted,
            );
            return;
        }
        if focused {
            self.canvas
                .fill_rrect(rect, m.radius_card * 0.7, self.theme.pal.subtle);
        }
        let pad = m.pad;
        let label_w = rect.w * 0.56;
        if row.detail.is_empty() {
            self.line(
                row.label,
                TextRole::Body,
                rect.x + pad,
                rect.cy() - m.body * 0.65,
                label_w,
                self.theme.pal.text,
            );
        } else {
            self.line(
                row.label,
                TextRole::Body,
                rect.x + pad,
                rect.y + rect.h * 0.2,
                label_w,
                self.theme.pal.text,
            );
            self.line(
                row.detail,
                TextRole::Caption,
                rect.x + pad,
                rect.y + rect.h * 0.56,
                rect.w - pad * 2.0,
                self.theme.pal.text_muted,
            );
        }
        match row.kind {
            RowKind::Toggle(on) => {
                let tw = m.row * 0.9;
                let th = m.row * 0.5;
                let track = Rect::new(rect.right() - pad - tw, rect.cy() - th * 0.5, tw, th);
                let bg = if on {
                    self.theme.pal.accent
                } else {
                    self.theme.pal.subtle_strong
                };
                self.canvas.fill_rrect(track, th * 0.5, bg);
                if !on {
                    // An off switch must still read as a switch in both modes.
                    self.canvas.stroke_rrect(
                        track,
                        th * 0.5,
                        m.hairline,
                        self.theme.pal.text.fade(0.38),
                    );
                }
                let kr = th * 0.42;
                let kx = if on {
                    track.right() - kr - (th * 0.5 - kr)
                } else {
                    track.x + kr + (th * 0.5 - kr)
                };
                self.canvas.fill_circle(kx, track.cy(), kr, Color::WHITE);
            }
            RowKind::Cycle => {
                // "‹ value ›": the arrows show while focused, so the row says how to change it.
                let cs = m.body;
                let w = self.measure(row.value, TextRole::BodyStrong);
                let right = rect.right() - pad;
                let vx = right - cs - pad * 0.3 - w;
                if focused {
                    self.icon(
                        Icon::ChevronRight,
                        cs,
                        right - cs,
                        rect.cy() - cs * 0.5,
                        self.theme.pal.accent,
                    );
                    self.icon(
                        Icon::ChevronLeft,
                        cs,
                        vx - cs - pad * 0.3,
                        rect.cy() - cs * 0.5,
                        self.theme.pal.accent,
                    );
                }
                self.line(
                    row.value,
                    TextRole::BodyStrong,
                    vx,
                    rect.cy() - m.body * 0.65,
                    w + 2.0,
                    self.theme.pal.text,
                );
            }
            RowKind::Value => {
                let w = self.measure(row.value, TextRole::Body);
                let vx = rect.right() - pad - w - (if row.chevron { m.body * 1.2 } else { 0.0 });
                self.line(
                    row.value,
                    TextRole::Body,
                    vx,
                    rect.cy() - m.body * 0.65,
                    w + 2.0,
                    self.theme.pal.text_muted,
                );
                if row.chevron {
                    let s = m.body;
                    self.icon(
                        Icon::ChevronRight,
                        s,
                        rect.right() - pad - s,
                        rect.cy() - s * 0.5,
                        self.theme.pal.text_muted,
                    );
                }
            }
            RowKind::Choice(sel) => {
                let s = m.body * 1.2;
                let cx = rect.right() - pad - s * 0.5;
                if sel {
                    self.canvas
                        .fill_circle(cx, rect.cy(), s * 0.5, self.theme.pal.accent);
                    self.icon_centered(
                        Icon::Check,
                        s * 0.8,
                        Rect::new(cx - s * 0.5, rect.cy() - s * 0.5, s, s),
                        self.theme.pal.on_accent,
                    );
                } else {
                    self.canvas.stroke_circle(
                        cx,
                        rect.cy(),
                        s * 0.5,
                        m.hairline.max(1.5),
                        self.theme.pal.text.fade(0.45),
                    );
                }
            }
            RowKind::Action => {
                let s = m.body;
                self.icon(
                    Icon::ChevronRight,
                    s,
                    rect.right() - pad - s,
                    rect.cy() - s * 0.5,
                    self.theme.pal.text_muted,
                );
            }
            RowKind::Section => {}
        }
        if focused && row.kind != RowKind::Section {
            self.focus_ring(rect, m.radius_card * 0.7);
        }
    }

    /// Draws a status line (icon + text) used under chat turns and elsewhere.
    pub fn status_line(
        &mut self,
        x: f32,
        y: f32,
        icon: Icon,
        text: &str,
        color: Color,
        max_w: f32,
    ) {
        let m = self.theme.metrics;
        let s = m.caption * 1.1;
        self.icon(icon, s, x, y, color);
        self.line(
            text,
            TextRole::Caption,
            x + s + m.pad * 0.4,
            y + s * 0.05,
            max_w - s,
            color,
        );
    }
}

/// A list row model.
#[derive(Clone, Copy)]
pub struct ListRow<'t> {
    /// Optional leading icon.
    pub icon: Option<Icon>,
    /// Optional icon tint.
    pub icon_color: Option<Color>,
    /// Title.
    pub title: &'t str,
    /// Optional subtitle.
    pub subtitle: &'t str,
    /// Whether to show a trailing chevron.
    pub chevron: bool,
}

impl<'t> ListRow<'t> {
    /// A simple titled row.
    pub fn new(title: &'t str) -> Self {
        ListRow {
            icon: None,
            icon_color: None,
            title,
            subtitle: "",
            chevron: false,
        }
    }
}

/// Kind of settings row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A section header (no interaction).
    Section,
    /// A toggle with its on/off state.
    Toggle(bool),
    /// A row with a value and optional chevron (opens a sub-screen).
    Value,
    /// A single-choice radio with selected state.
    Choice(bool),
    /// An action row (chevron only).
    Action,
    /// A row whose value steps through choices with left / right (or A).
    Cycle,
}

/// A settings row model.
#[derive(Clone, Copy)]
pub struct SettingRow<'t> {
    /// Row kind.
    pub kind: RowKind,
    /// Primary label.
    pub label: &'t str,
    /// Secondary detail text.
    pub detail: &'t str,
    /// Value text (for `Value` rows).
    pub value: &'t str,
    /// Whether to show a chevron on a `Value` row.
    pub chevron: bool,
}

impl<'t> SettingRow<'t> {
    /// A section header.
    pub fn section(label: &'t str) -> Self {
        SettingRow {
            kind: RowKind::Section,
            label,
            detail: "",
            value: "",
            chevron: false,
        }
    }

    /// A toggle row.
    pub fn toggle(label: &'t str, on: bool) -> Self {
        SettingRow {
            kind: RowKind::Toggle(on),
            label,
            detail: "",
            value: "",
            chevron: false,
        }
    }

    /// A value row that opens a sub-screen.
    pub fn value(label: &'t str, value: &'t str) -> Self {
        SettingRow {
            kind: RowKind::Value,
            label,
            detail: "",
            value,
            chevron: true,
        }
    }

    /// A row whose value steps through choices with left / right.
    pub fn cycle(label: &'t str, value: &'t str) -> Self {
        SettingRow {
            kind: RowKind::Cycle,
            label,
            detail: "",
            value,
            chevron: false,
        }
    }

    /// A single-choice row.
    pub fn choice(label: &'t str, selected: bool) -> Self {
        SettingRow {
            kind: RowKind::Choice(selected),
            label,
            detail: "",
            value: "",
            chevron: false,
        }
    }

    /// Adds detail text.
    pub fn with_detail(mut self, detail: &'t str) -> Self {
        self.detail = detail;
        self
    }
}

/// One hint in the hint bar.
#[derive(Clone, Copy)]
pub struct Hint {
    /// The button glyph.
    pub button: Button,
    /// The action label.
    pub label: &'static str,
}

/// A small ordered set of button hints.
#[derive(Clone, Default)]
pub struct Hints {
    items: Vec<Hint>,
}

impl Hints {
    /// An empty set.
    pub fn new() -> Self {
        Hints::default()
    }

    /// Adds a hint.
    pub fn add(mut self, button: Button, label: &'static str) -> Self {
        self.items.push(Hint { button, label });
        self
    }

    /// The hints in order.
    pub fn items(&self) -> &[Hint] {
        &self.items
    }
}

/// Marker type so screen code can name chips; chips are drawn via [`Painter::chip`].
pub struct Chip;

/// Alias for the hint-bar model used in screen code.
pub type HintBar = Hints;
