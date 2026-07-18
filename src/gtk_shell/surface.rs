//! The GTK surface: one widget showing the frame the shared renderer paints,
//! and the controllers that turn GTK events into shared input values.
//!
//! GTK's role here is a window, an event source, and something to put pixels on.
//! Everything the mixer looks like and everything it does on a click lives in
//! the shared modules, the same ones the native shell drives.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
// A GDK keyval is an X11 keysym once it is out of its newtype, which is what
// the shared key naming takes.
use gtk4::glib::translate::IntoGlib;
use gtk4::prelude::*;

use crate::render::buffer::PixelBuffer;
use crate::render::image::IconCache;
use crate::render::paint::paint_frame;
use crate::render::primitives::{Painter, Rect};
use crate::render::text::Font;
use crate::ui::UiState;
use crate::ui::input::{Key, KeyEvent, Modifiers, MouseButton, PointerAction, PointerEvent};
use crate::ui::theme::Palette;
use crate::view::snapshot::ViewSnapshot;

/// The size a frame was painted for: the widget's logical size and the scale it
/// was painted at.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Allocation {
    w: i32,
    h: i32,
    scale: i32,
}

/// The widget the frame is shown on, plus the buffer it is painted into.
pub struct Surface {
    pub widget: gtk::Picture,
    buffer: PixelBuffer,
    font: Font,
    palette: Palette,
    icons: IconCache,
    /// What the frame on screen was painted for, absent until the first paint.
    painted: Option<Allocation>,
}

impl Surface {
    pub fn new(font: Font) -> Self {
        let widget = gtk::Picture::new();
        // The texture is produced at exactly the widget's pixel size, so it
        // fills the widget rather than being letterboxed into it.
        widget.set_keep_aspect_ratio(false);
        widget.set_can_shrink(true);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_focusable(true);
        Surface {
            widget,
            buffer: PixelBuffer::new(1, 1),
            font,
            palette: Palette::dark(),
            icons: IconCache::new(),
            painted: None,
        }
    }

    /// The widget's logical size and scale, or None before GTK has laid it out.
    ///
    /// GTK reports HiDPI as an integer factor; painting at it keeps text sharp,
    /// and the widget scales the result back down.
    fn allocation(&self) -> Option<Allocation> {
        let (w, h) = (self.widget.width(), self.widget.height());
        (w > 0 && h > 0).then(|| Allocation {
            w,
            h,
            scale: self.widget.scale_factor().max(1),
        })
    }

    /// Whether the frame on screen no longer matches the widget showing it.
    ///
    /// GTK sizes a widget during layout, which is after the first frame is
    /// asked for, so the opening frame is always stale. Resizes land the same
    /// way. The dirty flags cannot see either, because neither changes what the
    /// mixer contains, only how much room it has.
    pub fn is_stale(&self) -> bool {
        match self.allocation() {
            Some(now) => self.painted != Some(now),
            None => false,
        }
    }

