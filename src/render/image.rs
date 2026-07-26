//! Application icons.
//!
//! An app row draws the PNG the freedesktop lookup resolved, decoded once and
//! resampled to the row's device size behind [`IconCache`]. Anything that does
//! not decode (SVG, XPM, a missing or malformed file) falls back to a rounded
//! square tinted from the app's name with its initial centered on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::platform::pixel::{
    premultiplied_from_straight, resample_argb, straight_from_premultiplied,
};
use crate::render::buffer::Color;
use crate::render::png;
use crate::render::primitives::{Painter, Rect};
use crate::render::text::{Font, TextStyle};

/// A decoded icon, already scaled to the size it draws at. ARGB8888 words with
/// straight alpha, row-major.
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

/// One path's decode result at the size it was built for. A failed decode is
/// kept as `None` so a broken or non-PNG file is not re-read every frame.
struct IconEntry {
    side: u32,
    image: Option<IconImage>,
}

/// Decoded icons keyed by file path. Lives with the window and outlives a
/// frame, so a repaint costs a hash lookup rather than a decode.
#[derive(Default)]
pub struct IconCache {
    entries: HashMap<PathBuf, IconEntry>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The icon at `path` fitted to a `side` by `side` square, or `None` when it
    /// does not decode. Entries rebuild when the output scale changes `side`.
    fn get(&mut self, path: &Path, side: u32) -> Option<&IconImage> {
        if self.entries.get(path).is_none_or(|e| e.side != side) {
            let image = load(path, side);
            self.entries
                .insert(path.to_path_buf(), IconEntry { side, image });
        }
        self.entries.get(path)?.image.as_ref()
    }
}

/// Read and decode `path`, scaled to fit a `side` by `side` square with its
/// aspect kept.
fn load(path: &Path, side: u32) -> Option<IconImage> {
    if side == 0 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let src = png::decode(&bytes).ok()?;
    if src.width == 0 || src.height == 0 {
        return None;
    }

    let fit = (side as f32 / src.width as f32).min(side as f32 / src.height as f32);
    let width = ((src.width as f32 * fit).round() as u32).max(1);
    let height = ((src.height as f32 * fit).round() as u32).max(1);
    let pixels = resample(&src.pixels, src.width, src.height, width, height);
    if pixels.is_empty() {
        return None;
    }
    Some(IconImage {
        width,
        height,
        pixels,
    })
}

/// Scale `src` to `dw` by `dh`, straight alpha in and out. Filtering happens in
/// premultiplied space, since a fully transparent pixel usually carries black
/// RGB and averaging that straight would pull a dark fringe around the icon's
/// edges. Equal sizes copy, which keeps translucent pixels exact rather than
/// letting them drift through the premultiply round trip.
fn resample(src: &[u32], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u32> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < (sw * sh) as usize {
        return Vec::new();
    }
    if (dw, dh) == (sw, sh) {
        return src[..(sw * sh) as usize].to_vec();
    }
    let premultiplied: Vec<u32> = src
        .iter()
        .copied()
        .map(premultiplied_from_straight)
        .collect();
    resample_argb(&premultiplied, sw, sh, dw as i32, dh as i32)
        .into_iter()
        .map(straight_from_premultiplied)
        .collect()
}

/// Draw the app icon filling `rect`: the resolved PNG when it decodes, else the
/// tinted fallback tile.
pub fn draw_icon(
    p: &mut Painter,
    rect: Rect,
    path: Option<&Path>,
    label: &str,
    font: &Font,
    radius: i32,
    cache: &mut IconCache,
) {
    let device = p.device(rect);
    let side = device.w.min(device.h).max(0) as u32;
    if let Some(path) = path
        && let Some(icon) = cache.get(path, side)
    {
        // Centred, since keeping the aspect can leave one axis short.
        let x = device.x + (device.w - icon.width as i32) / 2;
        let y = device.y + (device.h - icon.height as i32) / 2;
        p.blit_argb(x, y, icon.width as i32, icon.height as i32, &icon.pixels);
        return;
    }
    draw_fallback_icon(p, rect, label, font, radius);
}

/// A stable background tint for a label: the same name always maps to the same
/// color, so an app's fallback icon does not flicker between frames or launches.
pub fn fallback_color(label: &str) -> Color {
    // FNV-1a over the bytes, then a hue on a muted wheel.
    let mut h: u32 = 0x811c_9dc5;
    for b in label.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let hue = (h % 360) as f32;
    hsl_to_rgb(hue, 0.42, 0.46)
}

