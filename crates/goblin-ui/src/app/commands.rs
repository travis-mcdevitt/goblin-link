//! The command sheet of a chat with a Hermes (Y): the commands Hermes said it offers, in two
//! groups, each with Hermes's own description. Before it has said, and with an older plugin
//! that never does, the sheet is a few commands of our own.

use goblin_ipc::types::{HermesCommand, HermesCommandGroup};
use goblin_ui_kit::widgets::{Painter, TextRole};
use goblin_ui_kit::Rect;

/// What the sheet offers before Hermes has said what it has, under our own labels: the ones
/// that only show something first, so that Y pressed and then A by mistake changes nothing;
/// the one that starts over last. With Hermes's list, these head it as "Common".
pub(super) const COMMON: &[(&str, &str)] = &[
    ("Status", "status"),
    ("Choose a model", "model"),
    ("All commands", "help"),
    ("Try that again", "retry"),
    ("Take back my last message", "undo"),
    ("Shorten what it remembers", "compress"),
    ("New session", "new"),
];

/// The most rows the sheet shows at once.
const MOST_ROWS: usize = 10;

/// The sheet as the owner has left it, while it is open.
#[derive(Default)]
pub(super) struct Sheet {
    /// A reply was under way when the sheet opened: "Stop" is its first row.
    pub stop: bool,
    /// The skills are shown, not the commands.
    pub skills: bool,
    /// What the list is narrowed to, as typed.
    pub filter: String,
    /// The keyboard is up, typing the filter.
    pub typing: bool,
    /// The row chosen, and the first row drawn.
    pub focus: usize,
    top: usize,
    /// How many rows the last frame showed: a page, for L2 and R2.
    pub page: usize,
}

/// One row of the sheet.
pub(super) enum Row {
    Stop,
    /// Hermes's heading over the commands that follow.
    Heading(String),
    Command(Command),
}

/// A command as the sheet offers it.
pub(super) struct Command {
    pub label: String,
    /// What is sent: the command with its slash.
    pub text: String,
    pub description: String,
    pub needs_text: bool,
}

impl From<&HermesCommand> for Command {
    fn from(c: &HermesCommand) -> Self {
        Command {
            label: format!("/{}", c.name),
            text: format!("/{}", c.name),
            description: c.description.clone(),
            needs_text: c.needs_text,
        }
    }
}

impl Sheet {
    /// The sheet as Y opens it, on its first row.
    pub fn open(stop: bool, list: &[HermesCommand]) -> Self {
        let mut sheet = Sheet {
            stop,
            page: 6,
            ..Sheet::default()
        };
        sheet.restart(&sheet.rows(list).0);
        sheet
    }

    /// The rows for `list`, the commands Hermes offers, and whether they are Hermes's: with
    /// none, they are our own few.
    pub fn rows(&self, list: &[HermesCommand]) -> (Vec<Row>, bool) {
        let mut rows = Vec::new();
        if self.stop {
            rows.push(Row::Stop);
        }
        // Approvals are answered on their own cards, which show what is approved, never from
        // here: whatever a list says.
        let list: Vec<&HermesCommand> = list
            .iter()
            .filter(|c| c.name != "approve" && c.name != "deny")
            .collect();
        if list.is_empty() {
            rows.extend(COMMON.iter().map(|(label, name)| {
                Row::Command(Command {
                    label: (*label).to_owned(),
                    text: format!("/{name}"),
                    description: String::new(),
                    needs_text: false,
                })
            }));
            return (rows, false);
        }
        let group = if self.skills {
            HermesCommandGroup::Skills
        } else {
            HermesCommandGroup::Commands
        };
        let needle = self.filter.trim().trim_start_matches('/').to_lowercase();
        let shown: Vec<&HermesCommand> = list
            .into_iter()
            .filter(|c| {
                c.group == group
                    && (c.name.contains(&needle) || c.description.to_lowercase().contains(&needle))
            })
            .collect();
        // Our few first, under "Common", and then not again under Hermes's headings. While
        // finding, under Hermes's headings alone.
        let mut shown = shown;
        if group == HermesCommandGroup::Commands && needle.is_empty() {
            let common: Vec<&HermesCommand> = COMMON
                .iter()
                .filter_map(|(_, name)| shown.iter().copied().find(|c| c.name == *name))
                .collect();
            if !common.is_empty() {
                rows.push(Row::Heading("Common".into()));
                rows.extend(common.iter().map(|c| Row::Command(Command::from(*c))));
                shown.retain(|c| !common.iter().any(|k| k.name == c.name));
            }
        }
        // Hermes's headings, in the order it gave its commands.
        let mut headings: Vec<&str> = Vec::new();
        for c in &shown {
            if !headings.contains(&c.category.as_str()) {
                headings.push(&c.category);
            }
        }
        for heading in headings {
            if !heading.is_empty() {
                rows.push(Row::Heading(heading.to_owned()));
            }
            let under = shown.iter().filter(|c| c.category == heading);
            rows.extend(under.map(|c| Row::Command(Command::from(*c))));
        }
        (rows, true)
    }

