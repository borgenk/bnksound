//! The color emoji font: a bitmap-strike (CBDT) FreeType face plus a HarfBuzz
//! shaper over it. It turns a grapheme cluster (a ZWJ family, a flag, a skin
//! tone, a keycap) into a single ligature glyph, decodes that glyph's color
//! strike, and scales it to the surrounding text size, caching the result per
//! (cluster, size).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::platform::freetype::{ColorStrike, Face};
use crate::platform::grapheme;
use crate::platform::pixel::{premultiplied_over, resample_argb, straight_from_premultiplied};
use crate::platform::shape::{ShapedGlyph, Shaper};

/// Whether a grapheme cluster should render as color emoji. `mono_has` reports
/// whether the text face can draw a character itself, so symbols the text font
/// covers (dagger, copyright, box drawing) stay text.
///
/// The rules, in order:
/// - a text-presentation selector (U+FE0E) anywhere forces the text path;
/// - a single scalar goes color only when it is `Extended_Pictographic` *and*
///   the text face has no glyph for it (a smiley, not a copyright sign);
/// - a multi-scalar cluster goes color when it carries an emoji-presentation
///   selector (U+FE0F) or keycap (U+20E3), starts pictographic (ZWJ sequences,
///   skin tones), or starts with a regional indicator (flags). Anything else
///   (plain combining marks) is ordinary text.
pub fn wants_emoji(cluster: &str, mono_has: impl Fn(char) -> bool) -> bool {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if cluster.contains('\u{FE0E}') {
        return false;
    }
    if chars.next().is_none() {
        return grapheme::is_extended_pictographic(first) && !mono_has(first);
    }
    cluster.contains('\u{FE0F}')
        || cluster.contains('\u{20E3}')
        || grapheme::is_extended_pictographic(first)
        || matches!(first, '\u{1F1E6}'..='\u{1F1FF}')
}

/// A rasterized emoji cluster at display size: straight (unpremultiplied)
/// `0xAARRGGBB` pixels, row-major, plus the pen metrics a coverage glyph
/// carries. The blitter draws straight out of it.
pub struct ColorGlyph {
    /// Horizontal offset from the pen to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline up to the bitmap's top edge.
    pub top: i32,
    pub width: usize,
    pub rows: usize,
    /// Pen advance in pixels.
    pub advance: f32,
    pub argb: Vec<u32>,
}

/// Where distros install the Noto color emoji font. A machine with none of
/// these simply has no emoji, and every cluster falls back to the text path.
const EMOJI_FONTS: &[&str] = &[
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
];

/// Rasterized clusters, keyed by target pixel size then by cluster bytes.
/// Splitting on the size (a cheap `u32` key) lets the inner lookup borrow the
/// cluster `&str` directly (`Box<str>: Borrow<str>`), so a cache *hit* allocates
/// nothing; only a miss boxes the cluster to insert it. `None` records that the
/// font cannot form the cluster, so the miss is not re-shaped every frame.
type ClusterCache = RefCell<HashMap<u32, HashMap<Box<str>, Option<Rc<ColorGlyph>>>>>;

/// The color emoji font: a bitmap-strike (CBDT) face plus a HarfBuzz shaper
/// over it. The shaper is what turns a multi-scalar cluster (ZWJ family, flag,
/// skin tone, keycap) into the single ligature glyph the font's GSUB table
/// defines; FreeType then decodes that glyph's color strike, and the result is
/// scaled to the text size and cached per (cluster, size).
///
/// Everything degrades gracefully: no installed font, an outline-only emoji
/// font, or a cluster the font cannot form all yield `None`, and the caller
/// falls back to per-character rendering.
pub struct EmojiFont {
    /// Declared before `face` deliberately: fields drop in declaration order,
    /// and the shaper's HarfBuzz font holds a reference to the FreeType face
    /// that it releases with `FT_Done_Face` on destroy. The face's own `Drop`
    /// tears down its whole FreeType library, so HarfBuzz must let go first.
    shaper: Shaper,
    face: Face,
    /// The strike's nominal pixel size; glyph bitmaps and shaped positions are
    /// scaled by `target / strike` to land on the text's em square.
    strike: f32,
    /// Rasterized clusters. Measuring rasterizes, unlike a GPU renderer that
    /// can shape for metrics and let an atlas hold the pixels; here the same
    /// cached entry answers both, so measuring and drawing cannot disagree.
    glyphs: ClusterCache,
}

