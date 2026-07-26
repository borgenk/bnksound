//! Clipboard transfer plumbing.
//!
//! Wayland moves selection data over a pipe: to paste, the client hands the
//! write end to the source and reads the read end; to copy, the compositor
//! hands over a write end when another client pastes. Only the fd mechanics
//! live here; the protocol wiring is in the app.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};
use std::{fs, io};

use core::ffi::c_int;

use crate::platform::sys::{PollFd, poll};

/// The MIME type the mixer exchanges. Plain UTF-8 text is all its fields hold.
pub const MIME_UTF8: &str = "text/plain;charset=utf-8";

/// Largest selection accepted, so a hostile or huge clipboard cannot exhaust
/// memory on a paste.
const MAX_PASTE: usize = 64 * 1024;

const O_CLOEXEC: c_int = 0o2000000;

unsafe extern "C" {
    fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
}

/// A close-on-exec pipe as (read, write).
pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: fds is two writable ints, which is what pipe2 fills.
    if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both are fresh owned descriptors returned by pipe2.
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

/// Read a selection until the writer closes, bounded by `timeout` and
/// [`MAX_PASTE`].
///
/// The writer is another application, so it may be slow or never finish; the
/// deadline keeps a paste from stalling the event loop forever.
pub fn read_selection(fd: OwnedFd, timeout: Duration) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut file = fs::File::from(fd);
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = [PollFd::readable(file.as_raw_fd())];
        if poll(&mut fds, Some(left))? == 0 {
            break; // timed out waiting for the writer
        }
        match file.read(&mut chunk) {
            Ok(0) => break, // writer closed: transfer complete
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() >= MAX_PASTE {
                    out.truncate(MAX_PASTE);
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Hand our selection to a requester. Errors are ignored beyond stopping: a
/// paste target that goes away mid-write is not our problem to report.
pub fn write_selection(fd: OwnedFd, text: &str) {
    let mut file = fs::File::from(fd);
    let _ = file.write_all(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_selection_reads_back() {
        let (rd, wr) = pipe().expect("pipe");
        write_selection(wr, "profile name");
        let got = read_selection(rd, Duration::from_secs(1)).expect("read");
        assert_eq!(got, "profile name");
    }

    #[test]
    fn a_writer_that_never_writes_times_out_instead_of_hanging() {
        let (rd, wr) = pipe().expect("pipe");
        // Hold the write end open and silent; the read must give up.
        let started = Instant::now();
        let got = read_selection(rd, Duration::from_millis(80)).expect("read");
        assert!(got.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2), "bounded wait");
        drop(wr);
    }

    #[test]
    fn an_oversized_selection_is_capped() {
        let (rd, wr) = pipe().expect("pipe");
        let huge = "x".repeat(MAX_PASTE * 2);
        std::thread::spawn(move || write_selection(wr, &huge));
        let got = read_selection(rd, Duration::from_secs(2)).expect("read");
        assert_eq!(got.len(), MAX_PASTE);
    }
}