/// The initial to show on a fallback icon: the first alphanumeric char,
/// uppercased, or a placeholder when the name has none.
pub fn icon_initial(label: &str) -> char {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('?')
}

/// Draw a fallback icon filling `rect`: a tinted rounded square with the label's
/// initial centered on it.
pub fn draw_fallback_icon(p: &mut Painter, rect: Rect, label: &str, font: &Font, radius: i32) {
    p.rounded_rect(rect, radius, fallback_color(label));

    let mut buf = [0u8; 4];
    let initial = icon_initial(label).encode_utf8(&mut buf);
    let size = rect.h as f32 * 0.5;
    let style = TextStyle::new(size, Color::rgb(0xff, 0xff, 0xff));
    let tw = font.text_width(initial, style).round() as i32;
    let th = font.text_height(size) as i32;
    let tx = rect.x + (rect.w - tw) / 2;
    let ty = rect.y + (rect.h - th) / 2;
    font.draw_text(p, tx, ty, initial, style, rect.w);
}

/// HSL (h in 0..360, s and l in 0..=1) to an opaque color.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::rgb(to_u8(r1), to_u8(g1), to_u8(b1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::PixelBuffer;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TmpDir(PathBuf);
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fresh_tmp() -> TmpDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bnk_image_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        TmpDir(dir)
    }

    /// An opaque single-colour PNG on disk, for the decode path.
    fn write_png(dir: &Path, name: &str, side: u32, color: u32) -> PathBuf {
        let path = dir.join(name);
        let pixels = vec![color; (side * side) as usize];
        std::fs::write(&path, png::encode_rgb(&pixels, side, side)).expect("write png");
        path
    }

    fn argb(a: u32, r: u32, g: u32, b: u32) -> u32 {
        (a << 24) | (r << 16) | (g << 8) | b
    }

    #[test]
    fn resample_to_the_same_size_is_a_copy() {
        let src = vec![
            argb(255, 1, 2, 3),
            argb(128, 4, 5, 6),
            0,
            argb(255, 7, 8, 9),
        ];
        assert_eq!(resample(&src, 2, 2, 2, 2), src);
    }

    #[test]
    fn resample_averages_an_opaque_block() {
        // Four opaque pixels averaging to 100 in every channel.
        let src = vec![
            argb(255, 40, 40, 40),
            argb(255, 80, 80, 80),
            argb(255, 120, 120, 120),
            argb(255, 160, 160, 160),
        ];
        let out = resample(&src, 2, 2, 1, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0] >> 24, 255, "opaque input stays opaque");
        assert_eq!(out[0] & 0xff, 100);
    }

    #[test]
    fn resample_weights_colour_by_alpha() {
        // One opaque white beside a transparent black. Averaging straight would
        // halve the colour to grey; weighting by alpha keeps it white and only
        // drops the alpha.
        let src = vec![argb(255, 255, 255, 255), argb(0, 0, 0, 0)];
        let out = resample(&src, 2, 1, 1, 1);
        assert_eq!(out[0] >> 24, 128, "alpha is the mean of 255 and 0");
        assert_eq!(out[0] & 0xff_ffff, 0xff_ffff, "colour must not darken");
    }

    #[test]
    fn resample_fully_transparent_stays_clear() {
        let src = vec![0u32; 4];
        assert_eq!(resample(&src, 2, 2, 1, 1), vec![0]);
    }

    #[test]
    fn resample_rejects_degenerate_sizes() {
        let src = vec![argb(255, 1, 2, 3)];
        assert!(resample(&src, 0, 1, 1, 1).is_empty());
        assert!(resample(&src, 1, 1, 0, 1).is_empty());
        // Source shorter than its declared dimensions.
        assert!(resample(&src, 4, 4, 2, 2).is_empty());
    }

    #[test]
    fn resample_upscale_covers_every_destination_pixel() {
        let src = vec![argb(255, 10, 20, 30)];
        let out = resample(&src, 1, 1, 3, 3);
        assert_eq!(out.len(), 9);
        assert!(out.iter().all(|&p| p == argb(255, 10, 20, 30)));
    }

    #[test]
    fn cache_decodes_once_and_scales_to_the_requested_side() {
        let tmp = fresh_tmp();
        let path = write_png(&tmp.0, "app.png", 48, 0xff20_4060);
        let mut cache = IconCache::new();

        let icon = cache.get(&path, 24).expect("decodes");
        assert_eq!((icon.width, icon.height), (24, 24));
        cache.get(&path, 24).expect("still decodes");
        assert_eq!(cache.entries.len(), 1, "second get must not add an entry");

        // A scale change rebuilds the entry at the new side.
        let icon = cache.get(&path, 48).expect("decodes");
        assert_eq!((icon.width, icon.height), (48, 48));
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn cache_remembers_a_failed_decode() {
        let tmp = fresh_tmp();
        // Stands in for the SVG and XPM the lookup can also return.
        let path = tmp.0.join("app.svg");
        std::fs::write(&path, "<svg/>").expect("write svg");

        let mut cache = IconCache::new();
        assert!(cache.get(&path, 24).is_none());
        assert!(cache.get(&path, 24).is_none());
        assert_eq!(cache.entries.len(), 1, "failure is cached, not retried");
    }

    #[test]
    fn cache_misses_a_path_that_is_not_there() {
        let mut cache = IconCache::new();
        assert!(cache.get(Path::new("/nonexistent/app.png"), 24).is_none());
    }

    #[test]
    fn load_keeps_the_aspect_of_a_non_square_icon() {
        let tmp = fresh_tmp();
        let path = tmp.0.join("wide.png");
        let pixels = vec![0xff00_ff00u32; 40 * 20];
        std::fs::write(&path, png::encode_rgb(&pixels, 40, 20)).expect("write png");

        let icon = load(&path, 20).expect("decodes");
        assert_eq!((icon.width, icon.height), (20, 10));
    }

    #[test]
    fn draw_icon_paints_the_file_and_falls_back_without_one() {
        let tmp = fresh_tmp();
        let path = write_png(&tmp.0, "app.png", 28, 0xff20_4060);
        let mut cache = IconCache::new();
        let f = font();

        let mut buf = PixelBuffer::new(28, 28);
        draw_icon(
            &mut buf.painter(),
            Rect::new(0, 0, 28, 28),
            Some(&path),
            "Files",
            &f,
            ICON_RADIUS_FOR_TEST,
            &mut cache,
        );
        assert_eq!(
            buf.pixels()[14 * 28 + 14],
            0xff20_4060,
            "the icon's own colour should reach the centre"
        );

        // With no path the tinted tile takes over, which is a different colour.
        let mut buf = PixelBuffer::new(28, 28);
        draw_icon(
            &mut buf.painter(),
            Rect::new(0, 0, 28, 28),
            None,
            "Files",
            &f,
            ICON_RADIUS_FOR_TEST,
            &mut cache,
        );
        assert_eq!(
            buf.pixels()[14 * 28 + 14],
            fallback_color("Files").to_opaque_u32()
        );
    }

    /// Matches the painter's ICON_RADIUS; the fallback tile is rounded, the
    /// decoded icon is not.
    const ICON_RADIUS_FOR_TEST: i32 = 6;

    fn font() -> Font {
        Font::from_path(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/test-font.ttf"
        )))
        .expect("fixture font")
    }

    #[test]
    fn fallback_color_is_deterministic_and_varies() {
        assert_eq!(fallback_color("Spotify"), fallback_color("Spotify"));
        assert_ne!(fallback_color("Spotify"), fallback_color("Firefox"));
    }

    #[test]
    fn icon_initial_picks_first_alphanumeric_uppercased() {
        assert_eq!(icon_initial("firefox"), 'F');
        assert_eq!(icon_initial("  spotify"), 'S');
        assert_eq!(icon_initial("· YouTube"), 'Y');
        assert_eq!(icon_initial("123"), '1');
        assert_eq!(icon_initial(""), '?');
        assert_eq!(icon_initial("··"), '?');
    }

    #[test]
    fn hsl_endpoints_are_sane() {
        // Zero saturation is a gray at the lightness.
        let gray = hsl_to_rgb(0.0, 0.0, 0.5);
        assert_eq!(gray.r, gray.g);
        assert_eq!(gray.g, gray.b);
    }

    #[test]
    fn draw_fallback_icon_paints_the_square() {
        let f = font();
        let mut buf = PixelBuffer::new(28, 28);
        draw_fallback_icon(&mut buf.painter(), Rect::new(0, 0, 28, 28), "Files", &f, 6);
        // The tint fills the center, and the glyph leaves at least one bright
        // pixel distinct from the background.
        let center = buf.pixels()[14 * 28 + 14];
        assert_ne!(center, 0xff00_0000, "icon square should be tinted");
        assert!(
            buf.pixels()
                .iter()
                .any(|&p| p != center && p != 0xff00_0000)
        );
    }
}
