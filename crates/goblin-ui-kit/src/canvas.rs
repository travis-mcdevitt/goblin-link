//! The software canvas: an opaque RGBA framebuffer with anti-aliased primitives.
//!
//! All shapes are evaluated analytically (signed distance at pixel centres) so edges are
//! anti-aliased without supersampling. Interior spans are filled without per-pixel maths, so
//! large cards and bubbles cost little more than a `memset`. Every primitive honours the
//! current clip rectangle, which is how dirty-region redraws stay cheap.

use crate::color::{blend_px, Color};
use crate::geom::{IRect, Rect};
use crate::image::{Image, Mask};

/// An opaque RGBA framebuffer (bytes R, G, B, A in memory).
pub struct Canvas {
    w: i32,
    h: i32,
    px: Vec<u32>,
    clip: IRect,
    /// While set, nothing is written: drawing code runs for its measurements and other
    /// effects only.
    muted: bool,
    /// Set by any write; see [`Canvas::take_written`].
    written: bool,
}

/// Converts colour alpha times coverage to the 0..=256 blend factor.
#[inline(always)]
fn alpha256(color_a: u8, cov: f32) -> u32 {
    let a = color_a as f32 * cov.clamp(0.0, 1.0);
    (a * (256.0 / 255.0) + 0.5) as u32
}

/// Half-width at vertical offset `ady` (absolute distance from centre) of a rounded rectangle
/// with half extents `hw`×`hh` and radius `r`; `None` when the row misses the shape.
#[inline]
fn half_extent(ady: f32, hw: f32, hh: f32, r: f32) -> Option<f32> {
    if ady > hh || hw <= 0.0 || hh <= 0.0 {
        return None;
    }
    let r = r.min(hw).min(hh).max(0.0);
    let dy = ady - (hh - r);
    if dy <= 0.0 {
        Some(hw)
    } else {
        Some(hw - r + (r * r - dy * dy).max(0.0).sqrt())
    }
}