impl EmojiFont {
    /// Open the first installed emoji font, or `None` when there is none, the
    /// font has no bitmap strikes (an outline-only build), or the shaper cannot
    /// bind to it.
    pub fn open() -> Option<Self> {
        let path = EMOJI_FONTS
            .iter()
            .find(|p| std::path::Path::new(p).exists())?;
        let face = Face::from_path(std::path::Path::new(path)).ok()?;
        let strike = face.select_first_strike()?;
        // SAFETY: the face handle is valid and has a size selected; the shaper
        // takes its own FreeType reference, so drop order does not matter.
        let shaper = unsafe { Shaper::from_ft_face(face.ft_face_ptr()) }?;
        Some(Self {
            shaper,
            face,
            strike,
            glyphs: RefCell::new(HashMap::new()),
        })
    }

    /// The color glyph for `cluster` at `target` pixels, or `None` when the font
    /// cannot form it. Rasterized on first sight and cached after.
    pub fn glyph(&self, cluster: &str, target: u32) -> Option<Rc<ColorGlyph>> {
        let mut cache = self.glyphs.borrow_mut();
        let by_cluster = cache.entry(target).or_default();
        // Borrow the cluster to probe: a hit returns without boxing it.
        if let Some(hit) = by_cluster.get(cluster) {
            return hit.clone();
        }
        let glyph = self.raster(cluster, target).map(Rc::new);
        by_cluster.insert(cluster.into(), glyph.clone());
        glyph
    }

    /// Pen advance of `cluster` scaled to `target` pixels, or `None` when the
    /// font cannot form it. Reads the same cached entry the blitter draws.
    pub fn advance(&self, cluster: &str, target: u32) -> Option<f32> {
        self.glyph(cluster, target).map(|g| g.advance)
    }

    /// Shape `cluster` and keep the result only when the font can really form
    /// it: at least one glyph and no `.notdef`.
    fn shaped(&self, cluster: &str) -> Option<Vec<ShapedGlyph>> {
        let glyphs = self.shaper.shape(cluster);
        if glyphs.is_empty() || glyphs.iter().any(|g| g.id == 0) {
            return None;
        }
        Some(glyphs)
    }

    /// How much a strike-sized measurement shrinks to land on `target` pixels.
    fn scale(&self, target: u32) -> f32 {
        target as f32 / self.strike
    }

    /// Rasterize `cluster` at `target` pixels: decode each shaped glyph's color
    /// strike, composite them along the pen into one strike-sized image, then
    /// resample once to the target size. Emoji clusters almost always shape to
    /// a single glyph, so the composite loop usually runs once.
    ///
    /// The pipeline stays *premultiplied* (as FreeType decodes it) through
    /// compositing and resampling, and un-premultiplies only the final
    /// display-size pixels. Filtering straight alpha would mix the black of
    /// transparent texels into edge colors and fringe the glyph outline dark.
    fn raster(&self, cluster: &str, target: u32) -> Option<ColorGlyph> {
        let shaped = self.shaped(cluster)?;
        let scale = self.scale(target);
        let (placed, pen) = self.place_strikes(&shaped);
        let advance = pen * scale;
        if placed.is_empty() {
            // Shaped but inkless; keep the advance so layout stays consistent.
            return Some(ColorGlyph {
                left: 0,
                top: 0,
                width: 0,
                rows: 0,
                advance,
                argb: Vec::new(),
            });
        }
        let Composited {
            x0,
            top,
            w,
            h,
            argb,
        } = Self::composite(&placed);
        let dst_w = ((w as f32) * scale).round().max(1.0) as i32;
        let dst_h = ((h as f32) * scale).round().max(1.0) as i32;
        let argb: Vec<u32> = resample_argb(&argb, w as u32, h as u32, dst_w, dst_h)
            .into_iter()
            .map(straight_from_premultiplied)
            .collect();
        Some(ColorGlyph {
            left: (x0 as f32 * scale).round() as i32,
            top: (top as f32 * scale).round() as i32,
            width: dst_w as usize,
            rows: dst_h as usize,
            advance,
            argb,
        })
    }

