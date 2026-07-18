//! Fixed-capacity storage for text built on a hot path.
//!
//! [`ArrayString`] stands in for String where a value is assembled, read, and
//! dropped inside one frame. It keeps its bytes inline in a const-sized array,
//! so building one costs no allocation, and a write that would exceed the
//! capacity is refused rather than growing.
//!
//! Refusing is the right answer for what this holds: display text that is
//! already being cut to fit a rectangle. A caller picks a capacity no label it
//! draws can reach, and the cap is a backstop rather than a policy.

use core::fmt;
use core::ops::Deref;

/// A string with a compile-time capacity and no heap backing.
pub struct ArrayString<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> ArrayString<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the last write was refused for want of room.
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    pub fn as_str(&self) -> &str {
        // SAFETY: only push_str and push write the buffer, and both append
        // whole UTF-8 sequences, so buf[..len] is always a valid str.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    /// Append a slice, or refuse and leave the value unchanged when it would
    /// not fit. Whole-or-nothing, so the result never ends mid-character.
    pub fn push_str(&mut self, s: &str) -> bool {
        let bytes = s.as_bytes();
        let Some(end) = self.len.checked_add(bytes.len()).filter(|e| *e <= N) else {
            return false;
        };
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        true
    }

    /// Append one character, or refuse it whole.
    pub fn push(&mut self, c: char) -> bool {
        let mut tmp = [0u8; 4];
        self.push_str(c.encode_utf8(&mut tmp))
    }

    /// Append characters until the cap is reached, reporting how many landed.
    /// What does not fit is dropped, which for display text is the truncation
    /// the caller was going to do anyway.
    pub fn push_chars(&mut self, chars: impl Iterator<Item = char>, limit: usize) -> usize {
        let mut taken = 0;
        for c in chars.take(limit) {
            if !self.push(c) {
                break;
            }
            taken += 1;
        }
        taken
    }
}

impl<const N: usize> Default for ArrayString<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Deref for ArrayString<N> {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> fmt::Display for ArrayString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> fmt::Debug for ArrayString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Lets `write!` build one without a formatting allocation. A write that does
/// not fit reports an error, which is what refusing looks like to `write!`.
impl<const N: usize> fmt::Write for ArrayString<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.push_str(s) {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[test]
    fn it_holds_what_fits() {
        let mut s: ArrayString<16> = ArrayString::new();
        assert!(s.is_empty());
        assert!(s.push_str("hello"));
        assert!(s.push(' '));
        assert!(s.push_str("world"));
        assert_eq!(&*s, "hello world");
        assert_eq!(s.len(), 11);
    }

    #[test]
    fn a_write_that_would_not_fit_is_refused_whole() {
        let mut s: ArrayString<8> = ArrayString::new();
        assert!(s.push_str("1234"));
        assert!(
            !s.push_str("56789"),
            "nine bytes do not fit in the four left"
        );
        assert_eq!(&*s, "1234", "and the refused write left nothing behind");
        assert!(s.push_str("5678"), "exactly filling it is fine");
        assert!(s.is_full());
        assert!(!s.push('x'));
        assert_eq!(&*s, "12345678");
    }

    /// A multi-byte character is refused whole or not at all, so the buffer
    /// never holds half a code point.
    #[test]
    fn multibyte_characters_never_split() {
        let mut s: ArrayString<4> = ArrayString::new();
        assert!(s.push_str("ab"));
        assert!(!s.push('…'), "three more bytes do not fit in two");
        assert_eq!(&*s, "ab");
        assert!(s.push('é'), "two bytes do");
        assert_eq!(&*s, "abé");
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn push_chars_stops_at_the_limit_and_at_the_cap() {
        let mut s: ArrayString<64> = ArrayString::new();
        assert_eq!(s.push_chars("abcdefgh".chars(), 3), 3);
        assert_eq!(&*s, "abc");

        // Four-byte characters: eight of them exceed a 16-byte buffer.
        let mut small: ArrayString<16> = ArrayString::new();
        let taken = small.push_chars("🎧🎧🎧🎧🎧🎧🎧🎧".chars(), 8);
        assert_eq!(taken, 4, "only four fit");
        assert_eq!(small.len(), 16);
        assert_eq!(&*small, "🎧🎧🎧🎧");
    }

    #[test]
    fn it_formats_without_allocating() {
        let mut s: ArrayString<16> = ArrayString::new();
        write!(s, " ×{}", 12).expect("fits");
        assert_eq!(&*s, " ×12");
    }

    #[test]
    fn a_format_that_overflows_reports_rather_than_truncating() {
        let mut s: ArrayString<4> = ArrayString::new();
        assert!(write!(s, "{}", 123_456).is_err());
        assert!(s.is_empty(), "the refused write left it untouched");
    }
}
