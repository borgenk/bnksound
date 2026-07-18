//! The canonical pixel buffer and color type.
//!
//! Pixels are 32-bit ARGB8888 packed as 0xAARRGGBB in a native-endian u32. On
//! little-endian that is byte order B, G, R, A, which maps with no conversion
//! to wl_shm ARGB8888 and to a GTK memory texture. Frames are opaque (every
//! final pixel has alpha 0xff), so premultiplied and straight alpha coincide in
//! the stored buffer; alpha only ever appears mid-composite, in [`Color`].

/// A straight-alpha RGBA color. Alpha weights compositing; stored pixels end up
/// opaque once painted over an opaque background.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Opaque color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }

    /// Color with explicit alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    /// This color as an opaque 0xAARRGGBB word (alpha forced to 0xff).
    pub const fn to_opaque_u32(self) -> u32 {
        0xff00_0000 | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// This color scaled by an extra coverage factor (0..=255), e.g. a glyph's
    /// per-pixel alpha. Combines with the color's own alpha.
    pub const fn scale_alpha(self, coverage: u8) -> Self {
        let a = ((self.a as u32 * coverage as u32 + 127) / 255) as u8;
        Color { a, ..self }
    }
}

/// Composite `src` over an opaque destination word, returning an opaque word.
/// The window is opaque, so the result alpha is always 0xff.
#[inline]
pub fn over(dst: u32, src: Color) -> u32 {
    match src.a {
        0 => dst,
        255 => src.to_opaque_u32(),
        sa => {
            let sa = sa as u32;
            let inv = 255 - sa;
            let dr = (dst >> 16) & 0xff;
            let dg = (dst >> 8) & 0xff;
            let db = dst & 0xff;
            let r = (src.r as u32 * sa + dr * inv + 127) / 255;
            let g = (src.g as u32 * sa + dg * inv + 127) / 255;
            let b = (src.b as u32 * sa + db * inv + 127) / 255;
            0xff00_0000 | (r << 16) | (g << 8) | b
        }
    }
}

/// An owned ARGB8888 frame buffer, row-major with stride equal to width.
pub struct PixelBuffer {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl PixelBuffer {
    /// A buffer of `width` by `height` pixels, cleared to opaque black.
    pub fn new(width: u32, height: u32) -> Self {
        let len = width as usize * height as usize;
        PixelBuffer {
            width,
            height,
            pixels: vec![0xff00_0000; len],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw pixel words, for uploading to wl_shm or a GTK texture.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// The same pixels as bytes, for texture uploads that take a byte slice. On
    /// little-endian the byte order is B, G, R, A.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: u32 has no padding and no invalid bit patterns, so any slice
        // of them is also a valid byte slice four times as long. The lifetime
        // is tied to the borrow, so the pixels outlive the view.
        unsafe {
            std::slice::from_raw_parts(self.pixels.as_ptr().cast::<u8>(), self.pixels.len() * 4)
        }
    }

    /// The pixels and dimensions together, so a painter can borrow the buffer
    /// mutably without the size fields being borrowed with it.
    pub fn parts(&mut self) -> (&mut [u32], u32, u32) {
        (&mut self.pixels, self.width, self.height)
    }

    /// Resize in place, reallocating only when the pixel count changes. Contents
    /// are left undefined; the caller repaints the whole frame after a resize.
    pub fn resize(&mut self, width: u32, height: u32) {
        let len = width as usize * height as usize;
        if len != self.pixels.len() {
            self.pixels.resize(len, 0xff00_0000);
        }
        self.width = width;
        self.height = height;
    }

    /// Fill the whole buffer with one opaque color.
    pub fn clear(&mut self, color: Color) {
        let word = color.to_opaque_u32();
        self.pixels.fill(word);
    }

    /// A painter over the whole buffer, clip set to its bounds.
    pub fn painter(&mut self) -> crate::render::primitives::Painter<'_> {
        crate::render::primitives::Painter::new(&mut self.pixels, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_over_replaces() {
        assert_eq!(over(0xff00_0000, Color::rgb(0x12, 0x34, 0x56)), 0xff12_3456);
    }

    #[test]
    fn transparent_over_keeps_destination() {
        assert_eq!(over(0xff11_2233, Color::rgba(0, 0, 0, 0)), 0xff11_2233);
    }

    #[test]
    fn half_alpha_is_midpoint() {
        // White at 50% over black lands near mid-gray, result stays opaque.
        let out = over(0xff00_0000, Color::rgba(255, 255, 255, 128));
        assert_eq!(out & 0xff00_0000, 0xff00_0000);
        let r = (out >> 16) & 0xff;
        assert!((127..=129).contains(&r), "r was {r}");
    }

    #[test]
    fn scale_alpha_combines_with_coverage() {
        assert_eq!(Color::rgb(10, 20, 30).scale_alpha(0).a, 0);
        assert_eq!(Color::rgb(10, 20, 30).scale_alpha(255).a, 255);
        assert_eq!(Color::rgba(1, 2, 3, 128).scale_alpha(128).a, 64);
    }

    #[test]
    fn new_is_opaque_black_and_sized() {
        let buf = PixelBuffer::new(4, 3);
        assert_eq!(buf.width(), 4);
        assert_eq!(buf.height(), 3);
        assert_eq!(buf.pixels().len(), 12);
        assert!(buf.pixels().iter().all(|&p| p == 0xff00_0000));
    }

    #[test]
    fn resize_reallocates_only_on_count_change() {
        let mut buf = PixelBuffer::new(4, 4);
        buf.resize(8, 2); // same 16 pixels
        assert_eq!(buf.pixels().len(), 16);
        assert_eq!((buf.width(), buf.height()), (8, 2));
        buf.resize(8, 3);
        assert_eq!(buf.pixels().len(), 24);
    }
}
