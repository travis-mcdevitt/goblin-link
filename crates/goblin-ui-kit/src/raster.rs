//! Vector path flattening and rasterisation to coverage masks.
//!
//! Fills use signed-area accumulation (exact anti-aliasing, non-zero winding clamped to 1).
//! Strokes use the distance to each flattened segment, which yields round caps and joins for
//! free; that matches the outline icon set's `stroke-linecap="round"` style.

use crate::image::Mask;

/// A 2D point.
pub type Pt = (f32, f32);

/// One subpath: a polyline, optionally closed.
#[derive(Clone, Debug, Default)]
pub struct SubPath {
    /// Flattened points.
    pub pts: Vec<Pt>,
    /// True when the subpath was closed (`Z`) or ends where it started.
    pub closed: bool,
}

/// A flattened vector path.
#[derive(Clone, Debug, Default)]
pub struct Path {
    /// Subpaths in drawing order.
    pub subs: Vec<SubPath>,
}

impl Path {
    /// Starts a new subpath.
    pub fn move_to(&mut self, p: Pt) {
        self.subs.push(SubPath {
            pts: vec![p],
            closed: false,
        });
    }

    fn cur(&mut self) -> &mut SubPath {
        if self.subs.is_empty() {
            self.move_to((0.0, 0.0));
        }
        let n = self.subs.len() - 1;
        &mut self.subs[n]
    }

    /// Last point of the current subpath.
    pub fn last(&self) -> Pt {
        self.subs
            .last()
            .and_then(|s| s.pts.last().copied())
            .unwrap_or((0.0, 0.0))
    }

    /// Straight segment.
    pub fn line_to(&mut self, p: Pt) {
        self.cur().pts.push(p);
    }