    /// Back to the first row, as what is listed has changed.
    pub fn restart(&mut self, rows: &[Row]) {
        self.focus = first(rows);
        self.top = 0;
    }

    /// Keeps the choice on a row that can be chosen, as the list may have changed under it.
    pub fn settle(&mut self, rows: &[Row]) {
        if !matches!(rows.get(self.focus), Some(Row::Stop | Row::Command(_))) {
            self.restart(rows);
        }
    }

    /// Moves the choice `by` rows down (up when negative), past headings.
    pub fn step(&mut self, rows: &[Row], by: isize) {
        if rows.is_empty() {
            return;
        }
        let at = self
            .focus
            .saturating_add_signed(by)
            .min(rows.len().saturating_sub(1));
        let chosen = |i: &usize| !matches!(rows[*i], Row::Heading(_));
        let down = (at..rows.len()).find(chosen);
        let up = (0..=at).rev().find(chosen);
        let to = if by > 0 { down.or(up) } else { up.or(down) };
        self.focus = to.unwrap_or(self.focus);
    }
}

/// The first row that can be chosen.
fn first(rows: &[Row]) -> usize {
    rows.iter()
        .position(|r| !matches!(r, Row::Heading(_)))
        .unwrap_or(0)
}

/// Draws the sheet: the two groups along the top, the rows, and the chosen command's
/// description under them. While the filter is typed the keyboard takes the bottom, and the
/// sheet keeps to what is left without the description.
pub(super) fn draw(p: &mut Painter, bounds: Rect, sheet: &mut Sheet, rows: &[Row]) {
    let m = p.m();
    let pal = p.theme.pal;
    let area = if sheet.typing {
        let kb = p.keyboard_height() + m.hintbar;
        Rect::new(bounds.x, bounds.y, bounds.w, bounds.h - kb)
    } else {
        bounds
    };
    p.scrim(area);
    let pad = m.pad * 1.5;
    let w = (bounds.w * 0.7).min(m.grid * 50.0);
    let rowh = (m.row * 0.86).round();
    let tabh = (m.row * 0.8).round();
    let line = p.paragraph_height("Hg", TextRole::Caption, w);
    // Room for a description is kept, and the card takes it only when there is one to show:
    // its top stays where it is.
    let about = pad * 0.5 + line * 3.0;
    let said = match rows.get(sheet.focus) {
        Some(Row::Command(c)) if !sheet.typing => c.description.as_str(),
        _ => "",
    };
    let fixed = pad * 2.0 + tabh + pad * 0.5;
    let kept = if sheet.typing { 0.0 } else { about };
    let fit = ((area.h - m.pad * 2.0 - fixed - kept) / rowh)
        .floor()
        .max(1.0) as usize;
    let n = fit.min(MOST_ROWS);
    sheet.page = n;
    let top = area.centered(w, fixed + kept + rowh * n as f32).y;
    let shown_about = if said.is_empty() { 0.0 } else { about };
    let card = Rect::new(
        area.cx() - w * 0.5,
        top,
        w,
        fixed + shown_about + rowh * n as f32,
    );
    p.canvas.fill_rrect(card, m.radius_card, pal.surface);

    // The groups, and what they are narrowed to.
    let mut x = card.x + pad;
    for (label, on) in [("Commands", !sheet.skills), ("Skills", sheet.skills)] {
        let cw = p.measure(label, TextRole::Body) + m.pad * 2.0;
        p.chip(Rect::new(x, card.y + pad, cw, tabh), label, on, false);
        x += cw + m.pad * 0.5;
    }
    if !sheet.filter.is_empty() {
        let shown = format!("\u{201c}{}\u{201d}", sheet.filter);
        let y = card.y + pad + tabh * 0.5 - m.body * 0.65;
        let room = card.right() - pad - x - m.pad * 0.5;
        p.line(
            &shown,
            TextRole::Body,
            x + m.pad * 0.5,
            y,
            room,
            pal.text_muted,
        );
    }

    // The rows, scrolled to keep the choice in view with the heading above it.
    if sheet.focus < sheet.top {
        sheet.top = sheet.focus;
    }
    if sheet.focus >= sheet.top + n {
        sheet.top = sheet.focus + 1 - n;
    }
    if n > 1
        && sheet.top == sheet.focus
        && matches!(rows.get(sheet.focus.wrapping_sub(1)), Some(Row::Heading(_)))
    {
        sheet.top -= 1;
    }
    sheet.top = sheet.top.min(rows.len().saturating_sub(n));
    let list = Rect::new(
        card.x + pad * 0.5,
        card.y + pad + tabh + pad * 0.5,
        w - pad,
        rowh * n as f32,
    );
    let text_y = |r: Rect| r.cy() - m.body * 0.65;
    for (i, row) in rows.iter().enumerate().skip(sheet.top).take(n) {
        let r = Rect::new(list.x, list.y + rowh * (i - sheet.top) as f32, list.w, rowh);
        let (label, color) = match row {
            // A heading with none of its commands in view yet waits for them.
            Row::Heading(_) if i + 1 == sheet.top + n => continue,
            Row::Heading(h) => {
                let x = r.x + pad * 0.5;
                p.line(
                    h,
                    TextRole::BodyStrong,
                    x,
                    text_y(r),
                    r.w - pad,
                    pal.text_muted,
                );
                continue;
            }
            Row::Stop => ("Stop", pal.text),
            Row::Command(c) => (c.label.as_str(), pal.text),
        };
        if i == sheet.focus {
            p.canvas
                .fill_rrect(r, m.radius_card * 0.6, pal.subtle_strong);
        }
        p.line(
            label,
            TextRole::Body,
            r.x + pad * 0.5,
            text_y(r),
            r.w - pad,
            color,
        );
    }
    if !rows.iter().any(|r| matches!(r, Row::Command(_))) {
        let at = usize::from(sheet.stop) as f32;
        let r = Rect::new(list.x, list.y + rowh * at, list.w, rowh);
        let none = if sheet.filter.is_empty() {
            "None"
        } else {
            "Nothing found"
        };
        p.line(
            none,
            TextRole::Body,
            r.x + pad * 0.5,
            text_y(r),
            r.w - pad,
            pal.text_muted,
        );
    }
    // Where the view is in a list longer than it.
    if rows.len() > n {
        let bar = (m.pad * 0.3).round().max(2.0);
        let thumb = (list.h * n as f32 / rows.len() as f32).max(bar * 2.0);
        let y = list.y + (list.h - thumb) * sheet.top as f32 / (rows.len() - n) as f32;
        let track = Rect::new(
            card.right() - pad * 0.5 + (pad * 0.5 - bar) * 0.5,
            y,
            bar,
            thumb,
        );
        p.canvas.fill_rrect(track, bar * 0.5, pal.subtle_strong);
    }

    // What the chosen command does, in Hermes's words.
    if said.is_empty() {
        return;
    }
    let y = list.bottom() + pad * 0.5;
    p.divider(card.x + pad, card.right() - pad, y - pad * 0.25);
    let tw = w - pad * 2.0;
    let said = at_most(p, said, tw, 3);
    p.paragraph(
        &said,
        TextRole::Caption,
        card.x + pad,
        y,
        tw,
        pal.text_muted,
    );
}

