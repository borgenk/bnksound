//! Font lookup through fontconfig.
//!
//! Turns a family name into a file on disk, which is the step between "the
//! desktop is configured to use Segoe UI Variable" and something FreeType can
//! open. Matching goes through the user's own fontconfig setup, so aliases,
//! substitutions, and per-family tweaks apply the same way they do for every
//! other application.
//!
//! A weight is part of the query rather than something applied afterward, so
//! bold comes from a real bold face. The index that comes back may name a
//! variable font's instance rather than a whole file, which is why callers
//! carry it through to FreeType instead of assuming zero.

use std::ffi::{CStr, CString};
use std::path::PathBuf;

use core::ffi::{c_char, c_int, c_uchar, c_void};

use crate::platform::freetype::Hinting;

/// FcMatchPattern, the substitution phase for a query pattern.
const MATCH_PATTERN: c_int = 0;
/// FcResultMatch, returned when a property was present.
const RESULT_MATCH: c_int = 0;

/// fontconfig weight for a normal face.
pub const WEIGHT_REGULAR: c_int = 80;
/// fontconfig weight for a bold face.
pub const WEIGHT_BOLD: c_int = 200;

#[link(name = "fontconfig")]
unsafe extern "C" {
    fn FcInit() -> c_int;
    fn FcPatternCreate() -> *mut c_void;
    fn FcPatternDestroy(pattern: *mut c_void);
    fn FcPatternAddString(pattern: *mut c_void, object: *const c_char, s: *const c_uchar) -> c_int;
    fn FcPatternAddInteger(pattern: *mut c_void, object: *const c_char, value: c_int) -> c_int;
    fn FcPatternGetString(
        pattern: *mut c_void,
        object: *const c_char,
        index: c_int,
        s: *mut *mut c_uchar,
    ) -> c_int;
    fn FcPatternGetInteger(
        pattern: *mut c_void,
        object: *const c_char,
        index: c_int,
        value: *mut c_int,
    ) -> c_int;
    fn FcConfigSubstitute(config: *mut c_void, pattern: *mut c_void, kind: c_int) -> c_int;
    fn FcDefaultSubstitute(pattern: *mut c_void);
    fn FcFontMatch(config: *mut c_void, pattern: *mut c_void, result: *mut c_int) -> *mut c_void;
    fn FcPatternAddBool(pattern: *mut c_void, object: *const c_char, value: c_int) -> c_int;
    fn FcFontSort(
        config: *mut c_void,
        pattern: *mut c_void,
        trim: c_int,
        csp: *mut c_void,
        result: *mut c_int,
    ) -> *mut FcFontSet;
    fn FcFontSetDestroy(set: *mut FcFontSet);
    fn FcPatternGetCharSet(
        pattern: *mut c_void,
        object: *const c_char,
        index: c_int,
        charset: *mut *mut c_void,
    ) -> c_int;
    fn FcCharSetHasChar(charset: *mut c_void, ch: u32) -> c_int;
    fn FcPatternGetBool(
        pattern: *mut c_void,
        object: *const c_char,
        index: c_int,
        value: *mut c_int,
    ) -> c_int;
}

/// The desktop's grid-fitting preference for a matched face.
///
/// fontconfig resolves this from the user's settings and the per-font rules a
/// distribution ships, which is why it is read off the match rather than
/// assumed. Ignoring it is what makes text look unlike every other application
/// on the machine.
fn hinting_of(pattern: *mut c_void) -> Hinting {
    let mut enabled: c_int = FC_TRUE;
    // SAFETY: pattern is live; enabled is a live local. A missing property
    // leaves the default in place.
    unsafe { FcPatternGetBool(pattern, c"hinting".as_ptr(), 0, &mut enabled) };
    if enabled != FC_TRUE {
        return Hinting::None;
    }
    let mut style: c_int = -1;
    // SAFETY: pattern is live; style is a live local.
    unsafe { FcPatternGetInteger(pattern, c"hintstyle".as_ptr(), 0, &mut style) };
    match style {
        // FC_HINT_NONE, FC_HINT_SLIGHT.
        0 => Hinting::None,
        1 => Hinting::Slight,
        // FC_HINT_MEDIUM, FC_HINT_FULL, and an unset property, which the
        // toolkits treat as full.
        _ => Hinting::Full,
    }
}

/// FcFontSet: a count and an array of pattern pointers. Only those two fields
/// are read; fontconfig owns the patterns.
#[repr(C)]
struct FcFontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut *mut c_void,
}

/// FcTrue.
const FC_TRUE: c_int = 1;

/// A face fontconfig picked: the file holding it, and which face inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFace {
    pub path: PathBuf,
    /// How the desktop wants this face grid-fitted.
    pub hinting: Hinting,
    /// FreeType face index. The high bits may select a named instance of a
    /// variable font, so it is passed through untouched.
    pub index: i32,
}

