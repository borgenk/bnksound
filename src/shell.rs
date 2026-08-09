//! The half of a running window that does not depend on where its events came
//! from: the core, the retained UI, the current projection, and the ticks whose
//! timing is the same either way.
//!
//! A shell owns one of these and keeps for itself only its event source and the
//! path a painted frame takes to the screen. Everything both shells would
//! otherwise write twice lives here, which is what keeps them from drifting.

use std::collections::HashSet;
use std::time::Instant;

use crate::mpris::Mpris;
use crate::pipewire_worker::Event as WorkerEvent;
use crate::runtime::Runtime;
use crate::settings::Settings;
use crate::state::Message;
use crate::ui::UiState;
use crate::ui::layout::RowId;
use crate::view::snapshot::{ViewSnapshot, build_snapshot};

/// The shared half of a running window.
pub struct Shell {
    pub runtime: Runtime,
    pub ui: UiState,
    pub snapshot: ViewSnapshot,
    pub mpris: Mpris,
}

impl Shell {
    /// Take ownership of the booted core and project it once, so the first
    /// frame has something to draw.
    pub fn new(runtime: Runtime, mpris: Mpris, settings: Settings) -> Self {
        let mut ui = UiState::new();
        ui.settings = settings;
        let mut shell = Shell {
            runtime,
            ui,
            snapshot: build_snapshot(&crate::state::empty(), |_| None),
            mpris,
        };
        shell.refresh();
        shell
    }

    /// Rebuild the render-ready projection after a state change.
    pub fn refresh(&mut self) {
        let mpris = &self.mpris;
        self.snapshot = build_snapshot(self.runtime.state(), |stream| mpris.resolve_title(stream));
        // Rows that no longer exist stop being animated and stop being kept.
        // Nothing else drops them, and every reconnect mints a fresh node id,
        // so without this the decay walks a growing list of dead rows forever.
        let live: HashSet<&RowId> = self.snapshot.meter_routes.values().flatten().collect();
        self.ui.meters.retain(|row| live.contains(row));
        self.ui.dirty.mark_layout();
    }

    /// Reduce messages, reprojecting when any of them changed the state.
    pub fn dispatch(&mut self, messages: impl IntoIterator<Item = Message>) {
        let mut changed = false;
        for message in messages {
            changed |= self.runtime.dispatch(message);
        }
        if changed {
            self.refresh();
        }
    }

    /// The same for events coming back from the PipeWire worker.
    pub fn dispatch_worker(&mut self, events: impl IntoIterator<Item = WorkerEvent>) {
        let mut changed = false;
        for event in events {
            changed |= self.runtime.dispatch_worker(event);
        }
        if changed {
            self.refresh();
        }
    }

    /// Flush whatever the session has changed since the last tick. Edits land
    /// in state as they happen, so this is only what gets them onto disk, and
    /// it reprojects only when a failed save has something to say.
    pub fn tick_autosave(&mut self) {
        self.dispatch([Message::AutoSaveTick]);
    }

    /// Step the meters: decay every bar, then fold in the peaks the audio
    /// threads have left since the last step. Reports whether anything moved,
    /// so a window whose bars are all at rest skips its repaint.
    pub fn tick_meters(&mut self) -> bool {
        let mut moved = self.ui.meters.decay();
        // Three disjoint fields, so the routes can be read off the snapshot
        // while the meters take a mutable borrow.
        let routes = &self.snapshot.meter_routes;
        let meters = &mut self.ui.meters;
        self.runtime.peaks().drain(|node_id, values| {
            if let Some(rows) = routes.get(&node_id) {
                for row in rows {
                    moved |= meters.apply(row, values);
                }
            }
        });
        moved
    }

    /// Ease what fades toward where it is going: the knob rings toward wherever
    /// the pointer is, the fit button's press mark toward nothing. Driven by the
    /// clock rather than a step per call, so it is safe on whatever turn the
    /// loop is on. Reports whether anything is still moving.
    pub fn tick_fades(&mut self, now: Instant) -> bool {
        let lit = self.ui.lit_knob();
        let halo = self.ui.halo.advance(lit.as_ref(), now);
        let mark = self.ui.fit_mark.advance(now);
        halo || mark
    }

    /// What a press on the fit button comes to, measured against the columns
    /// the current snapshot puts in the strip.
    pub fn fit_press(&mut self, width: i32) -> Option<crate::ui::FitStep> {
        let columns = self.columns();
        self.ui.fit_press(width, columns)
    }

    /// Flip the caret, while a field has focus. Reports whether it changed.
    pub fn tick_caret(&mut self) -> bool {
        self.ui.blink_caret()
    }

    /// How many columns the current snapshot puts in the strip, which is what
    /// the fit is measured against.
    pub fn columns(&self) -> usize {
        crate::ui::layout::column_count(&self.snapshot)
    }

    /// Whether the window stands at the width its columns want. What a relaunch
    /// reads to know it should work the width out again rather than restore the
    /// one this session happened to end on.
    pub fn is_fitted(&self, width: i32) -> bool {
        self.ui.is_fitted(width, self.columns())
    }

    /// Persist geometry and flush a final save on the way out.
    pub fn shutdown(&mut self, width: u32, height: u32, maximized: bool) {
        let fitted = self.is_fitted(width as i32);
        let _ = self.runtime.dispatch(Message::GeometryChanged {
            width,
            height,
            maximized,
            fitted,
        });
        self.runtime.shutdown();
    }
}
