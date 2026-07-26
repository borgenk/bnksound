//! D-Bus wire-format marshalling.
//!
//! A message is a fixed 16-byte prefix (endianness, type, flags, protocol
//! version, body length, serial), then an a(yv) array of header fields, then
//! nul padding to an 8-byte boundary, then the body. Every value sits at its
//! type's natural alignment measured from the start of the message: 4 for
//! uint32 and the length prefix of strings and arrays, 8 for structs, dict
//! entries, and 64-bit numbers, 1 for bytes, signatures, and variants.
//!
//! Because the header is padded to 8, the body always starts 8-aligned, so a
//! body can be encoded and decoded on its own buffer and still land on the
//! same boundaries it would inside the whole message.

/// Largest body accepted from the bus. The spec allows far more, but nothing
/// this client asks for comes close, so a low cap bounds what a hostile or
/// broken peer can make us allocate.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Fixed-size prefix before the header field array.
const PREFIX_LEN: usize = 16;

/// Message types this client sees. Anything else is parsed and ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageType {
    MethodCall,
    MethodReturn,
    Error,
    Signal,
    Other(u8),
}

impl MessageType {
    fn from_byte(v: u8) -> Self {
        match v {
            1 => MessageType::MethodCall,
            2 => MessageType::MethodReturn,
            3 => MessageType::Error,
            4 => MessageType::Signal,
            other => MessageType::Other(other),
        }
    }
}

/// Header field codes, the byte key of each a(yv) entry.
mod field {
    pub const PATH: u8 = 1;
    pub const INTERFACE: u8 = 2;
    pub const MEMBER: u8 = 3;
    pub const ERROR_NAME: u8 = 4;
    pub const REPLY_SERIAL: u8 = 5;
    pub const DESTINATION: u8 = 6;
    pub const SENDER: u8 = 7;
    pub const SIGNATURE: u8 = 8;
}

/// A decoded D-Bus value. Covers every type the demarshaller can meet, since
/// skipping an unwanted value still means parsing it.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Path(String),
    Signature(String),
    Array(Vec<Value>),
    Struct(Vec<Value>),
    DictEntry(Box<Value>, Box<Value>),
    Variant(Box<Value>),
}

impl Value {
    /// Peel any variant wrappers. Metadata values arrive boxed, sometimes more
    /// than once, and callers only want what is inside.
    pub fn unboxed(&self) -> &Value {
        let mut v = self;
        while let Value::Variant(inner) = v {
            v = inner;
        }
        v
    }