/// Signed distance from a point (relative to the centre) to a rounded rectangle.
#[inline]
fn rrect_sdf(dx: f32, dy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let r = r.min(hw).min(hh).max(0.0);
    let qx = dx.abs() - (hw - r);
    let qy = dy.abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// A rounded rectangle described by centre, half extents and radius.
#[derive(Clone, Copy)]
struct RRect {
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    r: f32,
}

impl RRect {
    fn new(rect: Rect, radius: f32) -> Self {
        let hw = rect.w * 0.5;
        let hh = rect.h * 0.5;
        RRect {
            cx: rect.cx(),
            cy: rect.cy(),
            hw,
            hh,
            r: radius.min(hw).min(hh).max(0.0),
        }
    }

    #[inline]
    fn coverage(&self, px: f32, py: f32, feather: f32) -> f32 {
        let d = rrect_sdf(px - self.cx, py - self.cy, self.hw, self.hh, self.r);
        (0.5 - d / feather).clamp(0.0, 1.0)
    }

    /// (solid half extent, outer half extent) for a row whose centre is at `py`.
    #[inline]
    fn row_extents(&self, py: f32, feather: f32) -> (Option<f32>, Option<f32>) {
        let ady = (py - self.cy).abs();
        let g = 0.5 * feather;
        let solid = half_extent(ady, self.hw - g, self.hh - g, (self.r - g).max(0.0));
        let outer = half_extent(ady, self.hw + g, self.hh + g, self.r + g);
        (solid, outer)
    }
}

impl Canvas {
    /// Creates a canvas filled with opaque black.
    pub fn new(w: u32, h: u32) -> Self {
        let (w, h) = (w as i32, h as i32);
        Canvas {
            w,
            h,
            px: vec![Color::BLACK.pack(); (w * h) as usize],
            clip: IRect::new(0, 0, w, h),
            muted: false,
            written: false,
        }
    }

    /// Makes the canvas `w` by `h`, every pixel `fill`, with the clip opened to all of it.
    /// The pixel buffer is kept when it is large enough.
    pub fn reset(&mut self, w: i32, h: i32, fill: Color) {
        self.w = w.max(0);
        self.h = h.max(0);
        let n = (self.w * self.h) as usize;
        self.px.clear();
        self.px.resize(n, fill.with_alpha(255).pack());
        self.clip = IRect::new(0, 0, self.w, self.h);
        self.muted = false;
    }

    /// Stops (or resumes) writing; returns what it was. Drawing code still runs, so its
    /// measurements and the focus targets it registers are as if it had drawn.
    pub fn set_muted(&mut self, muted: bool) -> bool {
        std::mem::replace(&mut self.muted, muted)
    }

    /// Whether anything was written since this was last asked, and forgets it.
    pub fn take_written(&mut self) -> bool {
        std::mem::take(&mut self.written)
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.w as u32
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.h as u32
    }

    /// Packed pixels, row-major.
    pub fn pixels(&self) -> &[u32] {
        &self.px
    }

    /// Packed pixels for drawing into directly (a full-screen compositor of the caller's
    /// own). The clip is not applied.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.px
    }

    /// Pixels as bytes (R, G, B, A per pixel), ready for an `ABGR8888` texture upload.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.px)
    }

    /// Bytes of one sub-rectangle's first row onward; rows are `width * 4` bytes apart.
    pub fn bytes_from(&self, r: IRect) -> &[u8] {
        let start = (r.y * self.w + r.x) as usize * 4;
        &self.as_bytes()[start..]
    }

    /// Reads one pixel (for tests and tools).
    pub fn get(&self, x: i32, y: i32) -> Color {
        Color::unpack(self.px[(y * self.w + x) as usize])
    }

    /// Full canvas bounds.
    pub fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.w, self.h)
    }

    /// Current clip rectangle.
    pub fn clip(&self) -> IRect {
        self.clip
    }

    /// Sets the clip (intersected with the canvas) and returns the previous one.
    pub fn set_clip(&mut self, clip: IRect) -> IRect {
        let next = clip.intersect(&self.bounds());
        std::mem::replace(&mut self.clip, next)
    }

    /// Narrows the clip to its intersection with `r`; returns the previous clip for restoring.
    pub fn push_clip(&mut self, r: Rect) -> IRect {
        let next = self.clip.intersect(&r.round_out());
        std::mem::replace(&mut self.clip, next)
    }

    /// Fills the clip area with an opaque colour.
    pub fn clear(&mut self, c: Color) {
        if self.muted {
            return;
        }
        self.written = true;
        let p = c.with_alpha(255).pack();
        let cl = self.clip;
        for y in cl.y..cl.bottom() {
            let row = (y * self.w) as usize;
            self.px[row + cl.x as usize..row + cl.right() as usize].fill(p);
        }
    }

    #[inline]
    fn span(&mut self, y: i32, x0: i32, x1: i32, c: u32, a: u32) {
        let x0 = x0.max(self.clip.x);
        let x1 = x1.min(self.clip.right());
        if x0 >= x1 || y < self.clip.y || y >= self.clip.bottom() || a == 0 || self.muted {
            return;
        }
        self.written = true;
        let row = (y * self.w) as usize;
        let s = &mut self.px[row + x0 as usize..row + x1 as usize];
        if a >= 256 {
            s.fill(c);
        } else {
            for p in s {
                *p = blend_px(*p, c, a);
            }
        }
    }

    #[inline]
    fn plot(&mut self, x: i32, y: i32, c: u32, a: u32) {
        if a == 0
            || self.muted
            || x < self.clip.x
            || x >= self.clip.right()
            || y < self.clip.y
            || y >= self.clip.bottom()
        {
            return;
        }
        self.written = true;
        let i = (y * self.w + x) as usize;
        self.px[i] = if a >= 256 {
            c
        } else {
            blend_px(self.px[i], c, a)
        };
    }

    /// Row range of a float rectangle intersected with the clip.
    fn rows(&self, top: f32, bottom: f32) -> (i32, i32) {
        (
            (top.floor() as i32).max(self.clip.y),
            (bottom.ceil() as i32).min(self.clip.bottom()),
        )
    }

    /// Fills a rectangle snapped to whole pixels.
    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        let r = r.snap();
        let c32 = c.with_alpha(255).pack();
        let a = alpha256(c.a, 1.0);
        let (y0, y1) = self.rows(r.y, r.bottom());
        for y in y0..y1 {
            self.span(y, r.x as i32, r.right() as i32, c32, a);
        }
    }

    /// One-pixel horizontal line from `x0` to `x1` at row `y`.
    pub fn hline(&mut self, x0: f32, x1: f32, y: f32, c: Color) {
        self.fill_rect(Rect::new(x0, y.floor(), x1 - x0, 1.0), c);
    }

    /// One-pixel vertical line.
    pub fn vline(&mut self, x: f32, y0: f32, y1: f32, c: Color) {
        self.fill_rect(Rect::new(x.floor(), y0, 1.0, y1 - y0), c);
    }

    /// Fills an anti-aliased rounded rectangle.
    pub fn fill_rrect(&mut self, rect: Rect, radius: f32, c: Color) {
        self.fill_rrect_feather(rect, radius, 1.0, c);
    }

    /// Fills a rounded rectangle with a soft edge `feather` pixels wide (used for shadows and
    /// glows; `feather = 1` is a normal anti-aliased edge).
    pub fn fill_rrect_feather(&mut self, rect: Rect, radius: f32, feather: f32, c: Color) {
        if rect.w <= 0.0 || rect.h <= 0.0 || c.a == 0 {
            return;
        }
        let feather = feather.max(1.0);
        let rr = RRect::new(rect, radius);
        let c32 = c.with_alpha(255).pack();
        let g = feather * 0.5 + 1.0;
        let (y0, y1) = self.rows(rect.y - g, rect.bottom() + g);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let (solid, outer) = rr.row_extents(py, feather);
            let Some(so) = outer else { continue };
            let xa = (rr.cx - so - 0.5).ceil() as i32;
            let xb = (rr.cx + so - 0.5).floor() as i32 + 1;
            let (sa, sb) = match solid {
                Some(si) => (
                    ((rr.cx - si - 0.5).ceil() as i32).max(xa),
                    ((rr.cx + si - 0.5).floor() as i32 + 1).min(xb),
                ),
                None => (xb, xb),
            };
            let (sa, sb) = if sa < sb { (sa, sb) } else { (xb, xb) };
            let lo = xa.max(self.clip.x);
            for x in lo..sa.min(self.clip.right()) {
                let cov = rr.coverage(x as f32 + 0.5, py, feather);
                self.plot(x, y, c32, alpha256(c.a, cov));
            }
            self.span(y, sa, sb, c32, alpha256(c.a, 1.0));
            let hi = xb.min(self.clip.right());
            for x in sb.max(lo)..hi {
                let cov = rr.coverage(x as f32 + 0.5, py, feather);
                self.plot(x, y, c32, alpha256(c.a, cov));
            }
        }
    }

    /// Strokes a rounded rectangle; the stroke lies inside `rect`.
    pub fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, c: Color) {
        if rect.w <= 0.0 || rect.h <= 0.0 || c.a == 0 || width <= 0.0 {
            return;
        }
        let outer = RRect::new(rect, radius);
        let inner = RRect::new(rect.inflate(-width), (radius - width).max(0.0));
        let has_inner = rect.w > 2.0 * width && rect.h > 2.0 * width;
        let c32 = c.with_alpha(255).pack();
        let (y0, y1) = self.rows(rect.y - 1.5, rect.bottom() + 1.5);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let (_, so) = outer.row_extents(py, 1.0);
            let Some(so) = so else { continue };
            let xa = (outer.cx - so - 0.5).ceil() as i32;
            let xb = (outer.cx + so - 0.5).floor() as i32 + 1;
            // Pixels whose centre is inside the inner shape's solid area have zero coverage.
            let gap = if has_inner {
                inner.row_extents(py, 1.0).0.map(|si| {
                    (
                        (inner.cx - si - 0.5).ceil() as i32,
                        (inner.cx + si - 0.5).floor() as i32 + 1,
                    )
                })
            } else {
                None
            };
            let (ga, gb) = gap.unwrap_or((xb, xb));
            let px_cov = |canvas: &mut Canvas, x: i32| {
                let fx = x as f32 + 0.5;
                let co = outer.coverage(fx, py, 1.0);
                let ci = if has_inner {
                    inner.coverage(fx, py, 1.0)
                } else {
                    0.0
                };
                canvas.plot(x, y, c32, alpha256(c.a, co - ci));
            };
            let lo = xa.max(self.clip.x);
            let hi = xb.min(self.clip.right());
            for x in lo..ga.min(hi) {
                px_cov(self, x);
            }
            for x in gb.max(lo)..hi {
                px_cov(self, x);
            }
        }
    }

    /// Fills an anti-aliased circle.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        self.fill_rrect(Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r), r, c);
    }

    /// Strokes a circle; the stroke lies inside radius `r`.
    pub fn stroke_circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: Color) {
        self.stroke_rrect(Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r), r, width, c);
    }

    /// Per-pixel evaluation over a bounding box, used by the small curved primitives.
    fn eval_box(&mut self, bb: Rect, c: Color, f: impl Fn(f32, f32) -> f32) {
        let c32 = c.with_alpha(255).pack();
        let ir = bb.round_out().intersect(&self.clip);
        for y in ir.y..ir.bottom() {
            for x in ir.x..ir.right() {
                let cov = f(x as f32 + 0.5, y as f32 + 0.5);
                if cov > 0.0 {
                    self.plot(x, y, c32, alpha256(c.a, cov));
                }
            }
        }
    }

    /// Fills an axis-aligned ellipse.
    pub fn fill_ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color) {
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let bb = Rect::new(cx - rx - 1.0, cy - ry - 1.0, 2.0 * rx + 2.0, 2.0 * ry + 2.0);
        self.eval_box(bb, c, |x, y| {
            let (px, py) = ((x - cx) / rx, (y - cy) / ry);
            let k0 = (px * px + py * py).sqrt();
            let k1 = ((px / rx).powi(2) + (py / ry).powi(2)).sqrt();
            let d = if k1 > 0.0 {
                k0 * (k0 - 1.0) / k1
            } else {
                -rx.min(ry)
            };
            (0.5 - d).clamp(0.0, 1.0)
        });
    }

    /// Strokes a line segment with round caps.
    pub fn stroke_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let hw = width * 0.5;
        let bb =
            Rect::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()).inflate(hw + 1.0);
        self.eval_box(bb, c, |x, y| {
            let d = crate::raster::seg_dist(x, y, x0, y0, x1, y1);
            (hw - d + 0.5).clamp(0.0, 1.0)
        });
    }

    /// Strokes a circular arc from angle `a0` to `a1` (radians, clockwise in screen space,
    /// 0 = +x) with round caps.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, width: f32, c: Color) {
        let hw = width * 0.5;
        let (a0, a1) = if a1 < a0 { (a1, a0) } else { (a0, a1) };
        let span = a1 - a0;
        let mid = (a0 + a1) * 0.5;
        let (e0x, e0y) = (cx + r * a0.cos(), cy + r * a0.sin());
        let (e1x, e1y) = (cx + r * a1.cos(), cy + r * a1.sin());
        let bb = Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r).inflate(hw + 1.0);
        self.eval_box(bb, c, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            let ang = dy.atan2(dx);
            let mut rel = ang - mid;
            while rel > std::f32::consts::PI {
                rel -= std::f32::consts::TAU;
            }
            while rel < -std::f32::consts::PI {
                rel += std::f32::consts::TAU;
            }
            let d = if rel.abs() <= span * 0.5 {
                ((dx * dx + dy * dy).sqrt() - r).abs()
            } else {
                let d0 = ((x - e0x).powi(2) + (y - e0y).powi(2)).sqrt();
                let d1 = ((x - e1x).powi(2) + (y - e1y).powi(2)).sqrt();
                d0.min(d1)
            };
            (hw - d + 0.5).clamp(0.0, 1.0)
        });
    }

    /// Draws an 8-bit coverage mask tinted with `c`, top-left at integer `(x, y)`.
    pub fn draw_mask(&mut self, m: &Mask, x: i32, y: i32, c: Color) {
        let dst = IRect::new(x, y, m.w as i32, m.h as i32).intersect(&self.clip);
        if dst.is_empty() || c.a == 0 || self.muted {
            return;
        }
        self.written = true;
        let c32 = c.with_alpha(255).pack();
        let ca = c.a as u32;
        for yy in dst.y..dst.bottom() {
            let mrow = ((yy - y) as usize) * m.w as usize;
            let row = (yy * self.w) as usize;
            for xx in dst.x..dst.right() {
                let cov = m.data[mrow + (xx - x) as usize] as u32;
                if cov == 0 {
                    continue;
                }
                let a = (cov * ca + 127) / 255;
                let a = a + (a >> 7);
                let i = row + xx as usize;
                self.px[i] = if a >= 256 {
                    c32
                } else {
                    blend_px(self.px[i], c32, a)
                };
            }
        }
    }

    /// Draws an image scaled (bilinear) into `dst`, clipped to a rounded rectangle of radius
    /// `radius`, at `opacity`.
    ///
    /// Inside the rounded shape (most of the picture) the pixels are worked out in fixed
    /// point and written straight in, several times faster than the general path, which is
    /// kept for the feathered edge and for see-through texels.
    pub fn draw_image(&mut self, img: &Image, dst: Rect, radius: f32, opacity: f32) {
        if img.w == 0 || img.h == 0 || dst.w <= 0.0 || dst.h <= 0.0 || self.muted {
            return;
        }
        self.written = true;
        let rr = RRect::new(dst, radius);
        let ir = dst.round_out().intersect(&self.clip);
        let sx = img.w as f32 / dst.w;
        let sy = img.h as f32 / dst.h;
        let op = opacity.clamp(0.0, 1.0);
        let op256 = (op * 256.0) as u32;
        let ipx = img.pixels();
        let last_u = ((img.w - 1) as i64) << 16;
        let step = (sx * 65536.0) as i64;
        let start = (((ir.x as f32 + 0.5 - dst.x) * sx - 0.5) * 65536.0) as i64;
        for y in ir.y..ir.bottom() {
            let py = y as f32 + 0.5;
            let (solid, outer) = rr.row_extents(py, 1.0);
            let Some(so) = outer else { continue };
            let si = solid.unwrap_or(-1.0);
            let v = ((py - dst.y) * sy - 0.5).clamp(0.0, img.h as f32 - 1.0);
            let (v0, fv) = (v.floor() as u32, v.fract());
            let v1 = (v0 + 1).min(img.h - 1);
            // The run of pixels wholly inside the shape on this row.
            let (xs, xe) = if si >= 0.0 {
                (
                    ((rr.cx - si - 0.5).ceil() as i32).clamp(ir.x, ir.right()),
                    ((rr.cx + si - 0.5).floor() as i32 + 1).clamp(ir.x, ir.right()),
                )
            } else {
                (ir.x, ir.x)
            };
            let edge = |c: &mut Canvas, x: i32| {
                let px = x as f32 + 0.5;
                let adx = (px - rr.cx).abs();
                if adx > so {
                    return;
                }
                let shape = if adx <= si {
                    1.0
                } else {
                    rr.coverage(px, py, 1.0)
                };
                let u = ((px - dst.x) * sx - 0.5).clamp(0.0, img.w as f32 - 1.0);
                let (u0, fu) = (u.floor() as u32, u.fract());
                let u1 = (u0 + 1).min(img.w - 1);
                let s = bilerp(
                    img.get(u0, v0),
                    img.get(u1, v0),
                    img.get(u0, v1),
                    img.get(u1, v1),
                    fu,
                    fv,
                );
                let a = alpha256(s.a, shape * op);
                c.plot(x, y, s.with_alpha(255).pack(), a);
            };
            for x in ir.x..xs {
                edge(self, x);
            }
            let row = (y * self.w) as usize;
            let r0 = (v0 * img.w) as usize;
            let r1 = (v1 * img.w) as usize;
            let fvi = (fv * 256.0) as u32;
            let mut u = start + step * (xs - ir.x) as i64;
            for x in xs..xe {
                let uc = u.clamp(0, last_u);
                u += step;
                let (u0, fu) = ((uc >> 16) as usize, ((uc & 0xFFFF) >> 8) as u32);
                let u1 = (u0 + 1).min(img.w as usize - 1);
                let (a, b) = (ipx[r0 + u0], ipx[r0 + u1]);
                let (c, d) = (ipx[r1 + u0], ipx[r1 + u1]);
                if (a & b & c & d) >> 24 != 255 {
                    edge(self, x);
                    continue;
                }
                let s = mix_px(mix_px(a, b, fu), mix_px(c, d, fu), fvi);
                let i = row + x as usize;
                self.px[i] = if op256 >= 256 {
                    s
                } else {
                    blend_px(self.px[i], s, op256)
                };
            }
            for x in xe..ir.right() {
                edge(self, x);
            }
        }
    }

    /// Copies the canvas into an [`Image`].
    pub fn to_image(&self) -> Image {
        Image::from_packed(self.w as u32, self.h as u32, self.px.clone())
    }
}

