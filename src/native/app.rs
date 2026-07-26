//! The native Wayland application: bind the globals, map an xdg-shell toplevel,
//! present wl_shm frames painted by the shared renderer, and translate input.
//!
//! One poll loop waits on the Wayland socket, both bus wakeup fds, and the
//! one-window lock, with the meter tick as its timeout, so an idle window does
//! no work beyond the tick.

use std::io;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use crate::APP_ID;
use crate::bus::Receiver;
use crate::mpris;
use crate::native::clipboard::{self, MIME_UTF8};
use crate::native::instance::{self, Launch, Listener};
use crate::pipewire_worker::Event as WorkerEvent;
use crate::platform::conn::Connection;
use crate::platform::protocol::{cursor, evt, req, *};
use crate::platform::shm::ShmPool;
use crate::platform::sys::{PollFd, poll};
use crate::platform::wire::{Arg, Message, encode};
use crate::platform::xkb::Keyboard;
use crate::render::image::IconCache;
use crate::render::paint::paint_frame;
use crate::render::primitives::{Painter, Rect};
use crate::render::screenshot;
use crate::render::text::Font;
use crate::runtime::Runtime;
use crate::settings::Decorations;
use crate::shell::Shell;
use crate::state::Message as AppMessage;
use crate::ui::input::{
    self, ClipboardAction, Key, KeyEvent, Modifiers, MouseButton, PointerAction, PointerEvent,
    WindowAction,
};
use crate::ui::layout::{self, ResizeEdge};
use crate::ui::meter::PEAK_DECAY_INTERVAL;
use crate::ui::theme::Palette;
use crate::ui::{CARET_BLINK, Chrome, Drag};

/// One wl_shm buffer and whether the compositor still holds it.
#[derive(Clone, Copy, Default)]
struct BufferSlot {
    obj: u32,
    busy: bool,
}

/// How often state is flushed to disk while the app runs. Matches the interval
/// the GTK shell has always used.
const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(500);

/// Globals we bind, with the versions we speak.
const COMPOSITOR_VERSION: u32 = 4;
const SHM_VERSION: u32 = 1;
const WM_BASE_VERSION: u32 = 2;
const SEAT_VERSION: u32 = 5;
const ACTIVATION_VERSION: u32 = 1;

pub struct App {
    conn: Connection,
    next_id: u32,

    // Bound globals.
    registry: u32,
    compositor: u32,
    shm: u32,
    wm_base: u32,
    seat: u32,
    cursor_mgr: u32,
    cursor_device: u32,
    /// Serial of the latest pointer enter, which set_shape must quote.
    pointer_serial: u32,
    /// Shape currently set, so an unchanged hover does not re-request it.
    cursor_shape: u32,
    /// Whether the pointer is over the window. The cursor is only ours to shape
    /// while it is.
    pointer_inside: bool,
    data_device_mgr: u32,
    data_device: u32,
    /// The offer holding the current selection, or 0 when the clipboard is
    /// empty or holds nothing we can read.
    selection_offer: u32,
    /// Our own selection source, and the text it serves.
    data_source: u32,
    clipboard_text: String,
    /// Latest input serial, which set_selection must quote.
    last_serial: u32,
    decoration_mgr: u32,
    decoration: u32,
    activation: u32,
    /// This launch's own activation token, spent on the first configure to ask
    /// for focus the way a second launch asks on our behalf. Empty when the
    /// desktop passed none.
    startup_token: String,
    /// A token request in flight, which the compositor answers with a done
    /// event. Zero when none is outstanding.
    activation_token: u32,

    // Surface objects.
    surface: u32,
    xdg_surface: u32,
    xdg_toplevel: u32,
    pointer: u32,
    keyboard: u32,

    // HiDPI: the compositor's preferred scale, and the viewport that maps the
    // scaled buffer back onto the logical window size.
    fractional_mgr: u32,
    fractional: u32,
    viewporter: u32,
    viewport: u32,
    scale: f32,

    // Presentation: two buffers in one pool, alternated so a frame is never
    // painted into memory the compositor is still sampling.
    pool: Option<ShmPool>,
    pool_obj: u32,
    buffers: [BufferSlot; 2],
    /// Buffer size in device pixels, which the scale moves independently of the
    /// window's logical size.
    buffer_dims: (i32, i32),
    /// The slot the last presented frame went into, which a screenshot reads.
    last_painted: usize,

    // Window state.
    width: i32,
    height: i32,
    /// Last size the window had while not maximized, which is what a relaunch
    /// restores to.
    normal_size: (i32, i32),
    configured: bool,
    pub closed: bool,

    /// The core, the retained UI, and the projection, which is everything this
    /// shell shares with the GTK one.
    shell: Shell,
    font: Font,
    palette: Palette,
    icons: IconCache,
    msg_rx: Receiver<AppMessage>,
    evt_rx: Receiver<WorkerEvent>,

    /// The one-window lock. Later launches hand themselves over on it, and the
    /// window comes forward instead of a second one opening. None when the lock
    /// could not be taken, which leaves them to open their own.
    instance: Option<Listener>,

    // Input.
    xkb: Option<Keyboard>,
    /// Key repeat, which Wayland leaves to the client: the compositor only
    /// reports the rate and delay, and we run the timer.
    repeat_delay: Duration,
    repeat_period: Duration,
    held_key: Option<(u32, Instant)>,
    ptr_x: f64,
    ptr_y: f64,
    /// When the caret next flips, while a field has focus.
    caret_deadline: Instant,
    /// When the next save tick is due. Edits land in state as they happen, so
    /// this is what gets them onto disk.
    autosave_deadline: Instant,
    /// When the meters next step. Their decay is a step per tick, so the step
    /// has to be paced rather than taken on whatever turn the loop is on.
    meter_deadline: Instant,
    started: Instant,
}

