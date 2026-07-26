//! A session-bus connection: the AF_UNIX socket, the SASL handshake that opens
//! it, and blocking method calls over it.
//!
//! The handshake is line based and ASCII, and runs once before any message: a
//! lone nul byte, then AUTH EXTERNAL with the effective uid hex-encoded, then
//! BEGIN. Message framing starts immediately after BEGIN.
//!
//! Calls block, so this belongs on a thread of its own. Reads carry a timeout
//! so an unresponsive peer stalls one call instead of the connection: buffered
//! bytes are kept across attempts, so a timeout mid-message resumes rather than
//! desyncs.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::time::Duration;

use crate::dbus::wire::{self, MessageType, MethodCall};
use crate::platform::sys;

/// How long a single read waits before giving up.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Signals buffered while a call waits for its reply. Past this the oldest are
/// dropped: they are only used to avoid missing an update that raced a call,
/// and the next refresh re-reads the players anyway.
const MAX_PENDING: usize = 256;

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";

/// A connection to the session bus.
pub struct Connection {
    sock: UnixStream,
    in_buf: Vec<u8>,
    serial: u32,
    /// Signals that arrived while a call was waiting for its reply.
    pending: VecDeque<wire::Message>,
}

impl Connection {
    /// Connect to the session bus, authenticate, and say Hello. Fails when the
    /// bus address is unset or unreachable, which is normal outside a desktop
    /// session.
    pub fn session() -> io::Result<Self> {
        let sock = connect_session_socket()?;
        sock.set_read_timeout(Some(READ_TIMEOUT))?;

        let mut conn = Connection {
            sock,
            in_buf: Vec::new(),
            serial: 0,
            pending: VecDeque::new(),
        };
        conn.authenticate()?;
        // Hello has to be the first message on a connection. The reply names
        // us on the bus, which nothing here needs.
        conn.call(&MethodCall {
            destination: BUS_NAME,
            path: BUS_PATH,
            interface: BUS_NAME,
            member: "Hello",
            args: &[],
        })?;
        Ok(conn)
    }

    /// Run the SASL EXTERNAL exchange, which proves our uid through the
    /// socket's own credentials rather than any secret.
    fn authenticate(&mut self) -> io::Result<()> {
        // The leading nul is part of the protocol, not the auth line.
        self.sock.write_all(&[0])?;

        let uid = sys::effective_uid().to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.sock
            .write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())?;

        let reply = self.read_line()?;
        if !reply.starts_with("OK") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("session bus rejected authentication: {reply}"),
            ));
        }

        // No fd passing is needed, so BEGIN goes out without negotiating it.
        self.sock.write_all(b"BEGIN\r\n")?;
        Ok(())
    }

    /// Read one CRLF-terminated handshake line. Bounded so a peer that never
    /// sends a terminator cannot grow the buffer without end.
    fn read_line(&mut self) -> io::Result<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while line.len() < 512 {
            let n = self.sock.read(&mut byte)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "session bus closed during authentication",
                ));
            }
            if byte[0] == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            line.push(byte[0]);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authentication line too long",
        ))
    }

    /// Serial numbers identify a reply with its call. Zero is reserved, so the
    /// counter skips it on wrap.
    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        if self.serial == 0 {
            self.serial = 1;
        }
        self.serial
    }

    /// Send a method call and wait for its reply. A D-Bus error reply comes
    /// back as an Err carrying the error name.
    pub fn call(&mut self, call: &MethodCall) -> io::Result<wire::Message> {
        let serial = self.next_serial();
        self.sock
            .write_all(&wire::encode_method_call(serial, call))?;

        loop {
            let msg = self.read_message()?;
            match msg.kind {
                MessageType::MethodReturn if msg.reply_serial == Some(serial) => return Ok(msg),
                MessageType::Error if msg.reply_serial == Some(serial) => {
                    let name = msg.error_name.unwrap_or_else(|| "unknown".to_string());
                    return Err(io::Error::other(format!("{}: {name}", call.member)));
                }
                MessageType::Signal => {
                    if self.pending.len() >= MAX_PENDING {
                        self.pending.pop_front();
                    }
                    self.pending.push_back(msg);
                }
                _ => {}
            }
        }
    }

    /// Ask the bus to deliver signals matching `rule`.
    pub fn add_match(&mut self, rule: &str) -> io::Result<()> {
        self.call(&MethodCall {
            destination: BUS_NAME,
            path: BUS_PATH,
            interface: BUS_NAME,
            member: "AddMatch",
            args: &[rule],
        })?;
        Ok(())
    }

    /// Block until the next signal arrives. Replies to calls we are no longer
    /// waiting on are discarded, and a read timeout just means nothing has
    /// happened yet.
    pub fn next_signal(&mut self) -> io::Result<wire::Message> {
        loop {
            if let Some(msg) = self.pending.pop_front() {
                return Ok(msg);
            }
            match self.read_message() {
                Ok(msg) if msg.kind == MessageType::Signal => return Ok(msg),
                Ok(_) => continue,
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Frame the next whole message, reading more bytes as needed.
    fn read_message(&mut self) -> io::Result<wire::Message> {
        loop {
            match wire::parse(&self.in_buf) {
                Some(Ok((msg, used))) => {
                    self.in_buf.drain(..used);
                    return Ok(msg);
                }
                Some(Err(e)) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
                None => {}
            }
            let mut chunk = [0u8; 4096];
            let n = self.sock.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "session bus closed",
                ));
            }
            self.in_buf.extend_from_slice(&chunk[..n]);
        }
    }
}