/// `text` as far as it fits in `lines` lines of `w`, with an ellipsis where it is cut.
fn at_most(p: &mut Painter, text: &str, w: f32, lines: usize) -> String {
    let most = p.paragraph_height("Hg", TextRole::Caption, w) * lines as f32 + 0.5;
    let fits = |p: &mut Painter, s: &str| p.paragraph_height(s, TextRole::Caption, w) <= most;
    if fits(p, text) {
        return text.to_owned();
    }
    let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let cut = |k: usize| format!("{}\u{2026}", text[..ends[k]].trim_end());
    // The most characters that fit, found by halving.
    let (mut lo, mut hi) = (0, ends.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(p, &cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, group: HermesCommandGroup) -> HermesCommand {
        HermesCommand {
            name: name.into(),
            description: format!("What {name} does"),
            group,
            category: "Session".into(),
            args: String::new(),
            needs_text: false,
        }
    }

    fn labels(sheet: &Sheet, list: &[HermesCommand]) -> Vec<String> {
        let (rows, _) = sheet.rows(list);
        rows.iter()
            .filter_map(|r| match r {
                Row::Command(c) => Some(c.label.clone()),
                _ => None,
            })
            .collect()
    }

    /// Approvals are answered on their cards only: a list that offers them anyway, in
    /// either group, shows neither, and one with nothing else is our own few.
    #[test]
    fn approve_and_deny_are_never_offered() {
        use HermesCommandGroup::{Commands, Skills};
        let list = [
            command("approve", Commands),
            command("status", Commands),
            command("deny", Skills),
        ];
        let mut sheet = Sheet::open(false, &list);
        assert_eq!(labels(&sheet, &list), ["/status"]);
        sheet.skills = true;
        assert!(labels(&sheet, &list).is_empty());
        sheet.filter = "approve".into();
        sheet.skills = false;
        assert!(labels(&sheet, &list).is_empty());
        let only = [command("approve", Commands), command("deny", Commands)];
        let (rows, native) = Sheet::open(false, &only).rows(&only);
        assert!(!native);
        assert_eq!(rows.len(), COMMON.len());
    }

    /// A group with nothing in it, or a filter that finds nothing, leaves nothing to move to:
    /// moving there does nothing.
    #[test]
    fn moving_in_an_empty_list_does_nothing() {
        let list = [command("status", HermesCommandGroup::Commands)];
        let mut sheet = Sheet::open(false, &list);
        sheet.skills = true;
        for skills in [true, false] {
            sheet.skills = skills;
            sheet.filter = if skills { "" } else { "nothing like it" }.into();
            let (rows, native) = sheet.rows(&list);
            assert!(native && rows.is_empty());
            sheet.restart(&rows);
            for by in [1, -1, 6, -6] {
                sheet.step(&rows, by);
                assert_eq!(sheet.focus, 0);
            }
        }
    }

    /// The choice passes over headings, a row or a page at a time, and stays in the list.
    #[test]
    fn the_choice_steps_over_headings() {
        let list: Vec<HermesCommand> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, n)| HermesCommand {
                category: format!("H{}", i.min(1)),
                ..command(n, HermesCommandGroup::Commands)
            })
            .collect();
        let mut sheet = Sheet::open(false, &list);
        let (rows, _) = sheet.rows(&list);
        // # H0, /a, # H1, /b, /c
        assert_eq!(sheet.focus, 1);
        sheet.step(&rows, 1);
        assert_eq!(sheet.focus, 3);
        sheet.step(&rows, -1);
        assert_eq!(sheet.focus, 1);
        sheet.step(&rows, -1);
        assert_eq!(sheet.focus, 1);
        sheet.step(&rows, 10);
        assert_eq!(sheet.focus, 4);
    }
}
