//! Software frame renderer.
//!
//! A window-agnostic pixel buffer and the clipped primitives that composite the
//! mixer body into it, the text and icon drawing on top of them, and the frame
//! painter. Nothing here knows about Wayland, GTK, or application state; it
//! receives a buffer and draws.
//!
//! The machinery underneath, rasterizing a glyph, shaping a run, segmenting
//! graphemes, blending ARGB, is `crate::platform`. This module is what bnksound
//! does with it.

pub mod buffer;
pub mod desktop_font;
pub mod image;
pub mod paint;
pub mod png;
pub mod primitives;
pub mod screenshot;
pub mod text;
