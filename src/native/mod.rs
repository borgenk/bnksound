//! Native Wayland shell.
//!
//! The application's own Wayland code: the xdg-shell surface, wl_shm
//! presentation, input translation, and a small poll-based event loop that
//! drives the shared renderer. Compiled into the default `bnksound` binary.
//!
//! The protocol underneath it, the socket, wire codec, constants, buffers, and
//! keyboard, lives in `crate::platform` with no knowledge of this application.

pub mod app;
pub mod clipboard;
pub mod instance;