impl App {
    /// Connect, bind globals, and map the toplevel. `instance` is the lock this
    /// launch took, which the loop watches for later ones.
    pub fn new(instance: Option<Listener>, startup_token: String) -> io::Result<Self> {
        let (runtime, msg_rx, evt_rx) = Runtime::boot()?;
        let mpris = mpris::init(runtime.sender());
        let font = Font::load()?;
        let conn = Connection::connect()?;
        let shell = Shell::new(runtime, mpris, crate::settings::load());
        let geometry = shell.runtime.state().geometry;
        // A saved size from a build with a different minimum, or none saved at
        // all, still opens at something a column fits in.
        let startup_size = layout::at_least_minimum(
            i32::try_from(geometry.width).unwrap_or(560),
            i32::try_from(geometry.height).unwrap_or(720),
        );

        let mut app = App {
            conn,
            next_id: 2,
            registry: 0,
            compositor: 0,
            shm: 0,
            wm_base: 0,
            seat: 0,
            cursor_mgr: 0,
            cursor_device: 0,
            pointer_serial: 0,
            cursor_shape: 0,
            pointer_inside: false,
            data_device_mgr: 0,
            data_device: 0,
            selection_offer: 0,
            data_source: 0,
            clipboard_text: String::new(),
            last_serial: 0,
            decoration_mgr: 0,
            decoration: 0,
            activation: 0,
            activation_token: 0,
            startup_token,
            surface: 0,
            xdg_surface: 0,
            xdg_toplevel: 0,
            pointer: 0,
            keyboard: 0,
            fractional_mgr: 0,
            fractional: 0,
            viewporter: 0,
            viewport: 0,
            scale: 1.0,
            pool: None,
            pool_obj: 0,
            buffers: [BufferSlot::default(), BufferSlot::default()],
            buffer_dims: (0, 0),
            last_painted: 0,
            width: startup_size.0,
            height: startup_size.1,
            normal_size: startup_size,
            configured: false,
            closed: false,
            shell,
            font,
            palette: Palette::dark(),
            icons: IconCache::new(),
            msg_rx,
            evt_rx,
            instance,
            xkb: None,
            // Sensible defaults until repeat_info arrives.
            repeat_delay: Duration::from_millis(600),
            repeat_period: Duration::from_millis(40),
            held_key: None,
            ptr_x: 0.0,
            ptr_y: 0.0,
            caret_deadline: Instant::now() + CARET_BLINK,
            autosave_deadline: Instant::now() + AUTOSAVE_INTERVAL,
            meter_deadline: Instant::now() + PEAK_DECAY_INTERVAL,
            started: Instant::now(),
        };
        app.bind_globals()?;
        app.create_window()?;
        Ok(app)
    }