    /// Decode and place each shaped glyph's color strike in strike space,
    /// returning the placed strikes and the total pen advance (in strike units,
    /// before the display-size scale). A glyph the font cannot decode is skipped
    /// but still advances the pen, so an inkless cluster keeps its width.
    fn place_strikes(&self, shaped: &[ShapedGlyph]) -> (Vec<Placed>, f32) {
        let mut placed = Vec::new();
        let mut pen = 0.0f32;
        for g in shaped {
            if let Some(bitmap) = self.face.color_strike(g.id) {
                // x/y are the bitmap's left and top edges relative to the pen
                // origin and baseline.
                let x = (pen + g.x_offset).round() as i32 + bitmap.left;
                let y = bitmap.top + g.y_offset.round() as i32;
                placed.push(Placed { x, y, bitmap });
            }
            pen += g.x_advance;
        }
        (placed, pen)
    }

    /// Composite the placed strikes into one premultiplied, strike-sized canvas.
    /// A single pass over the strikes finds the union box (top-left `x0`/`top`,
    /// size `w` x `h`); a second blends each strike onto the canvas with
    /// premultiplied source-over. Only called for a non-empty placement.
    fn composite(placed: &[Placed]) -> Composited {
        // The union box of every placed bitmap, still in strike space, folded in
        // one pass: x0/x1 are the horizontal extent, top/bottom the vertical.
        let (mut x0, mut x1, mut top, mut bottom) = (i32::MAX, i32::MIN, i32::MIN, i32::MAX);
        for p in placed {
            x0 = x0.min(p.x);
            x1 = x1.max(p.x + p.bitmap.width as i32);
            top = top.max(p.y);
            bottom = bottom.min(p.y - p.bitmap.rows as i32);
        }
        let (w, h) = ((x1 - x0).max(0) as usize, (top - bottom).max(0) as usize);
        let mut argb = vec![0u32; w * h];
        for p in placed {
            for row in 0..p.bitmap.rows {
                let dst_row = (top - p.y) as usize + row;
                for col in 0..p.bitmap.width {
                    let dst_col = (p.x - x0) as usize + col;
                    let src = p.bitmap.argb[row * p.bitmap.width + col];
                    let dst = &mut argb[dst_row * w + dst_col];
                    *dst = premultiplied_over(src, *dst);
                }
            }
        }
        Composited {
            x0,
            top,
            w,
            h,
            argb,
        }
    }
}

/// A decoded strike positioned in strike space: `x`/`y` are the bitmap's left
/// and top edges relative to the pen origin and baseline.
struct Placed {
    x: i32,
    y: i32,
    bitmap: ColorStrike,
}

/// A composited, premultiplied, strike-sized canvas: its top-left corner in
/// strike space (`x0`, `top`) and the `w` x `h` `argb` pixels.
struct Composited {
    x0: i32,
    top: i32,
    w: usize,
    h: usize,
    argb: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emoji font, or `None` on a machine without one. Every test skips
    /// rather than fails there, since the font is a system package.
    fn emoji() -> Option<EmojiFont> {
        EmojiFont::open()
    }

