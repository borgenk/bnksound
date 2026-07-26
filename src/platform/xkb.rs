//! Keyboard decoding through the system libxkbcommon.
//!
//! The compositor hands over a keymap as a file descriptor; xkb turns raw
//! evdev keycodes into keysyms and characters under the active layout and
//! modifier state.
//!
//! What a keysym means to an application is the application's business, so this
//! reports the keysym, the character it produces, and which modifiers are held,
//! and leaves the naming of keys to the caller.

use std::ffi::{CString, c_void};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr::NonNull;

use core::ffi::{c_char, c_int, c_uint};

/// XKB_KEYMAP_FORMAT_TEXT_V1, the only format wl_keyboard sends.
const KEYMAP_FORMAT_TEXT_V1: c_uint = 1;
/// XKB_STATE_MODS_EFFECTIVE.
const STATE_MODS_EFFECTIVE: c_uint = 1 << 3;
/// Wayland reports evdev codes; xkb keycodes are offset by 8.
const EVDEV_OFFSET: u32 = 8;

const PROT_READ: c_int = 0x1;
const MAP_PRIVATE: c_int = 0x02;

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: c_uint) -> *mut c_void;
    fn xkb_context_unref(context: *mut c_void);
    fn xkb_keymap_new_from_string(
        context: *mut c_void,
        string: *const c_char,
        format: c_uint,
        flags: c_uint,
    ) -> *mut c_void;
    fn xkb_keymap_unref(keymap: *mut c_void);
    fn xkb_state_new(keymap: *mut c_void) -> *mut c_void;
    fn xkb_state_unref(state: *mut c_void);
    fn xkb_state_update_mask(
        state: *mut c_void,
        depressed_mods: c_uint,
        latched_mods: c_uint,
        locked_mods: c_uint,
        depressed_layout: c_uint,
        latched_layout: c_uint,
        locked_layout: c_uint,
    ) -> c_uint;
    fn xkb_state_key_get_one_sym(state: *mut c_void, key: c_uint) -> c_uint;
    fn xkb_keysym_to_utf32(keysym: c_uint) -> c_uint;
    fn xkb_keymap_key_repeats(keymap: *mut c_void, key: c_uint) -> c_int;
    fn xkb_state_mod_name_is_active(
        state: *mut c_void,
        name: *const c_char,
        component: c_uint,
    ) -> c_int;
}

/// An xkb context, keymap, and state, owning its C-side handles.
pub struct Keyboard {
    context: NonNull<c_void>,
    keymap: NonNull<c_void>,
    state: NonNull<c_void>,
}

impl Keyboard {
    /// Build from the keymap the compositor sent on `fd`, `size` bytes long.
    ///
    /// The keymap must be mapped rather than read: the compositor writes it and
    /// leaves the shared file offset at the end, so a read returns nothing.
    pub fn from_keymap_fd(fd: OwnedFd, size: u32) -> io::Result<Self> {
        let len = size as usize;
        if len == 0 {
            return Err(io::Error::other("empty keymap"));
        }
        // SAFETY: a private read-only mapping of the whole keymap.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ,
                MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the mapping holds len readable bytes; copy them out before
        // unmapping so nothing points into it afterwards.
        let mut bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }.to_vec();
        // SAFETY: we own this mapping and unmap it exactly once.
        unsafe { munmap(ptr, len) };