    fn new_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, object: u32, opcode: u16, args: &[Arg]) {
        encode(self.conn.out(), object, opcode, args);
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush(None)
    }

    /// Ask for the registry and wait for the globals to arrive.
    fn bind_globals(&mut self) -> io::Result<()> {
        self.registry = self.new_id();
        let registry = self.registry;
        self.send(
            WL_DISPLAY,
            req::DISPLAY_GET_REGISTRY,
            &[Arg::NewId(registry)],
        );
        self.roundtrip()?;

        if std::env::var_os("BNKSOUND_DEBUG").is_some() {
            eprintln!(
                "bound: compositor={} shm={} wm_base={} seat={} decoration_mgr={} activation={}",
                self.compositor,
                self.shm,
                self.wm_base,
                self.seat,
                self.decoration_mgr,
                self.activation,
            );
        }
        if self.compositor == 0 || self.shm == 0 || self.wm_base == 0 {
            return Err(io::Error::other(
                "compositor is missing wl_compositor, wl_shm, or xdg_wm_base",
            ));
        }
        Ok(())
    }

    /// A display.sync round trip: everything the compositor had queued before
    /// the callback has been processed once it fires.
    fn roundtrip(&mut self) -> io::Result<()> {
        let cb = self.new_id();
        self.send(WL_DISPLAY, req::DISPLAY_SYNC, &[Arg::NewId(cb)]);
        self.flush()?;
        loop {
            if !self.conn.fill()? {
                return Err(io::Error::other("compositor closed the connection"));
            }
            let mut done = false;
            while let Some(msg) = self.conn.next_message() {
                if msg.object == cb && msg.opcode == evt::CALLBACK_DONE {
                    done = true;
                } else {
                    self.handle(msg)?;
                }
            }
            self.flush()?;
            if done {
                return Ok(());
            }
            let mut fds = [PollFd::readable(self.conn.fd())];
            poll(&mut fds, Some(Duration::from_millis(500)))?;
        }
    }

    /// Create the surface and its xdg-shell roles, then commit to be configured.
    fn create_window(&mut self) -> io::Result<()> {
        self.surface = self.new_id();
        let (compositor, surface) = (self.compositor, self.surface);
        self.send(
            compositor,
            req::COMPOSITOR_CREATE_SURFACE,
            &[Arg::NewId(surface)],
        );

        self.xdg_surface = self.new_id();
        let (wm_base, xdg_surface) = (self.wm_base, self.xdg_surface);
        self.send(
            wm_base,
            req::XDG_WM_BASE_GET_XDG_SURFACE,
            &[Arg::NewId(xdg_surface), Arg::Object(surface)],
        );

        self.xdg_toplevel = self.new_id();
        let toplevel = self.xdg_toplevel;
        self.send(
            xdg_surface,
            req::XDG_SURFACE_GET_TOPLEVEL,
            &[Arg::NewId(toplevel)],
        );
        self.send(
            toplevel,
            req::XDG_TOPLEVEL_SET_TITLE,
            &[Arg::Str("BNK Sound")],
        );
        self.send(toplevel, req::XDG_TOPLEVEL_SET_APP_ID, &[Arg::Str(APP_ID)]);
        // Never shrink below one full column, which would cut the sliders off.
        let (min_w, min_h) = layout::minimum_size();
        self.send(
            toplevel,
            req::XDG_TOPLEVEL_SET_MIN_SIZE,
            &[Arg::Int(min_w), Arg::Int(min_h)],
        );

        // Compositor-drawn chrome unless the user asked for ours, and with no
        // manager at all there is nothing to negotiate.
        let want_client = self.shell.ui.settings.decorations == Decorations::Client;
        if want_client {
            self.shell.ui.chrome = Chrome::Client;
        }
        if self.decoration_mgr != 0 && !want_client {
            self.decoration = self.new_id();
            let (mgr, deco) = (self.decoration_mgr, self.decoration);
            self.send(
                mgr,
                req::DECORATION_MANAGER_GET_TOPLEVEL,
                &[Arg::NewId(deco), Arg::Object(toplevel)],
            );
            self.send(
                deco,
                req::DECORATION_SET_MODE,
                &[Arg::Uint(DECORATION_MODE_SERVER_SIDE)],
            );
        }

        // HiDPI needs both halves: the fractional scale tells us how many device
        // pixels a logical one is worth, and the viewport maps the buffer we
        // paint at that scale back onto the logical window size.
        if self.fractional_mgr != 0 {
            self.fractional = self.new_id();
            let (mgr, obj) = (self.fractional_mgr, self.fractional);
            self.send(
                mgr,
                req::FRACTIONAL_SCALE_MANAGER_GET_SCALE,
                &[Arg::NewId(obj), Arg::Object(surface)],
            );
        }
        if self.viewporter != 0 {
            self.viewport = self.new_id();
            let (mgr, obj) = (self.viewporter, self.viewport);
            self.send(
                mgr,
                req::VIEWPORTER_GET_VIEWPORT,
                &[Arg::NewId(obj), Arg::Object(surface)],
            );
        }

        // Restore the maximized state the window was last closed in.
        if self.shell.runtime.state().geometry.maximized {
            self.send(toplevel, req::XDG_TOPLEVEL_SET_MAXIMIZED, &[]);
        }

        // An empty commit asks for the first configure.
        self.send(surface, req::SURFACE_COMMIT, &[]);
        self.flush()?;
        Ok(())
    }

    /// Dispatch one Wayland event.
    fn handle(&mut self, msg: Message) -> io::Result<()> {
        let mut r = msg.reader();
        match (msg.object, msg.opcode) {
            (WL_DISPLAY, evt::DISPLAY_ERROR) => {
                let obj = r.u32().unwrap_or(0);
                let code = r.u32().unwrap_or(0);
                let text = r.string().unwrap_or_default();
                return Err(io::Error::other(format!(
                    "wayland error on object {obj} (code {code}): {text}"
                )));
            }
            (WL_DISPLAY, evt::DISPLAY_DELETE_ID) => {}
            (_, evt::REGISTRY_GLOBAL) if msg.object == self.registry => {
                let name = r.u32().unwrap_or(0);
                let interface = r.string().unwrap_or_default();
                let version = r.u32().unwrap_or(1);
                self.bind_global(name, interface, version);
            }
            (_, evt::REGISTRY_GLOBAL_REMOVE) if msg.object == self.registry => {}
            (_, evt::XDG_WM_BASE_PING) if msg.object == self.wm_base => {
                let serial = r.u32().unwrap_or(0);
                let wm = self.wm_base;
                self.send(wm, req::XDG_WM_BASE_PONG, &[Arg::Uint(serial)]);
            }
            (_, evt::XDG_SURFACE_CONFIGURE) if msg.object == self.xdg_surface => {
                let serial = r.u32().unwrap_or(0);
                let xdg = self.xdg_surface;
                self.send(xdg, req::XDG_SURFACE_ACK_CONFIGURE, &[Arg::Uint(serial)]);
                // Declare which part of the surface is the window. Nothing is
                // drawn outside it, so it is the whole surface, but a
                // compositor with no geometry to go on is left to invent a size
                // for the window rather than honour the one it has.
                self.set_window_geometry();
                let first = !self.configured;
                self.configured = true;
                self.shell.ui.dirty.mark_full();
                // The surface only exists to be activated once it is
                // configured, and the token is good for one use.
                if first {
                    let token = std::mem::take(&mut self.startup_token);
                    self.raise(&token);
                }
            }
            (_, evt::XDG_TOPLEVEL_CONFIGURE) if msg.object == self.xdg_toplevel => {
                let w = r.i32().unwrap_or(0);
                let h = r.i32().unwrap_or(0);
                // The states trail the size as an array of u32 enum values.
                let raw_states = r.array().unwrap_or_default();
                let states = toplevel_states(raw_states);
                if debug_enabled() {
                    let listed: Vec<u32> = raw_states
                        .chunks_exact(4)
                        .filter_map(|c| c.try_into().ok().map(u32::from_ne_bytes))
                        .collect();
                    eprintln!(
                        "{:>6}ms toplevel configure: {w}x{h} states={listed:?} \
                         (window {}x{}, scale {}, keeping size: {})",
                        self.started.elapsed().as_millis(),
                        self.width,
                        self.height,
                        self.scale,
                        !states.compositor_sized(),
                    );
                }
                // A compositor that resizes past the declared minimum gets the
                // minimum back: the window keeps its own floor rather than
                // handing the user a column cut off at the knees.
                let (w, h) = if w > 0 && h > 0 {
                    layout::at_least_minimum(w, h)
                } else {
                    (w, h)
                };
                if w > 0 && h > 0 && (w, h) != (self.width, self.height) {
                    self.width = w;
                    self.height = h;
                    self.shell.ui.dirty.mark_full();
                }
                if states.maximized != self.shell.ui.maximized {
                    self.shell.ui.maximized = states.maximized;
                    self.shell.ui.dirty.mark_full();
                }
                // Only an ordinary window's size is worth keeping. Maximized,
                // fullscreen, and tiled sizes all belong to the compositor's
                // arrangement, and restoring into one of them on the next
                // launch would leave the window a shape the user never chose.
                if !states.compositor_sized() && w > 0 && h > 0 {
                    self.normal_size = (w, h);
                }
                // Tell the core the window moved, so the save tick carries the
                // new size. Waiting for shutdown would lose it to anything that
                // is not a clean exit.
                self.push_geometry();
            }
            (_, evt::FRACTIONAL_PREFERRED_SCALE)
                if self.fractional != 0 && msg.object == self.fractional =>
            {
                let scale = r.u32().unwrap_or(120) as f32 / FRACTIONAL_SCALE_DENOM;
                if debug_enabled() {
                    eprintln!(
                        "{:>6}ms preferred scale: {scale} (viewport {})",
                        self.started.elapsed().as_millis(),
                        self.viewport,
                    );
                }
                // Without a viewport there is nothing to map the scaled buffer
                // back onto the logical window, so stay at 1.
                let scale = if self.viewport == 0 { 1.0 } else { scale };
                if scale > 0.0 && (scale - self.scale).abs() > f32::EPSILON {
                    self.scale = scale;
                    // The buffers hold device pixels, so ensure_buffers will see
                    // a new size and rebuild them.
                    self.shell.ui.dirty.mark_full();
                }
            }
            (_, evt::XDG_TOPLEVEL_CLOSE) if msg.object == self.xdg_toplevel => {
                self.closed = true;
            }
            (_, evt::ACTIVATION_TOKEN_DONE)
                if self.activation_token != 0 && msg.object == self.activation_token =>
            {
                let token = r.string().unwrap_or_default();
                let obj = self.activation_token;
                self.send(obj, req::ACTIVATION_TOKEN_DESTROY, &[]);
                self.activation_token = 0;
                if debug_enabled() {
                    eprintln!(
                        "{:>6}ms activation token: {token:?}",
                        self.started.elapsed().as_millis(),
                    );
                }
                if !token.is_empty() {
                    self.activate(token);
                }
            }
            // Guarded on our own buffer ids: almost every event here is opcode
            // 0, so an unguarded arm would swallow the others.
            (_, evt::BUFFER_RELEASE) if self.buffers.iter().any(|b| b.obj == msg.object) => {
                for slot in &mut self.buffers {
                    if slot.obj == msg.object {
                        slot.busy = false;
                    }
                }
            }
            (_, evt::DECORATION_CONFIGURE)
                if self.decoration != 0 && msg.object == self.decoration =>
            {
                // A compositor may decline server-side chrome, in which case the
                // window has to draw its own or have none at all.
                let mode = r.u32().unwrap_or(0);
                let chrome = if mode == DECORATION_MODE_SERVER_SIDE {
                    Chrome::Server
                } else {
                    Chrome::Client
                };
                if chrome != self.shell.ui.chrome {
                    self.shell.ui.chrome = chrome;
                    self.shell.ui.dirty.mark_full();
                }
                if debug_enabled() {
                    eprintln!(
                        "{:>6}ms decoration configure: mode={mode} (2=server, 1=client)",
                        self.started.elapsed().as_millis(),
                    );
                }
            }
            (_, evt::SEAT_CAPABILITIES) if msg.object == self.seat => {
                let caps = r.u32().unwrap_or(0);
                self.bind_seat(caps);
            }
            (_, evt::KEYBOARD_KEYMAP) if msg.object == self.keyboard => {
                let _format = r.u32();
                // The fd rides as ancillary data (no bytes in the body); the
                // size that follows it is what the mapping needs.
                let size = r.u32().unwrap_or(0);
                if let Some(fd) = self.conn.take_fd() {
                    match Keyboard::from_keymap_fd(fd, size) {
                        Ok(kb) => {
                            if std::env::var_os("BNKSOUND_DEBUG").is_some() {
                                eprintln!("keymap loaded ({size} bytes)");
                            }
                            self.xkb = Some(kb);
                        }
                        Err(e) => eprintln!("bnksound: keymap: {e}"),
                    }
                }
            }
            (_, evt::KEYBOARD_MODIFIERS) if msg.object == self.keyboard => {
                let _serial = r.u32();
                let depressed = r.u32().unwrap_or(0);
                let latched = r.u32().unwrap_or(0);
                let locked = r.u32().unwrap_or(0);
                let group = r.u32().unwrap_or(0);
                if let Some(kb) = &self.xkb {
                    kb.update_mask(depressed, latched, locked, group);
                }
            }
            (_, evt::DATA_DEVICE_SELECTION) if msg.object == self.data_device => {
                // A null offer means the clipboard holds nothing readable.
                let offer = r.u32().unwrap_or(0);
                if self.selection_offer != 0 && self.selection_offer != offer {
                    let old = self.selection_offer;
                    self.send(old, req::DATA_OFFER_DESTROY, &[]);
                }
                self.selection_offer = offer;
            }
            (_, evt::DATA_SOURCE_SEND)
                if self.data_source != 0 && msg.object == self.data_source =>
            {
                let _mime = r.string();
                if let Some(fd) = self.conn.take_fd() {
                    clipboard::write_selection(fd, &self.clipboard_text);
                }
            }
            (_, evt::DATA_SOURCE_CANCELLED)
                if self.data_source != 0 && msg.object == self.data_source =>
            {
                let src = self.data_source;
                self.send(src, req::DATA_SOURCE_DESTROY, &[]);
                self.data_source = 0;
            }
            (_, evt::KEYBOARD_REPEAT_INFO) if msg.object == self.keyboard => {
                let rate = r.i32().unwrap_or(0);
                let delay = r.i32().unwrap_or(600);
                // A rate of zero disables repeat entirely.
                self.repeat_period = if rate > 0 {
                    Duration::from_micros(1_000_000 / rate as u64)
                } else {
                    Duration::ZERO
                };
                self.repeat_delay = Duration::from_millis(delay.max(0) as u64);
            }
            (_, evt::KEYBOARD_LEAVE) if msg.object == self.keyboard => {
                // Focus left mid-press; drop the held key so it cannot stick.
                self.held_key = None;
            }
            (_, evt::KEYBOARD_KEY) if msg.object == self.keyboard => {
                self.last_serial = r.u32().unwrap_or(self.last_serial);
                let _time = r.u32();
                let code = r.u32().unwrap_or(0);
                let state = r.u32().unwrap_or(0);
                if std::env::var_os("BNKSOUND_DEBUG").is_some() {
                    eprintln!("key: code={code} state={state} xkb={}", self.xkb.is_some());
                }
                if state == 1 {
                    self.key_press(code);
                    // Arm repeat for keys the layout says repeat.
                    let repeats = self.xkb.as_ref().is_some_and(|kb| kb.repeats(code));
                    if repeats && !self.repeat_period.is_zero() {
                        self.held_key = Some((code, Instant::now() + self.repeat_delay));
                    }
                } else if self.held_key.is_some_and(|(held, _)| held == code) {
                    self.held_key = None;
                }
            }
            (_, evt::POINTER_MOTION) if msg.object == self.pointer => {
                let _time = r.u32();
                self.ptr_x = r.fixed().unwrap_or(0.0);
                self.ptr_y = r.fixed().unwrap_or(0.0);
                self.pointer_event(PointerAction::Motion);
            }
            (_, evt::POINTER_ENTER) if msg.object == self.pointer => {
                self.pointer_serial = r.u32().unwrap_or(0);
                // A client owns its cursor from the moment the pointer enters.
                self.cursor_shape = 0;
                self.pointer_inside = true;
                let _surface = r.u32();
                self.ptr_x = r.fixed().unwrap_or(0.0);
                self.ptr_y = r.fixed().unwrap_or(0.0);
                self.pointer_event(PointerAction::Motion);
            }
            (_, evt::POINTER_LEAVE) if msg.object == self.pointer => {
                // Park the pointer outside the window so the hover, the knob's
                // ring, and anything else keyed off it clear through the same
                // path a motion into empty space takes. The cursor is the
                // compositor's again from here, so nothing is set on it.
                self.pointer_inside = false;
                self.ptr_x = -1.0;
                self.ptr_y = -1.0;
                self.pointer_event(PointerAction::Motion);
            }
            (_, evt::POINTER_BUTTON) if msg.object == self.pointer => {
                // Moving, resizing, and the clipboard all have to quote a recent
                // input serial, so track the freshest one from either device.
                self.last_serial = r.u32().unwrap_or(self.last_serial);
                let _time = r.u32();
                let button = r.u32().unwrap_or(0);
                let state = r.u32().unwrap_or(0);
                let b = match button {
                    BTN_RIGHT => MouseButton::Right,
                    BTN_MIDDLE => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                let action = if state == BUTTON_PRESSED {
                    PointerAction::Press(b)
                } else {
                    PointerAction::Release(b)
                };
                self.pointer_event(action);
            }
            (_, evt::POINTER_AXIS) if msg.object == self.pointer => {
                let _time = r.u32();
                let axis = r.u32().unwrap_or(0);
                let value = r.fixed().unwrap_or(0.0) as f32;
                // Axis 0 is vertical; the strip scrolls horizontally from it.
                let (dx, dy) = if axis == 0 {
                    (0.0, value)
                } else {
                    (value, 0.0)
                };
                self.pointer_event(PointerAction::Scroll { dx, dy });
            }
            _ => {}
        }
        Ok(())
    }

    fn bind_global(&mut self, name: u32, interface: &str, version: u32) {
        let (slot, want) = match interface {
            "wl_compositor" => (&mut self.compositor, COMPOSITOR_VERSION),
            "wl_shm" => (&mut self.shm, SHM_VERSION),
            "xdg_wm_base" => (&mut self.wm_base, WM_BASE_VERSION),
            "wl_seat" => (&mut self.seat, SEAT_VERSION),
            "zxdg_decoration_manager_v1" => (&mut self.decoration_mgr, 1),
            "xdg_activation_v1" => (&mut self.activation, ACTIVATION_VERSION),
            "wl_data_device_manager" => (&mut self.data_device_mgr, 3),
            "wp_cursor_shape_manager_v1" => (&mut self.cursor_mgr, 1),
            "wp_fractional_scale_manager_v1" => (&mut self.fractional_mgr, 1),
            "wp_viewporter" => (&mut self.viewporter, 1),
            _ => return,
        };
        if *slot != 0 {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        *slot = id;
        let v = want.min(version);
        let registry = self.registry;
        encode(
            self.conn.out(),
            registry,
            req::REGISTRY_BIND,
            &[
                Arg::Uint(name),
                Arg::Bind {
                    interface,
                    version: v,
                    new_id: id,
                },
            ],
        );
    }

    fn bind_seat(&mut self, caps: u32) {
        if std::env::var_os("BNKSOUND_DEBUG").is_some() {
            eprintln!("seat capabilities: {caps:#x} (1=pointer, 2=keyboard)");
        }
        if caps & SEAT_CAP_POINTER != 0 && self.pointer == 0 {
            self.pointer = self.new_id();
            let (seat, ptr) = (self.seat, self.pointer);
            self.send(seat, req::SEAT_GET_POINTER, &[Arg::NewId(ptr)]);
            if self.cursor_mgr != 0 {
                self.cursor_device = self.new_id();
                let (mgr, dev) = (self.cursor_mgr, self.cursor_device);
                self.send(
                    mgr,
                    req::CURSOR_SHAPE_MANAGER_GET_POINTER,
                    &[Arg::NewId(dev), Arg::Object(ptr)],
                );
            }
        }
        if self.data_device_mgr != 0 && self.data_device == 0 {
            self.data_device = self.new_id();
            let (mgr, dev, seat) = (self.data_device_mgr, self.data_device, self.seat);
            self.send(
                mgr,
                req::DATA_DEVICE_MANAGER_GET_DEVICE,
                &[Arg::NewId(dev), Arg::Object(seat)],
            );
        }
        if caps & SEAT_CAP_KEYBOARD != 0 && self.keyboard == 0 {
            self.keyboard = self.new_id();
            let (seat, kb) = (self.seat, self.keyboard);
            self.send(seat, req::SEAT_GET_KEYBOARD, &[Arg::NewId(kb)]);
        }
    }

    /// Project the current frame's geometry, in logical pixels.
    fn layout(&self) -> layout::Layout {
        let window = Rect::new(0, 0, self.width, self.height);
        layout::project(&self.shell.snapshot, &self.shell.ui, window)
    }

    /// The buffer size in device pixels for the current window and scale.
    fn device_size(&self) -> (i32, i32) {
        let px = |v: i32| ((v as f32 * self.scale).round() as i32).max(1);
        (px(self.width), px(self.height))
    }

    /// Feed a pointer event through the shared input mapping.
    fn pointer_event(&mut self, action: PointerAction) {
        let layout = self.layout();
        let event = PointerEvent {
            x: self.ptr_x,
            y: self.ptr_y,
            action,
        };
        // Moving, resizing, and closing the window belong to the compositor, so
        // those presses never reach the mixer's input mapping.
        if action == PointerAction::Press(MouseButton::Left) {
            let want = layout
                .hit(self.ptr_x as i32, self.ptr_y as i32)
                .and_then(input::window_action);
            if let Some(want) = want {
                self.window_action(want);
                return;
            }
        }
        let ms = self.started.elapsed().as_millis() as u64;
        let msgs = input::on_pointer(
            &mut self.shell.ui,
            &layout,
            &self.shell.snapshot,
            event,
            ms,
            &self.font,
        );
        self.apply_cursor();
        self.shell.dispatch(msgs);
    }

    /// Hand a window-management request to the compositor.
    fn window_action(&mut self, action: WindowAction) {
        let (toplevel, seat, serial) = (self.xdg_toplevel, self.seat, self.last_serial);
        match action {
            WindowAction::Move => self.send(
                toplevel,
                req::XDG_TOPLEVEL_MOVE,
                &[Arg::Object(seat), Arg::Uint(serial)],
            ),
            WindowAction::Resize(edge) => self.send(
                toplevel,
                req::XDG_TOPLEVEL_RESIZE,
                &[
                    Arg::Object(seat),
                    Arg::Uint(serial),
                    Arg::Uint(resize_edge(edge)),
                ],
            ),
            WindowAction::Minimize => self.send(toplevel, req::XDG_TOPLEVEL_SET_MINIMIZED, &[]),
            WindowAction::ToggleMaximize => {
                let op = if self.shell.ui.maximized {
                    req::XDG_TOPLEVEL_UNSET_MAXIMIZED
                } else {
                    req::XDG_TOPLEVEL_SET_MAXIMIZED
                };
                self.send(toplevel, op, &[]);
            }
            WindowAction::Close => self.closed = true,
        }
        let _ = self.flush();
    }

    /// The next launch waiting on the lock socket, and the activation token it
    /// handed over. None once none is left waiting.
    fn handed_over(&self) -> Option<String> {
        self.instance.as_ref()?.accept()
    }

    /// Bring the window forward for a launch that handed itself over.
    ///
    /// A launcher gives the process it starts an activation token, and that
    /// token is what tells the compositor the raise was asked for rather than
    /// stolen. Started from a shell there is none, so we ask the compositor for
    /// one of our own and raise on the answer. It may decline, since no input
    /// event of ours is behind the request, in which case the window is usually
    /// flagged as wanting attention instead.
    fn raise(&mut self, token: &str) {
        if debug_enabled() {
            eprintln!(
                "{:>6}ms launch handed over: token={token:?} (activation={})",
                self.started.elapsed().as_millis(),
                self.activation,
            );
        }
        // Without the activation global there is no way to ask for focus, so
        // the window stays where it is and only the second one is spared.
        if self.activation == 0 {
            return;
        }
        if !token.is_empty() {
            self.activate(token);
            return;
        }
        // One request in flight is enough; its done event does the raising.
        if self.activation_token != 0 {
            return;
        }
        self.activation_token = self.new_id();
        let (act, obj, surface) = (self.activation, self.activation_token, self.surface);
        self.send(act, req::ACTIVATION_GET_TOKEN, &[Arg::NewId(obj)]);
        self.send(obj, req::ACTIVATION_TOKEN_SET_APP_ID, &[Arg::Str(APP_ID)]);
        self.send(
            obj,
            req::ACTIVATION_TOKEN_SET_SURFACE,
            &[Arg::Object(surface)],
        );
        self.send(obj, req::ACTIVATION_TOKEN_COMMIT, &[]);
        let _ = self.flush();
    }

    /// Hand `token` to the compositor as the reason to activate our surface.
    fn activate(&mut self, token: &str) {
        let (act, surface) = (self.activation, self.surface);
        self.send(
            act,
            req::ACTIVATION_ACTIVATE,
            &[Arg::Str(token), Arg::Object(surface)],
        );
        let _ = self.flush();
    }

    /// Write the last presented frame to a PNG.
    fn screenshot(&mut self) {
        // The buffers' own size, not the size the current scale asks for: a
        // scale change that has not been painted yet leaves the two apart, and
        // the pool still holds the older frame.
        let (w, h) = self.buffer_dims;
        if w <= 0 || h <= 0 {
            return;
        }
        let frame_px = (w * h) as usize;
        let start = self.last_painted * frame_px;
        let Some(pool) = self.pool.as_mut() else {
            return;
        };
        let Some(pixels) = pool.pixels().get(start..start + frame_px) else {
            return;
        };
        screenshot::capture(pixels, w as u32, h as u32);
    }

    /// Decode a key press through xkb and feed the shared input mapping.
    fn key_press(&mut self, evdev_code: u32) {
        let Some(kb) = &self.xkb else {
            return;
        };
        // A named key, or else whatever character the keycode types.
        let key = Key::from_keysym(kb.keysym(evdev_code))
            .or_else(|| kb.character(evdev_code).map(Key::Char));
        let Some(key) = key else {
            return;
        };
        let mods = Modifiers {
            ctrl: kb.ctrl_active(),
            shift: kb.shift_active(),
            alt: kb.alt_active(),
        };
        let event = KeyEvent { key, mods };
        if input::is_screenshot_key(event) {
            self.screenshot();
            return;
        }
        if let Some(action) = input::clipboard_action(&self.shell.ui, event) {
            self.clipboard(action);
            return;
        }
        if std::env::var_os("BNKSOUND_DEBUG").is_some() {
            eprintln!("  -> key={key:?} mods={mods:?}");
        }
        let msgs = input::on_key(&mut self.shell.ui, &self.shell.snapshot, event);
        self.shell.dispatch(msgs);
    }

    /// Copy, cut, or paste for the focused editor.
    fn clipboard(&mut self, action: ClipboardAction) {
        match action {
            ClipboardAction::Copy | ClipboardAction::Cut => {
                let Some(text) = self.shell.ui.editor.selected_text() else {
                    return;
                };
                self.offer_selection(text);
                if action == ClipboardAction::Cut {
                    self.shell.ui.editor.delete_selection();
                    if let Some(m) = input::editor_text_message(&self.shell.ui) {
                        self.shell.dispatch([m]);
                    }
                }
            }
            ClipboardAction::Paste => {
                let Some(text) = self.read_selection() else {
                    return;
                };
                if self.shell.ui.editor.paste(&text)
                    && let Some(m) = input::editor_text_message(&self.shell.ui)
                {
                    self.shell.dispatch([m]);
                }
            }
        }
        self.shell.ui.dirty.mark_full();
    }

    /// Publish `text` as the selection, replacing any source we already own.
    fn offer_selection(&mut self, text: String) {
        if self.data_device == 0 || self.data_device_mgr == 0 {
            return;
        }
        if self.data_source != 0 {
            let old = self.data_source;
            self.send(old, req::DATA_SOURCE_DESTROY, &[]);
        }
        self.clipboard_text = text;
        self.data_source = self.new_id();
        let (mgr, src, dev, serial) = (
            self.data_device_mgr,
            self.data_source,
            self.data_device,
            self.last_serial,
        );
        self.send(
            mgr,
            req::DATA_DEVICE_MANAGER_CREATE_SOURCE,
            &[Arg::NewId(src)],
        );
        self.send(src, req::DATA_SOURCE_OFFER, &[Arg::Str(MIME_UTF8)]);
        self.send(
            dev,
            req::DATA_DEVICE_SET_SELECTION,
            &[Arg::Object(src), Arg::Uint(serial)],
        );
        let _ = self.flush();
    }

    /// Read the current selection as text, if the clipboard holds any.
    fn read_selection(&mut self) -> Option<String> {
        // Our own source answers a read with DATA_SOURCE_SEND, which arrives on
        // the loop this call is blocking. Waiting for it would deadlock until
        // the read gives up, so serve the text we already hold.
        if self.data_source != 0 {
            return Some(self.clipboard_text.clone());
        }
        if self.selection_offer == 0 {
            return None;
        }
        let (read_fd, write_fd) = clipboard::pipe().ok()?;
        let offer = self.selection_offer;
        encode(
            self.conn.out(),
            offer,
            req::DATA_OFFER_RECEIVE,
            &[Arg::Str(MIME_UTF8)],
        );
        // The request carries the write end as ancillary data, so it flushes
        // alone; our copy then closes so the read sees EOF when the source ends.
        self.conn.flush(Some(write_fd.as_raw_fd())).ok()?;
        drop(write_fd);
        clipboard::read_selection(read_fd, Duration::from_millis(200)).ok()
    }

    /// Keep the cursor in step with what the pointer is over.
    fn apply_cursor(&mut self) {
        // A client may only shape the cursor while the pointer is over it.
        if self.cursor_device == 0 || !self.pointer_inside {
            return;
        }
        use crate::ui::layout::HitTarget;
        let want = match (&self.shell.ui.drag, &self.shell.ui.hover) {
            // A fader being dragged keeps the closed hand wherever the pointer
            // wanders, since the grab holds until the button comes up.
            (Some(Drag::Slider(_)), _) => cursor::GRABBING,
            // An open hand says the knob is there to be picked up.
            (_, Some(HitTarget::RowSlider(_))) => cursor::GRAB,
            (_, Some(HitTarget::PaletteInput | HitTarget::ModalInput)) => cursor::TEXT,
            (_, Some(HitTarget::ResizeEdge(edge))) => resize_cursor(*edge),
            // Chrome that is not a button leaves the cursor alone.
            (_, Some(HitTarget::TitlebarDrag | HitTarget::Backdrop)) => cursor::DEFAULT,
            (_, Some(_)) => cursor::POINTER,
            (_, None) => cursor::DEFAULT,
        };
        if want == self.cursor_shape {
            return;
        }
        self.cursor_shape = want;
        let (dev, serial) = (self.cursor_device, self.pointer_serial);
        self.send(
            dev,
            req::CURSOR_SHAPE_DEVICE_SET_SHAPE,
            &[Arg::Uint(serial), Arg::Uint(want)],
        );
    }

    /// Ensure a pool and two buffers matching the current device size.
    fn ensure_buffers(&mut self) -> io::Result<()> {
        let (dw, dh) = self.device_size();
        if self.buffers[0].obj != 0 && self.buffer_dims == (dw, dh) {
            return Ok(());
        }
        // Drop the old objects before remapping.
        for i in 0..self.buffers.len() {
            let obj = self.buffers[i].obj;
            if obj != 0 {
                self.send(obj, req::BUFFER_DESTROY, &[]);
                self.buffers[i] = BufferSlot::default();
            }
        }
        if self.pool_obj != 0 {
            let p = self.pool_obj;
            self.send(p, req::SHM_POOL_DESTROY, &[]);
            self.pool_obj = 0;
        }
        self.flush()?;

        let stride = dw * 4;
        let frame = (stride * dh) as usize;
        let pool = ShmPool::new(frame * 2)?;

        // create_pool carries its fd as ancillary data, so it is flushed alone.
        self.pool_obj = self.new_id();
        let (shm, pool_obj) = (self.shm, self.pool_obj);
        encode(
            self.conn.out(),
            shm,
            req::SHM_CREATE_POOL,
            &[Arg::NewId(pool_obj), Arg::Int((frame * 2) as i32)],
        );
        let fd = pool.fd();
        self.conn.flush(Some(fd))?;

        for i in 0..2 {
            let obj = self.new_id();
            self.send(
                pool_obj,
                req::SHM_POOL_CREATE_BUFFER,
                &[
                    Arg::NewId(obj),
                    Arg::Int((frame * i) as i32),
                    Arg::Int(dw),
                    Arg::Int(dh),
                    Arg::Int(stride),
                    Arg::Uint(SHM_FORMAT_ARGB8888),
                ],
            );
            self.buffers[i] = BufferSlot { obj, busy: false };
        }
        self.pool = Some(pool);
        self.buffer_dims = (dw, dh);

        // The viewport maps the scaled buffer back onto the logical window, so
        // the compositor lays the window out at the size the mixer was laid out
        // for whatever the scale is.
        if debug_enabled() {
            eprintln!(
                "{:>6}ms buffers rebuilt: {dw}x{dh} device for {}x{} logical",
                self.started.elapsed().as_millis(),
                self.width,
                self.height,
            );
        }
        if self.viewport != 0 {
            let (vp, w, h) = (self.viewport, self.width, self.height);
            self.send(
                vp,
                req::VIEWPORT_SET_DESTINATION,
                &[Arg::Int(w), Arg::Int(h)],
            );
        }
        self.flush()
    }

    /// Paint the frame into a free buffer and present it.
    fn present(&mut self) -> io::Result<()> {
        if !self.configured || self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        self.ensure_buffers()?;
        // Both buffers still held by the compositor: skip this turn rather than
        // draw over memory it is sampling. The next release wakes us.
        let Some(slot) = self.buffers.iter().position(|b| !b.busy) else {
            return Ok(());
        };

        let layout = self.layout();
        let (dw, dh) = self.device_size();
        let scale = self.scale;
        let frame_px = (dw * dh) as usize;
        {
            let Some(pool) = self.pool.as_mut() else {
                return Ok(());
            };
            let start = slot * frame_px;
            let pixels = &mut pool.pixels()[start..start + frame_px];
            let mut painter = Painter::scaled(pixels, dw as u32, dh as u32, scale);
            paint_frame(
                &mut painter,
                &self.shell.snapshot,
                &self.shell.ui,
                &layout,
                &self.font,
                &self.palette,
                &mut self.icons,
            );
        }

        let (surface, buffer) = (self.surface, self.buffers[slot].obj);
        self.send(
            surface,
            req::SURFACE_ATTACH,
            &[Arg::Object(buffer), Arg::Int(0), Arg::Int(0)],
        );
        self.send(
            surface,
            req::SURFACE_DAMAGE,
            &[
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(self.width),
                Arg::Int(self.height),
            ],
        );
        self.send(surface, req::SURFACE_COMMIT, &[]);
        self.buffers[slot].busy = true;
        self.last_painted = slot;
        self.shell.ui.dirty.clear();
        self.flush()
    }

    /// One loop turn: wait for the socket, the buses, or the meter deadline.
    pub fn tick(&mut self) -> io::Result<()> {
        let mut fds = [
            PollFd::readable(self.conn.fd()),
            PollFd::readable(self.msg_rx.wake_fd()),
            PollFd::readable(self.evt_rx.wake_fd()),
            // poll ignores a negative fd, which covers a session where the
            // one-window lock could not be taken.
            PollFd::readable(self.instance.as_ref().map_or(-1, Listener::fd)),
        ];
        // Wake for whichever comes first: the meter tick, the save tick, the
        // next key repeat, or the caret's next flip.
        let now = Instant::now();
        // Wake when the meters are next due rather than a flat interval later,
        // so a turn spent on input does not push their step out.
        let mut timeout = self.meter_deadline.saturating_duration_since(now);
        timeout = timeout.min(self.autosave_deadline.saturating_duration_since(now));
        if let Some((_, next)) = self.held_key {
            timeout = timeout.min(next.saturating_duration_since(now));
        }
        if self.shell.ui.overlay_focused() {
            timeout = timeout.min(self.caret_deadline.saturating_duration_since(now));
        }
        poll(&mut fds, Some(timeout))?;

        // Persist anything that changed since the last tick. Without this the
        // only save is at shutdown, so a kill or a crash would drop the whole
        // session's edits.
        if Instant::now() >= self.autosave_deadline {
            self.shell.tick_autosave();
            self.autosave_deadline = Instant::now() + AUTOSAVE_INTERVAL;
        }

        // Blink the caret. Off-focus this settles it visible and then costs
        // nothing until a field takes focus again.
        if !self.shell.ui.overlay_focused() || Instant::now() >= self.caret_deadline {
            if self.shell.tick_caret() {
                self.shell.ui.dirty.mark_full();
            }
            self.caret_deadline = Instant::now() + CARET_BLINK;
        }

        // Emit any due key repeats before the rest of the turn.
        while let Some((code, next)) = self.held_key {
            if Instant::now() < next {
                break;
            }
            self.key_press(code);
            self.held_key = Some((code, next + self.repeat_period));
        }

        if fds[0].is_ready() {
            if !self.conn.fill()? {
                self.closed = true;
                return Ok(());
            }
            while let Some(msg) = self.conn.next_message() {
                self.handle(msg)?;
            }
        }

        // Later launches, which hand over whatever their launcher told them and
        // leave the window to us.
        if fds[3].is_ready() {
            while let Some(token) = self.handed_over() {
                self.raise(&token);
            }
        }

        // UI and worker messages.
        let mut batch = Vec::new();
        self.msg_rx.drain(|m| batch.push(m));
        self.shell.dispatch(batch);

        let mut worker = Vec::new();
        self.evt_rx.drain(|e| worker.push(e));
        self.shell.dispatch_worker(worker);

        // Meter animation: decay, then fold in the newest peaks. An idle window
        // whose bars are all at rest changes nothing and skips its repaint.
        //
        // The decay is a step per tick, so it has to run on its own clock. The
        // loop also wakes for input, and taking a step on those turns would let
        // the bars fall faster the more the pointer moved.
        if Instant::now() >= self.meter_deadline {
            self.meter_deadline = Instant::now() + PEAK_DECAY_INTERVAL;
            if self.shell.tick_meters() {
                self.shell.ui.dirty.mark_full();
            }
        }

        // The knob's ring eases in and out, so it keeps painting for as long as
        // it is still moving.
        if self.shell.tick_halo(Instant::now()) {
            self.shell.ui.dirty.mark_full();
        }

        if self.shell.ui.dirty.needs_paint() {
            self.present()?;
        }
        self.flush()
    }

    /// Tell the compositor which part of the surface is the window.
    ///
    /// The surface carries no shadow or decoration of its own, so the window is
    /// all of it. Sending it anyway is what tells the compositor the size the
    /// window means to be, which it otherwise has to guess at.
    fn set_window_geometry(&mut self) {
        let (xdg, w, h) = (self.xdg_surface, self.width, self.height);
        if xdg == 0 {
            return;
        }
        self.send(
            xdg,
            req::XDG_SURFACE_SET_WINDOW_GEOMETRY,
            &[Arg::Int(0), Arg::Int(0), Arg::Int(w), Arg::Int(h)],
        );
    }

    /// Hand the current window geometry to the core, which marks it for the
    /// next save if it actually changed.
    fn push_geometry(&mut self) {
        let (w, h) = self.normal_size;
        let _ = self.shell.runtime.dispatch(AppMessage::GeometryChanged {
            width: w.max(0) as u32,
            height: h.max(0) as u32,
            maximized: self.shell.ui.maximized,
        });
    }

    /// Persist geometry and flush a final save.
    pub fn shutdown(&mut self) {
        let (w, h) = self.normal_size;
        self.shell
            .shutdown(w.max(0) as u32, h.max(0) as u32, self.shell.ui.maximized);
    }
}

/// What a configure's state array says about the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct ToplevelStates {
    /// Maximized or fullscreen, which the maximize button and the resize edges
    /// both read.
    maximized: bool,
    /// Snapped against a screen edge by the compositor.
    tiled: bool,
}

