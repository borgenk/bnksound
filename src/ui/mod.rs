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
use crate::ui::halo::{HaloState, PressMark};
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

/// What one press of the fit button comes to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FitStep {
    /// Width to ask the window for.
    pub width: i32,
    /// What [`UiState::fit_restore`] becomes: the width being left behind, or
    /// None once the width the user had is back.
    pub restore: Option<i32>,
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
    /// The mark a press on the fit button leaves, which outlives the hover that
    /// the resize can carry off.
    pub fit_mark: PressMark,
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
    /// Whether the compositor has the window in a tiled layout. Together with
    /// `maximized` this is the width being the compositor's rather than ours,
    /// which is what the fit button reads to know it has nothing to offer.
    pub tiled: bool,
    /// The width to go back to when a fit is undone, and None when there is no
    /// fit to undo. Fitting only ever moves the side edges, so the height is
    /// not part of what there is to restore.
    pub fit_restore: Option<i32>,
    /// Widest the window may ask to be, from whatever the platform will say
    /// about its usable area. Unbounded until something says otherwise.
    pub max_width: i32,
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
            fit_mark: PressMark::default(),
            caret_visible: true,
            settings: Settings::default(),
            chrome: Chrome::Server,
            maximized: false,
            tiled: false,
            fit_restore: None,
            max_width: i32::MAX,
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

    /// Whether the window's size is the compositor's arrangement rather than
    /// something the window can ask for. Fitting is inert in either state.
    pub fn compositor_sized(&self) -> bool {
        self.maximized || self.tiled
    }

    /// The width `columns` columns want, within the window's own floor and
    /// whatever ceiling the platform gives.
    ///
    /// The floor outranks the ceiling: a screen too narrow for one column is a
    /// window that overflows its bound, not one that cuts the column down.
    pub fn fit_width(&self, columns: usize) -> i32 {
        let show_sidebar = self.settings.show_sidebar;
        let (min_w, _) = layout::minimum_size(show_sidebar);
        layout::natural_width(columns, show_sidebar)
            .min(self.max_width)
            .max(min_w)
    }

    /// Whether the window stands at the width its columns want right now.
    ///
    /// Worked out by comparing rather than remembered, which is what keeps it
    /// honest across a restart and across the columns changing underneath it. A
    /// stream leaving makes a fitted window un-fitted the moment it goes, and
    /// the next press fits it to what is actually there.
    pub fn is_fitted(&self, width: i32, columns: usize) -> bool {
        width == self.fit_width(columns)
    }

    /// What a press of the fit button comes to, or None when there is nothing
    /// for it to do: the compositor owns the width, or the window is already
    /// fitted and there is no earlier width left to go back to.
    ///
    /// This is the one place the answer is worked out. The button draws itself
    /// from it too, so an inert button and a press that does nothing cannot
    /// come apart.
    pub fn fit_step(&self, width: i32, columns: usize) -> Option<FitStep> {
        if self.compositor_sized() {
            return None;
        }
        if self.is_fitted(width, columns) {
            // Fitted, so a press gives the earlier width back. A window that
            // opened fitted has no earlier width, and the press has nothing.
            return self
                .fit_restore
                .filter(|restore| *restore != width)
                .map(|restore| FitStep {
                    width: restore,
                    restore: None,
                });
        }
        Some(FitStep {
            width: self.fit_width(columns),
            restore: Some(width),
        })
    }

    /// The same step, with the press marked so the button acknowledges it.
    ///
    /// Fitting widens the window and the button hangs off the edge that moves,
    /// so a press can carry the button out from under the pointer and take the
    /// hover with it. A press that comes to nothing leaves no mark: there is
    /// nothing to acknowledge.
    pub fn fit_press(&mut self, width: i32, columns: usize) -> Option<FitStep> {
        let step = self.fit_step(width, columns);
        if step.is_some() {
            self.fit_mark.strike();
        }
        step
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

    /// One press fits, the next puts the old width back.
    #[test]
    fn fitting_and_unfitting_come_back_to_where_they_started() {
        let mut ui = UiState::new();
        let wide = 900;
        let step = ui.fit_step(wide, 3).expect("a fit to make");
        assert_eq!(step.width, layout::natural_width(3, true));
        assert!(step.width < wide, "three columns are narrower than 900px");
        ui.fit_restore = step.restore;
        assert!(ui.is_fitted(step.width, 3));

        let back = ui.fit_step(step.width, 3).expect("a width to restore");
        assert_eq!(back.width, wide);
        assert_eq!(back.restore, None, "the restore leaves nothing to undo");
        ui.fit_restore = back.restore;
        assert!(!ui.is_fitted(wide, 3));
    }

    /// Being fitted is worked out by comparing, not remembered, so a column
    /// leaving un-fits the window the moment it goes. Without that the button
    /// would still claim the window was fitted while it stood a column too wide.
    #[test]
    fn a_column_leaving_un_fits_the_window_on_its_own() {
        let mut ui = UiState::new();
        let step = ui.fit_step(900, 4).expect("a fit");
        ui.fit_restore = step.restore;
        assert!(ui.is_fitted(step.width, 4));

        // A stream stops and its column goes with it. Nothing resized, but the
        // window is a column too wide for what is left.
        assert!(!ui.is_fitted(step.width, 3));
        let again = ui.fit_step(step.width, 3).expect("a fit to the three left");
        assert_eq!(again.width, layout::natural_width(3, true));
        assert_eq!(
            again.restore,
            Some(step.width),
            "the four-column width is what the next press gives back"
        );
    }

    /// A window that opens already fitted has no earlier width to go back to,
    /// so the press has nothing to do and the button has to say so rather than
    /// look pressable and sit there.
    #[test]
    fn a_window_opening_at_its_fit_width_has_an_inert_button() {
        let ui = UiState::new();
        let fit = layout::natural_width(4, true);
        assert!(ui.is_fitted(fit, 4));
        assert_eq!(ui.fit_step(fit, 4), None, "nothing for a press to do");

        // One manual resize is all it takes for the button to have work again.
        assert!(ui.fit_step(fit + 200, 4).is_some());
    }

    /// A width that is neither the fitted one nor the one it replaced is the
    /// user's own, so the next press fits afresh rather than yanking the window
    /// back to a size from before they touched it.
    #[test]
    fn a_resize_between_presses_is_a_new_width_to_come_back_to() {
        let mut ui = UiState::new();
        let step = ui.fit_step(900, 3).expect("a fit to make");
        ui.fit_restore = step.restore;

        // The user drags the edge in to 700.
        assert!(!ui.is_fitted(700, 3));
        let again = ui.fit_step(700, 3).expect("a fresh fit");
        assert_eq!(
            again.width, step.width,
            "the same columns want the same width"
        );
        assert_eq!(
            again.restore,
            Some(700),
            "the width to come back to is the one the user chose, not 900"
        );
    }

    /// A press that resizes the window marks itself, because the resize can
    /// carry the button out from under the pointer and the hover with it. A
    /// press that comes to nothing has nothing to acknowledge.
    #[test]
    fn only_a_press_that_does_something_leaves_a_mark() {
        let mut ui = UiState::new();
        assert!(ui.fit_press(900, 3).is_some(), "an unfitted window fits");
        assert_eq!(ui.fit_mark.strength(), 1.0, "and the press shows");

        // Fitted, with no width recorded to go back to: the press is inert.
        let mut ui = UiState::new();
        let fitted = ui.fit_width(3);
        assert_eq!(ui.fit_press(fitted, 3), None);
        assert_eq!(
            ui.fit_mark.strength(),
            0.0,
            "a press that does nothing says nothing",
        );

        // The compositor owns the width, so there is nothing to give either.
        let mut ui = UiState::new();
        ui.maximized = true;
        assert_eq!(ui.fit_press(900, 3), None);
        assert_eq!(ui.fit_mark.strength(), 0.0);
    }

    /// Maximized, fullscreen, or tiled, the width belongs to the compositor and
    /// the press has nothing to offer.
    #[test]
    fn a_compositor_sized_window_has_no_fit_to_make() {
        let mut ui = UiState::new();
        ui.maximized = true;
        assert_eq!(ui.fit_step(900, 3), None);
        ui.maximized = false;
        ui.tiled = true;
        assert_eq!(ui.fit_step(900, 3), None);
        ui.tiled = false;
        assert!(ui.fit_step(900, 3).is_some());
    }

    /// Expanding a group is the user asking for the extra columns, so a fitted
    /// window makes room the way one grows when a disclosure is twisted open.
    /// What it goes back to is still the user's own width, not the narrower
    /// fitted width the expand moved off.
    #[test]
    fn a_fitted_window_follows_a_deliberate_change_in_columns() {
        let mut ui = UiState::new();
        let chosen = 900;

        // Fitted to three columns, with 900 as the width to come back to.
        let fit = ui.fit_step(chosen, 3).expect("a fit");
        ui.fit_restore = fit.restore;
        let mut width = fit.width;
        assert!(ui.is_fitted(width, 3));

        // The group expands into two more. What the shells do next: the count
        // moved, so the window is no longer fitted and follows to the new one.
        assert!(!ui.is_fitted(width, 5));
        let restore = ui.fit_restore;
        let step = ui.fit_step(width, 5).expect("a fit to five");
        width = step.width;
        ui.fit_restore = restore;

        assert_eq!(width, layout::natural_width(5, true));
        assert!(ui.is_fitted(width, 5));
        assert_eq!(
            ui.fit_step(width, 5).expect("a restore").width,
            chosen,
            "one press still goes back to the width the user chose"
        );
    }

    /// The stream list arrives in pieces, so the startup fit runs repeatedly
    /// and has to converge rather than oscillate.
    #[test]
    fn refitting_as_the_streams_arrive_lands_on_the_last_count() {
        let mut ui = UiState::new();
        let opened_at = 900;
        let mut width = opened_at;
        for columns in [1, 1, 2, 4, 4, 4] {
            // What the shells' settling tick does: leave a fitted window alone,
            // and always come back to the width it opened at.
            if ui.is_fitted(width, columns) {
                continue;
            }
            if let Some(step) = ui.fit_step(width, columns) {
                ui.fit_restore = Some(opened_at);
                width = step.width;
            }
        }
        assert_eq!(width, layout::natural_width(4, true));
        assert!(ui.is_fitted(width, 4));
        assert_eq!(
            ui.fit_step(width, 4).expect("a restore").width,
            opened_at,
            "the settling leaves one press between here and where it opened"
        );
    }

    /// The bound the platform gives is a ceiling on the fit, so many columns
    /// stop at the usable area instead of running off the screen. A bound under
    /// the window's own floor loses to the floor.
    #[test]
    fn the_fit_stops_at_the_bound_but_never_under_the_minimum() {
        let mut ui = UiState::new();
        ui.max_width = 800;
        let step = ui.fit_step(400, 40).expect("a fit");
        assert_eq!(step.width, 800, "forty columns are clamped to the bound");

        ui.max_width = 20;
        let (min_w, _) = layout::minimum_size(true);
        let step = ui.fit_step(min_w + 50, 1).expect("a fit");
        assert_eq!(step.width, min_w, "a bound under the floor does not win");
    }
}
