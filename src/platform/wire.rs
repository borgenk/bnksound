//! Wayland wire-format encoding and decoding.
//!
//! A message is: u32 object id, then u32 (size << 16 | opcode) where size is the
//! whole length in bytes including this 8-byte header, then the arguments.
//! Everything is host byte order (little-endian on this target), each argument
//! padded to a 4-byte boundary. File descriptors are not in the byte stream;
//! they travel as SCM_RIGHTS ancillary data (see connection).

/// Header size in bytes: object id plus size/opcode word.
pub const HEADER_SIZE: usize = 8;

/// One request argument. Bind is the generic new-id used only by
/// wl_registry.bind, where the interface is sent inline.
pub enum Arg<'a> {
    Int(i32),
    Uint(u32),
    Object(u32),
    NewId(u32),
    Str(&'a str),
    Array(&'a [u8]),
    Bind {
        interface: &'a str,
        version: u32,
        new_id: u32,
    },
}

/// Append an encoded request to `buf`.
pub fn encode(buf: &mut Vec<u8>, object: u32, opcode: u16, args: &[Arg]) {
    let start = buf.len();
    buf.extend_from_slice(&object.to_ne_bytes());
    buf.extend_from_slice(&[0u8; 4]); // size|opcode, patched below.
    for arg in args {
        match arg {
            Arg::Int(v) => buf.extend_from_slice(&v.to_ne_bytes()),
            Arg::Uint(v) | Arg::Object(v) | Arg::NewId(v) => {
                buf.extend_from_slice(&v.to_ne_bytes())
            }
            Arg::Str(s) => put_str(buf, s),
            Arg::Array(a) => put_array(buf, a),
            Arg::Bind {
                interface,
                version,
                new_id,
            } => {
                put_str(buf, interface);
                buf.extend_from_slice(&version.to_ne_bytes());
                buf.extend_from_slice(&new_id.to_ne_bytes());
            }
        }
    }
    let size = (buf.len() - start) as u32;
    // The size lives in the header's high 16 bits, so a request must stay under
    // 64 KiB. Nothing here builds one anywhere near that; assert it rather than
    // let the shift truncate the length into the opcode bits.
    debug_assert!(
        size <= 0xffff,
        "wayland request too large to encode: {size} bytes"
    );
    let word = (size << 16) | u32::from(opcode);
    buf[start + 4..start + 8].copy_from_slice(&word.to_ne_bytes());
}

/// Encode a string: length including the trailing NUL, the bytes, the NUL, then
/// zero padding to a 4-byte boundary.
fn put_str(buf: &mut Vec<u8>, s: &str) {
    let len = s.len() + 1;
    buf.extend_from_slice(&(len as u32).to_ne_bytes());
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    let pad = (4 - (len % 4)) % 4;
    buf.extend(std::iter::repeat_n(0u8, pad));
}

/// Encode a byte array: length, the bytes, then padding to a 4-byte boundary.
fn put_array(buf: &mut Vec<u8>, a: &[u8]) {
    buf.extend_from_slice(&(a.len() as u32).to_ne_bytes());
    buf.extend_from_slice(a);
    let pad = (4 - (a.len() % 4)) % 4;
    buf.extend(std::iter::repeat_n(0u8, pad));
}

/// A parsed event: its target object, opcode, and the argument bytes.
pub struct Message {
    pub object: u32,
    pub opcode: u16,
    pub body: Vec<u8>,
}

impl Message {
    /// A reader over this message's argument bytes.
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(&self.body)
    }
}

/// Parse one message from the front of `buf`, returning it and the number of
/// bytes consumed, or None if a whole message is not yet buffered.
pub fn parse(buf: &[u8]) -> Option<(Message, usize)> {
    if buf.len() < HEADER_SIZE {
        return None;
    }
    let object = u32::from_ne_bytes(buf[0..4].try_into().ok()?);
    let word = u32::from_ne_bytes(buf[4..8].try_into().ok()?);
    let size = (word >> 16) as usize;
    let opcode = (word & 0xffff) as u16;
    if size < HEADER_SIZE || buf.len() < size {
        return None;
    }
    let body = buf[HEADER_SIZE..size].to_vec();
    Some((
        Message {
            object,
            opcode,
            body,
        },
        size,
    ))
}