impl ToplevelStates {
    /// Whether the compositor, rather than the user, chose this size. Such a
    /// size is never kept as the one to restore to.
    fn compositor_sized(self) -> bool {
        self.maximized || self.tiled
    }
}

/// Read a configure's trailing state array, which is a run of u32 enum values.
fn toplevel_states(bytes: &[u8]) -> ToplevelStates {
    let mut out = ToplevelStates::default();
    for state in bytes
        .chunks_exact(4)
        .filter_map(|c| c.try_into().ok().map(u32::from_ne_bytes))
    {
        match state {
            TOPLEVEL_STATE_MAXIMIZED | TOPLEVEL_STATE_FULLSCREEN => out.maximized = true,
            TOPLEVEL_STATE_TILED_LEFT
            | TOPLEVEL_STATE_TILED_RIGHT
            | TOPLEVEL_STATE_TILED_TOP
            | TOPLEVEL_STATE_TILED_BOTTOM => out.tiled = true,
            _ => {}
        }
    }
    out
}

/// Whether to narrate window sizing to stderr. Sizing is negotiated with the
/// compositor, so when a window comes up wrong the exchange is the only place
/// the reason shows.
fn debug_enabled() -> bool {
    std::env::var_os("BNKSOUND_DEBUG").is_some()
}

