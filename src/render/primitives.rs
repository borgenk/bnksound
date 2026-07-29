//! Clipped drawing primitives over a borrowed pixel slice.
//!
//! A Painter carries a clip rectangle and composites into a plain &mut [u32],
//! so the same code draws the owned headless buffer and wl_shm memory. Every
//! primitive intersects the clip, so drawing out of bounds is a no-op rather
//! than a panic.
//!
//! Callers work in logical pixels; the painter holds the output scale and is the
//! single place logical turns into device. Layout, hit testing, and input all
//! stay at scale 1, and a HiDPI window differs only in how big the buffer is.

use crate::render::buffer::{Color, over};

/// An integer rectangle in device pixels. A non-positive width or height is
/// treated as empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Whether device point (px, py) falls inside the rectangle.
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    /// The overlap of two rectangles, possibly empty.
    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    /// This rectangle shrunk by `d` on every side.
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
}

/// A clipped compositing surface over a borrowed pixel slice.
///
/// Public geometry is logical; `clip`, `width`, and `height` are device pixels,
/// and [`Painter::device`] is the one crossing between them.
pub struct Painter<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    clip: Rect,
    scale: f32,
}

impl<'a> Painter<'a> {
    /// A painter over `pixels` (width*height words) at scale 1, clip set to the
    /// bounds. The slice length must be at least width*height.
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        Self::scaled(pixels, width, height, 1.0)
    }

    /// A painter whose buffer holds `width` by `height` device pixels for a
    /// logical size `scale` times smaller.
    pub fn scaled(pixels: &'a mut [u32], width: u32, height: u32, scale: f32) -> Self {
        let (w, h) = (width as i32, height as i32);
        debug_assert!(pixels.len() >= (width as usize) * (height as usize));
        Painter {
            pixels,
            width: w,
            height: h,
            clip: Rect::new(0, 0, w, h),
            scale: if scale > 0.0 { scale } else { 1.0 },
        }
    }

    /// The output scale: device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// A logical rectangle in device pixels. Edges round to nearest so adjacent
    /// rectangles keep sharing an edge and no seam opens between them.
    pub fn device(&self, rect: Rect) -> Rect {
        if self.scale == 1.0 {
            return rect;
        }
        let px = |v: i32| (v as f32 * self.scale).round() as i32;
        let (x, y) = (px(rect.x), px(rect.y));
        Rect::new(x, y, px(rect.right()) - x, px(rect.bottom()) - y)
    }

    /// The buffer bounds, in logical pixels.
    pub fn bounds(&self) -> Rect {
        let logical = |v: i32| (v as f32 / self.scale).round() as i32;
        Rect::new(0, 0, logical(self.width), logical(self.height))
    }

    /// The active clip rectangle, in device pixels.
    pub fn clip(&self) -> Rect {
        self.clip
    }

    /// Whether a logical rectangle has any pixel inside the clip. Drawing it
    /// when this is false paints nothing, so a caller with work to do first can
    /// stop here instead.
    pub fn intersects(&self, rect: Rect) -> bool {
        !self.device(rect).intersect(self.clip).is_empty()
    }

    /// A sub-painter over the same pixels, clipped to the overlap of the
    /// current clip and `rect`. Drawing through it never escapes either.
    pub fn clipped(&mut self, rect: Rect) -> Painter<'_> {
        let clip = self.clip.intersect(self.device(rect));
        Painter {
            pixels: &mut *self.pixels,
            width: self.width,
            height: self.height,
            clip,
            scale: self.scale,
        }
    }

    /// Composite `color` onto one device pixel, honoring the clip. Glyph blits
    /// come through here already scaled.
    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Color) {
        if color.a == 0 || !self.clip.contains(x, y) {
            return;
        }
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.pixels[idx] = over(self.pixels[idx], color);
    }

    /// Fill a logical rectangle, blending `color` (opaque colors overwrite).
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let r = self.device(rect).intersect(self.clip);
        self.fill_device(r, color);
    }

    /// Fill an already-scaled, already-clipped rectangle.
    fn fill_device(&mut self, r: Rect, color: Color) {
        if r.is_empty() || color.a == 0 {
            return;
        }
        let stride = self.width as usize;
        if color.a == 255 {
            let word = color.to_opaque_u32();
            for y in r.y..r.bottom() {
                let row = y as usize * stride;
                self.pixels[row + r.x as usize..row + r.right() as usize].fill(word);
            }
        } else {
            for y in r.y..r.bottom() {
                let row = y as usize * stride;
                for x in r.x..r.right() {
                    let idx = row + x as usize;
                    self.pixels[idx] = over(self.pixels[idx], color);
                }
            }
        }
    }

    /// A horizontal line `len` pixels wide, one pixel tall.
    pub fn hline(&mut self, x: i32, y: i32, len: i32, color: Color) {
        self.fill(Rect::new(x, y, len, 1), color);
    }

    /// A vertical line `len` pixels tall, one pixel wide.
    pub fn vline(&mut self, x: i32, y: i32, len: i32, color: Color) {
        self.fill(Rect::new(x, y, 1, len), color);
    }

    /// A `thickness`-pixel border inside the edges of `rect`.
    pub fn stroke_rect(&mut self, rect: Rect, thickness: i32, color: Color) {
        if rect.is_empty() || thickness <= 0 {
            return;
        }
        let t = thickness.min(rect.w).min(rect.h);
        self.fill(Rect::new(rect.x, rect.y, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.y, t, rect.h), color);
        self.fill(Rect::new(rect.right() - t, rect.y, t, rect.h), color);
    }

    /// A filled rounded rectangle with anti-aliased corners. `radius` is clamped
    /// to half the shorter side; radius 0 is a plain fill.
    pub fn rounded_rect(&mut self, rect: Rect, radius: i32, color: Color) {
        if rect.is_empty() || color.a == 0 {
            return;
        }
        let rect = self.device(rect);
        let radius = ((radius as f32 * self.scale).round() as i32).clamp(0, rect.w.min(rect.h) / 2);
        if radius == 0 {
            let r = rect.intersect(self.clip);
            self.fill_device(r, color);
            return;
        }
        let r = rect.intersect(self.clip);
        if r.is_empty() {
            return;
        }
        let rf = radius as f32;
        // Corner circle centers, at pixel-center coordinates.
        let cl = rect.x as f32 + rf;
        let cr = rect.right() as f32 - rf;
        let ct = rect.y as f32 + rf;
        let cb = rect.bottom() as f32 - rf;
        for y in r.y..r.bottom() {
            let py = y as f32 + 0.5;
            for x in r.x..r.right() {
                let px = x as f32 + 0.5;
                // Only the four corner squares need coverage; the rest is solid.
                let cx = if px < cl {
                    cl
                } else if px > cr {
                    cr
                } else {
                    px
                };
                let cy = if py < ct {
                    ct
                } else if py > cb {
                    cb
                } else {
                    py
                };
                let cov = if cx == px && cy == py {
                    1.0
                } else {
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    (rf - d + 0.5).clamp(0.0, 1.0)
                };
                if cov <= 0.0 {
                    continue;
                }
                let idx = y as usize * self.width as usize + x as usize;
                let a = (cov * 255.0 + 0.5) as u8;
                self.pixels[idx] = over(self.pixels[idx], color.scale_alpha(a));
            }
        }
    }

    /// An anti-aliased outline just inside the edge of a rounded rectangle.
    ///
    /// [`Painter::stroke_rect`] draws square corners, so a rounded fill given a
    /// stroked border ends up with corners that disagree. This traces the same
    /// shape [`Painter::rounded_rect`] fills.
    pub fn rounded_stroke(&mut self, rect: Rect, radius: i32, thickness: f32, color: Color) {
        if rect.is_empty() || color.a == 0 || thickness <= 0.0 {
            return;
        }
        let d = self.device(rect);
        let area = d.intersect(self.clip);
        if area.is_empty() {
            return;
        }
        let rad = (radius as f32 * self.scale).clamp(0.0, d.w.min(d.h) as f32 / 2.0);
        let t = (thickness * self.scale).max(1.0);

        let (cx, cy) = (d.x as f32 + d.w as f32 / 2.0, d.y as f32 + d.h as f32 / 2.0);
        let (hx, hy) = (d.w as f32 / 2.0 - rad, d.h as f32 / 2.0 - rad);

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Signed distance to the rounded rectangle's edge: negative
                // inside, zero on it.
                let qx = (px - cx).abs() - hx;
                let qy = (py - cy).abs() - hy;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let edge = outside + qx.max(qy).min(0.0) - rad;
                // The border sits inside the edge, spanning -t..0.
                let cov = (t / 2.0 - (edge + t / 2.0).abs() + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let idx = y as usize * self.width as usize + x as usize;
                let a = (cov * 255.0 + 0.5) as u8;
                self.pixels[idx] = over(self.pixels[idx], color.scale_alpha(a));
            }
        }
    }

    /// A `thickness`-wide anti-aliased line between two logical points.
    ///
    /// Coverage comes from each pixel's distance to the segment, which keeps a
    /// diagonal smooth. Window-button glyphs are drawn with this rather than
    /// typeset, because the box-drawing characters they would need are missing
    /// from most system fonts and come out as tofu.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thickness: f32, color: Color) {
        if color.a == 0 || thickness <= 0.0 {
            return;
        }
        let s = self.scale;
        let (ax, ay) = (x0 as f32 * s, y0 as f32 * s);
        let (bx, by) = (x1 as f32 * s, y1 as f32 * s);
        let half = thickness * s / 2.0;

        // Only the segment's bounding box, grown by the half-width, can be lit.
        let pad = half.ceil() + 1.0;
        let bounds = Rect::new(
            (ax.min(bx) - pad) as i32,
            (ay.min(by) - pad) as i32,
            (ax.max(bx) - ax.min(bx) + 2.0 * pad) as i32,
            (ay.max(by) - ay.min(by) + 2.0 * pad) as i32,
        );
        let r = bounds.intersect(self.clip);
        if r.is_empty() {
            return;
        }

        let (dx, dy) = (bx - ax, by - ay);
        let len_sq = dx * dx + dy * dy;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Distance to the segment: project onto it, clamped to its ends.
                let t = if len_sq > 0.0 {
                    (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (cx, cy) = (ax + t * dx, ay + t * dy);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let cov = (half - d + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let idx = y as usize * self.width as usize + x as usize;
                let a = (cov * 255.0 + 0.5) as u8;
                self.pixels[idx] = over(self.pixels[idx], color.scale_alpha(a));
            }
        }
    }

    /// A filled triangle from three logical points, scanline filled with
    /// anti-aliased edges via 2x2 supersampling.
    pub fn triangle(&mut self, pts: [(i32, i32); 3], color: Color) {
        if color.a == 0 {
            return;
        }
        let s = self.scale;
        let p: [(f32, f32); 3] = pts.map(|(x, y)| (x as f32 * s, y as f32 * s));
        let bounds = Rect::new(
            p.iter().map(|q| q.0).fold(f32::MAX, f32::min) as i32 - 1,
            p.iter().map(|q| q.1).fold(f32::MAX, f32::min) as i32 - 1,
            0,
            0,
        );
        let max_x = p.iter().map(|q| q.0).fold(f32::MIN, f32::max) as i32 + 2;
        let max_y = p.iter().map(|q| q.1).fold(f32::MIN, f32::max) as i32 + 2;
        let bounds = Rect::new(bounds.x, bounds.y, max_x - bounds.x, max_y - bounds.y);
        let r = bounds.intersect(self.clip);
        if r.is_empty() {
            return;
        }

        // The sign of the cross product against each edge; inside is where all
        // three agree.
        let edge = |a: (f32, f32), b: (f32, f32), q: (f32, f32)| {
            (b.0 - a.0) * (q.1 - a.1) - (b.1 - a.1) * (q.0 - a.0)
        };
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let mut hits = 0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let q = (x as f32 + ox, y as f32 + oy);
                    let (e0, e1, e2) = (
                        edge(p[0], p[1], q),
                        edge(p[1], p[2], q),
                        edge(p[2], p[0], q),
                    );
                    if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0)
                        || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0)
                    {
                        hits += 1;
                    }
                }
                if hits == 0 {
                    continue;
                }
                let idx = y as usize * self.width as usize + x as usize;
                let a = (hits as f32 / 4.0 * 255.0 + 0.5) as u8;
                self.pixels[idx] = over(self.pixels[idx], color.scale_alpha(a));
            }
        }
    }

    /// Composite `color` through an 8-bit coverage mask (`w` by `h`, row-major)
    /// placed at device (x, y). Glyphs rasterize at the output scale, so this
    /// takes device coordinates rather than logical ones.
    pub fn blit_coverage(&mut self, x: i32, y: i32, w: i32, h: i32, mask: &[u8], color: Color) {
        if color.a == 0 || w <= 0 || h <= 0 {
            return;
        }
        debug_assert!(mask.len() >= (w * h) as usize);
        for row in 0..h {
            let mrow = (row * w) as usize;
            for col in 0..w {
                let cov = mask[mrow + col as usize];
                if cov == 0 {
                    continue;
                }
                self.blend_pixel(x + col, y + row, color.scale_alpha(cov));
            }
        }
    }

    /// Composite a straight-alpha ARGB image (`w` by `h`, row-major) placed at
    /// device (x, y). Icons decode and resample at the output scale, so like
    /// glyphs they arrive in device coordinates.
    pub fn blit_argb(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u32]) {
        if w <= 0 || h <= 0 {
            return;
        }
        debug_assert!(src.len() >= (w * h) as usize);
        for row in 0..h {
            let srow = (row * w) as usize;
            for col in 0..w {
                let px = src[srow + col as usize];
                let a = (px >> 24) as u8;
                if a == 0 {
                    continue;
                }
                let color = Color::rgba((px >> 16) as u8, (px >> 8) as u8, px as u8, a);
                self.blend_pixel(x + col, y + row, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::PixelBuffer;

    fn at(buf: &PixelBuffer, x: i32, y: i32) -> u32 {
        buf.pixels()[y as usize * buf.width() as usize + x as usize]
    }

    #[test]
    fn rect_intersect_and_contains() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(b), Rect::new(5, 5, 5, 5));
        assert!(a.intersect(Rect::new(20, 20, 1, 1)).is_empty());
        assert!(a.contains(0, 0));
        assert!(!a.contains(10, 0));
    }

    #[test]
    fn fill_clips_to_bounds_without_panicking() {
        let mut buf = PixelBuffer::new(4, 4);
        // A rect spilling past every edge fills only the buffer.
        buf.painter()
            .fill(Rect::new(-2, -2, 100, 100), Color::rgb(1, 2, 3));
        for p in buf.pixels() {
            assert_eq!(*p, 0xff01_0203);
        }
    }

    #[test]
    fn fill_respects_clip() {
        let mut buf = PixelBuffer::new(8, 8);
        buf.painter()
            .clipped(Rect::new(2, 2, 3, 3))
            .fill(Rect::new(0, 0, 8, 8), Color::rgb(255, 0, 0));
        assert_eq!(at(&buf, 0, 0), 0xff00_0000); // outside clip, untouched
        assert_eq!(at(&buf, 2, 2), 0xffff_0000); // inside clip
        assert_eq!(at(&buf, 4, 4), 0xffff_0000);
        assert_eq!(at(&buf, 5, 5), 0xff00_0000); // just past clip
    }

    #[test]
    fn blended_fill_darkens_toward_source() {
        let mut buf = PixelBuffer::new(2, 2);
        buf.clear(Color::rgb(0, 0, 0));
        buf.painter()
            .fill(Rect::new(0, 0, 2, 2), Color::rgba(255, 255, 255, 128));
        let r = (at(&buf, 0, 0) >> 16) & 0xff;
        assert!((127..=129).contains(&r), "r was {r}");
    }

    #[test]
    fn stroke_rect_draws_four_edges_only() {
        let mut buf = PixelBuffer::new(6, 6);
        buf.painter()
            .stroke_rect(Rect::new(1, 1, 4, 4), 1, Color::rgb(0, 255, 0));
        assert_eq!(at(&buf, 1, 1), 0xff00_ff00); // corner on the border
        assert_eq!(at(&buf, 4, 4), 0xff00_ff00); // opposite corner
        assert_eq!(at(&buf, 2, 2), 0xff00_0000); // interior untouched
    }

    #[test]
    fn rounded_rect_softens_corners_but_fills_center() {
        let mut buf = PixelBuffer::new(20, 20);
        buf.painter()
            .rounded_rect(Rect::new(0, 0, 20, 20), 6, Color::rgb(255, 255, 255));
        // Center is fully filled.
        assert_eq!(at(&buf, 10, 10), 0xffff_ffff);
        // The extreme corner pixel is not fully covered (rounded away).
        assert!((at(&buf, 0, 0) & 0xff) < 0xff, "corner should be partial");
    }

    /// The whole point of the scale: callers keep speaking logical pixels and
    /// the ink lands on the matching device pixels.
    #[test]
    fn a_scaled_painter_maps_logical_rects_onto_device_pixels() {
        let mut buf = PixelBuffer::new(8, 8);
        {
            let (pixels, w, h) = buf.parts();
            let mut p = Painter::scaled(pixels, w, h, 2.0);
            // The logical buffer is 4x4, so this is its top-left quarter.
            assert_eq!(p.bounds(), Rect::new(0, 0, 4, 4));
            p.fill(Rect::new(0, 0, 2, 2), Color::rgb(255, 0, 0));
        }
        assert_eq!(at(&buf, 0, 0), 0xffff_0000);
        assert_eq!(at(&buf, 3, 3), 0xffff_0000, "2 logical px is 4 device px");
        assert_eq!(at(&buf, 4, 4), 0xff00_0000, "and no further");
    }

    /// Adjacent logical rectangles must stay adjacent once scaled, or a seam of
    /// background shows through between them.
    #[test]
    fn abutting_rects_leave_no_seam_at_a_fractional_scale() {
        let mut buf = PixelBuffer::new(32, 8);
        {
            let (pixels, w, h) = buf.parts();
            let mut p = Painter::scaled(pixels, w, h, 1.25);
            let white = Color::rgb(255, 255, 255);
            for i in 0..5 {
                p.fill(Rect::new(i * 5, 0, 5, 4), white);
            }
        }
        // 25 logical px at 1.25 covers 31 device columns with no gap.
        for x in 0..31 {
            assert_eq!(at(&buf, x, 0), 0xffff_ffff, "column {x} should be filled");
        }
    }

    #[test]
    fn a_clip_on_a_scaled_painter_is_scaled_too() {
        let mut buf = PixelBuffer::new(8, 8);
        {
            let (pixels, w, h) = buf.parts();
            let mut p = Painter::scaled(pixels, w, h, 2.0);
            p.clipped(Rect::new(1, 1, 1, 1))
                .fill(Rect::new(0, 0, 4, 4), Color::rgb(0, 255, 0));
        }
        assert_eq!(at(&buf, 1, 1), 0xff00_0000, "outside the scaled clip");
        assert_eq!(at(&buf, 2, 2), 0xff00_ff00, "inside it");
        assert_eq!(at(&buf, 3, 3), 0xff00_ff00);
        assert_eq!(at(&buf, 4, 4), 0xff00_0000, "just past it");
    }

    /// A rounded outline has to leave the corners open and the middle hollow,
    /// or it is just stroke_rect with extra steps.
    #[test]
    fn rounded_stroke_traces_the_edge_and_leaves_the_corners_and_centre_clear() {
        let mut buf = PixelBuffer::new(24, 24);
        buf.painter()
            .rounded_stroke(Rect::new(2, 2, 20, 20), 6, 1.0, Color::rgb(255, 255, 255));

        // The extreme corner is outside the rounded shape.
        assert_eq!(at(&buf, 2, 2), 0xff00_0000, "corner is cut away");
        // Mid-edge is on the border.
        let top = at(&buf, 12, 2) & 0xff;
        assert!(top > 0x80, "top edge should be drawn, got {top:#x}");
        let left = at(&buf, 2, 12) & 0xff;
        assert!(left > 0x80, "left edge should be drawn, got {left:#x}");
        // The interior stays empty: this is an outline, not a fill.
        assert_eq!(at(&buf, 12, 12), 0xff00_0000, "centre is not filled");
    }

    /// The outline sits inside the rectangle, so it cannot bleed onto whatever
    /// is drawn next to it.
    #[test]
    fn rounded_stroke_stays_within_its_rectangle() {
        let mut buf = PixelBuffer::new(16, 16);
        buf.painter()
            .rounded_stroke(Rect::new(4, 4, 8, 8), 3, 1.0, Color::rgb(255, 0, 0));
        for x in 0..16 {
            for y in 0..16 {
                let inside = (4..12).contains(&x) && (4..12).contains(&y);
                if !inside {
                    assert_eq!(at(&buf, x, y), 0xff00_0000, "ink escaped at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn blit_coverage_scales_alpha() {
        let mut buf = PixelBuffer::new(2, 1);
        buf.clear(Color::rgb(0, 0, 0));
        // Left pixel full coverage, right pixel half.
        let mask = [255u8, 128u8];
        buf.painter()
            .blit_coverage(0, 0, 2, 1, &mask, Color::rgb(255, 255, 255));
        assert_eq!(at(&buf, 0, 0), 0xffff_ffff);
        let r = (at(&buf, 1, 0) >> 16) & 0xff;
        assert!((127..=129).contains(&r), "r was {r}");
    }
}
