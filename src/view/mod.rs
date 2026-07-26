//! GTK-free view projection.
//!
//! Turns state into the concrete row data a renderer draws: the app-stream
//! grouping and meter routing, plus the plain row value types. Both shells
//! build from these, so the projection stays out of either one.

pub mod app_group;
pub mod rows;
pub mod snapshot;
