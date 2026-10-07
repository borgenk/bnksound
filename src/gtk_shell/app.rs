//! The GTK application: a window, a surface, and the same shared core the
//! native shell runs.
//!
//! Nothing about the mixer is decided here. GTK supplies the window and the
//! events; the shared runtime reduces messages, the shared layout places
//! everything, and the shared renderer draws it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::Sender;
use crate::gtk_shell::frames::Frames;
use crate::gtk_shell::glib_fd;
use crate::gtk_shell::header;
use crate::gtk_shell::profile::ProfileSelector;
use crate::gtk_shell::style;
use crate::gtk_shell::surface::{self, Input, Surface};
use crate::mpris;
use crate::render::screenshot;
use crate::render::text::Font;
use crate::runtime::Runtime;
use crate::settings;
use crate::shell::{AUTOSAVE_DELAY, Shell};
use crate::state::Message;
use crate::ui::input::{self, ClipboardAction, PointerAction};
use crate::ui::{CARET_BLINK, Chrome, FitStep};

pub fn activate(app: &gtk::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    // Boot the shell-agnostic core: buses, peak pool, PipeWire worker, state.
    let (runtime, msg_rx, evt_rx) = match Runtime::boot() {
        Ok(parts) => parts,
        Err(e) => {
            eprintln!("bnksound: cannot start: {e}");
            return;
        }
    };
    let font = match Font::load() {
        Ok(font) => font,
        Err(e) => {
            eprintln!("bnksound: cannot start: {e}");
            return;
        }
    };

    let msg_tx = runtime.sender();
    let geometry = runtime.state().geometry;
    let config = settings::load();

    // MPRIS metadata the snapshot queries for stream titles. Inert when the
    // session bus is unavailable; the labels fall back to their own ladder.
    let mpris = mpris::init(msg_tx.clone());

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("BNK Sound")
        .default_width(geometry.width as i32)
        .default_height(geometry.height as i32)
        .build();
    if geometry.maximized {
        window.maximize();
    }

    let surface = Rc::new(RefCell::new(Surface::new(font)));
    style::install(surface.borrow().palette(), config.gtk_chrome);
    let shell = Rc::new(RefCell::new(Shell::new(runtime, mpris, config)));

    // GTK owns the titlebar and the profile selector both, so the surface
    // paints neither and the window comes to one bar.
    shell.borrow_mut().ui.chrome = Chrome::Toolkit;
    let profiles = Rc::new(ProfileSelector::new(msg_tx.clone()));
    header::install(&window, profiles.widget());

    // The drawing area asks for nothing, so without this the window would
    // shrink past the point where a column still fits.
    let (min_w, min_h) = crate::ui::layout::minimum_size(shell.borrow().ui.settings.show_sidebar);
    surface.borrow().widget.set_size_request(min_w, min_h);

    window.set_child(Some(&surface.borrow().widget));

    let frames = Frames::new(&shell, &surface);
    let timers = Timers::new(&shell, &frames);

    // One entry point for every producer below, run after each change. The
    // selector follows the snapshot at once, the timers pick up any work the
    // change gave them, and the drawing waits for the next frame, where the
    // dirty flags decide whether it is worth painting.
    let redraw: Rc<dyn Fn()> = {
        let shell = Rc::clone(&shell);
        let profiles = Rc::clone(&profiles);
        let frames = Rc::clone(&frames);
        Rc::new(move || {
            // The selector is a widget rather than paint, so it follows the
            // snapshot even on a turn the surface has nothing to redraw for.
            profiles.sync(&shell.borrow().snapshot);
            timers.arm();
            frames.request();
        })
    };

    wire_input(&surface, &window, &shell, &msg_tx, &redraw);
    wire_buses(&window, &shell, &redraw, msg_rx, evt_rx);
    wire_resize(&window, &surface, &frames);
    wire_geometry(&window, &shell);
    if geometry.fitted {
        wire_refit(&window, &shell, geometry.width as i32);
    }

    window.present();
    redraw();
}

