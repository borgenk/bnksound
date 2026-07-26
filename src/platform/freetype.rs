//! Glyph rasterization backed by the system FreeType (libfreetype.so.6).
//!
//! FreeType turns a TrueType or OpenType outline into an anti-aliased coverage
//! bitmap: hinting, scan conversion, and the per-size metrics layout needs.
//! This is the thin FFI layer over it: a small extern block, one struct that
//! owns the C-side handles plus the font bytes they read, a Drop impl, and
//! methods that return owned data.
//!
//! FreeType exposes glyph data as struct fields reached from the Face pointer
//! (face->glyph->bitmap, face->size->metrics), not through accessors. The
//! repr(C) structs below mirror those layouts up to the last field this module
//! reads; FreeType allocates the full structs, this code only reads a prefix,
//! so declaring it with matching primitive types and repr(C) padding is enough.
//! The fields touched here are old and load-bearing for every FreeType consumer,
//! so the offsets have been ABI-stable for years.

use core::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_ushort, c_void};
use core::ptr::{self, NonNull};
use std::cell::{Cell, RefCell};
use std::io;

type FtError = c_int;
/// FT_Library: opaque, only ever held as a handle and freed.
type FtLibrary = *mut c_void;
/// FT_Face: a pointer to a struct whose fields this module reads.
type FtFace = *mut FtFaceRec;

/// Rasterize to an 8-bit coverage bitmap (FT_LOAD_RENDER, default gray mode).
const FT_LOAD_RENDER: i32 = 1 << 2;
/// Ignore embedded bitmap strikes so an outline glyph renders to the 8-bit gray
/// bitmap this module expects (FT_LOAD_NO_BITMAP).
const FT_LOAD_NO_BITMAP: i32 = 1 << 3;
/// FT_PIXEL_MODE_GRAY: one coverage byte per pixel.
const FT_PIXEL_MODE_GRAY: u8 = 2;
/// FT_LOAD_NO_HINTING: rasterize the outline unfitted.
const FT_LOAD_NO_HINTING: i32 = 1 << 1;
/// FT_LOAD_TARGET_LIGHT: grid-fit vertically only, leaving horizontal metrics
/// untouched.
const FT_LOAD_TARGET_LIGHT: i32 = 1 << 16;

/// How much a glyph's outline is grid-fitted before it is scan-converted.
///
/// Vertical grid-fitting is what puts a small feature (the dot on an i, a
/// horizontal bar) on one pixel row rather than straddling two at half
/// intensity, which reads as blur at UI sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hinting {
    /// No fitting; outlines land wherever they fall.
    None,
    /// Fit vertically only. What nearly every Linux desktop configures, and
    /// what a toolkit applies on this platform by default.
    #[default]
    Slight,
    /// Fit both axes. Crisper stems, at the cost of distorted advances.
    Full,
}

impl Hinting {
    /// The FreeType load flags this asks for.
    fn load_flags(self) -> i32 {
        match self {
            Hinting::None => FT_LOAD_NO_HINTING,
            Hinting::Slight => FT_LOAD_TARGET_LIGHT,
            Hinting::Full => 0,
        }
    }
}

/// Decode a glyph's embedded color bitmap rather than skipping it
/// (FT_LOAD_COLOR). The gray path deliberately does not set this.
const FT_LOAD_COLOR: i32 = 1 << 20;
/// FT_PIXEL_MODE_BGRA: four premultiplied bytes per pixel, what a CBDT color
/// strike decodes to.
const FT_PIXEL_MODE_BGRA: u8 = 7;

#[link(name = "freetype")]
#[allow(non_snake_case)]
unsafe extern "C" {
    fn FT_Init_FreeType(alibrary: *mut FtLibrary) -> FtError;
    fn FT_Done_FreeType(library: FtLibrary) -> FtError;
    fn FT_New_Memory_Face(
        library: FtLibrary,
        file_base: *const u8,
        file_size: c_long,
        face_index: c_long,
        aface: *mut FtFace,
    ) -> FtError;
    fn FT_Done_Face(face: FtFace) -> FtError;
    fn FT_Set_Pixel_Sizes(face: FtFace, pixel_width: c_uint, pixel_height: c_uint) -> FtError;
    fn FT_Load_Char(face: FtFace, char_code: c_ulong, load_flags: i32) -> FtError;
    fn FT_Load_Glyph(face: FtFace, glyph_index: c_uint, load_flags: i32) -> FtError;
    fn FT_Get_Char_Index(face: FtFace, charcode: c_ulong) -> c_uint;
    fn FT_Select_Size(face: FtFace, strike_index: c_int) -> FtError;
    fn FT_Get_MM_Var(face: FtFace, amaster: *mut *mut FtMmVar) -> FtError;
    fn FT_Done_MM_Var(library: FtLibrary, amaster: *mut FtMmVar) -> FtError;
    fn FT_Get_Var_Design_Coordinates(
        face: FtFace,
        num_coords: c_uint,
        coords: *mut FtFixed,
    ) -> FtError;
    fn FT_Set_Var_Design_Coordinates(
        face: FtFace,
        num_coords: c_uint,
        coords: *mut FtFixed,
    ) -> FtError;
}

