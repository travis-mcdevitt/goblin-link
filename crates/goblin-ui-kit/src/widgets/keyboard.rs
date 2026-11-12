//! A compact on-screen keyboard driven by the D-pad. The draft being typed stays visible in a
//! line above the keys. Every row has ten keys, so the grid wraps cleanly in both directions.
//! Space, backspace, shift, symbols and send are buttons, not keys:
//! X backspace · Y space · L1 shift · R1 symbols · Start send · B close.

use super::{Painter, TextRole};
use crate::geom::Rect;
use crate::input::Button;

/// Which character layer the keyboard shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardLayer {
    /// Lowercase letters (with a number row).
    Lower,
    /// Uppercase letters (with a number row).
    Upper,
    /// Symbols.
    Symbols,
}

/// A key's action when activated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Insert this character.
    Char(char),
    /// Backspace.
    Backspace,
    /// Space.
    Space,
    /// Submit / enter.
    Enter,
    /// Toggle to the next layer.
    Shift,
    /// Switch to symbols / back to letters.
    Symbols,
}

/// Rows and columns of every layer.
pub const ROWS: u8 = 4;
/// Keys per row.
pub const COLS: u8 = 10;

const LETTERS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl'", "zxcvbnm,.?"];
const SYMBOLS: [&str; 4] = ["1234567890", "@#$%&*()-+", "!\"':;/\\=_~", "<>[]{}^|`€"];

/// Keyboard state: the current layer and focused cell.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardState {
    /// Active layer.
    pub layer: KeyboardLayer,
    /// Focused row.
    pub row: u8,
    /// Focused column.
    pub col: u8,
}

impl Default for KeyboardState {
    /// Starts on "q": the first letter row, below the numbers.
    fn default() -> Self {
        KeyboardState {
            layer: KeyboardLayer::Lower,
            row: 1,
            col: 0,
        }
    }
}

impl KeyboardState {
    /// The characters of a row on the active layer.
    fn row_chars(&self, row: u8) -> Vec<char> {
        let rows = match self.layer {
            KeyboardLayer::Symbols => SYMBOLS,
            _ => LETTERS,
        };
        let upper = self.layer == KeyboardLayer::Upper;
        rows[(row % ROWS) as usize]
            .chars()
            .map(|c| if upper { c.to_ascii_uppercase() } else { c })
            .collect()
    }

    /// Moves focus one key; the grid wraps at every edge.
    pub fn navigate(&mut self, dir: Button) {
        match dir {
            Button::Up => self.row = (self.row + ROWS - 1) % ROWS,
            Button::Down => self.row = (self.row + 1) % ROWS,
            Button::Left => self.col = (self.col + COLS - 1) % COLS,
            Button::Right => self.col = (self.col + 1) % COLS,
            _ => {}
        }
    }

    /// Shift (L1): lower ↔ upper; from symbols, back to lower.
    pub fn toggle_case(&mut self) {
        self.layer = match self.layer {
            KeyboardLayer::Lower => KeyboardLayer::Upper,
            _ => KeyboardLayer::Lower,
        };
    }

    /// Symbols (R1): letters ↔ symbols.
    pub fn toggle_symbols(&mut self) {
        self.layer = match self.layer {
            KeyboardLayer::Symbols => KeyboardLayer::Lower,
            _ => KeyboardLayer::Symbols,
        };
    }

    /// The action of the currently focused key.
    pub fn action(&self) -> KeyAction {
        match self.row_chars(self.row).get(self.col as usize) {
            Some(&c) => KeyAction::Char(c),
            None => KeyAction::Space,
        }
    }
}

/// Renders the on-screen keyboard.
pub struct Keyboard;