    /// Cubic Bézier.
    pub fn cubic_to(&mut self, c1: Pt, c2: Pt, p: Pt) {
        let p0 = self.last();
        let n = segs(dist(p0, c1) + dist(c1, c2) + dist(c2, p));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let a = mt * mt * mt;
            let b = 3.0 * mt * mt * t;
            let c = 3.0 * mt * t * t;
            let d = t * t * t;
            self.line_to((
                a * p0.0 + b * c1.0 + c * c2.0 + d * p.0,
                a * p0.1 + b * c1.1 + c * c2.1 + d * p.1,
            ));
        }
    }

    /// Quadratic Bézier.
    pub fn quad_to(&mut self, c: Pt, p: Pt) {
        let p0 = self.last();
        let n = segs(dist(p0, c) + dist(c, p));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            self.line_to((
                mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * p.0,
                mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * p.1,
            ));
        }
    }

    /// SVG elliptical arc (endpoint parameterisation, no x-axis rotation support beyond the
    /// general formula).
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(&mut self, rx: f32, ry: f32, rot_deg: f32, large: bool, sweep: bool, p: Pt) {
        let p0 = self.last();
        if dist(p0, p) < 1e-6 {
            return;
        }
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        if rx < 1e-6 || ry < 1e-6 {
            self.line_to(p);
            return;
        }
        let phi = rot_deg.to_radians();
        let (sphi, cphi) = phi.sin_cos();
        let dx2 = (p0.0 - p.0) * 0.5;
        let dy2 = (p0.1 - p.1) * 0.5;
        let x1p = cphi * dx2 + sphi * dy2;
        let y1p = -sphi * dx2 + cphi * dy2;
        let lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
        if lam > 1.0 {
            let s = lam.sqrt();
            rx *= s;
            ry *= s;
        }
        let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
        let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
        let mut co = (num / den).max(0.0).sqrt();
        if large == sweep {
            co = -co;
        }
        let cxp = co * rx * y1p / ry;
        let cyp = -co * ry * x1p / rx;
        let cx = cphi * cxp - sphi * cyp + (p0.0 + p.0) * 0.5;
        let cy = sphi * cxp + cphi * cyp + (p0.1 + p.1) * 0.5;
        let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        let t1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
        let mut dt = ang(
            (x1p - cxp) / rx,
            (y1p - cyp) / ry,
            (-x1p - cxp) / rx,
            (-y1p - cyp) / ry,
        );
        if !sweep && dt > 0.0 {
            dt -= std::f32::consts::TAU;
        } else if sweep && dt < 0.0 {
            dt += std::f32::consts::TAU;
        }
        let n = segs(dt.abs() * rx.max(ry));
        for i in 1..=n {
            let t = t1 + dt * i as f32 / n as f32;
            let (st, ct) = t.sin_cos();
            let x = cx + cphi * rx * ct - sphi * ry * st;
            let y = cy + sphi * rx * ct + cphi * ry * st;
            self.line_to((x, y));
        }
        // Land exactly on the endpoint.
        if let Some(last) = self.cur().pts.last_mut() {
            *last = p;
        }
    }

    /// Closes the current subpath.
    pub fn close(&mut self) {
        let s = self.cur();
        s.closed = true;
        if let (Some(&f), Some(&l)) = (s.pts.first(), s.pts.last()) {
            if dist(f, l) > 1e-4 {
                s.pts.push(f);
            }
        }
    }

    /// Full ellipse as a closed subpath.
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        let n = segs(std::f32::consts::TAU * rx.max(ry)).max(16);
        self.move_to((cx + rx, cy));
        for i in 1..n {
            let t = std::f32::consts::TAU * i as f32 / n as f32;
            self.line_to((cx + rx * t.cos(), cy + ry * t.sin()));
        }
        self.close();
    }

    /// Rounded rectangle as a closed subpath.
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) {
        let rx = rx.min(w * 0.5);
        let ry = ry.min(h * 0.5);
        if rx <= 0.0 || ry <= 0.0 {
            self.move_to((x, y));
            self.line_to((x + w, y));
            self.line_to((x + w, y + h));
            self.line_to((x, y + h));
            self.close();
            return;
        }
        self.move_to((x + rx, y));
        self.line_to((x + w - rx, y));
        self.arc_to(rx, ry, 0.0, false, true, (x + w, y + ry));
        self.line_to((x + w, y + h - ry));
        self.arc_to(rx, ry, 0.0, false, true, (x + w - rx, y + h));
        self.line_to((x + rx, y + h));
        self.arc_to(rx, ry, 0.0, false, true, (x, y + h - ry));
        self.line_to((x, y + ry));
        self.arc_to(rx, ry, 0.0, false, true, (x + rx, y));
        self.close();
    }

    /// Marks subpaths whose ends meet as closed (SVG icons often omit `Z`).
    pub fn finish(&mut self) {
        for s in &mut self.subs {
            if let (Some(&f), Some(&l)) = (s.pts.first(), s.pts.last()) {
                if s.pts.len() > 2 && dist(f, l) < 0.05 {
                    s.closed = true;
                }
            }
        }
    }
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Segment count for a curve of approximate length `len` (in 24-unit icon space this gives
/// sub-pixel flatness up to roughly 80 px icons).
fn segs(len: f32) -> usize {
    ((len * 3.0).ceil() as usize).clamp(2, 64)
}

/// Distance from point `(px, py)` to segment `a`–`b`.
#[inline]
pub(crate) fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);
    let l2 = vx * vx + vy * vy;
    let t = if l2 > 0.0 {
        ((wx * vx + wy * vy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (wx - t * vx, wy - t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// Signed-area accumulation buffer.
struct Accum {
    w: usize,
    h: usize,
    a: Vec<f32>,
}

impl Accum {
    fn new(w: usize, h: usize) -> Self {
        Accum {
            w,
            h,
            a: vec![0.0; w * h + 4],
        }
    }

    fn line(&mut self, p0: Pt, p1: Pt) {
        let clampx = |p: Pt| (p.0.clamp(0.0, self.w as f32 - 1.001), p.1);
        let (p0, p1) = (clampx(p0), clampx(p1));
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        if p0.1 < 0.0 {
            x -= p0.1 * dxdy;
        }
        let y_start = p0.1.max(0.0) as usize;
        let y_end = self.h.min(p1.1.ceil().max(0.0) as usize);
        for y in y_start..y_end {
            let ls = y * self.w;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let x0f = x0.floor();
            let x0i = x0f as usize;
            let x1c = x1.ceil();
            let x1i = x1c as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x + xnext) - x0f;
                self.a[ls + x0i] += d - d * xmf;
                self.a[ls + x0i + 1] += d * xmf;
            } else {
                let s = (x1 - x0).recip();
                let x0fr = x0 - x0f;
                let a0 = 0.5 * s * (1.0 - x0fr) * (1.0 - x0fr);
                let x1fr = x1 - x1c + 1.0;
                let am = 0.5 * s * x1fr * x1fr;
                self.a[ls + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.a[ls + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0fr);
                    self.a[ls + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.a[ls + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.a[ls + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.a[ls + x1i] += d * am;
            }
            x = xnext;
        }
    }

    fn coverage(&self) -> Vec<f32> {
        let mut acc = 0.0;
        let mut out = Vec::with_capacity(self.w * self.h);
        for &v in &self.a[..self.w * self.h] {
            acc += v;
            out.push(acc.abs().min(1.0));
        }
        out
    }
}

/// Coverage of the filled closed subpaths, in 0..=1 per pixel.
pub fn fill_coverage(path: &Path, w: u32, h: u32, scale: f32, off: Pt) -> Vec<f32> {
    let mut acc = Accum::new(w as usize, h as usize);
    let tf = |p: Pt| (p.0 * scale + off.0, p.1 * scale + off.1);
    for s in path.subs.iter().filter(|s| s.closed && s.pts.len() > 2) {
        for win in s.pts.windows(2) {
            acc.line(tf(win[0]), tf(win[1]));
        }
        if let (Some(&f), Some(&l)) = (s.pts.first(), s.pts.last()) {
            acc.line(tf(l), tf(f));
        }
    }
    acc.coverage()
}

/// Coverage of the stroked subpaths selected by `pick`, in 0..=1 per pixel.
pub fn stroke_coverage(
    path: &Path,
    w: u32,
    h: u32,
    scale: f32,
    off: Pt,
    width: f32,
    pick: impl Fn(&SubPath) -> bool,
) -> Vec<f32> {
    let mut out = vec![0.0f32; (w * h) as usize];
    let hw = width * 0.5;
    let tf = |p: Pt| (p.0 * scale + off.0, p.1 * scale + off.1);
    for s in path.subs.iter().filter(|s| pick(s)) {
        let pts: Vec<Pt> = s.pts.iter().map(|&p| tf(p)).collect();
        let segs: Vec<(Pt, Pt)> = if pts.len() == 1 {
            vec![(pts[0], pts[0])]
        } else {
            pts.windows(2).map(|w| (w[0], w[1])).collect()
        };
        for (a, b) in segs {
            let x0 = ((a.0.min(b.0) - hw - 1.0).floor().max(0.0)) as u32;
            let y0 = ((a.1.min(b.1) - hw - 1.0).floor().max(0.0)) as u32;
            let x1 = ((a.0.max(b.0) + hw + 1.0).ceil() as u32).min(w);
            let y1 = ((a.1.max(b.1) + hw + 1.0).ceil() as u32).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let d = seg_dist(x as f32 + 0.5, y as f32 + 0.5, a.0, a.1, b.0, b.1);
                    let c = (hw - d + 0.5).clamp(0.0, 1.0);
                    let o = &mut out[(y * w + x) as usize];
                    if c > *o {
                        *o = c;
                    }
                }
            }
        }
    }
    out
}

/// Converts 0..=1 coverage to a mask.
pub fn to_mask(cov: &[f32], w: u32, h: u32) -> Mask {
    Mask {
        w,
        h,
        data: cov.iter().map(|&c| (c * 255.0 + 0.5) as u8).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filled_square_has_full_interior() {
        let mut p = Path::default();
        p.rrect(2.0, 2.0, 12.0, 12.0, 0.0, 0.0);
        let cov = fill_coverage(&p, 16, 16, 1.0, (0.0, 0.0));
        assert!((cov[8 * 16 + 8] - 1.0).abs() < 1e-3);
        assert!(cov[0] < 1e-3);
        assert!(cov[8 * 16 + 15] < 1e-3);
    }

    #[test]
    fn arc_reaches_endpoint() {
        let mut p = Path::default();
        p.move_to((0.0, 10.0));
        p.arc_to(10.0, 10.0, 0.0, false, true, (10.0, 0.0));
        let l = p.last();
        assert!((l.0 - 10.0).abs() < 1e-4 && l.1.abs() < 1e-4);
    }

    #[test]
    fn stroke_is_round_capped() {
        let mut p = Path::default();
        p.move_to((4.0, 8.0));
        p.line_to((12.0, 8.0));
        let cov = stroke_coverage(&p, 16, 16, 1.0, (0.0, 0.0), 2.0, |_| true);
        assert!(cov[8 * 16 + 8] > 0.99);
        assert!(cov[8 * 16 + 3] > 0.3); // cap extends past the endpoint
        assert!(cov[2 * 16 + 8] < 1e-3);
    }
}