/// FT_Fixed: 16.16 fixed point, which is how variation coordinates are passed.
type FtFixed = c_long;

fn to_fixed(v: f32) -> FtFixed {
    (v * 65536.0) as FtFixed
}

fn from_fixed(v: FtFixed) -> f32 {
    v as f32 / 65536.0
}

/// The OpenType tag for the optical size axis, packed the way FreeType stores
/// a tag: one byte per character, most significant first.
const OPSZ_TAG: c_ulong = 0x6F70_737A;

// ---------------------------------------------------------------------------
// C struct layouts (read-only mirrors of the FreeType headers). Most fields
// only place the ones this module reads at the right offset, hence dead_code.
// ---------------------------------------------------------------------------

#[repr(C)]
#[allow(dead_code)]
struct FtGeneric {
    data: *mut c_void,
    finalizer: *mut c_void,
}

/// FT_Vector: advance is 26.6 fixed point (1/64 pixel).
#[repr(C)]
#[allow(dead_code)]
struct FtVector {
    x: c_long,
    y: c_long,
}

/// FT_Bitmap: buffer holds rows * |pitch| coverage bytes, 0..=255 in gray mode.
/// pitch is the signed byte stride between rows.
#[repr(C)]
#[allow(dead_code)]
struct FtBitmap {
    rows: c_uint,
    width: c_uint,
    pitch: c_int,
    buffer: *const u8,
    num_grays: c_ushort,
    pixel_mode: u8,
    palette_mode: u8,
    palette: *mut c_void,
}

/// FT_Size_Metrics: ascender and descender are 26.6 fixed point (descender < 0).
#[repr(C)]
#[allow(dead_code)]
struct FtSizeMetrics {
    x_ppem: c_ushort,
    y_ppem: c_ushort,
    x_scale: c_long,
    y_scale: c_long,
    ascender: c_long,
    descender: c_long,
    height: c_long,
    max_advance: c_long,
}

/// FT_GlyphSlotRec prefix up to bitmap_top. metrics stands in for
/// FT_Glyph_Metrics (eight FT_Pos), placing advance and bitmap correctly.
#[repr(C)]
#[allow(dead_code)]
struct FtGlyphSlotRec {
    library: *mut c_void,
    face: *mut c_void,
    next: *mut c_void,
    glyph_index: c_uint,
    generic: FtGeneric,
    metrics: [c_long; 8],
    linear_hori_advance: c_long,
    linear_vert_advance: c_long,
    advance: FtVector,
    format: c_int,
    bitmap: FtBitmap,
    bitmap_left: c_int,
    bitmap_top: c_int,
}

/// FT_Var_Axis: one variation axis's range and tag.
#[repr(C)]
#[allow(dead_code)]
struct FtVarAxis {
    name: *mut c_char,
    minimum: FtFixed,
    def: FtFixed,
    maximum: FtFixed,
    tag: c_ulong,
    strid: c_uint,
}

/// FT_MM_Var prefix up to the axis array. The named style array is not read.
#[repr(C)]
#[allow(dead_code)]
struct FtMmVar {
    num_axis: c_uint,
    num_designs: c_uint,
    num_namedstyles: c_uint,
    axis: *mut FtVarAxis,
    namedstyle: *mut c_void,
}

/// FT_SizeRec prefix up to metrics.
#[repr(C)]
#[allow(dead_code)]
struct FtSizeRec {
    face: *mut c_void,
    generic: FtGeneric,
    metrics: FtSizeMetrics,
}