/// Route normalized surface events into the shared input mapping.
fn wire_input(
    surface: &Rc<RefCell<Surface>>,
    window: &gtk::ApplicationWindow,
    shell: &Rc<RefCell<Shell>>,
    msg_tx: &Sender<Message>,
    redraw: &Rc<dyn Fn()>,
) {
    let handler = {
        let shell_rc = Rc::clone(shell);
        let surface = Rc::clone(surface);
        let redraw = Rc::clone(redraw);
        let msg_tx = msg_tx.clone();
        let window = window.clone();
        Rc::new(move |event: Input| {
            let msgs = {
                let mut shell = shell_rc.borrow_mut();
                let surface = surface.borrow();
                let (w, h) = (
                    surface.widget.width().max(1),
                    surface.widget.height().max(1),
                );
                let layout = crate::ui::layout::project(
                    &shell.snapshot,
                    &shell.ui,
                    crate::render::primitives::Rect::new(0, 0, w, h),
                );
                match event {
                    Input::Pointer(event, ms) => {
                        // Scroll arrives without coordinates, so it reuses the
                        // pointer's last position.
                        let event = match event.action {
                            PointerAction::Scroll { .. } => crate::ui::input::PointerEvent {
                                x: shell.ui.pointer.0,
                                y: shell.ui.pointer.1,
                                ..event
                            },
                            _ => event,
                        };
                        let Shell { ui, snapshot, .. } = &mut *shell;
                        input::on_pointer(
                            ui,
                            &layout,
                            snapshot,
                            event,
                            u64::from(ms),
                            surface.font(),
                        )
                    }
                    Input::Key(key) => {
                        if input::is_screenshot_key(key) {
                            let (pixels, w, h) = surface.frame();
                            screenshot::capture(pixels, w, h);
                            return;
                        }
                        if input::is_fit_key(&shell.ui, key) {
                            drop(surface);
                            // Resizing settles through the frame clock, which
                            // borrows the shell again on its way back.
                            drop(shell);
                            toggle_fit_width(&window, &shell_rc);
                            return;
                        }
                        if let Some(action) = input::clipboard_action(&shell.ui, key) {
                            drop(surface);
                            // Paste borrows the shell again when GDK answers,
                            // so this one must be released first.
                            drop(shell);
                            clipboard(&window, &shell_rc, &redraw, action, &msg_tx);
                            redraw();
                            return;
                        }
                        let Shell { ui, snapshot, .. } = &mut *shell;
                        input::on_key(ui, snapshot, key)
                    }
                }
            };
            dispatch(&window, &shell_rc, msgs);
            redraw();
        })
    };

    surface::attach_controllers(&surface.borrow().widget, window, handler);
}

/// Reduce messages, and let a fitted window follow the columns when the user
/// has just asked for a different set of them.
///
/// Whether the window was fitted is read before the messages land, because
/// afterwards the count has moved and every fitted window would read as
/// un-fitted. A window the user sized is theirs: the new columns scroll into the
/// strip and the width stays put.
fn dispatch(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>, msgs: Vec<Message>) {
    let follows = msgs.iter().any(Message::changes_columns);
    let width = window.default_width();
    let follows = {
        let mut shell = shell.borrow_mut();
        let was_fitted = follows && shell.is_fitted(width);
        shell.dispatch(msgs);
        was_fitted
    };
    if !follows {
        return;
    }
    // The width to come back to is still the user's own, not the fitted width
    // the toggle just moved off.
    let step = {
        let shell = shell.borrow();
        let columns = shell.columns();
        if shell.ui.is_fitted(width, columns) {
            return;
        }
        let restore = shell.ui.fit_restore;
        shell
            .ui
            .fit_step(width, columns)
            .map(|step| FitStep { restore, ..step })
    };
    apply_fit(window, shell, step);
}