        // The blob is NUL-terminated and may be padded; keep the text only.
        if let Some(nul) = bytes.iter().position(|&b| b == 0) {
            bytes.truncate(nul);
        }
        let text = String::from_utf8(bytes).map_err(io::Error::other)?;
        Self::from_keymap_text(&text)
    }

    /// Build from keymap text. Split out from the fd path so the decoding rules
    /// can be exercised without a compositor.
    pub fn from_keymap_text(text: &str) -> io::Result<Self> {
        let text = CString::new(text).map_err(io::Error::other)?;

        // SAFETY: a fresh context with default flags.
        let context = NonNull::new(unsafe { xkb_context_new(0) })
            .ok_or_else(|| io::Error::other("xkb_context_new failed"))?;
        // SAFETY: context is live; text is a valid NUL-terminated keymap.
        let keymap = unsafe {
            xkb_keymap_new_from_string(context.as_ptr(), text.as_ptr(), KEYMAP_FORMAT_TEXT_V1, 0)
        };
        let Some(keymap) = NonNull::new(keymap) else {
            // SAFETY: context came from xkb_context_new and is freed once.
            unsafe { xkb_context_unref(context.as_ptr()) };
            return Err(io::Error::other("xkb_keymap_new_from_string failed"));
        };
        // SAFETY: keymap is live.
        let state = unsafe { xkb_state_new(keymap.as_ptr()) };
        let Some(state) = NonNull::new(state) else {
            // SAFETY: both handles are live and freed once, keymap before context.
            unsafe {
                xkb_keymap_unref(keymap.as_ptr());
                xkb_context_unref(context.as_ptr());
            }
            return Err(io::Error::other("xkb_state_new failed"));
        };
        Ok(Keyboard {
            context,
            keymap,
            state,
        })
    }

    /// Apply a wl_keyboard.modifiers update.
    pub fn update_mask(&self, depressed: u32, latched: u32, locked: u32, group: u32) {
        // SAFETY: state is live; the masks are opaque bit sets from the
        // compositor and are validated by xkb itself.
        unsafe {
            xkb_state_update_mask(self.state.as_ptr(), depressed, latched, locked, 0, 0, group);
        }
    }

    /// Whether Control is in effect.
    pub fn ctrl_active(&self) -> bool {
        self.mod_active("Control")
    }

    /// Whether Shift is in effect.
    pub fn shift_active(&self) -> bool {
        self.mod_active("Shift")
    }

    /// Whether Alt is in effect.
    pub fn alt_active(&self) -> bool {
        self.mod_active("Mod1")
    }

    fn mod_active(&self, name: &str) -> bool {
        let Ok(c) = CString::new(name) else {
            return false;
        };
        // SAFETY: state is live and name is a valid NUL-terminated string.
        let active = unsafe {
            xkb_state_mod_name_is_active(self.state.as_ptr(), c.as_ptr(), STATE_MODS_EFFECTIVE)
        };
        active > 0
    }

    /// Whether holding this key should repeat. Modifiers and other latching
    /// keys report false, so holding Ctrl does not spam the UI.
    pub fn repeats(&self, evdev_code: u32) -> bool {
        // SAFETY: keymap is live; an unknown keycode simply reports false.
        unsafe { xkb_keymap_key_repeats(self.keymap.as_ptr(), evdev_code + EVDEV_OFFSET) > 0 }
    }

    /// The keysym an evdev keycode produces under the current layout and
    /// modifiers. XKB_KEY_NoSymbol (0) when the keycode maps to nothing.
    pub fn keysym(&self, evdev_code: u32) -> u32 {
        // SAFETY: state is live; an unknown keycode yields XKB_KEY_NoSymbol.
        unsafe { xkb_state_key_get_one_sym(self.state.as_ptr(), evdev_code + EVDEV_OFFSET) }
    }

    /// The graphic character an evdev keycode types, if it types one. Controls
    /// and DEL are not characters for this purpose.
    ///
    /// Derived from the keysym, which reflects Shift and the layout but not
    /// Ctrl or Alt. Asking the state for text instead would fold Ctrl in
    /// (Ctrl+K becomes 0x0b) and the shortcut would be lost as an unprintable
    /// control character.
    pub fn character(&self, evdev_code: u32) -> Option<char> {
        // SAFETY: a pure value conversion; 0 means the keysym has no Unicode form.
        let cp = unsafe { xkb_keysym_to_utf32(self.keysym(evdev_code)) };
        if cp < 0x20 {
            return None;
        }
        char::from_u32(cp).filter(|&c| c != '\u{7f}')
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // The state and keymap must go before the context they came from.
        // SAFETY: each handle came from its matching xkb constructor and is
        // freed exactly once here.
        unsafe {
            xkb_state_unref(self.state.as_ptr());
            xkb_keymap_unref(self.keymap.as_ptr());
            xkb_context_unref(self.context.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest keymap that answers "is Ctrl held" and "what does K type":
    /// one key bound to Control_L and mapped into the real Control modifier,
    /// plus one letter key. Written out here rather than read from the system's
    /// xkb data so the test depends on nothing outside the process.
    ///
    /// Keycodes are xkb-side (evdev + 8): 37 is LEFTCTRL(29), 45 is K(37).
    const MINIMAL_KEYMAP: &str = r#"
xkb_keymap {
  xkb_keycodes "min" { minimum = 8; maximum = 255; <LCTL> = 37; <KK> = 45; };
  xkb_types "min" {
    type "ONE_LEVEL" { modifiers = none; map[none] = Level1; level_name[Level1] = "Any"; };
  };
  xkb_compatibility "min" {
    interpret Control_L { action = SetMods(modifiers = Control); };
  };
  xkb_symbols "min" {
    key <LCTL> { type = "ONE_LEVEL", [ Control_L ] };
    key <KK> { type = "ONE_LEVEL", [ k ] };
    modifier_map Control { <LCTL> };
  };
};
"#;

    /// Real-modifier bit for Control, as xkb orders them.
    const CONTROL_MASK: u32 = 1 << 2;
    /// evdev codes, which `key` offsets by 8 itself.
    const EVDEV_LEFTCTRL: u32 = 29;
    const EVDEV_K: u32 = 37;

    fn keyboard() -> Keyboard {
        Keyboard::from_keymap_text(MINIMAL_KEYMAP).expect("minimal keymap")
    }

    #[test]
    fn a_keymap_parses_from_text() {
        let _ = keyboard();
    }

    #[test]
    fn modifiers_start_inactive_and_follow_the_mask() {
        let kb = keyboard();
        assert!(!kb.ctrl_active());
        kb.update_mask(CONTROL_MASK, 0, 0, 0);
        assert!(kb.ctrl_active(), "Control should read as held");
        kb.update_mask(0, 0, 0, 0);
        assert!(!kb.ctrl_active(), "and as released again");
    }

    #[test]
    fn a_letter_decodes_to_its_character() {
        let kb = keyboard();
        assert_eq!(kb.character(EVDEV_K), Some('k'));
        assert_eq!(kb.keysym(EVDEV_K), 0x6b, "the keysym for lowercase k");
    }

    /// Asking xkb for the *state's* text folds Ctrl in, so Ctrl+K yields 0x0b,
    /// which is unprintable and was being dropped: the shortcut never fired.
    /// Deriving the character from the keysym keeps it a plain 'k'.
    #[test]
    fn ctrl_held_still_decodes_the_letter_not_a_control_code() {
        let kb = keyboard();
        kb.update_mask(CONTROL_MASK, 0, 0, 0);
        assert!(kb.ctrl_active());
        assert_eq!(
            kb.character(EVDEV_K),
            Some('k'),
            "Ctrl+K must still read as the letter k"
        );
    }

    #[test]
    fn letters_repeat_but_modifiers_do_not() {
        let kb = keyboard();
        assert!(kb.repeats(EVDEV_K), "a letter should auto-repeat");
        assert!(
            !kb.repeats(EVDEV_LEFTCTRL),
            "holding a modifier must not repeat"
        );
    }

    #[test]
    fn a_modifier_key_produces_no_character() {
        let kb = keyboard();
        assert_eq!(kb.character(EVDEV_LEFTCTRL), None);
    }
}
