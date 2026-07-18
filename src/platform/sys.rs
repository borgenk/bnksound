//! Small Linux syscall surface shared by both binaries.
//!
//! std already links libc, so these symbols resolve without a link attribute;
//! only the handful the event loop needs are declared. Two jobs live here: an
//! eventfd used purely as a pollable wakeup for the message bus, and a poll
//! wrapper the native loop waits on over the Wayland socket and that same
//! wakeup fd.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use core::ffi::{c_int, c_short, c_uint, c_void};

// eventfd2(2) flags.
const EFD_CLOEXEC: c_int = 0o2000000;
const EFD_NONBLOCK: c_int = 0o4000;

// poll(2) event bits.
pub const POLLIN: c_short = 0x001;
pub const POLLOUT: c_short = 0x004;
pub const POLLERR: c_short = 0x008;
pub const POLLHUP: c_short = 0x010;

const EINTR: c_int = 4;

unsafe extern "C" {
    fn eventfd(initval: c_uint, flags: c_int) -> c_int;
    fn geteuid() -> u32;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn ppoll(
        fds: *mut PollFd,
        nfds: c_uint,
        timeout: *const KernelTimespec,
        sigmask: *const c_void,
    ) -> c_int;
}

/// struct pollfd (poll.h). The ABI layout is pinned in the tests.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

impl PollFd {
    /// Watch `fd` for readability.
    pub fn readable(fd: RawFd) -> Self {
        PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }
    }

    /// Watch `fd` for room to write.
    pub fn writable(fd: RawFd) -> Self {
        PollFd {
            fd,
            events: POLLOUT,
            revents: 0,
        }
    }

    /// Whether the last poll reported this fd readable, errored, or hung up.
    pub fn is_ready(&self) -> bool {
        self.revents & (POLLIN | POLLERR | POLLHUP) != 0
    }
}

/// struct timespec (time.h), the ppoll timeout. Layout pinned in the tests.
#[repr(C)]
struct KernelTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// The caller's effective user id, which D-Bus authentication sends to prove
/// who is connecting.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid reads the calling process's own credentials and cannot
    // fail.
    unsafe { geteuid() }
}

/// A close-on-exec, nonblocking counter eventfd used purely as a wakeup.
pub fn make_eventfd() -> io::Result<OwnedFd> {
    // SAFETY: eventfd with valid flags returns a fresh owned fd or -1.
    let fd = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh, owned descriptor returned by eventfd.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Add 1 to the eventfd's counter to wake a poller. Best-effort: a lost wake is
/// covered by the poll timeout, so a failed write is never fatal.
pub fn eventfd_signal(fd: RawFd) {
    let one: u64 = 1;
    // SAFETY: writing eight bytes of a u64 is the eventfd write contract;
    // counter overflow at u64::MAX is unreachable at these rates.
    unsafe { write(fd, (&one as *const u64).cast(), 8) };
}

/// Drain the eventfd's counter to zero. Nonblocking, so an already-clear fd
/// returns EAGAIN, which is fine; retried only on EINTR.
pub fn eventfd_clear(fd: RawFd) {
    let mut v: u64 = 0;
    loop {
        // SAFETY: reading eight bytes into a u64 is the eventfd read contract.
        let n = unsafe { read(fd, (&mut v as *mut u64).cast(), 8) };
        if n < 0 && io::Error::last_os_error().raw_os_error() == Some(EINTR) {
            continue;
        }
        return;
    }
}

/// Wait until one of `fds` is ready or `timeout` elapses. `None` blocks until a
/// fd is ready. Retries on EINTR. Returns the number of ready fds; inspect each
/// PollFd's revents (or is_ready) for which.
pub fn poll(fds: &mut [PollFd], timeout: Option<Duration>) -> io::Result<usize> {
    loop {
        let ts = timeout.map(|d| KernelTimespec {
            tv_sec: d.as_secs() as i64,
            tv_nsec: d.subsec_nanos() as i64,
        });
        let ts_ptr = ts
            .as_ref()
            .map_or(std::ptr::null(), |t| t as *const KernelTimespec);
        // SAFETY: fds points at nfds valid, writable PollFd; ts_ptr is null or a
        // valid timespec; a null sigmask leaves the signal mask unchanged.
        let n = unsafe {
            ppoll(
                fds.as_mut_ptr(),
                fds.len() as c_uint,
                ts_ptr,
                std::ptr::null(),
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(EINTR) {
                continue;
            }
            return Err(err);
        }
        return Ok(n as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn pollfd_matches_c_abi() {
        assert_eq!(std::mem::size_of::<PollFd>(), 8);
        assert_eq!(std::mem::align_of::<PollFd>(), 4);
    }

    #[test]
    fn timespec_matches_c_abi() {
        assert_eq!(std::mem::size_of::<KernelTimespec>(), 16);
    }

    #[test]
    fn signal_makes_eventfd_readable_and_clear_resets_it() {
        let efd = make_eventfd().expect("eventfd");
        let raw = efd.as_raw_fd();

        // Nothing written yet: a zero-timeout poll reports not ready.
        let mut fds = [PollFd::readable(raw)];
        assert_eq!(poll(&mut fds, Some(Duration::ZERO)).unwrap(), 0);
        assert!(!fds[0].is_ready());

        // After a signal the fd is readable.
        eventfd_signal(raw);
        let mut fds = [PollFd::readable(raw)];
        assert_eq!(poll(&mut fds, Some(Duration::ZERO)).unwrap(), 1);
        assert!(fds[0].is_ready());

        // Clearing drains the counter back to not-ready.
        eventfd_clear(raw);
        let mut fds = [PollFd::readable(raw)];
        assert_eq!(poll(&mut fds, Some(Duration::ZERO)).unwrap(), 0);
    }

    #[test]
    fn many_signals_are_cleared_by_one_clear() {
        let efd = make_eventfd().expect("eventfd");
        let raw = efd.as_raw_fd();
        for _ in 0..100 {
            eventfd_signal(raw);
        }
        eventfd_clear(raw);
        let mut fds = [PollFd::readable(raw)];
        assert_eq!(poll(&mut fds, Some(Duration::ZERO)).unwrap(), 0);
    }
}