/// Copy, cut, or paste for the focused editor, through GDK's clipboard.
fn clipboard(
    window: &gtk::ApplicationWindow,
    shell: &Rc<RefCell<Shell>>,
    redraw: &Rc<dyn Fn()>,
    action: ClipboardAction,
    msg_tx: &Sender<Message>,
) {
    let clipboard = WidgetExt::display(window).clipboard();
    match action {
        ClipboardAction::Copy | ClipboardAction::Cut => {
            let mut shell = shell.borrow_mut();
            let Some(text) = shell.ui.editor.selected_text() else {
                return;
            };
            clipboard.set_text(&text);
            if action == ClipboardAction::Cut {
                shell.ui.editor.delete_selection();
                if let Some(m) = input::editor_text_message(&shell.ui) {
                    let _ = msg_tx.send(m);
                }
            }
            shell.ui.dirty.mark_full();
        }
        // GDK reads asynchronously, so the text lands a turn later. It goes
        // through the editor, which owns the caret, the selection, and the
        // length limit; the message then carries what the editor now holds.
        ClipboardAction::Paste => {
            let shell = Rc::clone(shell);
            let redraw = Rc::clone(redraw);
            let msg_tx = msg_tx.clone();
            clipboard.read_text_async(gtk::gio::Cancellable::NONE, move |result| {
                let Ok(Some(text)) = result else {
                    return;
                };
                let message = {
                    let mut shell = shell.borrow_mut();
                    if !shell.ui.editor.paste(&text) {
                        return;
                    }
                    shell.ui.dirty.mark_full();
                    input::editor_text_message(&shell.ui)
                };
                if let Some(m) = message {
                    let _ = msg_tx.send(m);
                }
                redraw();
            });
        }
    }
}

/// Drain both buses whenever a producer wakes their fd.
fn wire_buses(
    window: &gtk::ApplicationWindow,
    shell: &Rc<RefCell<Shell>>,
    redraw: &Rc<dyn Fn()>,
    msg_rx: crate::bus::Receiver<Message>,
    evt_rx: crate::bus::Receiver<crate::pipewire_worker::Event>,
) {
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        let window = window.clone();
        let fd = msg_rx.wake_fd();
        glib_fd::watch_readable(fd, move || {
            let mut batch = Vec::new();
            msg_rx.drain(|m| batch.push(m));
            dispatch(&window, &shell, batch);
            redraw();
            glib::ControlFlow::Continue
        });
    }
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        let fd = evt_rx.wake_fd();
        glib_fd::watch_readable(fd, move || {
            let mut batch = Vec::new();
            evt_rx.drain(|e| batch.push(e));
            shell.borrow_mut().dispatch_worker(batch);
            redraw();
            glib::ControlFlow::Continue
        });
    }
}

/// A resize changes nothing the dirty flags track, so the frame it needs is
/// asked for here. The surface's layout signal comes with every size the
/// compositor gives the window, maximized and tiled included, and a new scale
/// changes the pixels without changing the size.
fn wire_resize(
    window: &gtk::ApplicationWindow,
    surface: &Rc<RefCell<Surface>>,
    frames: &Rc<Frames>,
) {
    {
        let frames = Rc::clone(frames);
        window.connect_realize(move |window| {
            if let Some(surface) = window.surface() {
                let frames = Rc::clone(&frames);
                surface.connect_layout(move |_, _, _| frames.request());
            }
        });
    }
    let frames = Rc::clone(frames);
    surface
        .borrow()
        .widget
        .connect_scale_factor_notify(move |_| frames.request());
}

/// The autosave and the caret blink, each a timer that runs only while it has
/// work. What each one does belongs to the shared shell; when it happens is
/// GTK's to schedule.
struct Timers {
    shell: Rc<RefCell<Shell>>,
    frames: Rc<Frames>,
    /// Whether a save is armed.
    saving: Cell<bool>,
    /// Whether the caret's blink timer is running.
    blinking: Cell<bool>,
}

impl Timers {
    fn new(shell: &Rc<RefCell<Shell>>, frames: &Rc<Frames>) -> Rc<Self> {
        Rc::new(Timers {
            shell: Rc::clone(shell),
            frames: Rc::clone(frames),
            saving: Cell::new(false),
            blinking: Cell::new(false),
        })
    }

