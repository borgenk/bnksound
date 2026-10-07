//! When the GTK shell paints, and what keeps it stepping.
//!
//! A change asks for a frame, and the next turn of GTK's frame clock steps
//! whatever moves and repaints whatever changed. The clock follows the
//! compositor's frame callbacks, so the window paints at most once per refresh
//! and not at all while hidden. The tick stays registered only while the
//! meters or the fades move; a still window has none and asks for no frames.
//!
//! Meters at rest sleep on the peak pool's fd instead. The watch removes itself
//! when it fires and comes back once the bars are at rest again, so peaks that
//! arrive while the bars move never wake the loop on their own.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::gtk_shell::glib_fd;
use crate::gtk_shell::surface::Surface;
use crate::shell::Shell;

/// The frame clock's side of the GTK shell.
pub struct Frames {
    shell: Rc<RefCell<Shell>>,
    surface: Rc<RefCell<Surface>>,
    widget: gtk::Picture,
    /// Whether the tick callback is registered.
    ticking: Cell<bool>,
    /// Whether the peak pool's fd is watched, which it is while the meters
    /// rest.
    watching: Cell<bool>,
}

impl Frames {
    /// Pace the surface's frames, with the meters starting at rest.
    pub fn new(shell: &Rc<RefCell<Shell>>, surface: &Rc<RefCell<Surface>>) -> Rc<Self> {
        let widget = surface.borrow().widget.clone();
        let frames = Rc::new(Frames {
            shell: Rc::clone(shell),
            surface: Rc::clone(surface),
            widget,
            ticking: Cell::new(false),
            watching: Cell::new(false),
        });
        frames.watch_peaks();
        frames
    }

    /// Ask for a frame. The next frame clock tick steps what moves and paints
    /// what changed, and the tick stays only while something still moves.
    pub fn request(self: &Rc<Self>) {
        if self.ticking.replace(true) {
            return;
        }
        let frames = Rc::clone(self);
        self.widget.add_tick_callback(move |_, _| frames.tick());
    }

    fn tick(self: &Rc<Self>) -> glib::ControlFlow {
        let now = Instant::now();
        let (moving, resting) = {
            let mut shell = self.shell.borrow_mut();
            if shell.tick_meters(now) {
                shell.ui.dirty.mark_meters();
            }
            // The knob's ring and the fit button's press mark ease on the same
            // tick.
            let fading = shell.tick_fades(now);
            if fading {
                shell.ui.dirty.mark_full();
            }
            let resting = shell.meters_due().is_none();
            (fading || !resting, resting)
        };
        self.paint();
        if resting {
            self.watch_peaks();
        }
        if moving {
            glib::ControlFlow::Continue
        } else {
            self.ticking.set(false);
            glib::ControlFlow::Break
        }
    }

    fn paint(&self) {
        let mut shell = self.shell.borrow_mut();
        let mut surface = self.surface.borrow_mut();
        // A stale frame is one painted for a different size than the widget
        // now has, which the dirty flags know nothing about.
        if !shell.ui.dirty.needs_paint() && !surface.is_stale() {
            return;
        }
        let (snapshot, ui) = (&shell.snapshot, &shell.ui);
        // A widget GTK has not sized yet paints nothing and keeps its flags
        // for the frame after its first layout.
        if surface.render(snapshot, ui) {
            shell.ui.dirty.clear();
        }
    }

    /// Sleep the meters on the peak pool's fd until the audio threads have
    /// something audible, then step them at once and tick from there.
    fn watch_peaks(self: &Rc<Self>) {
        if self.watching.replace(true) {
            return;
        }
        let frames = Rc::clone(self);
        let fd = self.shell.borrow().runtime.peaks().wake_fd();
        glib_fd::watch_readable(fd, move || {
            frames.watching.set(false);
            {
                let mut shell = frames.shell.borrow_mut();
                if shell.wake_meters(Instant::now()) {
                    shell.ui.dirty.mark_meters();
                }
            }
            // The tick puts the watch back once the bars are at rest.
            frames.request();
            glib::ControlFlow::Break
        });
    }
}