/// Whether an error is a read timing out rather than the connection failing.
fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Connect to the socket named by DBUS_SESSION_BUS_ADDRESS, falling back to
/// the well-known path under XDG_RUNTIME_DIR.
fn connect_session_socket() -> io::Result<UnixStream> {
    if let Ok(addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        && !addr.is_empty()
    {
        // The variable may list several addresses; the first unix socket that
        // connects wins.
        let mut last_err = None;
        for entry in addr.split(';') {
            match parse_unix_address(entry) {
                Some(UnixAddress::Path(p)) => match UnixStream::connect(&p) {
                    Ok(sock) => return Ok(sock),
                    Err(e) => last_err = Some(e),
                },
                Some(UnixAddress::Abstract(name)) => {
                    match SocketAddr::from_abstract_name(name.as_bytes())
                        .and_then(|a| UnixStream::connect_addr(&a))
                    {
                        Ok(sock) => return Ok(sock),
                        Err(e) => last_err = Some(e),
                    }
                }
                None => {}
            }
        }
        if let Some(e) = last_err {
            return Err(e);
        }
    }

    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no DBUS_SESSION_BUS_ADDRESS and no XDG_RUNTIME_DIR",
        )
    })?;
    UnixStream::connect(format!("{runtime}/bus"))
}

/// The two socket forms a session bus address can name.
#[derive(Debug, PartialEq, Eq)]
enum UnixAddress {
    Path(String),
    Abstract(String),
}

/// Pull the socket out of one address entry, e.g.
/// `unix:path=/run/user/1000/bus,guid=abc`. Non-unix transports yield None.
fn parse_unix_address(entry: &str) -> Option<UnixAddress> {
    let params = entry.trim().strip_prefix("unix:")?;
    for pair in params.split(',') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "path" => return Some(UnixAddress::Path(unescape(value))),
            "abstract" => return Some(UnixAddress::Abstract(unescape(value))),
            _ => {}
        }
    }
    None
}

/// Decode the percent escapes an address value may carry.
fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_address_parses() {
        assert_eq!(
            parse_unix_address("unix:path=/run/user/1000/bus"),
            Some(UnixAddress::Path("/run/user/1000/bus".into())),
        );
    }

    #[test]
    fn the_guid_after_the_path_is_ignored() {
        assert_eq!(
            parse_unix_address("unix:path=/run/user/1000/bus,guid=deadbeef"),
            Some(UnixAddress::Path("/run/user/1000/bus".into())),
        );
    }

    #[test]
    fn an_abstract_address_parses() {
        assert_eq!(
            parse_unix_address("unix:abstract=/tmp/dbus-AbCdEf,guid=1"),
            Some(UnixAddress::Abstract("/tmp/dbus-AbCdEf".into())),
        );
    }

    #[test]
    fn a_non_unix_transport_is_skipped() {
        assert_eq!(parse_unix_address("tcp:host=localhost,port=1234"), None);
        assert_eq!(parse_unix_address("unix:guid=only"), None);
    }

    #[test]
    fn percent_escapes_decode() {
        assert_eq!(unescape("/tmp/a%20b"), "/tmp/a b");
        assert_eq!(unescape("/run/bus"), "/run/bus");
        // A stray percent with no hex digits stays as written.
        assert_eq!(unescape("100%"), "100%");
        assert_eq!(unescape("%zz"), "%zz");
    }

    #[test]
    fn a_timeout_is_told_apart_from_a_real_failure() {
        assert!(is_timeout(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(is_timeout(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(!is_timeout(&io::Error::from(io::ErrorKind::UnexpectedEof)));
    }
}