    /// Start whichever timer the latest change gave work to: a save once the
    /// disk is behind, a blink once a field has focus.
    fn arm(self: &Rc<Self>) {
        let (unsaved, blinking) = {
            let shell = self.shell.borrow();
            (shell.unsaved(), shell.ui.caret_blinking())
        };
        if unsaved && !self.saving.replace(true) {
            let timers = Rc::clone(self);
            glib::timeout_add_local_once(AUTOSAVE_DELAY, move || {
                timers.saving.set(false);
                timers.shell.borrow_mut().tick_autosave();
                // A failed save has something to say on the status line.
                timers.frames.request();
            });
        }
        if blinking && !self.blinking.replace(true) {
            let timers = Rc::clone(self);
            glib::timeout_add_local(CARET_BLINK, move || {
                let mut shell = timers.shell.borrow_mut();
                if shell.tick_caret() {
                    shell.ui.dirty.mark_full();
                    timers.frames.request();
                }
                if shell.ui.caret_blinking() {
                    glib::ControlFlow::Continue
                } else {
                    timers.blinking.set(false);
                    glib::ControlFlow::Break
                }
            });
        }
    }
}

/// Size the window to the width its columns need, or put back the width it had
/// before the last fit.
///
/// GTK4 has no resize call. Setting the default size again is what moves a
/// window that is already on screen, and it is also the property that tracks
/// the normal-state width while the window is maximized, so it is both the
/// width to read and the one to write.
///
/// A maximized or fullscreen window has no width to give. Tiled, GTK offers no
/// way to ask, and the request goes out to be ignored.
fn toggle_fit_width(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>) {
    let current = window.default_width();
    // GTK offers nothing like configure_bounds, so the ceiling is the window
    // manager's to apply rather than ours to guess at.
    let step = shell.borrow_mut().fit_press(current);
    apply_fit(window, shell, step);
}

/// Follow the stream list for the first moments of a session, so a window that
/// closed at a fitted width opens at the width this session's columns need.
///
/// PipeWire reports its streams over those first moments rather than all at
/// once, so a single fit would land on however many had arrived by then. The
/// tick stops itself once the list has settled and leaves the width alone after
/// that; nothing here resizes a window on a stream that turns up later.
fn wire_refit(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>, opened_at: i32) {
    const STEP: Duration = Duration::from_millis(100);
    const SETTLE: Duration = Duration::from_millis(1500);

    let (window, shell) = (window.clone(), Rc::clone(shell));
    let mut left = SETTLE.as_millis() / STEP.as_millis();
    glib::timeout_add_local(STEP, move || {
        tick_refit(&window, &shell, opened_at);
        left -= 1;
        if left == 0 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// One turn of the settling above: fit to whatever columns exist right now. A
/// window already standing where its columns want it is left alone, which keeps
/// a settled fit from bouncing back on the next tick.
fn tick_refit(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>, opened_at: i32) {
    let current = window.default_width();
    let step = {
        let shell = shell.borrow();
        let columns = shell.columns();
        if columns == 0 || shell.ui.is_fitted(current, columns) {
            return;
        }
        shell.ui.fit_step(current, columns)
    };
    // The width to go back to is the one the window opened at, not whatever an
    // earlier turn of this same settling left behind.
    let step = step.map(|step| FitStep {
        restore: Some(opened_at),
        ..step
    });
    apply_fit(window, shell, step);
}

/// Take a width the fit asked for and make it the window's.
fn apply_fit(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>, step: Option<FitStep>) {
    if window.is_maximized() || window.is_fullscreen() {
        return;
    }
    let Some(step) = step else {
        return;
    };
    shell.borrow_mut().ui.fit_restore = step.restore;
    window.set_default_size(step.width, window.default_height());
}

/// Persist the window's normal-state size and its maximized flag, once, on
/// close.
///
/// Watching the size properties instead would also catch every
/// compositor-driven configure during startup and save sizes the user never
/// picked. The default size is the one to restore to: GTK keeps tracking it
/// while the window is maximized, whose own size is the screen's.
///
/// The save runs here rather than going out on the bus, which has no one left
/// to drain it once the last window closes.
fn wire_geometry(window: &gtk::ApplicationWindow, shell: &Rc<RefCell<Shell>>) {
    let shell = Rc::clone(shell);
    window.connect_close_request(move |w| {
        shell.borrow_mut().shutdown(
            w.default_width().max(0) as u32,
            w.default_height().max(0) as u32,
            w.is_maximized(),
        );
        glib::Propagation::Proceed
    });
}