    #[test]
    fn wants_emoji_routes_clusters_correctly() {
        let mono_lacks = |_: char| false;
        let mono_has = |_: char| true;
        // Plain text and combining marks never route to emoji.
        assert!(!wants_emoji("a", mono_lacks));
        assert!(
            !wants_emoji("e\u{301}", mono_lacks),
            "combining mark is text"
        );
        // A lone pictographic scalar goes color only when the text face lacks it.
        assert!(wants_emoji("\u{1F600}", mono_lacks));
        assert!(
            !wants_emoji("\u{1F600}", mono_has),
            "the text face covers it"
        );
        // Presentation selectors win in both directions.
        assert!(wants_emoji("\u{263A}\u{fe0f}", mono_has), "emoji selector");
        assert!(
            !wants_emoji("\u{263A}\u{fe0e}", mono_lacks),
            "text selector"
        );
        // Flags, skin tones, ZWJ sequences, and keycaps are emoji outright.
        assert!(wants_emoji("\u{1F1F3}\u{1F1F4}", mono_has));
        assert!(wants_emoji("\u{1F44D}\u{1F3FD}", mono_has));
        assert!(wants_emoji(
            "\u{1F468}\u{200d}\u{1F469}\u{200d}\u{1F467}",
            mono_has
        ));
        assert!(wants_emoji("1\u{fe0f}\u{20e3}", mono_has));
    }

    #[test]
    fn a_smiley_rasterizes_to_the_text_size_with_ink() {
        let Some(font) = emoji() else { return };
        let glyph = font
            .glyph("\u{1F600}", 32)
            .expect("the smiley should raster");
        assert!(glyph.width > 0 && glyph.rows > 0, "the smiley has pixels");
        assert!(glyph.argb.iter().any(|&p| p >> 24 != 0), "and visible ink");
        assert!(
            glyph.rows <= 40,
            "scaled to the 32px em square, not the 128px strike ({} rows)",
            glyph.rows
        );
        assert_eq!(
            font.advance("\u{1F600}", 32),
            Some(glyph.advance),
            "measure and draw read the same entry"
        );
    }

    #[test]
    fn zwj_and_flag_clusters_ligate_to_one_glyph() {
        let Some(font) = emoji() else { return };
        let single = font.advance("\u{1F468}", 32).expect("the man emoji shapes");
        let family = font
            .advance("\u{1F468}\u{200d}\u{1F469}\u{200d}\u{1F467}", 32)
            .expect("the family should ligate");
        assert!(
            family < single * 2.0,
            "a family is one ligature, not three glyphs ({family} vs {single})"
        );
        let flag = font
            .advance("\u{1F1F3}\u{1F1F4}", 32)
            .expect("the flag should ligate");
        assert!(
            flag < single * 2.0,
            "a flag is one glyph, not two letter symbols"
        );
    }

    #[test]
    fn a_cluster_is_rasterized_once_and_served_from_cache() {
        let Some(font) = emoji() else { return };
        let first = font.glyph("\u{1F389}", 24).expect("party popper");
        let second = font.glyph("\u{1F389}", 24).expect("party popper again");
        assert!(
            Rc::ptr_eq(&first, &second),
            "the second look-up must not re-rasterize"
        );
    }

    #[test]
    fn an_unformable_cluster_answers_none_and_stays_negative() {
        let Some(font) = emoji() else { return };
        // A letter with a combining mark is no emoji ligature; the emoji font
        // has no glyphs for it and must say so rather than render garbage.
        // (Routing filters this out anyway; the font still answers safely.)
        assert!(font.glyph("e\u{301}", 32).is_none());
        assert!(font.advance("e\u{301}", 32).is_none());
    }

    #[test]
    fn the_same_cluster_at_two_sizes_scales_independently() {
        let Some(font) = emoji() else { return };
        let small = font.glyph("\u{1F600}", 16).expect("small");
        let large = font.glyph("\u{1F600}", 48).expect("large");
        assert!(
            large.rows > small.rows && large.advance > small.advance,
            "a larger target yields a larger glyph"
        );
    }
}