/// `a` toward `b` by `f`/256, each colour channel on its own; the result is opaque.
#[inline]
fn mix_px(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let rb = (((a & 0x00FF_00FF) * g + (b & 0x00FF_00FF) * f) >> 8) & 0x00FF_00FF;
    let gg = (((a & 0x0000_FF00) * g + (b & 0x0000_FF00) * f) >> 8) & 0x0000_FF00;
    0xFF00_0000 | rb | gg
}

fn bilerp(a: Color, b: Color, c: Color, d: Color, fu: f32, fv: f32) -> Color {
    // Premultiply so transparent texels do not bleed their colour.
    let ch = |f: fn(Color) -> u8| -> f32 {
        let pa = |x: Color| f(x) as f32 * x.a as f32;
        let top = pa(a) + (pa(b) - pa(a)) * fu;
        let bot = pa(c) + (pa(d) - pa(c)) * fu;
        top + (bot - top) * fv
    };
    let al = {
        let top = a.a as f32 + (b.a as f32 - a.a as f32) * fu;
        let bot = c.a as f32 + (d.a as f32 - c.a as f32) * fu;
        top + (bot - top) * fv
    };
    if al <= 0.0 {
        return Color::TRANSPARENT;
    }
    let un = |v: f32| (v / al).round().clamp(0.0, 255.0) as u8;
    Color::rgba(
        un(ch(|x| x.r)),
        un(ch(|x| x.g)),
        un(ch(|x| x.b)),
        al.round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrect_fill_is_solid_inside_and_clean_outside() {
        let mut c = Canvas::new(64, 64);
        c.clear(Color::WHITE);
        c.fill_rrect(Rect::new(8.0, 8.0, 48.0, 48.0), 12.0, Color::BLACK);
        assert_eq!(c.get(32, 32), Color::BLACK);
        assert_eq!(c.get(8, 32), Color::BLACK);
        assert_eq!(c.get(2, 2), Color::WHITE);
        // The corner pixel is outside the radius.
        assert_eq!(c.get(8, 8), Color::WHITE);
        // An anti-aliased pixel somewhere on the corner arc.
        let mid = (0..12)
            .map(|i| c.get(8 + i, 8 + i).r)
            .any(|v| v > 10 && v < 245);
        assert!(mid);
    }

    #[test]
    fn stroke_leaves_interior_untouched() {
        let mut c = Canvas::new(64, 64);
        c.clear(Color::WHITE);
        c.stroke_rrect(Rect::new(4.0, 4.0, 56.0, 56.0), 10.0, 3.0, Color::BLACK);
        assert_eq!(c.get(32, 32), Color::WHITE);
        assert_eq!(c.get(5, 32), Color::BLACK);
        assert_eq!(c.get(32, 58), Color::BLACK);
    }

    #[test]
    fn clip_is_respected() {
        let mut c = Canvas::new(32, 32);
        c.clear(Color::WHITE);
        c.set_clip(IRect::new(0, 0, 16, 32));
        c.fill_rect(Rect::new(0.0, 0.0, 32.0, 32.0), Color::BLACK);
        assert_eq!(c.get(15, 5), Color::BLACK);
        assert_eq!(c.get(16, 5), Color::WHITE);
    }

    #[test]
    fn image_blit_scales() {
        let img = Image::from_packed(2, 2, vec![Color::BLACK.pack(); 4]);
        let mut c = Canvas::new(16, 16);
        c.clear(Color::WHITE);
        c.draw_image(&img, Rect::new(0.0, 0.0, 16.0, 16.0), 0.0, 1.0);
        assert_eq!(c.get(8, 8), Color::BLACK);
    }
}
