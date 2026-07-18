//! Shared memory for wl_shm presentation.
//!
//! A memfd is created, sized, and mapped once; the renderer draws straight into
//! the mapping and the compositor reads the same pages, so a frame costs no
//! copy. The fd is passed to wl_shm.create_pool as SCM_RIGHTS ancillary data.

use std::ffi::{CString, c_void};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use core::ffi::{c_char, c_int, c_uint};

const MFD_CLOEXEC: c_uint = 0x0001;
const PROT_READ: c_int = 0x1;
const PROT_WRITE: c_int = 0x2;
const MAP_SHARED: c_int = 0x01;

unsafe extern "C" {
    fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    fn ftruncate(fd: c_int, length: i64) -> c_int;
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

/// A mapped memfd the renderer paints into and the compositor samples from.
pub struct ShmPool {
    fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
}

impl ShmPool {
    /// Create and map a pool of `len` bytes.
    pub fn new(len: usize) -> io::Result<Self> {
        let len = len.max(4);
        let name = CString::new("bnksound").map_err(io::Error::other)?;
        // SAFETY: name is a valid NUL-terminated string for the call's duration.
        let raw = unsafe { memfd_create(name.as_ptr(), MFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a fresh owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // SAFETY: fd is a live memfd; sizing it is what makes the mapping valid.
        if unsafe { ftruncate(fd.as_raw_fd(), len as i64) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a shared read/write mapping of the whole sized memfd.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(ShmPool {
            fd,
            ptr: ptr.cast::<u8>(),
            len,
        })
    }

    /// The pool fd, to hand to wl_shm.create_pool.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Byte length of the mapping.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mapping as 32-bit pixels, for the painter to draw into.
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: the mapping is len bytes of writable shared memory, 4-byte
        // aligned by mmap, and lives as long as self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u32>(), self.len / 4) }
    }
}

impl Drop for ShmPool {
    fn drop(&mut self) {
        // SAFETY: we own this mapping and unmap it exactly once; the fd closes
        // with the OwnedFd afterwards.
        unsafe { munmap(self.ptr.cast::<c_void>(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_maps_and_is_writable() {
        let mut pool = ShmPool::new(64 * 4).expect("pool");
        assert_eq!(pool.len(), 64 * 4);
        assert!(!pool.is_empty());
        assert!(pool.fd() >= 0);
        let px = pool.pixels();
        assert_eq!(px.len(), 64);
        px[0] = 0xff00_00ff;
        px[63] = 0xff11_2233;
        assert_eq!(pool.pixels()[0], 0xff00_00ff);
        assert_eq!(pool.pixels()[63], 0xff11_2233);
    }

    #[test]
    fn a_tiny_request_still_maps() {
        let pool = ShmPool::new(0).expect("pool");
        assert!(pool.len() >= 4);
    }
}
