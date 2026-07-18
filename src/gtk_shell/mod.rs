//! GTK shell: the GtkApplication lifecycle and the surface the shared renderer
//! paints on.
//!
//! Compiled only with the gtk feature and consumed only by the bnksound-gtk
//! binary. All GTK, GDK, GLib, GIO, and Cairo usage lives under this module
//! root; shared modules never import it.

pub mod app;
pub(crate) mod glib_fd;
pub(crate) mod header;
pub(crate) mod profile;
pub(crate) mod style;
pub(crate) mod surface;
