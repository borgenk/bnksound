//! Text rendering on top of FreeType.
//!
//! Loads a system TrueType or OpenType font and draws anti-aliased text through
//! the painter, with a glyph cache so a character at a size is rasterized once.
//! Rasterization and metrics come from the freetype module; this owns
//! discovery, the cache, measurement, and the blit.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::platform::emoji::{ColorGlyph, EmojiFont, wants_emoji};
use crate::platform::fontconfig::Fontconfig;
use crate::platform::freetype::Face;
use crate::platform::{fontconfig, grapheme};
use crate::render::buffer::Color;
use crate::render::desktop_font;
use crate::render::primitives::Painter;

/// How a run of text is drawn: its size, color, letter spacing, and weight.
#[derive(Clone, Copy)]
pub struct TextStyle {
    pub size: f32,
    pub color: Color,
    /// Extra pixels added after every glyph. Caps headings use it to breathe;
    /// everything else leaves it at zero.
    pub tracking: f32,
    /// Draw from the bold face. Falls back to the regular one when the family
    /// has no bold, so a missing face costs weight rather than text.
    pub bold: bool,
}

impl TextStyle {
    /// A run at its natural spacing.
    pub const fn new(size: f32, color: Color) -> Self {
        TextStyle {
            size,
            color,
            tracking: 0.0,
            bold: false,
        }
    }

    /// The same run with letters spread apart.
    pub const fn tracked(self, tracking: f32) -> Self {
        TextStyle { tracking, ..self }
    }

    /// The same run in bold.
    pub const fn bold(self) -> Self {
        TextStyle { bold: true, ..self }
    }
}

/// One rasterization's size: the pixel grid FreeType fits outlines to, and the
/// optical size those outlines are drawn for.
///
/// The two come apart under HiDPI, where a run keeps the optical size its
/// logical size asks for while rasterizing at more device pixels. A glyph
/// cached for one cannot stand in for the other, so both belong in the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RasterSize {
    px: u32,
    /// Optical size in 1/64 point, an integer so the cache key stays hashable.
    optical: u32,
}

impl RasterSize {
    /// The size a logical UI size renders at on a surface scaled by `scale`.
    /// The desktop's font DPI turns pixels into the points the optical axis is
    /// measured in.
    fn new(size: f32, scale: f32, dpi: f32) -> Self {
        let points = size * 72.0 / dpi;
        RasterSize {
            px: pixel_size(size * scale),
            optical: (points * 64.0).round().max(0.0) as u32,
        }
    }

    /// Point `face` at this size, pixel grid and optical size together.
    fn apply(self, face: &Face) {
        let _ = face.set_pixel_size(self.px);
        face.set_optical_size(self.optical as f32 / 64.0);
    }
}

/// One rasterized glyph: its metrics and its coverage bitmap.
struct CachedGlyph {
    left: i32,
    top: i32,
    width: usize,
    rows: usize,
    advance: f32,
    coverage: Vec<u8>,
}

/// A loaded font that draws and measures text.
///
/// The cache sits behind a cell because drawing and measuring take the font by
/// shared reference: filling the cache is not a change anyone can observe. The
/// caret's advance and the blit's advance come from the same entry, so they
/// cannot drift apart.
pub struct Font {
    face: Face,
    /// The family's bold face. None when it has none, in which case bold runs
    /// draw from the regular face.
    bold_face: Option<Face>,
    cache: RefCell<HashMap<(RasterSize, char, bool), CachedGlyph>>,
    /// The desktop's font DPI, which turns a UI size into the optical size the
    /// faces are set to.
    dpi: f32,
    /// The color emoji font, opened on first sight of a cluster that needs it.
    /// Lazy because most runs are plain text and the emoji font is a large file
    /// no ASCII label ever reads. The inner `None` records that the machine has
    /// none, so the open is attempted once.
    emoji: OnceCell<Option<EmojiFont>>,
    /// The system's ranked fonts, built on the first character the configured
    /// font cannot draw. A session of plain Latin never builds it.
    discovery: OnceCell<Option<Fontconfig>>,
    /// Which face covers a character, `None` when nothing installed does.
    /// Negative answers are kept so an uncoverable character is asked once.
    fallbacks: RefCell<HashMap<char, Option<Rc<Face>>>>,
    /// Faces opened for fallback, keyed by file. One font serving twenty
    /// characters is opened once.
    opened: RefCell<HashMap<(PathBuf, i32), Rc<Face>>>,
}