/// FT_FaceRec prefix up to size. bbox stands in for FT_BBox (four FT_Pos); the
/// seven c_short fields are the ascender..underline run. Only glyph and size
/// are read.
#[repr(C)]
#[allow(dead_code)]
struct FtFaceRec {
    num_faces: c_long,
    face_index: c_long,
    face_flags: c_long,
    style_flags: c_long,
    num_glyphs: c_long,
    family_name: *mut c_char,
    style_name: *mut c_char,
    num_fixed_sizes: c_int,
    available_sizes: *mut c_void,
    num_charmaps: c_int,
    charmaps: *mut c_void,
    generic: FtGeneric,
    bbox: [c_long; 4],
    units_per_em: c_ushort,
    ascender: c_short,
    descender: c_short,
    height: c_short,
    max_advance_width: c_short,
    max_advance_height: c_short,
    underline_position: c_short,
    underline_thickness: c_short,
    glyph: *mut FtGlyphSlotRec,
    size: *mut FtSizeRec,
}

/// A rasterized glyph, owned so no reference into FreeType memory escapes.
/// coverage is tightly packed top-down, width bytes per row, rows rows.
pub struct Glyph {
    /// Horizontal offset from the pen to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline up to the bitmap's top edge.
    pub top: i32,
    pub width: usize,
    pub rows: usize,
    /// Pen advance in pixels, unrounded.
    pub advance: f32,
    pub coverage: Vec<u8>,
}

impl Glyph {
    /// A glyph with no ink and no advance (a load failure or missing glyph).
    fn empty() -> Self {
        Glyph {
            left: 0,
            top: 0,
            width: 0,
            rows: 0,
            advance: 0.0,
            coverage: Vec::new(),
        }
    }
}

/// One glyph's decoded color strike: premultiplied `0xAARRGGBB`, row-major,
/// tightly packed, plus its placement relative to the pen and baseline. Owned,
/// so the FreeType glyph slot is not borrowed past the call that filled it.
pub struct ColorStrike {
    /// Horizontal offset from the pen to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline up to the bitmap's top edge.
    pub top: i32,
    pub width: usize,
    pub rows: usize,
    pub argb: Vec<u32>,
}

/// Where a face's optical size axis sits in its design coordinate vector, and
/// the range that axis accepts. A property of the file, so it is read once at
/// open rather than on every size change.
struct OpticalAxis {
    /// Which coordinate the optical axis occupies.
    index: usize,
    /// How many axes the face has, the length every coordinate vector must have.
    axes: usize,
    min: f32,
    max: f32,
    /// The coordinate vector handed to FreeType, sized once at open and reused.
    /// Every text run sets an optical size, so allocating a fresh one per call
    /// put a heap allocation on the frame path for a few numbers.
    coords: RefCell<Vec<FtFixed>>,
}

/// Owns the FreeType library, the face, and the font bytes the face borrows.
///
/// FT_New_Memory_Face does not copy its input; it keeps a pointer into the
/// bytes for the face's life. The owned Vec backs them: its heap allocation is
/// stable however the Face value is moved, and it is freed only after
/// FT_Done_Face runs in Drop (fields drop after the Drop body).
pub struct Face {
    library: NonNull<c_void>,
    face: NonNull<FtFaceRec>,
    /// How outlines are grid-fitted. Set from the desktop's fontconfig
    /// settings, so text matches what every other application on the machine
    /// draws rather than whatever FreeType happens to default to.
    hinting: Cell<Hinting>,
    /// The optical size axis, when the file is a variable font that has one.
    optical: Option<OpticalAxis>,
    // Read by FreeType for the face's life; must outlive `face`. Kept last so it
    // drops after the Drop impl frees the FreeType handles.
    _data: Vec<u8>,
}

impl Face {
    /// Build a face from owned font bytes.
    ///
    /// `index` selects which face in the file to open. Its low bits are the
    /// face number and its high bits may name an instance of a variable font,
    /// which is how a bold instance is reached in a file that holds several.
    pub fn from_bytes(data: Vec<u8>, index: i32) -> io::Result<Self> {
        if data.is_empty() {
            return Err(io::Error::other("empty font file"));
        }
        let mut library: FtLibrary = ptr::null_mut();
        // SAFETY: alibrary points at a live local; FT writes the handle through
        // it and returns nonzero on failure.
        let err = unsafe { FT_Init_FreeType(&mut library) };
        let library = NonNull::new(library)
            .filter(|_| err == 0)
            .ok_or_else(|| io::Error::other("FT_Init_FreeType failed"))?;

        let mut face: FtFace = ptr::null_mut();
        // SAFETY: library is valid; data outlives the face (freed in Drop after
        // FT_Done_Face) and describes the font bytes; aface is a live local.
        let err = unsafe {
            FT_New_Memory_Face(
                library.as_ptr(),
                data.as_ptr(),
                data.len() as c_long,
                c_long::from(index),
                &mut face,
            )
        };
        let Some(face) = NonNull::new(face).filter(|_| err == 0) else {
            // SAFETY: library came from FT_Init_FreeType and is freed once here.
            unsafe { FT_Done_FreeType(library.as_ptr()) };
            return Err(io::Error::other("FT_New_Memory_Face failed"));
        };

        Ok(Face {
            library,
            face,
            hinting: Cell::new(Hinting::default()),
            optical: optical_axis(library.as_ptr(), face.as_ptr()),
            _data: data,
        })
    }

