//! Geometry: float rectangles in physical pixels and integer clip rectangles.

/// An axis-aligned rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width (never negative in practice).
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Creates a rectangle.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    /// Right edge.
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Horizontal centre.
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }

    /// Vertical centre.
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }

    /// Grows each side by `d` (negative shrinks).
    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }

    /// Shrinks by separate horizontal and vertical insets.
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    /// Moves by an offset.
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Smallest rectangle containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }

    /// Splits off `h` from the top: returns (top, rest).
    pub fn split_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// A rectangle of size `w`×`h` centred in this one.
    pub fn centered(&self, w: f32, h: f32) -> Rect {
        Rect::new(self.cx() - w * 0.5, self.cy() - h * 0.5, w, h)
    }

    /// Rounds edges outward to whole pixels.
    pub fn round_out(&self) -> IRect {
        let x0 = self.x.floor() as i32;
        let y0 = self.y.floor() as i32;
        let x1 = self.right().ceil() as i32;
        let y1 = self.bottom().ceil() as i32;
        IRect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Snaps edges to the nearest whole pixel.
    pub fn snap(&self) -> Rect {
        let x0 = self.x.round();
        let y0 = self.y.round();
        Rect::new(
            x0,
            y0,
            self.right().round() - x0,
            self.bottom().round() - y0,
        )
    }
}

/// An integer rectangle used for clipping and damage tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct IRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl IRect {
    /// Creates a rectangle.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        IRect { x, y, w, h }
    }

    /// Right edge (exclusive).
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    /// Bottom edge (exclusive).
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// True when the rectangle has no area.
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Intersection (may be empty).
    pub fn intersect(&self, o: &IRect) -> IRect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        IRect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    /// Bounding union; an empty operand is ignored.
    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        IRect::new(
            x0,
            y0,
            self.right().max(o.right()) - x0,
            self.bottom().max(o.bottom()) - y0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_unions() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (top, rest) = r.split_top(10.0);
        assert_eq!(top.h, 10.0);
        assert_eq!(rest.y, 10.0);
        assert_eq!(top.union(&rest), r);
        let a = IRect::new(0, 0, 10, 10);
        let b = IRect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), IRect::new(5, 5, 5, 5));
        assert_eq!(a.union(&IRect::default()), a);
    }
}
