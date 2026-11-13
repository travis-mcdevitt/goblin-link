//! Overlays: the voice-note recording overlay with a level meter, a confirmation dialog, and a
//! toast/notification banner. Overlays dim the page behind them.

use super::{ButtonKind, Painter, TextRole};
use crate::color::Color;
use crate::geom::Rect;
use crate::icons::Icon;

/// Voice overlay model (voice note, dictation preview or the listening half of a call).
pub struct VoiceOverlay<'t> {
    /// Title, e.g. "Voice note" or "Dictation".
    pub title: &'t str,
    /// Elapsed time text, e.g. "0:03".
    pub timer: &'t str,
    /// Live mic level 0..1.
    pub level: f32,
    /// Optional transcript preview (dictation).
    pub transcript: &'t str,
    /// Hint shown at the bottom (e.g. "Release F1 to send · hold B to cancel").
    pub hint: &'t str,
}

/// A confirmation dialog model.
pub struct ConfirmDialog<'t> {
    /// Title.
    pub title: &'t str,
    /// Body message.
    pub message: &'t str,
    /// Confirm button label.
    pub confirm: &'t str,
    /// Cancel button label.
    pub cancel: &'t str,
    /// Whether the confirm action is destructive.
    pub danger: bool,
    /// Index of the focused button (0 = cancel, 1 = confirm).
    pub focus: u8,
}

/// Toast severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral info.
    Info,
    /// Success.
    Success,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// A toast/notification banner model.
pub struct Toast<'t> {
    /// Severity.
    pub kind: ToastKind,
    /// Title.
    pub title: &'t str,
    /// Optional body.
    pub body: &'t str,
}

