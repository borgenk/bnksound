//! Minimal D-Bus client.
//!
//! Speaks the D-Bus wire protocol directly over a unix socket, with no libdbus
//! and no toolkit main loop, so either shell can use it. The surface is only
//! what MPRIS needs: connect to the session bus, make method calls whose
//! arguments are strings, and receive signals.

pub mod connection;
pub mod wire;