impl Font {
    /// Load the desktop's configured UI font, regular and bold.
    ///
    /// Both faces come from fontconfig, so the app draws with the same family
    /// the rest of the desktop uses.
    pub fn load() -> io::Result<Self> {
        let family = desktop_font::family();
        let regular = fontconfig::resolve(&family, fontconfig::WEIGHT_REGULAR)
            .or_else(|| {
                fontconfig::resolve(desktop_font::FALLBACK_FAMILY, fontconfig::WEIGHT_REGULAR)
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("fontconfig found no font for {family:?} or a generic fallback"),
                )
            })?;

        let mut font = Self::from_face(&regular.path, regular.index, desktop_font::dpi())?;
        font.face.set_hinting(regular.hinting);

        // A family with no bold of its own resolves to some other file, which
        // would pair a foreign face with the regular one. Only take the bold
        // when it comes from the same file.
        font.bold_face = fontconfig::resolve(&family, fontconfig::WEIGHT_BOLD)
            .filter(|bold| bold.path == regular.path && bold.index != regular.index)
            .and_then(|bold| {
                let face = load_face(&bold.path, bold.index).ok()?;
                face.set_hinting(regular.hinting);
                Some(face)
            });

        Ok(font)
    }

    /// Load a font from a specific file, with no bold face.
    pub fn from_path(path: &Path) -> io::Result<Self> {
        Self::from_face(path, 0, desktop_font::DEFAULT_DPI)
    }

    /// Load a font from a file and nothing else. A character the file does not
    /// cover rasterizes as .notdef rather than reaching fontconfig or the
    /// colour emoji font: both lookups start already resolved to nothing found,
    /// so neither lazy cell ever asks the system.
    ///
    /// Golden-frame tests render through this, which makes a frame a function
    /// of bytes in the repository rather than of what the machine has
    /// installed.
    pub fn from_path_sealed(path: &Path) -> io::Result<Self> {
        Ok(Font {
            emoji: OnceCell::from(None),
            discovery: OnceCell::from(None),
            ..Self::from_path(path)?
        })
    }

    /// Load one face of a font file.
    fn from_face(path: &Path, index: i32, dpi: f32) -> io::Result<Self> {
        Ok(Font {
            face: load_face(path, index)?,
            bold_face: None,
            cache: RefCell::new(HashMap::new()),
            dpi,
            emoji: OnceCell::new(),
            discovery: OnceCell::new(),
            fallbacks: RefCell::new(HashMap::new()),
            opened: RefCell::new(HashMap::new()),
        })
    }

    /// The face a style draws from.
    fn face_for(&self, bold: bool) -> &Face {
        match (bold, self.bold_face.as_ref()) {
            (true, Some(face)) => face,
            _ => &self.face,
        }
    }

    /// The color emoji font, opened on first use.
    fn emoji(&self) -> Option<&EmojiFont> {
        self.emoji.get_or_init(EmojiFont::open).as_ref()
    }

    /// The size a logical UI size rasterizes at on a surface scaled by `scale`.
    fn raster(&self, size: f32, scale: f32) -> RasterSize {
        RasterSize::new(size, scale, self.dpi)
    }

    /// The color glyph for `cluster` at `px`, or `None` when it is not emoji,
    /// the machine has no emoji font, or that font cannot form the cluster.
    ///
    /// Routing asks the text face whether it covers the character, so symbols
    /// the UI font draws itself (copyright, dagger, arrows) stay text and only
    /// what it genuinely lacks becomes a picture.
    fn color_glyph(&self, cluster: &str, size: RasterSize, bold: bool) -> Option<Rc<ColorGlyph>> {
        let emoji = self.emoji()?;
        let face = self.face_for(bold);
        wants_emoji(cluster, |ch| face.has_glyph(ch)).then_some(())?;
        // A color strike has no outlines, so only the pixel size reaches it.
        emoji.glyph(cluster, size.px)
    }

    /// The advance of one grapheme cluster: its color glyph's when it routes to
    /// emoji, else the sum of its characters'. Combining marks carry a zero
    /// advance, so a base plus its accents measures as the base alone.
    fn cluster_advance(&self, cluster: &str, size: RasterSize, bold: bool) -> f32 {
        if let Some(glyph) = self.color_glyph(cluster, size, bold) {
            return glyph.advance;
        }
        cluster.chars().map(|ch| self.advance(ch, size, bold)).sum()
    }

    /// The face fontconfig says can draw `ch`, opened once per file and
    /// remembered per character. A negative answer is cached too.
    fn fallback_face(&self, ch: char) -> Option<Rc<Face>> {
        if let Some(hit) = self.fallbacks.borrow().get(&ch) {
            return hit.clone();
        }
        let found = self
            .discovery
            .get_or_init(Fontconfig::new)
            .as_ref()
            .and_then(|fc| fc.font_for_char(ch))
            .and_then(|resolved| {
                let key = (resolved.path.clone(), resolved.index);
                if let Some(face) = self.opened.borrow().get(&key) {
                    return Some(face.clone());
                }
                let face = Rc::new(load_face(&resolved.path, resolved.index).ok()?);
                self.opened.borrow_mut().insert(key, face.clone());
                Some(face)
            });
        self.fallbacks.borrow_mut().insert(ch, found.clone());
        found
    }

    /// Rasterize `ch` from whichever face can draw it: the style's own when it
    /// has the glyph, else one fontconfig points at.
    ///
    /// A missing character does not fail to load, it silently rasterizes as
    /// .notdef, so the cmap has to be asked before drawing rather than after.
    /// When nothing installed covers it, the style's face draws that .notdef,
    /// which at least makes the gap visible.
    fn rasterize_for(&self, ch: char, size: RasterSize, bold: bool) -> CachedGlyph {
        let primary = self.face_for(bold);
        if primary.has_glyph(ch) {
            return rasterize(primary, size, ch);
        }
        match self.fallback_face(ch) {
            Some(face) => rasterize(&face, size, ch),
            None => rasterize(primary, size, ch),
        }
    }

    /// Run `f` over one character's cached glyph, rasterizing it on a miss.
    ///
    /// A miss resolves the face first and takes the cache borrow only to
    /// insert, since picking a fallback touches other cells and must not be
    /// done while this one is held.
    fn with_glyph<R>(
        &self,
        ch: char,
        size: RasterSize,
        bold: bool,
        f: impl FnOnce(&CachedGlyph) -> R,
    ) -> R {
        let key = (size, ch, bold);
        if let Some(glyph) = self.cache.borrow().get(&key) {
            return f(glyph);
        }
        let glyph = self.rasterize_for(ch, size, bold);
        let mut cache = self.cache.borrow_mut();
        f(cache.entry(key).or_insert(glyph))
    }

    /// Visual line height (ascent to descent) at `size`.
    pub fn text_height(&self, size: f32) -> f32 {
        self.raster(size, 1.0).apply(&self.face);
        let (ascent, descent) = self.face.line_metrics();
        ascent - descent
    }

    /// The distance from the line top to the baseline at `size`.
    pub fn ascent(&self, size: f32) -> f32 {
        self.raster(size, 1.0).apply(&self.face);
        self.face.line_metrics().0
    }

    /// Total advance width of `text` in `style`, letter spacing included.
    ///
    /// Measured per grapheme cluster, so an emoji cluster contributes its color
    /// glyph's single advance rather than one per scalar, and spacing lands
    /// between visible units rather than inside one.
    pub fn text_width(&self, text: &str, style: TextStyle) -> f32 {
        let size = self.raster(style.size, 1.0);
        grapheme::graphemes(text)
            .map(|(_, cluster)| self.cluster_advance(cluster, size, style.bold) + style.tracking)
            .sum()
    }

    /// Pixel x of the caret at char offset `offset` within `text`. Fields are
    /// never tracked, so this measures at the natural spacing.
    ///
    /// Measured by cluster, the way the text is drawn. A cluster that ligates
    /// into one picture advances once however many scalars spell it, so
    /// counting per char would put the caret several glyphs past the ink. An
    /// offset landing inside a cluster resolves to its left edge, which is the
    /// nearest place a caret can actually sit.
    pub fn x_at_char_offset(&self, text: &str, offset: usize, size: f32) -> f32 {
        let size = self.raster(size, 1.0);
        let mut x = 0.0;
        let mut chars = 0;
        for (_, cluster) in grapheme::graphemes(text) {
            let len = cluster.chars().count();
            if chars + len > offset {
                break;
            }
            x += self.cluster_advance(cluster, size, false);
            chars += len;
        }
        x
    }

    /// Char offset whose caret sits nearest pixel x, for mouse hit-testing.
    /// The inverse of [`Self::x_at_char_offset`], so it lands on cluster
    /// boundaries too.
    pub fn char_offset_at_x(&self, text: &str, x: f32, size: f32) -> usize {
        let size = self.raster(size, 1.0);
        let mut acc = 0.0;
        let mut chars = 0;
        for (_, cluster) in grapheme::graphemes(text) {
            let advance = self.cluster_advance(cluster, size, false);
            if x < acc + advance / 2.0 {
                return chars;
            }
            acc += advance;
            chars += cluster.chars().count();
        }
        chars
    }

    /// The longest run of whole clusters from the front of `text` that fits
    /// `max_w`. Cutting between clusters rather than between chars is what
    /// keeps a ligated sequence from being sliced into its parts.
    pub fn truncate_to_width<'t>(&self, text: &'t str, style: TextStyle, max_w: f32) -> &'t str {
        let size = self.raster(style.size, 1.0);
        let mut used = 0.0;
        for (at, cluster) in grapheme::graphemes(text) {
            let advance = self.cluster_advance(cluster, size, style.bold) + style.tracking;
            if used + advance > max_w {
                return &text[..at];
            }
            used += advance;
        }
        text
    }

    /// Draw `text` with its top-left at logical (x, y), stopping before a glyph
    /// whose advance would cross x + max_width. Returns the advanced logical
    /// width. The painter clips ink to its rectangle, so overhang past the box
    /// cannot bleed.
    ///
    /// Glyphs rasterize at the painter's scale and blit at device coordinates,
    /// so HiDPI text is sharp rather than an upscaled bitmap. Layout stays
    /// logical: the run's width is measured at the unscaled size, so where the
    /// text ends does not drift with the scale.
    pub fn draw_text(
        &self,
        p: &mut Painter,
        x: i32,
        y: i32,
        text: &str,
        style: TextStyle,
        max_width: i32,
    ) -> i32 {
        let scale = p.scale();
        let face = self.face_for(style.bold);
        let logical = self.raster(style.size, 1.0);
        let device = self.raster(style.size, scale);
        device.apply(face);
        let baseline = y as f32 * scale + face.line_metrics().0;
        let max_x = (x + max_width) as f32;
        let mut cursor = x as f32;

        for (_, cluster) in grapheme::graphemes(text) {
            // Advance in logical space, rasterize in device space.
            let advance = self.cluster_advance(cluster, logical, style.bold);
            if cursor + advance > max_x {
                break;
            }
            // A color glyph is one picture for the whole cluster; anything else
            // draws its characters in turn, each at its own pen position so a
            // combining mark lands on the base it follows.
            if let Some(glyph) = self.color_glyph(cluster, device, style.bold) {
                p.blit_argb(
                    (cursor * scale).round() as i32 + glyph.left,
                    baseline.round() as i32 - glyph.top,
                    glyph.width as i32,
                    glyph.rows as i32,
                    &glyph.argb,
                );
            } else {
                let mut pen = cursor;
                for ch in cluster.chars() {
                    self.with_glyph(ch, device, style.bold, |g| {
                        p.blit_coverage(
                            (pen * scale).round() as i32 + g.left,
                            baseline.round() as i32 - g.top,
                            g.width as i32,
                            g.rows as i32,
                            &g.coverage,
                            style.color,
                        );
                    });
                    pen += self.advance(ch, logical, style.bold);
                }
            }
            cursor += advance + style.tracking;
        }
        (cursor - x as f32).round() as i32
    }

    /// The pen advance of one character, from the same cache the blit reads.
    fn advance(&self, ch: char, size: RasterSize, bold: bool) -> f32 {
        self.with_glyph(ch, size, bold, |g| g.advance)
    }
}