impl Painter<'_> {
    /// Dims the whole screen behind an overlay.
    pub fn scrim(&mut self, bounds: Rect) {
        let a = if self.theme.mode == crate::theme::Mode::Dark {
            0.6
        } else {
            0.45
        };
        self.canvas.fill_rect(bounds, Color::BLACK.fade(a));
    }

    /// The voice-note recorder, drawn where the composer is: discard, timer, live level, and
    /// how to send. As in the Muse app, the composer becomes the recorder; there is no panel.
    pub fn recorder_bar(&mut self, rect: Rect, secs: u64, level: f32) {
        let m = self.theme.metrics;
        let pad = m.pad;
        self.canvas
            .fill_rrect(rect, rect.h * 0.5, self.theme.pal.subtle);
        self.canvas
            .stroke_rrect(rect, rect.h * 0.5, m.focus, self.theme.pal.danger.fade(0.8));
        let kh = rect.h * 0.56;
        let ic = m.body * 1.1;
        // Left: B discards.
        let mut x = rect.x + pad * 0.7;
        x += self.keycap(crate::input::Button::B, x, rect.cy(), kh) + pad * 0.4;
        self.icon(
            Icon::Delete,
            ic,
            x,
            rect.cy() - ic * 0.5,
            self.theme.pal.text_muted,
        );
        x += ic + pad * 0.8;
        // A red dot and the timer.
        let dot = m.caption * 0.5;
        self.canvas
            .fill_circle(x + dot * 0.5, rect.cy(), dot * 0.5, self.theme.pal.danger);
        x += dot + pad * 0.4;
        let timer = format!("{}:{:02}", secs / 60, secs % 60);
        let tw = self.measure("0:00", TextRole::BodyStrong);
        self.line(
            &timer,
            TextRole::BodyStrong,
            x,
            rect.cy() - m.body * 0.65,
            tw + 4.0,
            self.theme.pal.text,
        );
        x += tw + pad;
        // Right: release F1 to send.
        let label = "Release to send";
        let lw = self.measure(label, TextRole::Caption);
        let fw = self.keycap_width(crate::input::Button::F1, kh);
        let right = rect.right() - pad * 0.9;
        self.line(
            label,
            TextRole::Caption,
            right - lw,
            rect.cy() - m.caption * 0.65,
            lw + 2.0,
            self.theme.pal.text_muted,
        );
        self.keycap(
            crate::input::Button::F1,
            right - lw - pad * 0.4 - fw,
            rect.cy(),
            kh,
        );
        // The level fills what is left between them.
        let meter_right = right - lw - pad * 0.4 - fw - pad;
        if meter_right - x > pad * 3.0 {
            self.level_meter(
                Rect::new(x, rect.cy() - rect.h * 0.28, meter_right - x, rect.h * 0.56),
                level,
            );
        }
    }

    /// Draws a symmetric audio level meter.
    pub fn level_meter(&mut self, rect: Rect, level: f32) {
        let m = self.theme.metrics;
        let bars = 21usize;
        let gap = m.hairline * 2.0;
        let bw = (rect.w - gap * (bars as f32 - 1.0)) / bars as f32;
        let lvl = level.clamp(0.0, 1.0);
        for i in 0..bars {
            let d = (i as f32 - (bars as f32 - 1.0) * 0.5).abs() / ((bars as f32 - 1.0) * 0.5);
            // Center bars tallest; height modulated by level and a little shimmer.
            let shimmer = 1.0 + 0.15 * ((self.now_ms as f32 / 120.0) + i as f32).sin();
            let active = lvl * (1.0 - d * 0.7) * shimmer;
            let bh = (rect.h * (0.2 + active)).clamp(rect.h * 0.12, rect.h);
            let x = rect.x + i as f32 * (bw + gap);
            let br = Rect::new(x, rect.cy() - bh * 0.5, bw, bh);
            let on = active > 0.05;
            let col = if on {
                self.theme.pal.accent
            } else {
                self.theme.pal.subtle_strong
            };
            self.canvas.fill_rrect(br, bw * 0.5, col);
        }
    }

    /// Draws a confirmation dialog centred in `bounds`. Returns (cancel_rect, confirm_rect).
    pub fn confirm_dialog(&mut self, bounds: Rect, dlg: &ConfirmDialog) -> (Rect, Rect) {
        self.scrim(bounds);
        let m = self.theme.metrics;
        let w = (bounds.w * 0.64).min(m.grid * 46.0);
        // Measure message height.
        let inner = w - m.pad * 3.0;
        let msg_h = {
            let style = self.style(TextRole::Body);
            let spans = [crate::text::Span::normal(dlg.message)];
            self.ui.text.layout(&spans, style, Some(inner)).height
        };
        let h = m.grid * 6.0 + msg_h + m.row + m.pad * 2.0;
        let card = bounds.centered(w, h);
        self.canvas
            .fill_rrect(card, m.radius_card, self.theme.pal.surface);

        let pad = m.pad * 1.5;
        self.line(
            dlg.title,
            TextRole::Title,
            card.x + pad,
            card.y + pad,
            inner,
            self.theme.pal.text,
        );
        let my = card.y + pad + m.title * 1.6;
        self.paragraph(
            dlg.message,
            TextRole::Body,
            card.x + pad,
            my,
            inner,
            self.theme.pal.text_muted,
        );

        // Buttons row.
        let bh = m.row;
        let by = card.bottom() - pad - bh;
        let bw = (inner - m.pad) * 0.5;
        let cancel = Rect::new(card.x + pad, by, bw, bh);
        let confirm = Rect::new(cancel.right() + m.pad, by, bw, bh);
        self.button(cancel, dlg.cancel, ButtonKind::Secondary, dlg.focus == 0);
        let kind = if dlg.danger {
            ButtonKind::Danger
        } else {
            ButtonKind::Primary
        };
        self.button(confirm, dlg.confirm, kind, dlg.focus == 1);
        (cancel, confirm)
    }

    /// Draws a toast banner at the top of `bounds`. Returns its rect.
    pub fn toast(&mut self, bounds: Rect, toast: &Toast) -> Rect {
        let m = self.theme.metrics;
        let (accent, icon) = match toast.kind {
            ToastKind::Info => (self.theme.pal.accent, Icon::Info),
            ToastKind::Success => (self.theme.pal.ok, Icon::CircleCheck),
            ToastKind::Warning => (self.theme.pal.warn, Icon::Alert),
            ToastKind::Error => (self.theme.pal.danger, Icon::CircleX),
        };
        let w = (bounds.w * 0.9).min(m.grid * 60.0);
        let has_body = !toast.body.is_empty();
        let h = if has_body { m.grid * 8.0 } else { m.grid * 5.5 };
        // Below the floating header, so the tabs and the status pill stay visible.
        let rect = Rect::new(
            bounds.cx() - w * 0.5,
            bounds.y + m.topbar + m.pad * 0.4,
            w,
            h,
        );
        // soft shadow
        self.canvas.fill_rrect_feather(
            rect.translate(0.0, m.hairline * 2.0),
            m.radius_card,
            m.grid * 0.6,
            Color::BLACK.fade(0.18),
        );
        self.canvas
            .fill_rrect(rect, m.radius_card, self.theme.pal.surface);
        self.canvas
            .stroke_rrect(rect, m.radius_card, m.hairline, self.theme.pal.divider);
        // accent stripe
        self.canvas.fill_rrect(
            Rect::new(rect.x, rect.y + m.pad, m.focus * 1.5, rect.h - m.pad * 2.0),
            m.focus,
            accent,
        );

        let pad = m.pad * 1.3;
        let isz = m.body * 1.3;
        self.icon(icon, isz, rect.x + pad, rect.cy() - isz * 0.5, accent);
        let tx = rect.x + pad + isz + m.pad * 0.6;
        let tw = rect.right() - tx - pad;
        if has_body {
            self.line(
                toast.title,
                TextRole::BodyStrong,
                tx,
                rect.y + rect.h * 0.24,
                tw,
                self.theme.pal.text,
            );
            self.line(
                toast.body,
                TextRole::Caption,
                tx,
                rect.y + rect.h * 0.56,
                tw,
                self.theme.pal.text_muted,
            );
        } else {
            self.line(
                toast.title,
                TextRole::BodyStrong,
                tx,
                rect.cy() - m.body * 0.65,
                tw,
                self.theme.pal.text,
            );
        }
        rect
    }
}