/// Owns a pattern for as long as the query needs it.
///
/// fontconfig hands back raw pointers that must be destroyed exactly once, and
/// the query has several early exits; tying the pointer to a value means none
/// of them can leak it.
struct Pattern(*mut c_void);

impl Drop for Pattern {
    fn drop(&mut self) {
        // SAFETY: the pointer came from FcPatternCreate or FcFontMatch, is
        // non-null by construction, and is destroyed once here.
        unsafe { FcPatternDestroy(self.0) };
    }
}

/// Find the best face for `family` at `weight`.
///
/// None when fontconfig has nothing to offer, which callers treat as a reason
/// to fall back rather than as a failure.
pub fn resolve(family: &str, weight: c_int) -> Option<ResolvedFace> {
    // SAFETY: initializes the default configuration. Idempotent, and returns
    // false only if fontconfig itself cannot start.
    if unsafe { FcInit() } == 0 {
        return None;
    }

    let family_c = CString::new(family).ok()?;
    let obj_family = c"family";
    let obj_weight = c"weight";

    // SAFETY: allocates an empty pattern, null only when out of memory.
    let pattern = Pattern(unsafe { FcPatternCreate() });
    if pattern.0.is_null() {
        return None;
    }

    // SAFETY: pattern is live; the object names and family string are valid
    // nul-terminated C strings that fontconfig copies.
    unsafe {
        FcPatternAddString(
            pattern.0,
            obj_family.as_ptr(),
            family_c.as_ptr().cast::<c_uchar>(),
        );
        FcPatternAddInteger(pattern.0, obj_weight.as_ptr(), weight);
        // Apply the user's fontconfig rules, then fill in defaults, which is
        // the sequence a match is expected to have been through.
        FcConfigSubstitute(std::ptr::null_mut(), pattern.0, MATCH_PATTERN);
        FcDefaultSubstitute(pattern.0);
    }

    let mut result: c_int = 0;
    // SAFETY: a null config means the current one; pattern is live; result is a
    // live local. The returned pattern is owned by us.
    let matched = Pattern(unsafe { FcFontMatch(std::ptr::null_mut(), pattern.0, &mut result) });
    if matched.0.is_null() || result != RESULT_MATCH {
        return None;
    }

    face_from_pattern(matched.0)
}

/// The file and face index a pattern names, or `None` when it has no file (a
/// font that cannot be opened).
///
/// The pointer fontconfig writes points into the pattern's own storage, so the
/// path is copied out before the pattern can be destroyed.
fn face_from_pattern(pattern: *mut c_void) -> Option<ResolvedFace> {
    let mut file: *mut c_uchar = std::ptr::null_mut();
    // SAFETY: pattern is live; file is a live local written with a borrowed
    // pointer valid for as long as the pattern.
    let got = unsafe { FcPatternGetString(pattern, c"file".as_ptr(), 0, &mut file) };
    if got != RESULT_MATCH || file.is_null() {
        return None;
    }
    // SAFETY: fontconfig guarantees a nul-terminated string here.
    let path = unsafe { CStr::from_ptr(file.cast::<c_char>()) };
    let path = PathBuf::from(String::from_utf8_lossy(path.to_bytes()).into_owned());

    // A missing index means the first face, which is the common case. It is
    // load-bearing for a TrueType collection, where several faces share one
    // file and face 0 is the wrong language as often as the right one.
    let mut index: c_int = 0;
    // SAFETY: pattern is live; index is a live local.
    unsafe { FcPatternGetInteger(pattern, c"index".as_ptr(), 0, &mut index) };

    Some(ResolvedFace {
        path,
        index,
        hinting: hinting_of(pattern),
    })
}

/// The system's fonts, ranked once, ready to answer "who can draw this?".
///
/// The obvious binding is a fresh `FcFontMatch` per character, carrying a
/// one-character charset. That re-runs the whole configuration every time:
/// substitutions, defaults, and a scored sort of every font installed. Sorting
/// once instead and answering from the charsets fontconfig already holds in
/// memory turns each later question into a bitset test with no library call and
/// no allocation, which is cheap enough to ask per character.
///
/// `trim` drops any font adding no coverage the ones above it already have, so
/// the ranking stays short.
pub struct Fontconfig {
    /// The ranked fonts. Owned: the charsets read in [`Self::font_for_char`]
    /// point into these patterns, so the set outlives every query against it.
    set: *mut FcFontSet,
}