    /// Select the pixel size FreeType renders and reports metrics at. A width of
    /// zero tells FreeType to match it to the height.
    pub fn set_pixel_size(&self, size: u32) -> io::Result<()> {
        // SAFETY: face is valid; the call only reads its size arguments.
        let err = unsafe { FT_Set_Pixel_Sizes(self.face.as_ptr(), 0, size) };
        if err != 0 {
            return Err(io::Error::other("FT_Set_Pixel_Sizes failed"));
        }
        Ok(())
    }

    /// Draw outlines shaped for text displayed at `points`.
    ///
    /// A variable font's optical size axis reshapes letters across the size
    /// range: small sizes widen and loosen so they stay legible, display sizes
    /// tighten. The rest of the desktop ties this axis to the point size, so a
    /// face left at its default instance draws visibly different text from
    /// every other application. A face without the axis has nothing to set.
    pub fn set_optical_size(&self, points: f32) {
        let Some(axis) = self.optical.as_ref() else {
            return;
        };
        let Ok(mut coords) = axis.coords.try_borrow_mut() else {
            return;
        };
        // SAFETY: face is valid and coords has one slot per axis, which is the
        // length both calls read and write.
        unsafe {
            let err = FT_Get_Var_Design_Coordinates(
                self.face.as_ptr(),
                axis.axes as c_uint,
                coords.as_mut_ptr(),
            );
            if err != 0 {
                return;
            }
            // Only the optical axis moves. The others keep whatever instance the
            // face was opened at, which is how a bold face keeps its weight.
            // FreeType stores an out-of-range coordinate as given, so the clamp
            // is what keeps a very small size inside the axis.
            coords[axis.index] = to_fixed(points.clamp(axis.min, axis.max));
            FT_Set_Var_Design_Coordinates(
                self.face.as_ptr(),
                axis.axes as c_uint,
                coords.as_mut_ptr(),
            );
        }
    }

    /// Render a character at the current pixel size, copying the coverage out.
    /// A load failure or glyphless character yields an empty Glyph.
    pub fn rasterize(&self, ch: char) -> Glyph {
        // SAFETY: face is valid; FT_Load_Char renders into the face's shared
        // glyph slot. NO_BITMAP forces the outline path so the result is 8-bit
        // gray. No reference into the slot is held across a later FreeType call.
        let err = unsafe {
            FT_Load_Char(
                self.face.as_ptr(),
                ch as u32 as c_ulong,
                FT_LOAD_RENDER | FT_LOAD_NO_BITMAP | self.hinting.get().load_flags(),
            )
        };
        if err != 0 {
            return Glyph::empty();
        }
        // SAFETY: the load succeeded, so face->glyph points at the populated
        // slot; the borrow ends before any later FreeType call.
        let slot = unsafe { (*self.face.as_ptr()).glyph };
        let Some(slot) = NonNull::new(slot) else {
            return Glyph::empty();
        };
        let slot = unsafe { slot.as_ref() };
        // The linear advance rather than the hinted one. Light hinting fits the
        // outline vertically but rounds the advance to a whole pixel, and a run
        // of rounded advances drifts from where the rest of the desktop sets the
        // same text. Only the bitmap is grid-fitted; the pen stays fractional.
        let advance = slot.linear_hori_advance as f32 / 65536.0;
        // The blit reads one coverage byte per pixel, which holds only for gray
        // mode. A strike-only font can yield mono or color; keep the advance so
        // layout is undisturbed, but skip the ink.
        if slot.bitmap.pixel_mode != FT_PIXEL_MODE_GRAY {
            return Glyph {
                advance,
                ..Glyph::empty()
            };
        }
        Glyph {
            left: slot.bitmap_left,
            top: slot.bitmap_top,
            width: slot.bitmap.width as usize,
            rows: slot.bitmap.rows as usize,
            advance,
            coverage: copy_coverage(&slot.bitmap),
        }
    }

