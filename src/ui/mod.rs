//! Shared, GTK-free mixer body: theme, layout, input, editor, and meter state.
//!
//! This is the retained UI the software renderer draws and both shells drive.
//! Everything the mixer looks like and everything a click means is decided here,
//! so the native and GTK shells differ only in where their events come from.

pub mod editor;
pub mod halo;
pub mod input;
pub mod layout;
pub mod meter;
pub mod theme;

use std::time::Duration;

use crate::settings::Settings;
use crate::ui::editor::Editor;
use crate::ui::halo::HaloState;
use crate::ui::layout::{HitTarget, RowId};
use crate::ui::meter::MeterState;

/// How long the caret stays visible, then hidden, while a field has focus. Both
/// shells run their own timer against it so the blink matches.
pub const CARET_BLINK: Duration = Duration::from_millis(530);

/// Which overlay is holding keyboard focus. When one is open, typing goes to its
/// editor and shortcuts do not leak into the mixer body behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    /// The mixer body: hover shortcuts and Ctrl+K are live.
    #[default]
    Body,
    /// The command palette is open and typing filters it.
    Palette,
    /// A create/rename modal is open and typing edits its name.
    Modal,
}

/// An in-progress pointer drag. A drag is bound to what it started on and
/// continues even when the pointer leaves that rectangle, until release.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Drag {
    /// A volume slider, holding the press-time row so a stream refresh mid-drag
    /// cannot retarget it.
    Slider(RowId),
    /// Extending a selection in the focused text field.
    TextSelect,
    /// Reordering a profile, holding the dragged profile's name. Where it lands
    /// is read off the pointer at release, so there is no second copy to keep
    /// in step.
    ProfileReorder(String),
    /// Dragging the strip's scrollbar, holding where in the slider it was
    /// grabbed so the slider does not jump under the pointer.
    StripScroll { grab: i32 },
}

/// What the next frame must repaint. Meter ticks set only `meters`; anything
/// that changes geometry sets `layout` (and thus `full`).
#[derive(Clone, Copy, Default, Debug)]
pub struct Dirty {
    /// The layout must be reprojected before painting (size, scale, or content
    /// count changed).
    pub layout: bool,
    /// The whole frame must be repainted.
    pub full: bool,
    /// Only the meter rectangles changed.
    pub meters: bool,
}

impl Dirty {
    /// Request a full repaint next frame.
    pub fn mark_full(&mut self) {
        self.full = true;
    }

    /// Request a reprojection and a full repaint.
    pub fn mark_layout(&mut self) {
        self.layout = true;
        self.full = true;
    }

    /// Request a meter-only repaint.
    pub fn mark_meters(&mut self) {
        self.meters = true;
    }

    /// Whether anything needs drawing this frame.
    pub fn needs_paint(&self) -> bool {
        self.full || self.meters
    }

    /// Clear every flag after a frame is painted.
    pub fn clear(&mut self) {
        *self = Dirty::default();
    }
}

/// Multi-click tracking for the text editors. The shell stamps each press with
/// a millisecond time and position; a press near the last one within the window
/// increments the count (single, double, triple).
#[derive(Clone, Copy, Default)]
pub struct ClickTracker {
    count: u32,
    last_ms: u64,
    last_x: f64,
    last_y: f64,
}

impl ClickTracker {
    /// Presses within this many ms and pixels of the previous one chain.
    const INTERVAL_MS: u64 = 400;
    const RADIUS: f64 = 4.0;

    /// Record a press and return its chain count (1, 2, 3, ...).
    pub fn press(&mut self, ms: u64, x: f64, y: f64) -> u32 {
        let near =
            (x - self.last_x).abs() <= Self::RADIUS && (y - self.last_y).abs() <= Self::RADIUS;
        let soon = ms.saturating_sub(self.last_ms) <= Self::INTERVAL_MS;
        self.count = if near && soon { self.count + 1 } else { 1 };
        self.last_ms = ms;
        self.last_x = x;
        self.last_y = y;
        self.count
    }
}

/// Who paints the window's chrome. The mixer body is the same either way; what
/// changes is whether the surface owes the frame a titlebar, and where the
/// profile selector ends up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chrome {
    /// The compositor paints the titlebar, so the profile chip takes a strip of
    /// its own across the top.
    Server,
    /// The surface paints the titlebar, window buttons and resize edges
    /// included, and the profile chip rides in it.
    Client,
    /// The toolkit paints the titlebar and hosts the profile selector as a
    /// widget, so the surface paints neither.
    Toolkit,
}

