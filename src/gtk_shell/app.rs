//! The GTK application: a window, a surface, and the same shared core the
//! native shell runs.
//!
//! Nothing about the mixer is decided here. GTK supplies the window and the
//! events; the shared runtime reduces messages, the shared layout places
//! everything, and the shared renderer draws it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::Sender;
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
use crate::shell::Shell;
use crate::state::Message;
use crate::ui::input::{self, ClipboardAction, PointerAction};
use crate::ui::meter::PEAK_DECAY_INTERVAL;
use crate::ui::{CARET_BLINK, Chrome, Focus};

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
    let (min_w, min_h) = crate::ui::layout::minimum_size();
    surface.borrow().widget.set_size_request(min_w, min_h);

    window.set_child(Some(&surface.borrow().widget));

    // One repaint entry point, so every producer below asks for a frame the
    // same way and the dirty flags decide whether it is worth painting.
    let redraw: Rc<dyn Fn()> = {
        let shell = Rc::clone(&shell);
        let surface = Rc::clone(&surface);
        let profiles = Rc::clone(&profiles);
        Rc::new(move || {
            let mut shell = shell.borrow_mut();
            // The selector is a widget rather than paint, so it follows the
            // snapshot even on a frame the surface has nothing to redraw for.
            profiles.sync(&shell.snapshot);
            // A stale frame is one painted for a different size than the widget
            // now has, which the dirty flags know nothing about.
            if !shell.ui.dirty.needs_paint() && !surface.borrow().is_stale() {
                return;
            }
            let (snapshot, ui) = (&shell.snapshot, &shell.ui);
            surface.borrow_mut().render(snapshot, ui);
            shell.ui.dirty.clear();
        })
    };

    wire_input(&surface, &window, &shell, &msg_tx, &redraw);
    wire_buses(&shell, &redraw, msg_rx, evt_rx);
    wire_ticks(&shell, &redraw);
    wire_geometry(&window, &shell);

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
        let shell = Rc::clone(shell);
        let surface = Rc::clone(surface);
        let redraw = Rc::clone(redraw);
        let msg_tx = msg_tx.clone();
        let window = window.clone();
        Rc::new(move |event: Input| {
            let msgs = {
                let mut shell = shell.borrow_mut();
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
                        if let Some(action) = input::clipboard_action(&shell.ui, key) {
                            drop(surface);
                            clipboard(&window, &mut shell, action, &msg_tx);
                            drop(shell);
                            redraw();
                            return;
                        }
                        let Shell { ui, snapshot, .. } = &mut *shell;
                        input::on_key(ui, snapshot, key)
                    }
                }
            };
            shell.borrow_mut().dispatch(msgs);
            redraw();
        })
    };

    surface::attach_controllers(&surface.borrow().widget, window, handler);
}

/// Copy, cut, or paste for the focused editor, through GDK's clipboard.
fn clipboard(
    window: &gtk::ApplicationWindow,
    shell: &mut Shell,
    action: ClipboardAction,
    msg_tx: &Sender<Message>,
) {
    let clipboard = WidgetExt::display(window).clipboard();
    match action {
        ClipboardAction::Copy | ClipboardAction::Cut => {
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
        }
        // GDK reads asynchronously, so the paste lands a turn later and sends
        // its own message rather than returning one.
        ClipboardAction::Paste => {
            let focus = shell.ui.focus;
            let msg_tx = msg_tx.clone();
            clipboard.read_text_async(gtk::gio::Cancellable::NONE, move |result| {
                let Ok(Some(text)) = result else {
                    return;
                };
                let _ = msg_tx.send(match focus {
                    Focus::Palette => Message::PaletteQueryChanged(text.to_string()),
                    Focus::Modal => Message::ModalNameChanged(text.to_string()),
                    Focus::Body => return,
                });
            });
        }
    }
    shell.ui.dirty.mark_full();
}

/// Drain both buses whenever a producer wakes their fd.
fn wire_buses(
    shell: &Rc<RefCell<Shell>>,
    redraw: &Rc<dyn Fn()>,
    msg_rx: crate::bus::Receiver<Message>,
    evt_rx: crate::bus::Receiver<crate::pipewire_worker::Event>,
) {
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        let fd = msg_rx.wake_fd();
        glib_fd::watch_readable(fd, move || {
            let mut batch = Vec::new();
            msg_rx.drain(|m| batch.push(m));
            shell.borrow_mut().dispatch(batch);
            redraw();
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
        });
    }
}

/// The autosave debounce, the meter animation, and the caret blink. What each
/// one does belongs to the shared shell; when it happens is GTK's to schedule.
fn wire_ticks(shell: &Rc<RefCell<Shell>>, redraw: &Rc<dyn Fn()>) {
    // Coarse enough to collapse a slider drag into one write, fine enough to
    // survive a near-immediate window close.
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            shell.borrow_mut().tick_autosave();
            redraw();
            glib::ControlFlow::Continue
        });
    }

    // Peaks aren't events: a silent node decays to zero on its own here.
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        glib::timeout_add_local(PEAK_DECAY_INTERVAL, move || {
            {
                let mut shell = shell.borrow_mut();
                let mut moved = shell.tick_meters();
                // The knob's ring eases in and out on the same tick.
                moved |= shell.tick_halo(std::time::Instant::now());
                if moved {
                    shell.ui.dirty.mark_full();
                }
            }
            redraw();
            glib::ControlFlow::Continue
        });
    }

    // The caret blinks only while a field has focus; a blink is a repaint, and
    // an idle mixer should not be asking for one.
    {
        let shell = Rc::clone(shell);
        let redraw = Rc::clone(redraw);
        glib::timeout_add_local(CARET_BLINK, move || {
            if shell.borrow_mut().tick_caret() {
                shell.borrow_mut().ui.dirty.mark_full();
                redraw();
            }
            glib::ControlFlow::Continue
        });
    }
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