    /// Open the face at `path`, reading the file into memory the face borrows
    /// for its life.
    pub fn from_path(path: &std::path::Path) -> io::Result<Self> {
        Self::from_bytes(std::fs::read(path)?, 0)
    }

    /// The raw FT_Face handle, for binding a shaper to this face. Deliberately
    /// opaque: the struct behind it stays private to this module, so nothing
    /// outside walks FreeType's layouts.
    pub fn ft_face_ptr(&self) -> *mut c_void {
        self.face.as_ptr().cast()
    }

    /// Set how outlines are grid-fitted. Cached glyphs rasterized before this
    /// keep the old fitting, so callers set it once, at load.
    pub fn set_hinting(&self, hinting: Hinting) {
        self.hinting.set(hinting);
    }

    /// Whether this face has a glyph for `ch`. A cmap lookup, not a load:
    /// FT_Load_Char happily returns .notdef for a missing character, so this is
    /// the only way to know a fallback is needed before drawing a tofu box.
    pub fn has_glyph(&self, ch: char) -> bool {
        // SAFETY: face is valid; the call is a read-only cmap lookup.
        unsafe { FT_Get_Char_Index(self.face.as_ptr(), ch as u32 as c_ulong) != 0 }
    }

    /// Select this face's first bitmap strike and report its nominal pixel
    /// size, or `None` when the face has no strikes (an outline-only build).
    ///
    /// A color bitmap font carries fixed sizes rather than scalable outlines,
    /// so a size must be chosen from what it ships and the result scaled to the
    /// text size afterwards. Noto Color Emoji ships exactly one strike, so this
    /// runs once at open and is never re-selected.
    pub fn select_first_strike(&self) -> Option<f32> {
        // SAFETY: face is valid; num_fixed_sizes is a plain field read.
        let strikes = unsafe { (*self.face.as_ptr()).num_fixed_sizes };
        if strikes <= 0 {
            return None;
        }
        // SAFETY: strike 0 exists, checked above.
        let err = unsafe { FT_Select_Size(self.face.as_ptr(), 0) };
        if err != 0 {
            return None;
        }
        // SAFETY: a successful select populates face->size.
        let size = NonNull::new(unsafe { (*self.face.as_ptr()).size })?;
        let ppem = unsafe { size.as_ref().metrics.y_ppem };
        (ppem > 0).then(|| f32::from(ppem))
    }

    /// Decode `glyph_index`'s color strike into premultiplied ARGB, or `None`
    /// for a load failure or a glyph that is not a color bitmap.
    ///
    /// Takes a glyph index rather than a character because the shaper resolves
    /// a whole cluster (a flag, a ZWJ family) to one ligature glyph that no
    /// single character maps to.
    pub fn color_strike(&self, glyph_index: u32) -> Option<ColorStrike> {
        // SAFETY: face is valid with a strike selected; FT_LOAD_COLOR decodes
        // into the face's shared glyph slot, read out below before any later
        // FreeType call.
        let err = unsafe {
            FT_Load_Glyph(
                self.face.as_ptr(),
                glyph_index as c_uint,
                FT_LOAD_RENDER | FT_LOAD_COLOR,
            )
        };
        if err != 0 {
            return None;
        }
        // SAFETY: the load succeeded, so face->glyph points at the populated
        // slot; the borrow ends before any later FreeType call.
        let slot = NonNull::new(unsafe { (*self.face.as_ptr()).glyph })?;
        let slot = unsafe { slot.as_ref() };
        if slot.bitmap.pixel_mode != FT_PIXEL_MODE_BGRA {
            return None;
        }
        Some(ColorStrike {
            left: slot.bitmap_left,
            top: slot.bitmap_top,
            width: slot.bitmap.width as usize,
            rows: slot.bitmap.rows as usize,
            argb: copy_bgra(&slot.bitmap),
        })
    }

    /// Ascent and descent at the current pixel size, in pixels. Descent is
    /// negative, matching the FreeType convention.
    pub fn line_metrics(&self) -> (f32, f32) {
        // SAFETY: face is valid; face->size is non-null once a size is set,
        // which callers always do first. The metrics are copied out.
        let size = unsafe { (*self.face.as_ptr()).size };
        let Some(size) = NonNull::new(size) else {
            return (0.0, 0.0);
        };
        let metrics = unsafe { &size.as_ref().metrics };
        (
            metrics.ascender as f32 / 64.0,
            metrics.descender as f32 / 64.0,
        )
    }
}