    /// Repaint the frame and hand it to the widget as a texture. A widget GTK
    /// has not sized yet keeps the frame it already has.
    pub fn render(&mut self, snapshot: &ViewSnapshot, ui: &UiState) {
        let Some(alloc) = self.allocation() else {
            return;
        };
        let Allocation { w, h, scale } = alloc;
        let (dw, dh) = (w * scale, h * scale);
        self.buffer.resize(dw as u32, dh as u32);

        let layout = crate::ui::layout::project(snapshot, ui, Rect::new(0, 0, w, h));
        {
            let (pixels, bw, bh) = self.buffer.parts();
            let mut painter = Painter::scaled(pixels, bw, bh, scale as f32);
            paint_frame(
                &mut painter,
                snapshot,
                ui,
                &layout,
                &self.font,
                &self.palette,
                &mut self.icons,
            );
        }

        let bytes = glib::Bytes::from(self.buffer.bytes());
        let texture = gdk::MemoryTexture::new(
            dw,
            dh,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            (dw * 4) as usize,
        );
        self.widget.set_paintable(Some(&texture));
        self.painted = Some(alloc);
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    /// The palette the body is painted in, so the stylesheet can dress GTK's
    /// chrome in the same colours.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The last painted frame, for a screenshot.
    pub fn frame(&self) -> (&[u32], u32, u32) {
        (
            self.buffer.pixels(),
            self.buffer.width(),
            self.buffer.height(),
        )
    }
}

/// What a controller callback hands back to the shell.
pub type Handler = Rc<dyn Fn(Input)>;

/// A GTK event, normalized to what the shared input mapping takes.
pub enum Input {
    Pointer(PointerEvent, u32),
    Key(KeyEvent),
}

/// Attach the pointer, scroll, and keyboard controllers. Each one normalizes its
/// GTK event and hands it to `on_input`; none of them touch state directly.
pub fn attach_controllers(
    widget: &gtk::Picture,
    window: &gtk::ApplicationWindow,
    on_input: Handler,
) {
    let click = gtk::GestureClick::new();
    // Button 0 means every button; which one it was comes off the gesture.
    click.set_button(0);
    {
        let on_input = Rc::clone(&on_input);
        click.connect_pressed(move |g, _n, x, y| {
            let button = mouse_button(g.current_button());
            on_input(Input::Pointer(
                PointerEvent {
                    x,
                    y,
                    action: PointerAction::Press(button),
                },
                event_ms(g),
            ));
        });
    }
    {
        let on_input = Rc::clone(&on_input);
        click.connect_released(move |g, _n, x, y| {
            let button = mouse_button(g.current_button());
            on_input(Input::Pointer(
                PointerEvent {
                    x,
                    y,
                    action: PointerAction::Release(button),
                },
                event_ms(g),
            ));
        });
    }
    widget.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    {
        let on_input = Rc::clone(&on_input);
        motion.connect_motion(move |_, x, y| {
            on_input(Input::Pointer(
                PointerEvent {
                    x,
                    y,
                    action: PointerAction::Motion,
                },
                0,
            ));
        });
    }
    {
        // Leaving the widget parks the pointer far outside it, which clears the
        // hover the same way a motion to empty space would.
        let on_input = Rc::clone(&on_input);
        motion.connect_leave(move |_| {
            on_input(Input::Pointer(
                PointerEvent {
                    x: -1.0,
                    y: -1.0,
                    action: PointerAction::Motion,
                },
                0,
            ));
        });
    }
    widget.add_controller(motion);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let on_input = Rc::clone(&on_input);
        scroll.connect_scroll(move |_, dx, dy| {
            // GTK reports wheel notches; the strip scrolls in pixels.
            const NOTCH: f64 = 40.0;
            on_input(Input::Pointer(
                PointerEvent {
                    x: 0.0,
                    y: 0.0,
                    action: PointerAction::Scroll {
                        dx: (dx * NOTCH) as f32,
                        dy: (dy * NOTCH) as f32,
                    },
                },
                0,
            ));
            glib::Propagation::Stop
        });
    }
    widget.add_controller(scroll);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, keyval, _code, state| match map_key(keyval) {
        Some(key) => {
            on_input(Input::Key(KeyEvent {
                key,
                mods: modifiers(state),
            }));
            glib::Propagation::Stop
        }
        None => glib::Propagation::Proceed,
    });
    window.add_controller(keys);
}

/// Milliseconds since the event source started, for multi-click detection.
fn event_ms(gesture: &gtk::GestureClick) -> u32 {
    gesture
        .current_event()
        .map(|e| e.time())
        .unwrap_or_default()
}

fn mouse_button(button: u32) -> MouseButton {
    match button {
        gdk::BUTTON_SECONDARY => MouseButton::Right,
        gdk::BUTTON_MIDDLE => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn modifiers(state: gdk::ModifierType) -> Modifiers {
    Modifiers {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
    }
}

/// Normalize a GDK keyval. A GDK keyval is an X11 keysym, so the naming is the
/// shared table; anything it does not name becomes the character the keysym
/// stands for, which is the unmodified letter even while Ctrl is held, so
/// shortcuts survive.
fn map_key(keyval: gdk::Key) -> Option<Key> {
    Key::from_keysym(keyval.into_glib()).or_else(|| {
        keyval
            .to_unicode()
            .filter(|c| !c.is_control())
            .map(Key::Char)
    })
}