impl Fontconfig {
    /// Load and rank the system's fonts, or `None` when fontconfig will not
    /// start or offers nothing. Never fatal: the caller keeps drawing from the
    /// font it already has.
    ///
    /// This is the expensive call, so it is made lazily, on the first character
    /// the configured font cannot draw. A session of plain Latin text never
    /// makes it at all.
    pub fn new() -> Option<Self> {
        // SAFETY: takes no arguments and is idempotent; false means the
        // configuration could not be loaded.
        if unsafe { FcInit() } != FC_TRUE {
            return None;
        }

        // SAFETY: allocates an empty pattern, null only when out of memory.
        let query = Pattern(unsafe { FcPatternCreate() });
        if query.0.is_null() {
            return None;
        }
        // SAFETY: query is live; the object name is a nul-terminated literal.
        // Bitmap-only faces cannot be opened at an arbitrary pixel size, so
        // asking for scalable fonts keeps them out of the ranking entirely.
        unsafe {
            if FcPatternAddBool(query.0, c"scalable".as_ptr(), FC_TRUE) != FC_TRUE {
                return None;
            }
            FcConfigSubstitute(std::ptr::null_mut(), query.0, MATCH_PATTERN);
            FcDefaultSubstitute(query.0);
        }

        let mut result: c_int = 0;
        // SAFETY: query is live; a null config means the current one; trim
        // drops fonts adding no new coverage; a null charset pointer declines
        // the accumulated set. The returned set is owned and freed in Drop.
        let set = unsafe {
            FcFontSort(
                std::ptr::null_mut(),
                query.0,
                FC_TRUE,
                std::ptr::null_mut(),
                &mut result,
            )
        };
        if set.is_null() || result != RESULT_MATCH {
            return None;
        }
        // SAFETY: set is non-null and was just returned by FcFontSort.
        if unsafe { (*set).nfont } <= 0 {
            // SAFETY: a live owned set, destroyed exactly once here.
            unsafe { FcFontSetDestroy(set) };
            return None;
        }
        Some(Fontconfig { set })
    }

    /// The font the system would use to draw `ch`: the first in the ranking
    /// whose character map contains it, or `None` when nothing installed does.
    pub fn font_for_char(&self, ch: char) -> Option<ResolvedFace> {
        // SAFETY: set is live and owned with nfont > 0, checked at construction;
        // fonts points at nfont valid pattern pointers.
        let (count, fonts) = unsafe { ((*self.set).nfont, (*self.set).fonts) };
        for i in 0..count {
            // SAFETY: i is in 0..nfont, so this is one of the set's live
            // patterns, borrowed and never freed here.
            let font = unsafe { *fonts.add(i as usize) };
            if font.is_null() {
                continue;
            }
            let mut charset: *mut c_void = std::ptr::null_mut();
            // SAFETY: font is live; the object name is a nul-terminated
            // literal; fontconfig writes a borrowed pointer valid as long as
            // the pattern, and so the set.
            let found = unsafe { FcPatternGetCharSet(font, c"charset".as_ptr(), 0, &mut charset) };
            if found != RESULT_MATCH || charset.is_null() {
                continue;
            }
            // SAFETY: charset is a live set borrowed from the pattern above.
            if unsafe { FcCharSetHasChar(charset, ch as u32) } != FC_TRUE {
                continue;
            }
            // A font claiming the character but carrying no file cannot be
            // opened, so keep walking rather than give up on the character.
            if let Some(face) = face_from_pattern(font) {
                return Some(face);
            }
        }
        None
    }
}

impl Drop for Fontconfig {
    fn drop(&mut self) {
        // SAFETY: the set came from FcFontSort and is destroyed exactly once.
        // Every charset borrowed from it is gone: they are only read inside
        // font_for_char, which never lets one escape.
        unsafe { FcFontSetDestroy(self.set) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generic_family_resolves_to_a_real_file() {
        // Every system with fonts installed answers sans-serif, so this also
        // proves the linkage and the pattern dance work.
        let Some(face) = resolve("sans-serif", WEIGHT_REGULAR) else {
            // A machine with no fonts at all is not a test failure.
            return;
        };
        assert!(
            face.path.exists(),
            "fontconfig named a file that is not there: {}",
            face.path.display(),
        );
        assert!(face.index >= 0);
    }

    #[test]
    fn an_unknown_family_still_falls_back_rather_than_failing() {
        // fontconfig always substitutes something, which is what keeps the app
        // drawing text when the configured font is missing.
        if let Some(face) = resolve("this-font-does-not-exist-anywhere", WEIGHT_REGULAR) {
            assert!(face.path.exists());
        }
    }

    #[test]
    fn asking_for_bold_does_not_error() {
        if let Some(face) = resolve("sans-serif", WEIGHT_BOLD) {
            assert!(face.path.exists());
        }
    }

    #[test]
    fn a_family_name_with_an_interior_nul_is_rejected_not_passed_on() {
        assert_eq!(resolve("bad\0name", WEIGHT_REGULAR), None);
    }
}
