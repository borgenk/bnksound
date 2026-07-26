//! The Wayland socket connection: a raw AF_UNIX stream to the compositor, with
//! the SCM_RIGHTS plumbing to pass file descriptors (the shm pool out, the
//! keymap in). Requests are buffered and flushed; incoming bytes are framed
//! into messages by the wire module.

use std::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use core::ffi::c_int;

use crate::platform::sys::{PollFd, poll};
use crate::platform::wire::{self, Message};

/// How long a flush carrying a descriptor waits for the compositor to take any
/// bytes at all. A compositor that cannot accept a request in this long is not
/// going to; failing is better than a window that never comes up.
const SEND_FD_TIMEOUT: Duration = Duration::from_secs(1);

const SOL_SOCKET: c_int = 1;
const SCM_RIGHTS: c_int = 1;
const MSG_NOSIGNAL: c_int = 0x4000;
const MSG_DONTWAIT: c_int = 0x40;
const MSG_CMSG_CLOEXEC: c_int = 0x4000_0000;
const EAGAIN: i32 = 11;

#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}

#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}

#[repr(C)]
struct CmsgHdr {
    len: usize,
    level: c_int,
    kind: c_int,
}

unsafe extern "C" {
    fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
}

const CMSG_HDR_LEN: usize = size_of::<CmsgHdr>();

/// Align up to the size_t boundary, matching CMSG_ALIGN.
const fn cmsg_align(len: usize) -> usize {
    (len + size_of::<usize>() - 1) & !(size_of::<usize>() - 1)
}

/// A connection to the compositor.
pub struct Connection {
    sock: UnixStream,
    out: Vec<u8>,
    in_buf: Vec<u8>,
    fds: Vec<OwnedFd>,
}

impl Connection {
    /// Connect to the compositor named by WAYLAND_DISPLAY under
    /// XDG_RUNTIME_DIR (an absolute WAYLAND_DISPLAY is used as-is).
    pub fn connect() -> io::Result<Self> {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
        let path = if display.starts_with('/') {
            PathBuf::from(display)
        } else {
            let dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
                io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set")
            })?;
            PathBuf::from(dir).join(display)
        };
        let sock = UnixStream::connect(path)?;
        sock.set_nonblocking(true)?;
        Ok(Connection {
            sock,
            out: Vec::with_capacity(4096),
            in_buf: Vec::with_capacity(8192),
            fds: Vec::new(),
        })
    }

    /// The socket fd, to poll for readability.
    pub fn fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }

    /// Buffer an encoded request for the next flush.
    pub fn out(&mut self) -> &mut Vec<u8> {
        &mut self.out
    }

    /// Flush buffered requests. `pass_fd` (the shm pool fd) rides the first
    /// bytes as SCM_RIGHTS ancillary data.
    ///
    /// A descriptor travels with the bytes it is attached to, so a flush
    /// carrying one cannot leave them buffered: the request would go out on the
    /// next flush without its fd, which is a protocol error rather than a
    /// dropped frame. That case waits for room; a plain flush does not, and
    /// leaves the remainder for the next turn.
    pub fn flush(&mut self, pass_fd: Option<RawFd>) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        // Build the ancillary buffer for at most one fd.
        let mut cmsg = [0u8; 64];
        let mut controllen = pass_fd.map_or(0, |fd| write_scm_rights(&mut cmsg, fd));

        while !self.out.is_empty() {
            let mut iov = IoVec {
                base: self.out.as_ptr() as *mut c_void,
                len: self.out.len(),
            };
            let msg = MsgHdr {
                name: std::ptr::null_mut(),
                namelen: 0,
                iov: &mut iov,
                iovlen: 1,
                control: if controllen == 0 {
                    std::ptr::null_mut()
                } else {
                    cmsg.as_mut_ptr() as *mut c_void
                },
                controllen,
                flags: 0,
            };
            // SAFETY: msg points at a valid msghdr whose iov and control buffers
            // outlive the call; the fd (if any) is valid and owned by the caller.
            let n = unsafe { sendmsg(self.fd(), &msg, MSG_NOSIGNAL | MSG_DONTWAIT) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(EAGAIN) {
                    return Err(err);
                }
                if controllen == 0 {
                    // Nothing is riding on these bytes, so they can wait.
                    return Ok(());
                }
                let mut fds = [PollFd::writable(self.fd())];
                if poll(&mut fds, Some(SEND_FD_TIMEOUT))? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "compositor took no bytes, so a descriptor could not be handed over",
                    ));
                }
                continue;
            }
            self.out.drain(..n as usize);
            // The descriptor went with the first bytes to leave; whatever is
            // left of the request is plain bytes.
            controllen = 0;
        }
        Ok(())
    }

    /// Read available bytes and any passed fds into the buffers. Returns false
    /// when the peer closed the connection.
    pub fn fill(&mut self) -> io::Result<bool> {
        let mut chunk = [0u8; 8192];
        let mut cmsg = [0u8; 256];
        let mut iov = IoVec {
            base: chunk.as_mut_ptr() as *mut c_void,
            len: chunk.len(),
        };
        let mut msg = MsgHdr {
            name: std::ptr::null_mut(),
            namelen: 0,
            iov: &mut iov,
            iovlen: 1,
            control: cmsg.as_mut_ptr() as *mut c_void,
            controllen: cmsg.len(),
            flags: 0,
        };
        // SAFETY: msg points at a valid msghdr with writable iov and control
        // buffers that outlive the call.
        let n = unsafe { recvmsg(self.fd(), &mut msg, MSG_DONTWAIT | MSG_CMSG_CLOEXEC) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(EAGAIN) {
                return Ok(true);
            }
            return Err(err);
        }
        if n == 0 {
            return Ok(false);
        }
        self.in_buf.extend_from_slice(&chunk[..n as usize]);
        collect_fds(&cmsg[..msg.controllen], &mut self.fds);
        Ok(true)
    }

    /// Pop the next fully received message, if one is buffered.
    pub fn next_message(&mut self) -> Option<Message> {
        let (msg, used) = wire::parse(&self.in_buf)?;
        self.in_buf.drain(..used);
        Some(msg)
    }

    /// Take the oldest received fd (a message that declares an fd argument
    /// consumes it in arrival order).
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        if self.fds.is_empty() {
            None
        } else {
            Some(self.fds.remove(0))
        }
    }
}

