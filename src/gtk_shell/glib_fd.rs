//! Watch a raw fd on the GLib main loop.
//!
//! This glib version exposes no safe wrapper for g_unix_fd_add, so bind it
//! directly. libglib-2.0 (linked by gtk4) exports the symbol, so it resolves
//! without a link attribute. Used to wake the main loop and drain the bus
//! wakeup eventfds when a producer signals one, and to wake resting meters.

use std::ffi::c_void;
use std::os::fd::RawFd;

use gtk4::glib;

// glib-unix.h. GIOCondition is a guint flag set; G_IO_IN is readable.
const G_IO_IN: u32 = 1;
// GLib default source priority.
const G_PRIORITY_DEFAULT: i32 = 0;

type GUnixFDSourceFunc =
    unsafe extern "C" fn(fd: i32, condition: u32, user_data: *mut c_void) -> i32;
type GDestroyNotify = unsafe extern "C" fn(user_data: *mut c_void);

unsafe extern "C" {
    fn g_unix_fd_add_full(
        priority: i32,
        fd: i32,
        condition: u32,
        function: GUnixFDSourceFunc,
        user_data: *mut c_void,
        notify: GDestroyNotify,
    ) -> u32;
}

/// GUnixFDSourceFunc trampoline: run the boxed closure, and keep the source
/// for as long as the closure asks.
unsafe extern "C" fn trampoline<F: FnMut() -> glib::ControlFlow + 'static>(
    _fd: i32,
    _condition: u32,
    user_data: *mut c_void,
) -> i32 {
    // SAFETY: user_data is the Box<F> leaked in watch_readable, live until the
    // matching destroy runs. The main loop never calls this reentrantly.
    let f = unsafe { &mut *(user_data as *mut F) };
    match f() {
        // G_SOURCE_CONTINUE: keep watching.
        glib::ControlFlow::Continue => 1,
        // G_SOURCE_REMOVE: destroy runs once this returns.
        glib::ControlFlow::Break => 0,
    }
}

/// GDestroyNotify: reclaim the boxed closure when the source is removed.
unsafe extern "C" fn destroy<F>(user_data: *mut c_void) {
    // SAFETY: reclaims the Box<F> leaked in watch_readable exactly once.
    drop(unsafe { Box::from_raw(user_data as *mut F) });
}

/// Call `f` on the main loop whenever `fd` is readable, until it returns
/// Break. The closure runs on the main thread, so it may touch main-thread
/// state freely.
pub fn watch_readable<F: FnMut() -> glib::ControlFlow + 'static>(fd: RawFd, f: F) {
    let boxed = Box::into_raw(Box::new(f)).cast::<c_void>();
    // SAFETY: trampoline/destroy match GUnixFDSourceFunc/GDestroyNotify; boxed
    // outlives the source and is freed by destroy when the source is removed.
    unsafe {
        g_unix_fd_add_full(
            G_PRIORITY_DEFAULT,
            fd,
            G_IO_IN,
            trampoline::<F>,
            boxed,
            destroy::<F>,
        );
    }
}