    /// The string inside, for Str and Path alike.
    pub fn as_str(&self) -> Option<&str> {
        match self.unboxed() {
            Value::Str(s) | Value::Path(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self.unboxed() {
            Value::U32(v) => Some(*v),
            _ => None,
        }
    }

    /// The elements of an array, or None if this is not one.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self.unboxed() {
            Value::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Look up `key` in an a{sv} dict. Misses on a non-dict or absent key.
    pub fn dict_get(&self, key: &str) -> Option<&Value> {
        self.as_array()?.iter().find_map(|entry| match entry {
            Value::DictEntry(k, v) if k.as_str() == Some(key) => Some(v.as_ref()),
            _ => None,
        })
    }
}

/// Alignment of the type a signature starts with.
fn alignment(sig: u8) -> usize {
    match sig {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        // b, i, u, s, o, a, h and anything unknown.
        _ => 4,
    }
}

/// Byte length of the first complete type in `sig`, so a container can step
/// over its element type. None if the signature is truncated or unbalanced.
fn complete_type_len(sig: &[u8]) -> Option<usize> {
    match *sig.first()? {
        b'a' => Some(1 + complete_type_len(sig.get(1..)?)?),
        open @ (b'(' | b'{') => {
            let close = if open == b'(' { b')' } else { b'}' };
            let mut depth = 0usize;
            for (i, &c) in sig.iter().enumerate() {
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
            }
            None
        }
        _ => Some(1),
    }
}

/// Split a signature into its complete types, e.g. "sas" into "s", "as".
fn split_types(sig: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut rest = sig;
    while !rest.is_empty() {
        let len = complete_type_len(rest)?;
        out.push(rest.get(..len)?);
        rest = rest.get(len..)?;
    }
    Some(out)
}

/// Builds the byte stream for one message, tracking alignment from offset 0.
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Encoder { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Pad with nul bytes up to the next `n`-byte boundary.
    pub fn align(&mut self, n: usize) {
        while !self.buf.len().is_multiple_of(n) {
            self.buf.push(0);
        }
    }

    pub fn byte(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A string or object path: 4-byte length, the bytes, a nul.
    pub fn string(&mut self, s: &str) {
        self.align(4);
        self.buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// A signature: one length byte, the bytes, a nul. No alignment.
    pub fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// An array: a 4-byte byte-count, padding to the element alignment, then
    /// whatever `body` writes. The count covers only the elements, so it is
    /// patched in once their length is known.
    pub fn array(&mut self, elem_align: usize, body: impl FnOnce(&mut Encoder)) {
        self.align(4);
        let len_at = self.buf.len();
        self.buf.extend_from_slice(&0u32.to_le_bytes());
        self.align(elem_align);
        let start = self.buf.len();
        body(self);
        let len = (self.buf.len() - start) as u32;
        self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder::new()
    }
}

/// A method call to send. Every call this client makes takes string arguments,
/// so the body is a list of strings and the signature follows from its length.
pub struct MethodCall<'a> {
    pub destination: &'a str,
    pub path: &'a str,
    pub interface: &'a str,
    pub member: &'a str,
    pub args: &'a [&'a str],
}

/// Encode a method call with the given serial.
pub fn encode_method_call(serial: u32, call: &MethodCall) -> Vec<u8> {
    let mut body = Encoder::new();
    for arg in call.args {
        body.string(arg);
    }
    let body = body.into_bytes();

    let mut e = Encoder::new();
    e.byte(b'l');
    e.byte(1); // METHOD_CALL
    e.byte(0); // no flags
    e.byte(1); // protocol version
    e.u32(body.len() as u32);
    e.u32(serial);

    let signature: String = "s".repeat(call.args.len());
    e.array(8, |e| {
        header_str(e, field::PATH, "o", call.path);
        header_str(e, field::DESTINATION, "s", call.destination);
        header_str(e, field::INTERFACE, "s", call.interface);
        header_str(e, field::MEMBER, "s", call.member);
        if !signature.is_empty() {
            // The signature field carries a signature, not a string, so its
            // value uses the one-byte length form.
            e.align(8);
            e.byte(field::SIGNATURE);
            e.signature("g");
            e.signature(&signature);
        }
    });

    // The body starts on the next 8-byte boundary after the fields.
    e.align(8);
    let mut out = e.into_bytes();
    out.extend_from_slice(&body);
    out
}

/// One a(yv) header field whose value is a string or object path.
fn header_str(e: &mut Encoder, code: u8, sig: &str, value: &str) {
    e.align(8);
    e.byte(code);
    e.signature(sig);
    e.string(value);
}

/// Reads values out of a buffer, honouring alignment from the buffer's start.
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8], le: bool) -> Self {
        Decoder { buf, pos: 0, le }
    }

    /// A decoder starting partway into `buf`, so alignment still counts from
    /// the message start.
    pub fn at(buf: &'a [u8], pos: usize, le: bool) -> Self {
        Decoder { buf, pos, le }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    fn align(&mut self, n: usize) -> Option<()> {
        let pad = (n - self.pos % n) % n;
        self.pos = self.pos.checked_add(pad)?;
        (self.pos <= self.buf.len()).then_some(())
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.align(2)?;
        let b = self.take(2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&mut self) -> Option<u32> {
        self.align(4)?;
        let b = self.take(4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn u64(&mut self) -> Option<u64> {
        self.align(8)?;
        let b = self.take(8)?.try_into().ok()?;
        Some(if self.le {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    /// A 4-byte-length string, used for both STRING and OBJECT_PATH.
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        // Step over the terminating nul.
        self.take(1)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// A one-byte-length signature.
    fn signature(&mut self) -> Option<String> {
        let len = self.u8()? as usize;
        let bytes = self.take(len)?;
        self.take(1)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Read one complete value of type `sig`.
    pub fn read(&mut self, sig: &[u8]) -> Option<Value> {
        match *sig.first()? {
            b'y' => self.u8().map(Value::Byte),
            b'b' => self.u32().map(|v| Value::Bool(v != 0)),
            b'n' => self.u16().map(|v| Value::I16(v as i16)),
            b'q' => self.u16().map(Value::U16),
            b'i' => self.u32().map(|v| Value::I32(v as i32)),
            b'u' | b'h' => self.u32().map(Value::U32),
            b'x' => self.u64().map(|v| Value::I64(v as i64)),
            b't' => self.u64().map(Value::U64),
            b'd' => self.u64().map(|v| Value::F64(f64::from_bits(v))),
            b's' => self.string().map(Value::Str),
            b'o' => self.string().map(Value::Path),
            b'g' => self.signature().map(Value::Signature),
            b'v' => {
                let inner = self.signature()?;
                let value = self.read(inner.as_bytes())?;
                Some(Value::Variant(Box::new(value)))
            }
            b'a' => {
                let elem = sig.get(1..)?;
                let elem_len = complete_type_len(elem)?;
                let elem = elem.get(..elem_len)?;
                let bytes = self.u32()? as usize;
                if bytes > MAX_BODY {
                    return None;
                }
                self.align(alignment(*elem.first()?))?;
                let start = self.pos;
                let end = start.checked_add(bytes)?;
                if end > self.buf.len() {
                    return None;
                }
                let mut items = Vec::new();
                while self.pos < end {
                    items.push(self.read(elem)?);
                }
                // A malformed length can leave the cursor past the declared
                // end, which would desync every value after it.
                (self.pos == end).then_some(Value::Array(items))
            }
            b'(' => {
                let len = complete_type_len(sig)?;
                let inner = sig.get(1..len - 1)?;
                self.align(8)?;
                let mut fields = Vec::new();
                for t in split_types(inner)? {
                    fields.push(self.read(t)?);
                }
                Some(Value::Struct(fields))
            }
            b'{' => {
                let len = complete_type_len(sig)?;
                let inner = sig.get(1..len - 1)?;
                let types = split_types(inner)?;
                let key_type = types.first()?;
                let value_type = types.get(1)?;
                self.align(8)?;
                let key = self.read(key_type)?;
                let value = self.read(value_type)?;
                Some(Value::DictEntry(Box::new(key), Box::new(value)))
            }
            _ => None,
        }
    }

    /// Read every complete type in `sig`, in order.
    pub fn read_all(&mut self, sig: &[u8]) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        for t in split_types(sig)? {
            out.push(self.read(t)?);
        }
        Some(out)
    }
}

/// A parsed message: the header fields this client acts on, plus the raw body
/// and the signature needed to decode it.
pub struct Message {
    pub kind: MessageType,
    pub serial: u32,
    pub reply_serial: Option<u32>,
    pub sender: Option<String>,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub signature: String,
    pub body: Vec<u8>,
    pub le: bool,
}

impl Message {
    /// Decode the body into its values. None if it does not match its
    /// signature.
    pub fn body_values(&self) -> Option<Vec<Value>> {
        Decoder::new(&self.body, self.le).read_all(self.signature.as_bytes())
    }
}

/// Parse one message off the front of `buf`, returning it and the bytes
/// consumed. None while a whole message is not yet buffered; Some(Err) when
/// the stream is unusable and the connection should be dropped.
#[allow(clippy::type_complexity)]
pub fn parse(buf: &[u8]) -> Option<Result<(Message, usize), &'static str>> {
    if buf.len() < PREFIX_LEN {
        return None;
    }
    let le = match buf[0] {
        b'l' => true,
        b'B' => false,
        _ => return Some(Err("bad endianness flag")),
    };
    if buf[3] != 1 {
        return Some(Err("unsupported protocol version"));
    }
    let kind = MessageType::from_byte(buf[1]);

    let mut d = Decoder::at(buf, 4, le);
    let body_len = match d.u32() {
        Some(v) => v as usize,
        None => return Some(Err("truncated prefix")),
    };
    let serial = match d.u32() {
        Some(v) => v,
        None => return Some(Err("truncated prefix")),
    };
    if body_len > MAX_BODY {
        return Some(Err("body too large"));
    }

    // The field array length tells us how far the header runs before we can
    // decode it, so peek it before parsing the array itself.
    let fields_len = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    let fields_len = if le {
        fields_len as usize
    } else {
        u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize
    };
    let header_end = PREFIX_LEN.checked_add(fields_len)?;
    if header_end > MAX_BODY {
        return Some(Err("header too large"));
    }
    let body_start = header_end.div_ceil(8) * 8;
    let total = body_start.checked_add(body_len)?;
    if buf.len() < total {
        return None;
    }

    let fields = match Decoder::at(buf, 12, le).read(b"a(yv)") {
        Some(v) => v,
        None => return Some(Err("malformed header fields")),
    };

    let mut msg = Message {
        kind,
        serial,
        reply_serial: None,
        sender: None,
        path: None,
        interface: None,
        member: None,
        error_name: None,
        signature: String::new(),
        body: buf.get(body_start..total)?.to_vec(),
        le,
    };
    for entry in fields.as_array().unwrap_or(&[]) {
        let Value::Struct(pair) = entry else { continue };
        let (Some(Value::Byte(code)), Some(value)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        match *code {
            field::PATH => msg.path = value.as_str().map(str::to_string),
            field::INTERFACE => msg.interface = value.as_str().map(str::to_string),
            field::MEMBER => msg.member = value.as_str().map(str::to_string),
            field::ERROR_NAME => msg.error_name = value.as_str().map(str::to_string),
            field::REPLY_SERIAL => msg.reply_serial = value.as_u32(),
            field::DESTINATION => {}
            field::SENDER => msg.sender = value.as_str().map(str::to_string),
            field::SIGNATURE => {
                if let Value::Signature(s) = value.unboxed() {
                    msg.signature = s.clone();
                }
            }
            _ => {}
        }
    }
    Some(Ok((msg, total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a body of strings the way a method call would.
    fn body_of(args: &[&str]) -> Vec<u8> {
        let mut e = Encoder::new();
        for a in args {
            e.string(a);
        }
        e.into_bytes()
    }

    #[test]
    fn a_string_is_length_bytes_and_nul() {
        let bytes = body_of(&["foo"]);
        assert_eq!(bytes, vec![3, 0, 0, 0, b'f', b'o', b'o', 0]);
    }

    #[test]
    fn strings_realign_to_four_between_values() {
        // "ab" occupies 4 + 2 + 1 = 7 bytes, so the next length prefix pads
        // one byte to reach offset 8.
        let bytes = body_of(&["ab", "c"]);
        assert_eq!(&bytes[..7], &[2, 0, 0, 0, b'a', b'b', 0]);
        assert_eq!(bytes[7], 0, "pad byte before the next string");
        assert_eq!(&bytes[8..], &[1, 0, 0, 0, b'c', 0]);
    }

    #[test]
    fn a_signature_uses_a_single_length_byte() {
        let mut e = Encoder::new();
        e.signature("sas");
        assert_eq!(e.into_bytes(), vec![3, b's', b'a', b's', 0]);
    }

    #[test]
    fn an_array_length_counts_only_its_elements() {
        let mut e = Encoder::new();
        e.array(4, |e| {
            e.u32(5);
            e.u32(10);
        });
        let bytes = e.into_bytes();
        assert_eq!(&bytes[..4], &8u32.to_le_bytes(), "two uint32 elements");
        assert_eq!(bytes.len(), 12);
    }

    #[test]
    fn an_array_pads_to_its_element_alignment_outside_the_count() {
        // A struct element aligns to 8, so a 4-byte count is followed by 4
        // pad bytes that the count must not include.
        let mut e = Encoder::new();
        e.array(8, |e| e.u32(1));
        let bytes = e.into_bytes();
        assert_eq!(&bytes[..4], &4u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &[0, 0, 0, 0], "padding to the 8 boundary");
        assert_eq!(bytes.len(), 12);
    }

    #[test]
    fn complete_type_len_walks_nested_containers() {
        assert_eq!(complete_type_len(b"s"), Some(1));
        assert_eq!(complete_type_len(b"as"), Some(2));
        assert_eq!(complete_type_len(b"a{sv}"), Some(5));
        assert_eq!(complete_type_len(b"(sa{sv})x"), Some(8));
        assert_eq!(complete_type_len(b"aai"), Some(3));
        assert_eq!(complete_type_len(b"("), None, "unbalanced");
        assert_eq!(complete_type_len(b""), None);
    }

    #[test]
    fn split_types_separates_each_argument() {
        let parts = split_types(b"sas").expect("valid signature");
        assert_eq!(parts, vec![&b"s"[..], &b"as"[..]]);
        let parts = split_types(b"ssa{sv}").expect("valid signature");
        assert_eq!(parts, vec![&b"s"[..], &b"s"[..], &b"a{sv}"[..]]);
    }

    #[test]
    fn a_method_call_round_trips_through_parse() {
        let call = MethodCall {
            destination: "org.freedesktop.DBus",
            path: "/org/freedesktop/DBus",
            interface: "org.freedesktop.DBus",
            member: "GetConnectionUnixProcessID",
            args: &["org.mpris.MediaPlayer2.vlc"],
        };
        let bytes = encode_method_call(7, &call);

        let (msg, used) = parse(&bytes).expect("complete").expect("well formed");
        assert_eq!(used, bytes.len());
        assert_eq!(msg.kind, MessageType::MethodCall);
        assert_eq!(msg.serial, 7);
        assert_eq!(msg.path.as_deref(), Some("/org/freedesktop/DBus"));
        assert_eq!(msg.interface.as_deref(), Some("org.freedesktop.DBus"));
        assert_eq!(msg.member.as_deref(), Some("GetConnectionUnixProcessID"));
        assert_eq!(msg.signature, "s");
        let values = msg.body_values().expect("body matches signature");
        assert_eq!(values[0].as_str(), Some("org.mpris.MediaPlayer2.vlc"));
    }

    #[test]
    fn a_call_with_no_arguments_omits_the_signature_field() {
        let call = MethodCall {
            destination: "org.freedesktop.DBus",
            path: "/org/freedesktop/DBus",
            interface: "org.freedesktop.DBus",
            member: "ListNames",
            args: &[],
        };
        let bytes = encode_method_call(1, &call);
        let (msg, _) = parse(&bytes).expect("complete").expect("well formed");
        assert_eq!(msg.signature, "");
        assert!(msg.body.is_empty());
        assert_eq!(msg.body_values().expect("empty body"), vec![]);
    }

    #[test]
    fn the_header_is_padded_so_the_body_starts_eight_aligned() {
        // A member name of an awkward length must not shift the body off its
        // boundary.
        for member in ["A", "AB", "ABC", "ABCD", "ABCDE"] {
            let call = MethodCall {
                destination: "a.b",
                path: "/a",
                interface: "a.b",
                member,
                args: &["x"],
            };
            let bytes = encode_method_call(1, &call);
            let (msg, used) = parse(&bytes).expect("complete").expect("well formed");
            assert_eq!(used, bytes.len(), "member {member}");
            assert_eq!(msg.body_values().expect("body")[0].as_str(), Some("x"));
        }
    }

    #[test]
    fn parse_waits_for_a_whole_message() {
        let call = MethodCall {
            destination: "org.freedesktop.DBus",
            path: "/org/freedesktop/DBus",
            interface: "org.freedesktop.DBus",
            member: "Hello",
            args: &[],
        };
        let bytes = encode_method_call(1, &call);
        assert!(parse(&bytes[..8]).is_none(), "less than the prefix");
        assert!(parse(&bytes[..PREFIX_LEN]).is_none(), "prefix only");
        assert!(parse(&bytes[..bytes.len() - 1]).is_none(), "one byte short");
        assert!(parse(&bytes).is_some());
    }

    #[test]
    fn parse_rejects_a_stream_it_cannot_frame() {
        let mut bytes = encode_method_call(
            1,
            &MethodCall {
                destination: "a.b",
                path: "/a",
                interface: "a.b",
                member: "M",
                args: &[],
            },
        );
        bytes[0] = b'x';
        assert!(matches!(parse(&bytes), Some(Err(_))), "bad endianness flag");

        let mut bytes = encode_method_call(
            1,
            &MethodCall {
                destination: "a.b",
                path: "/a",
                interface: "a.b",
                member: "M",
                args: &[],
            },
        );
        bytes[3] = 9;
        assert!(matches!(parse(&bytes), Some(Err(_))), "bad version");
    }

    #[test]
    fn two_messages_parse_one_at_a_time() {
        let call = MethodCall {
            destination: "a.b",
            path: "/a",
            interface: "a.b",
            member: "M",
            args: &["one"],
        };
        let mut stream = encode_method_call(1, &call);
        let first_len = stream.len();
        stream.extend_from_slice(&encode_method_call(2, &call));

        let (first, used) = parse(&stream).expect("complete").expect("well formed");
        assert_eq!(used, first_len);
        assert_eq!(first.serial, 1);
        let (second, used2) = parse(&stream[used..]).expect("complete").expect("ok");
        assert_eq!(second.serial, 2);
        assert_eq!(used + used2, stream.len());
    }

    /// Build the body of a Properties.Get reply: a variant holding an a{sv}
    /// metadata dict with a string title and a string-array artist.
    fn metadata_reply_body() -> Vec<u8> {
        let mut e = Encoder::new();
        // The reply signature is "v", so the body opens with the variant's
        // own signature.
        e.signature("a{sv}");
        e.array(8, |e| {
            e.align(8);
            e.string("xesam:title");
            e.signature("s");
            e.string("Song");

            e.align(8);
            e.string("xesam:artist");
            e.signature("as");
            e.array(4, |e| {
                e.string("Band");
                e.string("Other");
            });

            // A key the parser does not want, of a type it must still skip.
            e.align(8);
            e.string("mpris:length");
            e.signature("x");
            e.align(8);
            e.byte(1);
            for _ in 0..7 {
                e.byte(0);
            }
        });
        e.into_bytes()
    }

    #[test]
    fn a_metadata_dict_decodes_title_and_artist() {
        let body = metadata_reply_body();
        let mut d = Decoder::new(&body, true);
        let value = d.read(b"v").expect("a variant");

        let title = value.dict_get("xesam:title").expect("title present");
        assert_eq!(title.as_str(), Some("Song"));

        let artist = value.dict_get("xesam:artist").expect("artist present");
        let names = artist.as_array().expect("artist is an array");
        assert_eq!(names[0].as_str(), Some("Band"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn an_unwanted_value_type_is_stepped_over_not_dropped() {
        // The int64 entry sits between nothing and the end, so decoding it
        // wrongly would truncate the dict rather than fail loudly.
        let body = metadata_reply_body();
        let value = Decoder::new(&body, true).read(b"v").expect("a variant");
        let entries = value.as_array().expect("a dict");
        assert_eq!(entries.len(), 3, "every entry survives the skip");
        assert!(value.dict_get("mpris:length").is_some());
    }

    #[test]
    fn dict_get_misses_cleanly_on_an_absent_key() {
        let body = metadata_reply_body();
        let value = Decoder::new(&body, true).read(b"v").expect("a variant");
        assert!(value.dict_get("xesam:album").is_none());
        assert!(Value::U32(1).dict_get("anything").is_none());
    }

    #[test]
    fn unboxed_peels_every_variant_layer() {
        let doubly = Value::Variant(Box::new(Value::Variant(Box::new(Value::Str("x".into())))));
        assert_eq!(doubly.unboxed(), &Value::Str("x".into()));
        assert_eq!(doubly.as_str(), Some("x"));
    }

    #[test]
    fn a_string_array_body_decodes() {
        // What ListNames returns: a single "as" argument.
        let mut e = Encoder::new();
        e.array(4, |e| {
            e.string("org.freedesktop.DBus");
            e.string("org.mpris.MediaPlayer2.vlc");
        });
        let body = e.into_bytes();
        let values = Decoder::new(&body, true).read_all(b"as").expect("decodes");
        let names = values[0].as_array().expect("an array");
        assert_eq!(names.len(), 2);
        assert_eq!(names[1].as_str(), Some("org.mpris.MediaPlayer2.vlc"));
    }

    #[test]
    fn a_truncated_body_fails_instead_of_panicking() {
        let body = metadata_reply_body();
        for cut in 1..body.len() {
            // Any prefix must either decode or return None, never panic.
            let _ = Decoder::new(&body[..cut], true).read(b"v");
        }
    }

    #[test]
    fn an_array_longer_than_its_buffer_is_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&64u32.to_le_bytes()); // claims 64 bytes
        body.extend_from_slice(&[0u8; 8]);
        assert!(Decoder::new(&body, true).read(b"au").is_none());
    }

    #[test]
    fn big_endian_values_decode_with_the_flag_set() {
        let mut body = Vec::new();
        body.extend_from_slice(&7u32.to_be_bytes());
        let values = Decoder::new(&body, false).read_all(b"u").expect("decodes");
        assert_eq!(values[0].as_u32(), Some(7));
    }
}