impl Painter<'_> {
    /// Height the keyboard needs: the draft line plus four rows of keys.
    pub fn keyboard_height(&self) -> f32 {
        let m = self.theme.metrics;
        m.row * 0.95 + (m.row * 0.86) * ROWS as f32 + m.pad * 0.9
    }

    /// Draws the keyboard within `rect` (see [`Painter::keyboard_height`]): the draft with a
    /// caret, then the keys with the focused one ringed. Returns the rect used.
    pub fn keyboard(&mut self, rect: Rect, state: &KeyboardState, draft: &str) -> Rect {
        let m = self.theme.metrics;
        self.canvas.fill_rect(rect, self.theme.pal.surface);
        self.divider(rect.x, rect.right(), rect.y);
        let pad = m.pad * 0.5;
        let gap = (m.pad * 0.3).round();

        // Draft line: what has been typed, its end always in view, with a caret.
        let draft_h = m.row * 0.95;
        let field = Rect::new(
            rect.x + pad,
            rect.y + pad * 0.7,
            rect.w - pad * 2.0,
            draft_h - pad * 0.7,
        );
        self.canvas
            .fill_rrect(field, field.h * 0.5, self.theme.pal.subtle);
        let inner = field.inset(m.pad, 0.0);
        let clip = self.canvas.push_clip(inner);
        let text_y = field.cy() - m.body * 0.65;
        let caret_h = m.body * 1.1;
        if draft.is_empty() {
            self.line(
                "Message",
                TextRole::Body,
                inner.x + m.hairline * 3.0,
                text_y,
                inner.w,
                self.theme.pal.text_muted,
            );
            self.canvas.fill_rect(
                Rect::new(
                    inner.x,
                    field.cy() - caret_h * 0.5,
                    m.hairline * 2.0,
                    caret_h,
                ),
                self.theme.pal.accent,
            );
        } else {
            // Right-align once the text is wider than the field, so the newest characters show.
            let w = self.measure(draft, TextRole::Body);
            let room = inner.w - m.hairline * 4.0;
            let x = if w > room {
                inner.x + room - w
            } else {
                inner.x
            };
            // Draw without truncation (the clip does the cutting on the left).
            self.line(
                draft,
                TextRole::Body,
                x,
                text_y,
                w + 4.0,
                self.theme.pal.text,
            );
            // A trailing space has no glyph to measure; nudge the caret for it.
            let trailing = draft.len() - draft.trim_end_matches(' ').len();
            let cx = x + w + trailing as f32 * m.body * 0.28 + m.hairline;
            self.canvas.fill_rect(
                Rect::new(cx, field.cy() - caret_h * 0.5, m.hairline * 2.0, caret_h),
                self.theme.pal.accent,
            );
        }
        self.canvas.set_clip(clip);

        // Keys.
        let area = Rect::new(
            rect.x + pad,
            rect.y + draft_h + pad * 0.4,
            rect.w - pad * 2.0,
            rect.h - draft_h - pad * 1.2,
        );
        let row_h = (area.h - gap * (ROWS as f32 - 1.0)) / ROWS as f32;
        let kw = (area.w - gap * (COLS as f32 - 1.0)) / COLS as f32;
        for r in 0..ROWS {
            let ry = area.y + r as f32 * (row_h + gap);
            for (c, ch) in state.row_chars(r).iter().enumerate() {
                let kr = Rect::new(area.x + c as f32 * (kw + gap), ry, kw, row_h);
                let focused = state.row == r && state.col == c as u8;
                self.key(kr, &ch.to_string(), focused, r == 0);
            }
        }
        rect
    }

    fn key(&mut self, rect: Rect, label: &str, focused: bool, quiet: bool) {
        let m = self.theme.metrics;
        let r = m.radius_card * 0.4;
        let ink = self.theme.pal.text;
        // A tint of the text colour, so keys stand out from the panel in both modes.
        self.canvas
            .fill_rrect(rect, r, ink.fade(if quiet { 0.06 } else { 0.11 }));
        self.line_centered(label, TextRole::Body, rect, ink);
        if focused {
            self.focus_ring(rect, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_on_q_and_wraps_in_both_directions() {
        let mut s = KeyboardState::default();
        assert_eq!(s.action(), KeyAction::Char('q'));
        s.navigate(Button::Left);
        assert_eq!(s.action(), KeyAction::Char('p'), "left from q wraps to p");
        s.navigate(Button::Right);
        s.navigate(Button::Up);
        assert_eq!(s.action(), KeyAction::Char('1'), "numbers are one row up");
        s.navigate(Button::Up);
        assert_eq!(
            s.action(),
            KeyAction::Char('z'),
            "up from the top wraps to the bottom"
        );
    }

    #[test]
    fn every_row_has_ten_keys_on_every_layer() {
        for layer in [
            KeyboardLayer::Lower,
            KeyboardLayer::Upper,
            KeyboardLayer::Symbols,
        ] {
            let s = KeyboardState {
                layer,
                ..Default::default()
            };
            for r in 0..ROWS {
                assert_eq!(s.row_chars(r).len(), COLS as usize, "{layer:?} row {r}");
            }
        }
    }

    #[test]
    fn case_and_symbol_layers() {
        let mut s = KeyboardState::default();
        s.toggle_case();
        assert_eq!(s.action(), KeyAction::Char('Q'));
        s.toggle_symbols();
        assert_eq!(s.action(), KeyAction::Char('@'));
        s.toggle_symbols();
        assert_eq!(s.action(), KeyAction::Char('q'));
    }
}