impl Drop for Face {
    fn drop(&mut self) {
        // The face must be freed before the library it came from; the font bytes
        // (the _data field) drop after this body, so FreeType stops reading them
        // first. SAFETY: each handle came from its matching new call and is freed
        // exactly once.
        unsafe {
            FT_Done_Face(self.face.as_ptr());
            FT_Done_FreeType(self.library.as_ptr());
        }
    }
}

/// Find a face's optical size axis, or `None` when it has no variation axes or
/// none of them is opsz.
fn optical_axis(library: FtLibrary, face: FtFace) -> Option<OpticalAxis> {
    let mut mm: *mut FtMmVar = ptr::null_mut();
    // SAFETY: face is valid; FreeType allocates the descriptor and writes its
    // pointer through amaster, returning nonzero for a font with no axes.
    let err = unsafe { FT_Get_MM_Var(face, &mut mm) };
    let mm = NonNull::new(mm).filter(|_| err == 0)?;
    // SAFETY: the call succeeded, so the descriptor and its num_axis-long axis
    // array are populated and live until FT_Done_MM_Var below.
    let found = unsafe {
        let var = mm.as_ref();
        let axes = var.num_axis as usize;
        (0..axes).find_map(|index| {
            let axis = &*var.axis.add(index);
            (axis.tag == OPSZ_TAG).then(|| OpticalAxis {
                index,
                axes,
                min: from_fixed(axis.minimum),
                max: from_fixed(axis.maximum),
                coords: RefCell::new(vec![0 as FtFixed; axes]),
            })
        })
    };
    // SAFETY: mm came from FT_Get_MM_Var and is freed exactly once here; the
    // borrow above has ended.
    unsafe { FT_Done_MM_Var(library, mm.as_ptr()) };
    found
}

/// Copy a BGRA color strike into tightly packed `0xAARRGGBB` pixels, keeping
/// FreeType's premultiplied alpha as-is: a pure byte reorder. Compositing and
/// resampling stay premultiplied, and only the final display-size pixels
/// convert to straight alpha for the blitter. The caller confirms
/// FT_PIXEL_MODE_BGRA first.
fn copy_bgra(bitmap: &FtBitmap) -> Vec<u32> {
    let width = bitmap.width as usize;
    let rows = bitmap.rows as usize;
    if width == 0 || rows == 0 || bitmap.buffer.is_null() {
        return Vec::new();
    }
    let mut out = vec![0u32; width * rows];
    let pitch = bitmap.pitch as isize;
    for y in 0..rows {
        for x in 0..width {
            // SAFETY: FreeType guarantees |pitch| >= width*4 bytes per BGRA row,
            // so the 4-byte read at buffer + y*pitch + x*4 stays within the row;
            // out is a distinct buffer.
            let [b, g, r, a] = unsafe {
                let p = bitmap.buffer.offset(y as isize * pitch + (x as isize) * 4);
                [*p, *p.add(1), *p.add(2), *p.add(3)]
            };
            out[y * width + x] = u32::from_be_bytes([a, r, g, b]);
        }
    }
    out
}

/// Copy a FreeType bitmap into a tightly packed top-down coverage buffer of
/// width * rows bytes, dropping inter-row padding. Row y starts at
/// buffer + y*pitch for either pitch sign. Assumes gray mode (one byte per
/// pixel); the caller confirms FT_PIXEL_MODE_GRAY first.
fn copy_coverage(bitmap: &FtBitmap) -> Vec<u8> {
    let width = bitmap.width as usize;
    let rows = bitmap.rows as usize;
    let total = rows * width;
    if width == 0 || rows == 0 || bitmap.buffer.is_null() {
        return Vec::new();
    }
    let mut out = vec![0u8; total];
    let pitch = bitmap.pitch as isize;
    for y in 0..rows {
        // SAFETY: FreeType guarantees |pitch| >= width bytes per row, so the
        // width-byte read at buffer + y*pitch stays within that row; out is
        // distinct from the source.
        unsafe {
            let src = bitmap.buffer.offset(y as isize * pitch);
            ptr::copy_nonoverlapping(src, out[y * width..].as_mut_ptr(), width);
        }
    }
    out
}
