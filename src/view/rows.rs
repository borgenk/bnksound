//! Concrete, GTK-free row data the view projection hands to a renderer.
//!
//! These are plain values computed from state and the grouping pass. No
//! widgets, so both the GTK widget path and the native renderer build from the
//! same numbers.

use crate::xdg::XdgInfo;

/// Aggregate state for a collapsed app row, materialised by
/// [`crate::view::app_group::AppRowGroup::to_info`] so the row renderer never
/// revisits state.streams.
pub struct AppRowInfo<'a> {
    /// Stable row key for the Group* messages from the picker/mute/slider.
    pub key: &'a str,
    /// Owned because the label may be MPRIS-enriched (e.g. "Spotify · Track").
    pub display_name: String,
    pub xdg: Option<&'a XdgInfo>,
    /// Cubic master volume (max of member cubics), drives slider + readout.
    pub master_cubic: f32,
    /// True iff every member is muted (or the group is empty).
    pub all_muted: bool,
    /// Some(name) only when every member pins the same sink; mixed/no-pin
    /// leaves "A" autoroute as the active button.
    pub effective_target: Option<&'a str>,
    /// True iff every member is tombstoned; drives the dimmed "idle" name.
    pub all_tombstoned: bool,
    /// Stream count; > 1 surfaces a ×N badge in the name label.
    pub member_count: usize,
    /// Whether the row is expanded; drives the toggle glyph.
    pub is_expanded: bool,
}
