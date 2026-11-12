//! Directional D-pad focus.
//!
//! A screen registers focusable rectangles (in content coordinates) each frame via [`FocusSet`].
//! Directional movement picks the candidate in the pressed direction that best balances
//! alignment and distance, so focus moves predictably between grid-like and list-like layouts.
//! [`FocusMemory`] remembers the last focused id per screen so returning to a screen restores
//! focus, and [`FocusSet::scroll_into_view`] reports the scroll delta needed to reveal focus.

use crate::geom::Rect;
use crate::input::Button;

/// A stable identifier for a focusable item within one screen build.
pub type FocusId = u64;

/// One focusable item.
#[derive(Clone, Copy, Debug)]
pub struct Focusable {
    /// Its id (stable across frames for the same logical item).
    pub id: FocusId,
    /// Its rectangle in content coordinates.
    pub rect: Rect,
    /// Larger groups (tab rail = 0, body = 1, composer = 2…) bias movement to stay within a
    /// group before jumping to another; lower numbers are "higher" regions.
    pub group: u8,
}

/// The set of focusables for the current frame plus the active focus.
#[derive(Default)]
pub struct FocusSet {
    items: Vec<Focusable>,
    focused: Option<FocusId>,
}

impl FocusSet {
    /// Starts a new frame, keeping the previously focused id.
    pub fn begin(&mut self, keep: Option<FocusId>) {
        self.items.clear();
        self.focused = keep;
    }

    /// How many focusables have been added this frame.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether none has been added this frame.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drops the focusables added after the first `len`.
    pub fn truncate(&mut self, len: usize) {
        self.items.truncate(len);
    }

    /// Adds a focusable; returns its id for convenience.
    pub fn add(&mut self, id: FocusId, rect: Rect, group: u8) -> FocusId {
        self.items.push(Focusable { id, rect, group });
        id
    }

    /// True when `id` is registered this frame.
    pub fn contains(&self, id: FocusId) -> bool {
        self.items.iter().any(|f| f.id == id)
    }

    /// The currently focused id, if it is still present.
    pub fn focused(&self) -> Option<FocusId> {
        self.focused.filter(|id| self.contains(*id))
    }

    /// The focused rectangle, if any.
    pub fn focused_rect(&self) -> Option<Rect> {
        let id = self.focused()?;
        self.items.iter().find(|f| f.id == id).map(|f| f.rect)
    }

    /// The raw focused id regardless of whether it has been registered this frame. Widgets use
    /// this to decide whether to draw their focus ring during an immediate-mode draw, before
    /// all focusables for the frame have been added.
    pub fn current(&self) -> Option<FocusId> {
        self.focused
    }

    /// True when `id` is the raw current focus (see [`FocusSet::current`]).
    pub fn is_focused(&self, id: FocusId) -> bool {
        self.focused == Some(id)
    }

    /// Ensures something is focused: if nothing valid is, focuses the first item in reading
    /// order (top, then left). Returns the resulting focus.
    pub fn ensure(&mut self) -> Option<FocusId> {
        if self.focused().is_none() {
            self.focused = self
                .items
                .iter()
                .min_by(|a, b| {
                    (a.group, ord(a.rect.y), ord(a.rect.x)).cmp(&(
                        b.group,
                        ord(b.rect.y),
                        ord(b.rect.x),
                    ))
                })
                .map(|f| f.id);
        }
        self.focused
    }

    /// Sets focus directly (e.g. pointer or programmatic).
    pub fn set(&mut self, id: FocusId) {
        if self.contains(id) {
            self.focused = Some(id);
        }
    }

    /// Moves focus in a direction; returns true if focus changed.
    pub fn move_focus(&mut self, dir: Button) -> bool {
        let Some(cur) = self.focused() else {
            return self.ensure().is_some();
        };
        let from = match self.items.iter().find(|f| f.id == cur) {
            Some(f) => *f,
            None => return false,
        };
        let best = self
            .items
            .iter()
            .filter(|c| c.id != cur)
            .filter_map(|c| score(&from, c, dir).map(|s| (s, c.id)))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, id)) = best {
            self.focused = Some(id);
            true
        } else {
            false
        }
    }

    /// The scroll delta (positive = scroll content up) needed so the focused item sits within
    /// the viewport `view` with `margin` padding. Returns 0 when already visible.
    pub fn scroll_into_view(&self, view: Rect, margin: f32) -> f32 {
        let Some(r) = self.focused_rect() else {
            return 0.0;
        };
        if r.y < view.y + margin {
            r.y - (view.y + margin)
        } else if r.bottom() > view.bottom() - margin {
            r.bottom() - (view.bottom() - margin)
        } else {
            0.0
        }
    }

    /// All focusables (for drawing debug overlays).
    pub fn items(&self) -> &[Focusable] {
        &self.items
    }
}

fn ord(v: f32) -> i32 {
    (v * 4.0).round() as i32
}