/// Sequential reader over an event's argument bytes, in host byte order.
pub struct Reader<'a> {
    body: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(body: &'a [u8]) -> Self {
        Reader { body, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.body.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_ne_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_ne_bytes(self.take(4)?.try_into().ok()?))
    }

    /// Read a wl_fixed: a signed 24.8 fixed-point number, as pixels. Pointer
    /// coordinates and axis values arrive this way.
    pub fn fixed(&mut self) -> Option<f64> {
        Some(f64::from(self.i32()?) / 256.0)
    }

    /// Read a length-prefixed, NUL-terminated, padded string. Borrowed from the
    /// message body, so nothing is copied to look at one.
    pub fn string(&mut self) -> Option<&'a str> {
        let len = self.u32()? as usize;
        if len == 0 {
            return Some("");
        }
        let padded = len.div_ceil(4) * 4;
        let bytes = self.take(padded)?;
        // Drop the trailing NUL and any padding.
        core::str::from_utf8(&bytes[..len - 1]).ok()
    }

    /// Read a length-prefixed, padded byte array, borrowed like a string.
    pub fn array(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        let padded = len.div_ceil(4) * 4;
        self.take(padded)?.get(..len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_parse_roundtrips_header() {
        let mut buf = Vec::new();
        encode(&mut buf, 3, 5, &[Arg::Uint(42), Arg::Int(-7)]);
        let (msg, used) = parse(&buf).expect("one message");
        assert_eq!(used, buf.len());
        assert_eq!(msg.object, 3);
        assert_eq!(msg.opcode, 5);
        let mut r = msg.reader();
        assert_eq!(r.u32(), Some(42));
        assert_eq!(r.i32(), Some(-7));
    }

    #[test]
    fn strings_are_length_prefixed_and_padded() {
        let mut buf = Vec::new();
        // registry.global sends: name(uint), interface(string), version(uint)
        encode(
            &mut buf,
            2,
            0,
            &[Arg::Uint(7), Arg::Str("wl_compositor"), Arg::Uint(4)],
        );
        assert_eq!(buf.len() % 4, 0, "message stays 4-byte aligned");
        let (msg, _) = parse(&buf).unwrap();
        let mut r = msg.reader();
        assert_eq!(r.u32(), Some(7));
        assert_eq!(r.string(), Some("wl_compositor"));
        assert_eq!(r.u32(), Some(4));
    }

    #[test]
    fn parse_reports_none_until_a_whole_message_is_buffered() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 0, &[Arg::Uint(1)]);
        assert!(parse(&buf[..HEADER_SIZE]).is_none(), "header only, no body");
        assert!(parse(&buf[..4]).is_none(), "not even a header");
        assert!(parse(&buf).is_some());
    }

    #[test]
    fn reader_impl_message() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 0, &[Arg::Str("x"), Arg::Uint(9)]);
        let (msg, _) = parse(&buf).unwrap();
        let mut r = msg.reader();
        assert_eq!(r.string(), Some("x"));
        assert_eq!(r.u32(), Some(9));
    }

    #[test]
    fn reader_stops_at_the_end_of_the_body() {
        let mut r = Reader::new(&[0u8, 0, 0]);
        assert_eq!(r.u32(), None, "three bytes are not a u32");
        let mut r = Reader::new(&[]);
        assert_eq!(r.string(), None);
        assert_eq!(r.array(), None);
    }

    #[test]
    fn fixed_decodes_signed_24_8() {
        // 256 is 1.0, -256 is -1.0, 128 is 0.5.
        let mut buf = Vec::new();
        for raw in [256i32, -256, 128] {
            buf.extend_from_slice(&raw.to_ne_bytes());
        }
        let mut r = Reader::new(&buf);
        assert_eq!(r.fixed(), Some(1.0));
        assert_eq!(r.fixed(), Some(-1.0));
        assert_eq!(r.fixed(), Some(0.5));
    }

    #[test]
    fn an_array_keeps_its_length_and_skips_its_padding() {
        // Five bytes: the length word, the bytes, then three of padding. The
        // trailing uint proves the reader resumed at the right offset.
        let mut buf = Vec::new();
        encode(
            &mut buf,
            1,
            0,
            &[Arg::Array(&[1, 2, 3, 4, 5]), Arg::Uint(99)],
        );
        let (msg, _) = parse(&buf).unwrap();
        let mut r = msg.reader();
        assert_eq!(r.array(), Some(&[1u8, 2, 3, 4, 5][..]));
        assert_eq!(r.u32(), Some(99));
    }

    /// Nothing in the reader copies, so a string borrows the message it came
    /// from rather than allocating one per event.
    #[test]
    fn strings_borrow_the_message_body() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 0, &[Arg::Str("wl_seat")]);
        let (msg, _) = parse(&buf).unwrap();
        let borrowed: &str = msg.reader().string().expect("a string");
        assert!(
            borrowed.as_ptr() >= msg.body.as_ptr()
                && borrowed.as_ptr() < msg.body.as_ptr().wrapping_add(msg.body.len()),
            "the str should point into the message body"
        );
    }
}