/// Map a layout edge onto the xdg_toplevel resize enum.
fn resize_edge(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::Top => resize::TOP,
        ResizeEdge::Bottom => resize::BOTTOM,
        ResizeEdge::Left => resize::LEFT,
        ResizeEdge::Right => resize::RIGHT,
        ResizeEdge::TopLeft => resize::TOP_LEFT,
        ResizeEdge::TopRight => resize::TOP_RIGHT,
        ResizeEdge::BottomLeft => resize::BOTTOM_LEFT,
        ResizeEdge::BottomRight => resize::BOTTOM_RIGHT,
    }
}

/// The cursor that says which way an edge drags.
fn resize_cursor(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => cursor::NS_RESIZE,
        ResizeEdge::Left | ResizeEdge::Right => cursor::EW_RESIZE,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => cursor::NWSE_RESIZE,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => cursor::NESW_RESIZE,
    }
}

/// Run the native shell until the compositor closes the window.
///
/// A launch that finds a window already up hands itself over to it and returns
/// before anything here is started, so the mixer runs one window per session.
pub fn run() -> io::Result<()> {
    let (instance, token) = match instance::claim() {
        Launch::Run { listener, token } => (listener, token),
        Launch::HandedOver => return Ok(()),
    };
    let mut app = App::new(instance, token)?;
    while !app.closed {
        app.tick()?;
    }
    app.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a configure's state array the way the compositor sends it.
    fn states(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    #[test]
    fn an_ordinary_window_owns_its_own_size() {
        let s = toplevel_states(&states(&[TOPLEVEL_STATE_ACTIVATED]));
        assert!(!s.maximized);
        assert!(!s.tiled);
        assert!(!s.compositor_sized(), "its size is the user's to keep");
    }

    #[test]
    fn maximized_and_fullscreen_sizes_belong_to_the_compositor() {
        for state in [TOPLEVEL_STATE_MAXIMIZED, TOPLEVEL_STATE_FULLSCREEN] {
            let s = toplevel_states(&states(&[state, TOPLEVEL_STATE_ACTIVATED]));
            assert!(s.maximized, "state {state} reads as maximized");
            assert!(s.compositor_sized());
        }
    }

    #[test]
    fn a_tiled_window_is_not_maximized_but_is_still_sized_for_us() {
        // Snapping a window to half the screen leaves it tiled against two
        // edges without ever maximizing it. Keeping that size would reopen the
        // window at the tile rather than at whatever the user had before.
        for edges in [
            vec![TOPLEVEL_STATE_TILED_LEFT],
            vec![TOPLEVEL_STATE_TILED_RIGHT],
            vec![TOPLEVEL_STATE_TILED_TOP],
            vec![TOPLEVEL_STATE_TILED_BOTTOM],
            vec![
                TOPLEVEL_STATE_TILED_LEFT,
                TOPLEVEL_STATE_TILED_TOP,
                TOPLEVEL_STATE_ACTIVATED,
            ],
        ] {
            let s = toplevel_states(&states(&edges));
            assert!(!s.maximized, "tiling is not maximizing: {edges:?}");
            assert!(s.tiled, "{edges:?} reads as tiled");
            assert!(
                s.compositor_sized(),
                "so the size is not kept as the one to restore",
            );
        }
    }

    #[test]
    fn an_empty_or_ragged_state_array_reads_as_ordinary() {
        assert!(!toplevel_states(&[]).compositor_sized());
        // A trailing partial value is ignored rather than misread.
        assert!(!toplevel_states(&[1, 0, 0]).compositor_sized());
        let mut ragged = states(&[TOPLEVEL_STATE_TILED_LEFT]);
        ragged.push(0);
        assert!(
            toplevel_states(&ragged).tiled,
            "the whole value still counts"
        );
    }

    #[test]
    fn unknown_states_are_ignored_rather_than_guessed_at() {
        // Later protocol versions add states; none of them should be taken to
        // mean the compositor sized the window.
        let s = toplevel_states(&states(&[3, 9, 99]));
        assert!(!s.compositor_sized());
    }
}