/// Open one face of a font file, reading its bytes.
fn load_face(path: &Path, index: i32) -> io::Result<Face> {
    Face::from_bytes(std::fs::read(path)?, index)
}

/// Rasterize one glyph at a size into an owned cache entry.
fn rasterize(face: &Face, size: RasterSize, ch: char) -> CachedGlyph {
    size.apply(face);
    let g = face.rasterize(ch);
    CachedGlyph {
        left: g.left,
        top: g.top,
        width: g.width,
        rows: g.rows,
        advance: g.advance,
        coverage: g.coverage,
    }
}

/// Map a UI font size to a FreeType pixel size, rounding to at least 1.
fn pixel_size(size: f32) -> u32 {
    ((size + 0.5) as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::buffer::PixelBuffer;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-font.ttf");
    const SIZE: f32 = 16.0;
    const WHITE: TextStyle = TextStyle::new(SIZE, Color::rgb(255, 255, 255));

    fn font() -> Font {
        Font::from_path(Path::new(FIXTURE)).expect("load fixture font")
    }

    /// A character the configured font lacks must be drawn by a face that
    /// actually has it. FT_Load_Char never fails for a missing character, it
    /// rasterizes .notdef, so without this the only symptom is a tofu box on
    /// whichever machine happens to lack the glyph.
    #[test]
    fn a_character_the_font_lacks_resolves_to_one_that_has_it() {
        let f = font();
        // U+25B8 is absent from most UI fonts; U+4E2D needs a CJK face.
        for ch in ['\u{25B8}', '\u{4E2D}'] {
            if f.face_for(false).has_glyph(ch) {
                continue;
            }
            let Some(face) = f.fallback_face(ch) else {
                continue; // nothing installed covers it, which is not a failure
            };
            assert!(
                face.has_glyph(ch),
                "the fallback for {ch:?} must actually have the glyph"
            );
        }
    }

    /// One font serving several characters is opened once, and a character
    /// nothing covers is asked about once rather than on every frame.
    #[test]
    fn fallback_lookups_are_remembered_both_ways() {
        let f = font();
        let ch = '\u{4E2D}';
        let first = f.fallback_face(ch);
        let second = f.fallback_face(ch);
        match (first, second) {
            (Some(a), Some(b)) => assert!(Rc::ptr_eq(&a, &b), "the face is reused"),
            (None, None) => {}
            _ => panic!("the same character answered differently twice"),
        }
        assert_eq!(f.fallbacks.borrow().len(), 1, "asked once, remembered");
    }

    /// End-to-end proof that routing reaches the color pipeline: the run's ink
    /// is pure white, which can only ever produce gray pixels, so any pixel
    /// with chroma had to come from a color glyph. Skips on a machine with no
    /// emoji font, where falling back to the text path is the correct answer.
    #[test]
    fn an_emoji_cluster_draws_as_one_coloured_picture() {
        let f = font();
        let size = f.raster(SIZE, 1.0);
        if f.color_glyph("\u{1F389}", size, false).is_none() {
            return;
        }
        let mut buf = PixelBuffer::new(64, 48);
        let style = TextStyle::new(SIZE, Color::rgb(0xff, 0xff, 0xff));
        f.draw_text(&mut buf.painter(), 4, 4, "\u{1F389}", style, 60);
        let coloured = buf.pixels().iter().any(|&p| {
            let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
            r != g || g != b
        });
        assert!(coloured, "the party popper should draw in colour");
    }

    /// A ZWJ family is one cluster and must measure as one glyph, not as the
    /// three people and two joiners it is spelled with.
    #[test]
    fn a_zwj_family_measures_as_one_cluster() {
        let f = font();
        let size = f.raster(SIZE, 1.0);
        if f.color_glyph("\u{1F468}", size, false).is_none() {
            return;
        }
        let style = TextStyle::new(SIZE, Color::rgb(0xff, 0xff, 0xff));
        let one = f.text_width("\u{1F468}", style);
        let family = f.text_width("\u{1F468}\u{200d}\u{1F469}\u{200d}\u{1F467}", style);
        assert!(
            family < one * 2.0,
            "the family ligates ({family} vs {one} for one person)"
        );
    }

    /// The optical size follows the logical size, not the device pixels. A run
    /// on a scaled surface rasterizes larger while still being displayed at the
    /// same apparent size, so tying the axis to the device size would reshape
    /// letters on a HiDPI monitor.
    #[test]
    fn optical_size_ignores_the_surface_scale() {
        let one = RasterSize::new(13.0, 1.0, 96.0);
        let two = RasterSize::new(13.0, 2.0, 96.0);
        assert_eq!(one.optical, two.optical);
        assert_eq!(two.px, one.px * 2);
    }

    /// Two runs can rasterize at the same pixel size while being displayed at
    /// different sizes, and their outlines then differ. Keying the cache on
    /// pixels alone would serve one the other's glyphs.
    #[test]
    fn the_same_pixel_size_at_different_scales_is_a_different_key() {
        let scaled = RasterSize::new(10.0, 2.0, 96.0);
        let plain = RasterSize::new(20.0, 1.0, 96.0);
        assert_eq!(scaled.px, plain.px);
        assert_ne!(scaled, plain);
    }

    /// The axis is measured in points, so the same pixel size asks for a
    /// different optical size on a desktop configured for a different font DPI.
    #[test]
    fn optical_size_is_points_not_pixels() {
        // 10px at 96 DPI is 7.5pt, which is what the desktop's text stack asks
        // for at that size.
        let at_96 = RasterSize::new(10.0, 1.0, 96.0);
        assert_eq!(at_96.optical, (7.5 * 64.0) as u32);
        let at_192 = RasterSize::new(10.0, 1.0, 192.0);
        assert_eq!(at_192.optical, (3.75 * 64.0) as u32);
    }

    /// A static font has no optical axis, so asking for one changes nothing.
    /// The golden fixtures render with such a font, which is what keeps them
    /// stable across this change.
    #[test]
    fn a_font_without_an_optical_axis_is_unaffected() {
        let f = font();
        f.raster(SIZE, 1.0).apply(&f.face);
        let before = f.face.rasterize('W').advance;
        f.face.set_optical_size(5.0);
        assert_eq!(f.face.rasterize('W').advance, before);
    }

    /// Advances keep their fractional part. Light hinting rounds each glyph's
    /// advance to a whole pixel, and a run of those drifts from where the rest
    /// of the desktop sets the same text.
    #[test]
    fn advances_are_not_rounded_to_whole_pixels() {
        let f = font();
        let size = f.raster(SIZE, 1.0);
        let fractional = "Schiit Modi+ A"
            .chars()
            .any(|ch| f.advance(ch, size, false).fract() != 0.0);
        assert!(fractional, "advances should not be whole pixels");
    }

    /// Whatever the session reports, the DPI has to be usable as a divisor.
    #[test]
    fn the_desktop_dpi_is_always_positive() {
        assert!(desktop_font::dpi() > 0.0);
    }

    #[test]
    fn text_height_and_ascent_are_positive() {
        let f = font();
        assert!(f.text_height(SIZE) > 0.0);
        assert!(f.ascent(SIZE) > 0.0);
        assert!(f.ascent(SIZE) < f.text_height(SIZE));
    }

    #[test]
    fn caret_x_grows_with_offset() {
        let f = font();
        let text = "Master";
        let mut last = -1.0;
        for offset in 0..=text.chars().count() {
            let x = f.x_at_char_offset(text, offset, SIZE);
            assert!(x > last, "x at {offset} should grow ({x} <= {last})");
            last = x;
        }
    }

    #[test]
    fn char_offset_round_trips_to_ends() {
        let f = font();
        let text = "Sinks";
        let count = text.chars().count();
        assert_eq!(f.char_offset_at_x(text, -5.0, SIZE), 0);
        let full = f.x_at_char_offset(text, count, SIZE);
        assert_eq!(f.char_offset_at_x(text, full + 50.0, SIZE), count);
    }

    /// The caret is measured the way the text is drawn. A cluster that ligates
    /// into one picture advances once however many scalars spell it, so
    /// counting per char would leave the caret several glyphs past the ink.
    #[test]
    fn the_caret_measures_clusters_the_way_the_text_draws_them() {
        let f = font();
        let size = f.raster(SIZE, 1.0);
        if f.color_glyph("\u{1F468}", size, false).is_none() {
            return;
        }
        let family = "\u{1F468}\u{200d}\u{1F469}\u{200d}\u{1F467}";
        let chars = family.chars().count();

        assert_eq!(
            f.x_at_char_offset(family, chars, SIZE),
            f.text_width(family, WHITE),
            "the caret past the cluster sits where the run ends",
        );
        // Anywhere inside the cluster resolves to its near edge, which is the
        // only place a caret can sit.
        assert_eq!(f.x_at_char_offset(family, 1, SIZE), 0.0);
        assert_eq!(f.char_offset_at_x(family, -5.0, SIZE), 0);
        assert_eq!(f.char_offset_at_x(family, 10_000.0, SIZE), chars);
    }

    /// Cutting a run to fit lands between clusters, so a ligated sequence is
    /// never sliced into the parts it is spelled with.
    #[test]
    fn truncating_stops_on_a_cluster_boundary() {
        let f = font();
        let text = "Master";
        // Wide enough for everything.
        assert_eq!(f.truncate_to_width(text, WHITE, 10_000.0), text);
        // Nothing fits at all.
        assert_eq!(f.truncate_to_width(text, WHITE, 0.0), "");
        // A prefix is a whole number of clusters, and it fits what it claims.
        let half = f.text_width(text, WHITE) / 2.0;
        let cut = f.truncate_to_width(text, WHITE, half);
        assert!(text.starts_with(cut));
        assert!(f.text_width(cut, WHITE) <= half);
        assert!(cut.len() < text.len());
    }

    #[test]
    fn text_width_matches_full_offset() {
        let f = font();
        let text = "Output";
        let w = f.text_width(text, WHITE);
        let via_offset = f.x_at_char_offset(text, text.chars().count(), SIZE);
        assert_eq!(w, via_offset);
        assert!(w > 0.0);
    }

    #[test]
    fn draw_text_blends_pixels_and_reports_width() {
        let f = font();
        let mut buf = PixelBuffer::new(200, 40);
        let mut p = buf.painter();
        let width = f.draw_text(&mut p, 2, 2, "Hi", WHITE, 200);
        assert!(width > 0);
        assert!(
            buf.pixels().iter().any(|&px| px != 0xff00_0000),
            "drawing should leave a blended pixel"
        );
    }

    #[test]
    fn draw_text_stops_at_max_width() {
        let f = font();
        let mut full = PixelBuffer::new(200, 40);
        let mut clipped = PixelBuffer::new(200, 40);
        let full_w = f.draw_text(&mut full.painter(), 0, 2, "WWWWWW", WHITE, 200);
        let narrow = full_w / 2;
        let clipped_w = f.draw_text(&mut clipped.painter(), 0, 2, "WWWWWW", WHITE, narrow);
        assert!(clipped_w <= narrow);
        assert!(clipped_w < full_w);
    }

    #[test]
    fn glyph_hanging_off_the_edge_is_clipped_not_a_panic() {
        let f = font();
        // Draw starting near the right edge and above the top; the painter must
        // clip the overhang instead of writing out of bounds.
        let mut buf = PixelBuffer::new(40, 24);
        f.draw_text(&mut buf.painter(), 36, 0, "Wg", WHITE, 200);
    }
}