/// Transient interaction state, owned by each shell. The persistent, domain, and
/// on-disk state lives in state::App; this holds only what interaction needs and
/// nothing that is persisted: pointer position, hover and press, drags, scroll
/// offsets, overlay focus and its editor, the meter animation, and the dirty
/// flags that decide what to repaint.
pub struct UiState {
    pub pointer: (f64, f64),
    pub hover: Option<HitTarget>,
    /// The row whose knob the pointer is actually over, which is a smaller
    /// target than the slider's own: the whole track takes a click, but only
    /// the knob wears the ring.
    pub knob_hover: Option<RowId>,
    pub pressed: Option<HitTarget>,
    pub drag: Option<Drag>,
    pub scroll_x: i32,
    pub scroll_y: i32,
    /// Scroll left over from the last event, under a whole pixel. A touchpad
    /// reports fractions of one, and dropping them would leave a slow drag
    /// moving nothing at all.
    pub scroll_residual: f32,
    /// The same for the palette's list, which counts in rows: pixels short of
    /// one wait here for the rest of the gesture.
    pub palette_wheel: f32,
    pub profile_menu_open: bool,
    pub focus: Focus,
    pub editor: Editor,
    pub click: ClickTracker,
    pub meters: MeterState,
    /// The knob rings and how far each has faded in or out.
    pub halo: HaloState,
    pub caret_visible: bool,
    /// The user's visual toggles, loaded once at startup. Layout reads them to
    /// decide which toolbar buttons exist.
    pub settings: Settings,
    /// Who paints the window's chrome, which decides where the profile selector
    /// lives and whether the surface owns its resize edges.
    pub chrome: Chrome,
    /// Whether the window is maximized, for the maximize button's glyph and the
    /// resize edges (a maximized window has none).
    pub maximized: bool,
    pub dirty: Dirty,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            pointer: (0.0, 0.0),
            hover: None,
            knob_hover: None,
            pressed: None,
            drag: None,
            scroll_x: 0,
            scroll_y: 0,
            scroll_residual: 0.0,
            palette_wheel: 0.0,
            profile_menu_open: false,
            focus: Focus::Body,
            editor: Editor::new(),
            click: ClickTracker::default(),
            meters: MeterState::new(),
            halo: HaloState::new(),
            caret_visible: true,
            settings: Settings::default(),
            chrome: Chrome::Server,
            maximized: false,
            // The first frame always paints.
            dirty: Dirty {
                layout: true,
                full: true,
                meters: false,
            },
        }
    }
}

impl UiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an overlay (palette or modal) is holding focus.
    pub fn overlay_focused(&self) -> bool {
        self.focus != Focus::Body
    }

    /// The knob wearing the hover ring: the one being dragged, or failing that
    /// the one under the pointer. A drag outranks the pointer so the ring stays
    /// on the knob being moved even once the pointer has slid off it.
    pub fn lit_knob(&self) -> Option<RowId> {
        if let Some(Drag::Slider(row)) = &self.drag {
            return Some(row.clone());
        }
        self.knob_hover.clone()
    }

    /// Advance the caret blink one step. Off-focus it settles visible, so the
    /// next field to take focus starts with a caret rather than a gap. Returns
    /// whether anything changed and the frame needs repainting.
    pub fn blink_caret(&mut self) -> bool {
        let next = if self.overlay_focused() {
            !self.caret_visible
        } else {
            true
        };
        let changed = next != self.caret_visible;
        self.caret_visible = next;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_ui_state_paints_its_first_frame() {
        let ui = UiState::new();
        assert!(ui.dirty.needs_paint());
        assert!(ui.dirty.full);
        assert_eq!(ui.focus, Focus::Body);
        assert!(!ui.overlay_focused());
    }

    #[test]
    fn dirty_marks_and_clears() {
        let mut d = Dirty::default();
        assert!(!d.needs_paint());
        d.mark_meters();
        assert!(d.needs_paint() && d.meters && !d.full);
        d.clear();
        d.mark_layout();
        assert!(d.layout && d.full);
        d.clear();
        assert!(!d.needs_paint());
    }

    #[test]
    fn clicks_chain_when_near_and_quick_and_reset_otherwise() {
        let mut c = ClickTracker::default();
        assert_eq!(c.press(1000, 10.0, 10.0), 1);
        assert_eq!(c.press(1100, 10.0, 11.0), 2); // near and soon
        assert_eq!(c.press(1150, 11.0, 10.0), 3);
        // Far away resets.
        assert_eq!(c.press(1200, 80.0, 80.0), 1);
        // Too slow resets.
        assert_eq!(c.press(5000, 80.0, 80.0), 1);
    }

    #[test]
    fn overlay_focus_reflects_the_focus_field() {
        let mut ui = UiState::new();
        ui.focus = Focus::Palette;
        assert!(ui.overlay_focused());
        ui.focus = Focus::Modal;
        assert!(ui.overlay_focused());
        ui.focus = Focus::Body;
        assert!(!ui.overlay_focused());
    }
}