/// Write a one-fd SCM_RIGHTS control message into `buf`, returning its length.
fn write_scm_rights(buf: &mut [u8], fd: RawFd) -> usize {
    let data_len = size_of::<RawFd>();
    let cmsg_len = CMSG_HDR_LEN + data_len;
    let hdr = CmsgHdr {
        len: cmsg_len,
        level: SOL_SOCKET,
        kind: SCM_RIGHTS,
    };
    // SAFETY: buf is at least cmsg_align(cmsg_len) bytes; the header and fd fit.
    unsafe {
        std::ptr::write_unaligned(buf.as_mut_ptr() as *mut CmsgHdr, hdr);
        std::ptr::write_unaligned(buf.as_mut_ptr().add(CMSG_HDR_LEN) as *mut RawFd, fd);
    }
    cmsg_align(cmsg_len)
}

/// Walk a received control buffer, taking ownership of every passed fd.
fn collect_fds(mut ctrl: &[u8], out: &mut Vec<OwnedFd>) {
    use std::os::fd::FromRawFd;
    while ctrl.len() >= CMSG_HDR_LEN {
        // SAFETY: ctrl holds at least a full cmsghdr; read it unaligned.
        let hdr = unsafe { std::ptr::read_unaligned(ctrl.as_ptr() as *const CmsgHdr) };
        if hdr.len < CMSG_HDR_LEN || hdr.len > ctrl.len() {
            break;
        }
        if hdr.level == SOL_SOCKET && hdr.kind == SCM_RIGHTS {
            let data = &ctrl[CMSG_HDR_LEN..hdr.len];
            for fd_bytes in data.chunks_exact(size_of::<RawFd>()) {
                let fd = RawFd::from_ne_bytes(fd_bytes.try_into().unwrap_or_default());
                // SAFETY: the kernel passed us an owned fd via SCM_RIGHTS.
                out.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
        let advance = cmsg_align(hdr.len);
        if advance == 0 || advance > ctrl.len() {
            break;
        }
        ctrl = &ctrl[advance..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmsg_align_rounds_to_word() {
        assert_eq!(cmsg_align(0), 0);
        assert_eq!(cmsg_align(1), 8);
        assert_eq!(cmsg_align(4), 8);
        assert_eq!(cmsg_align(8), 8);
        assert_eq!(cmsg_align(9), 16);
    }

    #[test]
    fn scm_rights_writes_a_well_formed_header() {
        let mut buf = [0u8; 64];
        let len = write_scm_rights(&mut buf, 7);
        // Header length is aligned; the fd sits right after the 16-byte header.
        assert_eq!(len, cmsg_align(CMSG_HDR_LEN + 4));
        let hdr = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const CmsgHdr) };
        assert_eq!(hdr.level, SOL_SOCKET);
        assert_eq!(hdr.kind, SCM_RIGHTS);
        assert_eq!(hdr.len, CMSG_HDR_LEN + 4);
    }
}