/// Directional score (lower is better); `None` when `c` is not in `dir` from `from`.
fn score(from: &Focusable, c: &Focusable, dir: Button) -> Option<f32> {
    let (fc, cc) = ((from.rect.cx(), from.rect.cy()), (c.rect.cx(), c.rect.cy()));
    let (primary, cross, forward) = match dir {
        Button::Up => (fc.1 - cc.1, (fc.0 - cc.0).abs(), from.rect.y - c.rect.y),
        Button::Down => (
            cc.1 - fc.1,
            (fc.0 - cc.0).abs(),
            c.rect.bottom() - from.rect.bottom(),
        ),
        Button::Left => (fc.0 - cc.0, (fc.1 - cc.1).abs(), from.rect.x - c.rect.x),
        Button::Right => (
            cc.0 - fc.0,
            (fc.1 - cc.1).abs(),
            c.rect.right() - from.rect.right(),
        ),
        _ => return None,
    };
    // Must make real progress in the primary axis.
    if primary <= 1.0 && forward <= 1.0 {
        return None;
    }
    let primary = primary.max(1.0);
    // Overlap on the cross axis is strongly preferred, so a list steps straight down even when
    // rows differ in width. A group change costs extra so focus settles within a region first.
    let overlap = cross_overlap(&from.rect, &c.rect, dir);
    let group_penalty = if from.group != c.group { 220.0 } else { 0.0 };
    let misalignment = if overlap {
        cross * 0.25
    } else {
        cross * 2.2 + 40.0
    };
    Some(primary + misalignment + group_penalty)
}

fn cross_overlap(a: &Rect, b: &Rect, dir: Button) -> bool {
    match dir {
        Button::Up | Button::Down => a.x < b.right() && b.x < a.right(),
        Button::Left | Button::Right => a.y < b.bottom() && b.y < a.bottom(),
        _ => false,
    }
}

/// Remembers focus per screen key so navigation restores where the user was.
#[derive(Default)]
pub struct FocusMemory {
    entries: Vec<(u64, FocusId)>,
}

impl FocusMemory {
    /// Recalls the last focus for `screen_key`.
    pub fn get(&self, screen_key: u64) -> Option<FocusId> {
        self.entries
            .iter()
            .find(|(k, _)| *k == screen_key)
            .map(|(_, id)| *id)
    }

    /// Stores the current focus for `screen_key`.
    pub fn set(&mut self, screen_key: u64, id: FocusId) {
        if let Some(e) = self.entries.iter_mut().find(|(k, _)| *k == screen_key) {
            e.1 = id;
        } else {
            self.entries.push((screen_key, id));
        }
    }

    /// Forgets a screen's focus (e.g. when its content is replaced).
    pub fn clear(&mut self, screen_key: u64) {
        self.entries.retain(|(k, _)| *k != screen_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[(FocusId, Rect, u8)], focus: FocusId) -> FocusSet {
        let mut fs = FocusSet::default();
        fs.begin(Some(focus));
        for &(id, r, g) in items {
            fs.add(id, r, g);
        }
        fs
    }

    #[test]
    fn vertical_list_moves_down() {
        let items = [
            (1, Rect::new(0.0, 0.0, 200.0, 40.0), 1),
            (2, Rect::new(0.0, 50.0, 200.0, 40.0), 1),
            (3, Rect::new(0.0, 100.0, 200.0, 40.0), 1),
        ];
        let mut fs = set(&items, 1);
        assert!(fs.move_focus(Button::Down));
        assert_eq!(fs.focused(), Some(2));
        assert!(fs.move_focus(Button::Down));
        assert_eq!(fs.focused(), Some(3));
        assert!(!fs.move_focus(Button::Down));
    }

    #[test]
    fn grid_prefers_aligned_column() {
        // 2x2 grid; from top-left, Down should hit the item directly below, not the diagonal.
        let items = [
            (1, Rect::new(0.0, 0.0, 90.0, 40.0), 1),
            (2, Rect::new(100.0, 0.0, 90.0, 40.0), 1),
            (3, Rect::new(0.0, 50.0, 90.0, 40.0), 1),
            (4, Rect::new(100.0, 50.0, 90.0, 40.0), 1),
        ];
        let mut fs = set(&items, 1);
        assert!(fs.move_focus(Button::Down));
        assert_eq!(fs.focused(), Some(3));
        assert!(fs.move_focus(Button::Right));
        assert_eq!(fs.focused(), Some(4));
    }

    #[test]
    fn group_change_is_penalised_but_possible() {
        let items = [
            (1, Rect::new(0.0, 0.0, 300.0, 30.0), 0),  // tab rail
            (2, Rect::new(0.0, 40.0, 150.0, 40.0), 1), // body
            (3, Rect::new(0.0, 90.0, 150.0, 40.0), 1),
        ];
        let mut fs = set(&items, 2);
        assert!(fs.move_focus(Button::Down));
        assert_eq!(fs.focused(), Some(3)); // stays in body group
        let mut fs = set(&items, 2);
        assert!(fs.move_focus(Button::Up));
        assert_eq!(fs.focused(), Some(1)); // crosses into tab rail
    }

    #[test]
    fn scroll_into_view_reports_delta() {
        let items = [(1, Rect::new(0.0, 500.0, 200.0, 40.0), 1)];
        let mut fs = FocusSet::default();
        fs.begin(Some(1));
        for &(id, r, g) in &items {
            fs.add(id, r, g);
        }
        let d = fs.scroll_into_view(Rect::new(0.0, 0.0, 200.0, 300.0), 10.0);
        assert!(d > 0.0);
    }

    #[test]
    fn memory_roundtrip() {
        let mut m = FocusMemory::default();
        m.set(7, 42);
        assert_eq!(m.get(7), Some(42));
        m.set(7, 43);
        assert_eq!(m.get(7), Some(43));
        m.clear(7);
        assert_eq!(m.get(7), None);
    }
}
