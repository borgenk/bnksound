//! The native Wayland application: bind the globals, map an xdg-shell toplevel,
//! present wl_shm frames painted by the shared renderer, and translate input.
//!
//! One poll loop waits on the Wayland socket, both bus wakeup fds, the
//! one-window lock, and the peak pool while the meters rest, with the soonest
//! deadline as its timeout. A window with nothing to show, save, or animate has
//! no deadline at all and sleeps until something arrives.

use std::collections::HashMap;
use std::fmt;
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
use crate::render::paint::{paint_frame, paint_meters};
use crate::render::primitives::{Painter, Rect};
use crate::render::screenshot;
use crate::render::text::Font;
use crate::runtime::Runtime;
use crate::settings::Decorations;
use crate::shell::{AUTOSAVE_DELAY, Shell};
use crate::state::Message as AppMessage;
use crate::ui::input::{
    self, ClipboardAction, Key, KeyEvent, Modifiers, MouseButton, PointerAction, PointerEvent,
    WindowAction,
};
use crate::ui::layout::{self, ResizeEdge};
use crate::ui::theme::Palette;
use crate::ui::{CARET_BLINK, Chrome, Drag, FitStep};

/// One wl_shm buffer and whether the compositor still holds it.
#[derive(Clone, Copy, Default)]
struct BufferSlot {
    obj: u32,
    busy: bool,
    /// How far this buffer's pixels have fallen behind the current frame.
    owed: Owed,
}

/// Charge both buffers with what changed this turn, and take what the one about
/// to be painted owes. Painting settles that buffer's debt and leaves the other
/// carrying it until its own turn comes round.
fn take_owed(buffers: &mut [BufferSlot; 2], slot: usize, change: Owed) -> Owed {
    for b in buffers.iter_mut() {
        b.owed = b.owed.max(change);
    }
    std::mem::take(&mut buffers[slot].owed)
}

/// What a buffer must be given before it can be presented.
///
/// Frames alternate between two buffers, so the one being painted holds the
/// frame before last, not the last one. A repaint that covers only what changed
/// since the last frame would leave everything that changed the frame before it
/// showing stale pixels. Each buffer therefore carries its own debt, and paying
/// it means painting the wider of what it owes and what this turn changed.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Owed {
    /// The buffer holds the last frame presented.
    #[default]
    Nothing,
    /// Only the meter rectangles have moved since it was painted.
    Meters,
    /// It is behind everywhere.
    All,
}

/// Running totals of what the exchange with the compositor produced. Read by
/// [`App::facts`] and nothing else.
#[derive(Clone, Copy, Default)]
struct Counts {
    /// Toplevel configures taken.
    configures: u32,
    /// Frames attached and committed.
    frames: u32,
    /// Frame callbacks the compositor answered.
    callbacks: u32,
    /// Activation tokens the compositor handed back.
    tokens: u32,
    /// Later launches that handed themselves over on the lock socket.
    handovers: u32,
}

/// Globals we bind, with the versions we speak.
const COMPOSITOR_VERSION: u32 = 4;
const SHM_VERSION: u32 = 1;
/// 4 is where xdg_toplevel.configure_bounds arrives, which is the only honest
/// source for how wide the window may ask to be.
const WM_BASE_VERSION: u32 = 4;

/// How long a launch keeps re-fitting to the stream list.
///
/// The columns are whatever PipeWire has told us about so far, and it tells us
/// over the first moments of a session rather than all at once. A fit taken on
/// the first stream to arrive would be a fit to one column, so the window keeps
/// following the list until it has settled and then leaves the width alone.
const REFIT_SETTLE: Duration = Duration::from_millis(1500);

/// A fit this launch owes its columns, because the last session ended at a
/// fitted width and the streams that decided that width are not this session's.
#[derive(Clone, Copy)]
struct Refit {
    /// Width the window opened at, which is what the fit's restore goes back to.
    opened_at: i32,
    /// When to stop following the stream list.
    until: Instant,
}
const SEAT_VERSION: u32 = 5;
const ACTIVATION_VERSION: u32 = 1;

pub struct App {
    conn: Connection,
    next_id: u32,
    /// Ids the compositor has deleted, free to reuse. Every frame takes a
    /// callback object, so without these the ids would count up forever.
    free_ids: Vec<u32>,

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
    /// Integer scale per bound wl_output, which is all a compositor without
    /// fractional scaling reports. Outputs come and go, so this is keyed by
    /// object rather than kept in one slot.
    output_scales: HashMap<u32, i32>,
    /// The outputs the surface is currently shown on. A window straddling two
    /// takes the larger of their scales, which is what leaves it sharp on both.
    entered_outputs: Vec<u32>,
    /// The integer scale last sent with set_buffer_scale, so an unchanged one
    /// is not re-sent on every enter.
    buffer_scale: i32,

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
    /// The layout the last frame was painted from. A frame where only the
    /// meters moved paints from it again, since anything that moves the layout
    /// asks for a full repaint.
    painted_layout: Option<layout::Layout>,
    /// The last commit's frame callback, which the compositor answers once it
    /// is ready for the next frame. Zero when none is outstanding, which is the
    /// only time a frame is painted.
    frame_callback: u32,
    /// The latest configure's serial, acked by the next commit, which carries a
    /// buffer of the size that configure closed on.
    pending_configure: Option<u32>,

    // Window state.
    width: i32,
    height: i32,
    /// Last size the window had while not maximized, which is what a relaunch
    /// restores to.
    normal_size: (i32, i32),
    /// The states the latest configure carried.
    states: ToplevelStates,
    /// The fit this launch still owes its columns, set when the last session
    /// ended fitted. None once it has been paid or given up on.
    refit: Option<Refit>,
    configured: bool,
    pub closed: bool,
    counts: Counts,

    /// The core, the retained UI, and the projection, which is everything this
    /// shell shares with the GTK one.
    shell: Shell,
    font: Font,
    palette: Palette,
    icons: IconCache,
    msg_rx: Receiver<AppMessage>,
    evt_rx: Receiver<WorkerEvent>,
    /// The compositor event being handled. Every event is decoded into this
    /// one.
    incoming: Message,

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
    /// When the edits the disk does not have yet are written out. None while
    /// there are none, so a window with nothing to save has no timer to wake
    /// for.
    autosave_due: Option<Instant>,
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
            shell.ui.settings.show_sidebar,
        );

        let mut app = App {
            conn,
            next_id: 2,
            free_ids: Vec::new(),
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
            output_scales: HashMap::new(),
            entered_outputs: Vec::new(),
            buffer_scale: 1,
            scale: 1.0,
            pool: None,
            pool_obj: 0,
            buffers: [BufferSlot::default(), BufferSlot::default()],
            buffer_dims: (0, 0),
            last_painted: 0,
            painted_layout: None,
            frame_callback: 0,
            pending_configure: None,
            width: startup_size.0,
            height: startup_size.1,
            normal_size: startup_size,
            states: ToplevelStates::default(),
            refit: geometry.fitted.then(|| Refit {
                opened_at: startup_size.0,
                until: Instant::now() + REFIT_SETTLE,
            }),
            configured: false,
            closed: false,
            counts: Counts::default(),
            shell,
            font,
            palette: Palette::dark(),
            icons: IconCache::new(),
            msg_rx,
            evt_rx,
            incoming: Message::default(),
            instance,
            xkb: None,
            // Sensible defaults until repeat_info arrives.
            repeat_delay: Duration::from_millis(600),
            repeat_period: Duration::from_millis(40),
            held_key: None,
            ptr_x: 0.0,
            ptr_y: 0.0,
            caret_deadline: Instant::now() + CARET_BLINK,
            autosave_due: None,
            started: Instant::now(),
        };
        app.bind_globals()?;
        app.create_window()?;
        Ok(app)
    }

    fn new_id(&mut self) -> u32 {
        if let Some(id) = self.free_ids.pop() {
            return id;
        }
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

        if debug_enabled() {
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
            let mut msg = std::mem::take(&mut self.incoming);
            while self.conn.next_message(&mut msg) {
                if msg.object == cb && msg.opcode == evt::CALLBACK_DONE {
                    done = true;
                } else {
                    self.handle(&msg)?;
                }
            }
            self.incoming = msg;
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
        let (min_w, min_h) = layout::minimum_size(self.shell.ui.settings.show_sidebar);
        self.send(
            toplevel,
            req::XDG_TOPLEVEL_SET_MIN_SIZE,
            &[Arg::Int(min_w), Arg::Int(min_h)],
        );

        let chrome = initial_chrome(self.shell.ui.settings.decorations, self.decoration_mgr != 0);
        self.shell.ui.chrome = chrome;
        // Only a compositor with a manager gets asked, and its answer arrives as
        // a decoration configure that may still say client.
        if chrome == Chrome::Server {
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
    fn handle(&mut self, msg: &Message) -> io::Result<()> {
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
            (WL_DISPLAY, evt::DISPLAY_DELETE_ID) => {
                // Ids from SERVER_ID_BASE up are the compositor's to hand out.
                let id = r.u32().unwrap_or(0);
                if id != 0 && id < SERVER_ID_BASE {
                    self.free_ids.push(id);
                }
            }
            (_, evt::REGISTRY_GLOBAL) if msg.object == self.registry => {
                let name = r.u32().unwrap_or(0);
                let interface = r.string().unwrap_or_default();
                let version = r.u32().unwrap_or(1);
                self.bind_global(name, interface, version);
            }
            (_, evt::REGISTRY_GLOBAL_REMOVE) if msg.object == self.registry => {}
            (_, evt::OUTPUT_SCALE) if self.output_scales.contains_key(&msg.object) => {
                let factor = r.i32().unwrap_or(1).max(1);
                self.output_scales.insert(msg.object, factor);
                self.apply_output_scale();
            }
            (_, evt::SURFACE_ENTER) if msg.object == self.surface => {
                let output = r.u32().unwrap_or(0);
                if output != 0 && !self.entered_outputs.contains(&output) {
                    self.entered_outputs.push(output);
                }
                self.apply_output_scale();
            }
            (_, evt::SURFACE_LEAVE) if msg.object == self.surface => {
                let output = r.u32().unwrap_or(0);
                self.entered_outputs.retain(|o| *o != output);
                self.apply_output_scale();
            }
            (_, evt::XDG_WM_BASE_PING) if msg.object == self.wm_base => {
                let serial = r.u32().unwrap_or(0);
                let wm = self.wm_base;
                self.send(wm, req::XDG_WM_BASE_PONG, &[Arg::Uint(serial)]);
            }
            (_, evt::XDG_SURFACE_CONFIGURE) if msg.object == self.xdg_surface => {
                // A newer configure replaces one not yet acked, so a storm of
                // them during a drag is answered once, by the next frame.
                self.pending_configure = Some(r.u32().unwrap_or(0));
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
                self.states = states;
                self.counts.configures += 1;
                if debug_enabled() {
                    let listed: Vec<u32> = raw_states
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|&c| u32::from_ne_bytes(c))
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
                let (w, h) =
                    configured_size(w, h, self.normal_size, self.shell.ui.settings.show_sidebar);
                if (w, h) != (self.width, self.height) {
                    self.width = w;
                    self.height = h;
                    self.shell.ui.dirty.mark_full();
                }
                if states.maximized != self.shell.ui.maximized
                    || states.tiled != self.shell.ui.tiled
                {
                    self.shell.ui.maximized = states.maximized;
                    self.shell.ui.tiled = states.tiled;
                    self.shell.ui.dirty.mark_full();
                }
                // Only an ordinary window's size is worth keeping. Maximized,
                // fullscreen, and tiled sizes all belong to the compositor's
                // arrangement, and restoring into one of them on the next
                // launch would leave the window a shape the user never chose.
                if !states.compositor_sized() {
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
                // back onto the logical window, so the preferred scale is not
                // ours to take and the wl_output scale is what runs instead.
                if !self.fractional_drives() {
                    return Ok(());
                }
                if scale > 0.0 && (scale - self.scale).abs() > f32::EPSILON {
                    self.scale = scale;
                    // The buffers hold device pixels, so ensure_buffers will see
                    // a new size and rebuild them.
                    self.shell.ui.dirty.mark_full();
                }
            }
            (_, evt::XDG_TOPLEVEL_CONFIGURE_BOUNDS) if msg.object == self.xdg_toplevel => {
                // Zero for either axis means the compositor has no bound to
                // give, which leaves the fit unclamped rather than pinned to 0.
                let w = r.i32().unwrap_or(0);
                let _h = r.i32().unwrap_or(0);
                self.shell.ui.max_width = if w > 0 { w } else { i32::MAX };
                if debug_enabled() {
                    eprintln!(
                        "{:>6}ms configure bounds: width {w}",
                        self.started.elapsed().as_millis(),
                    );
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
                self.counts.tokens += 1;
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
            (_, evt::CALLBACK_DONE)
                if self.frame_callback != 0 && msg.object == self.frame_callback =>
            {
                self.frame_callback = 0;
                self.counts.callbacks += 1;
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
                            if debug_enabled() {
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
                if self.shell.ui.rest_caret() {
                    self.shell.ui.dirty.mark_full();
                }
            }
            (_, evt::KEYBOARD_KEY) if msg.object == self.keyboard => {
                self.last_serial = r.u32().unwrap_or(self.last_serial);
                let _time = r.u32();
                let code = r.u32().unwrap_or(0);
                let state = r.u32().unwrap_or(0);
                if debug_enabled() {
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
            "wl_output" => return self.bind_output(name, version),
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

    /// Bind one wl_output. Every output is bound, not just the first, since
    /// which one the window lands on is not known until it enters.
    fn bind_output(&mut self, name: u32, version: u32) {
        // wl_output.scale arrives at version 2; below that a compositor reports
        // no scale and 1 is all there is.
        if version < 2 {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.output_scales.insert(id, 1);
        let registry = self.registry;
        encode(
            self.conn.out(),
            registry,
            req::REGISTRY_BIND,
            &[
                Arg::Uint(name),
                Arg::Bind {
                    interface: "wl_output",
                    version: 2.min(version),
                    new_id: id,
                },
            ],
        );
    }

    /// Whether the fractional scale is what sets the buffer size.
    ///
    /// It takes both halves: the scale says how many device pixels a logical
    /// one is worth, and the viewport is what maps the buffer painted at that
    /// scale back onto the logical window. With only one of them there is
    /// nothing to follow, and the integer wl_output scale is the scale.
    fn fractional_drives(&self) -> bool {
        self.fractional != 0 && self.viewport != 0
    }

    /// Follow the integer scale of the outputs the window is on.
    ///
    /// Only for compositors without fractional scaling, since the fractional
    /// scale is finer and mixing the two would scale the frame twice.
    fn apply_output_scale(&mut self) {
        // Outputs are bound during the registry roundtrip, so their scales can
        // arrive before there is a surface to set one on.
        if self.fractional_drives() || self.surface == 0 {
            return;
        }
        let want = preferred_scale(&self.entered_outputs, &self.output_scales);
        if want == self.buffer_scale {
            return;
        }
        self.buffer_scale = want;
        let surface = self.surface;
        self.send(surface, req::SURFACE_SET_BUFFER_SCALE, &[Arg::Int(want)]);
        // The buffers hold device pixels, so a new scale is a new buffer size.
        self.scale = want as f32;
        self.shell.ui.dirty.mark_full();
    }

    fn bind_seat(&mut self, caps: u32) {
        if debug_enabled() {
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
        self.dispatch(msgs);
    }

    /// Reduce messages, and let a fitted window follow the columns when the
    /// user has just asked for a different set of them.
    ///
    /// Whether the window was fitted is read before the messages land, because
    /// afterwards the count has moved and every fitted window would read as
    /// un-fitted. A window the user sized is theirs: the new columns scroll into
    /// the strip and the width stays put.
    fn dispatch(&mut self, msgs: Vec<AppMessage>) {
        let follows =
            msgs.iter().any(AppMessage::changes_columns) && self.shell.is_fitted(self.width);
        self.shell.dispatch(msgs);
        if follows {
            self.refit_to_columns();
        }
    }

    /// Take a fitted window to the width its columns now want, keeping the
    /// width it would go back to. The toggle moved the columns, not the user's
    /// own idea of how wide the window should be.
    fn refit_to_columns(&mut self) {
        let columns = layout::column_count(&self.shell.snapshot);
        if self.shell.ui.is_fitted(self.width, columns) {
            return;
        }
        let restore = self.shell.ui.fit_restore;
        let step = self
            .shell
            .ui
            .fit_step(self.width, columns)
            .map(|step| FitStep { restore, ..step });
        self.apply_fit(step);
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
            WindowAction::ToggleFitWidth => self.toggle_fit_width(),
            WindowAction::Close => self.closed = true,
        }
        let _ = self.flush();
    }

    /// Size the window to the width its columns need, or put back the width it
    /// had before the last fit.
    ///
    /// Only an ordinary window has a width to give. Maximized, fullscreen, or
    /// tiled, the size is the compositor's arrangement, and a window that
    /// committed its own would be told so.
    fn toggle_fit_width(&mut self) {
        let step = self.shell.fit_press(self.width);
        self.apply_fit(step);
    }

    /// Pay the fit this launch owes its columns, while the stream list is still
    /// settling.
    ///
    /// Fitting once on the first stream to arrive would fit to one column, so
    /// this follows the list for as long as [`REFIT_SETTLE`] and then stops. A
    /// window already standing where its columns want it is left alone, which
    /// is what keeps a settled fit from bouncing back on the next turn.
    fn tick_refit(&mut self) {
        let Some(refit) = self.refit else {
            return;
        };
        if Instant::now() >= refit.until {
            self.refit = None;
            return;
        }
        let columns = layout::column_count(&self.shell.snapshot);
        if columns == 0 || self.shell.ui.is_fitted(self.width, columns) {
            return;
        }
        // The width to go back to is the one the window opened at, not whatever
        // an earlier turn of this same settling left behind.
        let step = self
            .shell
            .ui
            .fit_step(self.width, columns)
            .map(|step| FitStep {
                restore: Some(refit.opened_at),
                ..step
            });
        self.apply_fit(step);
    }

    /// Take a width the fit asked for and make it the window's.
    fn apply_fit(&mut self, step: Option<FitStep>) {
        let Some(step) = step else {
            return;
        };
        self.shell.ui.fit_restore = step.restore;
        self.width = step.width;
        self.normal_size = (self.width, self.height);
        self.set_window_geometry();
        self.shell.ui.dirty.mark_full();
        // The chrome just moved under a pointer that did not, and no event is
        // coming to say so. Without this the hover keeps pointing at whatever
        // was under the pointer before the width changed.
        if self.pointer_inside {
            let layout = self.layout();
            let (x, y) = (self.ptr_x as i32, self.ptr_y as i32);
            input::rehover(&mut self.shell.ui, &layout, x, y);
        }
        // The save tick carries the fitted width from here, so a relaunch comes
        // back at the size the press asked for.
        self.push_geometry();
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
        if input::is_fit_key(&self.shell.ui, event) {
            self.toggle_fit_width();
            let _ = self.flush();
            return;
        }
        if let Some(action) = input::clipboard_action(&self.shell.ui, event) {
            self.clipboard(action);
            return;
        }
        if debug_enabled() {
            eprintln!("  -> key={key:?} mods={mods:?}");
        }
        let msgs = input::on_key(&mut self.shell.ui, &self.shell.snapshot, event);
        self.dispatch(msgs);
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
            // Fresh memory holds no frame at all, so neither buffer can be
            // presented until it has been painted whole.
            self.buffers[i] = BufferSlot {
                obj,
                busy: false,
                owed: Owed::All,
            };
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

        let change = if self.shell.ui.dirty.full {
            Owed::All
        } else {
            Owed::Meters
        };

        // The size check keeps a resize from painting into the old geometry.
        let window = Rect::new(0, 0, self.width, self.height);
        let layout = match self.painted_layout.take() {
            Some(layout) if change == Owed::Meters && layout.window == window => layout,
            _ => self.layout(),
        };
        let (dw, dh) = self.device_size();
        let scale = self.scale;
        let frame_px = (dw * dh) as usize;
        // Settled only once there is memory to paint into. Clearing the debt on
        // a turn that painted nothing would leave the buffer looking current.
        let owed;
        {
            let Some(pool) = self.pool.as_mut() else {
                return Ok(());
            };
            owed = take_owed(&mut self.buffers, slot, change);
            let start = slot * frame_px;
            let pixels = &mut pool.pixels()[start..start + frame_px];
            let mut painter = Painter::scaled(pixels, dw as u32, dh as u32, scale);
            let paint = if owed == Owed::All {
                paint_frame
            } else {
                paint_meters
            };
            paint(
                &mut painter,
                &self.shell.snapshot,
                &self.shell.ui,
                &layout,
                &self.font,
                &self.palette,
                &mut self.icons,
            );
        }

        // The ack and the window geometry ride the same commit as the buffer
        // painted at the size the configure asked for, so the compositor never
        // acts on a size the window is not showing yet.
        if let Some(serial) = self.pending_configure.take() {
            self.set_window_geometry();
            let xdg = self.xdg_surface;
            self.send(xdg, req::XDG_SURFACE_ACK_CONFIGURE, &[Arg::Uint(serial)]);
        }

        let (surface, buffer) = (self.surface, self.buffers[slot].obj);
        self.send(
            surface,
            req::SURFACE_ATTACH,
            &[Arg::Object(buffer), Arg::Int(0), Arg::Int(0)],
        );
        // Damage is what the compositor has to recomposite. Reporting only the
        // meters is what keeps a decay step off the rest of the screen.
        if owed == Owed::All {
            let (w, h) = (self.width, self.height);
            self.send(
                surface,
                req::SURFACE_DAMAGE,
                &[Arg::Int(0), Arg::Int(0), Arg::Int(w), Arg::Int(h)],
            );
        } else {
            for r in layout.meter_damage() {
                self.send(
                    surface,
                    req::SURFACE_DAMAGE,
                    &[Arg::Int(r.x), Arg::Int(r.y), Arg::Int(r.w), Arg::Int(r.h)],
                );
            }
        }
        self.frame_callback = self.new_id();
        let callback = self.frame_callback;
        self.send(surface, req::SURFACE_FRAME, &[Arg::NewId(callback)]);
        self.send(surface, req::SURFACE_COMMIT, &[]);
        self.buffers[slot].busy = true;
        self.last_painted = slot;
        self.painted_layout = Some(layout);
        self.counts.frames += 1;
        self.shell.ui.dirty.clear();
        self.flush()
    }

    /// One loop turn: wait for the socket, the buses, the peak pool, or the
    /// soonest deadline, then handle whatever came. `until` caps the wait for a
    /// caller with a deadline of its own.
    pub fn tick(&mut self, until: Option<Instant>) -> io::Result<()> {
        // Meters at rest sleep on the peak pool until the audio threads have
        // something audible. Moving ones step on their own deadline, and the
        // fd, readable until the next drain, stays out of the wait.
        let peaks = match self.shell.meters_due() {
            None => self.shell.runtime.peaks().wake_fd(),
            Some(_) => -1,
        };
        let mut fds = [
            // Requests a flush could not send wait for room on the socket.
            PollFd::readable(self.conn.fd()).or_writable(self.conn.has_pending_output()),
            PollFd::readable(self.msg_rx.wake_fd()),
            PollFd::readable(self.evt_rx.wake_fd()),
            // poll ignores a negative fd, which covers a session where the
            // one-window lock could not be taken.
            PollFd::readable(self.instance.as_ref().map_or(-1, Listener::fd)),
            PollFd::readable(peaks),
        ];
        // Animation deadlines stay out of the wait while a frame callback is
        // outstanding. Its answer wakes the loop anyway, and a hidden window,
        // which the compositor stops answering, has nothing to wake for.
        let animating = self.frame_callback == 0;
        let deadlines = [
            self.autosave_due,
            self.held_key.map(|(_, next)| next),
            until,
            self.shell.meters_due().filter(|_| animating),
            (animating && self.shell.ui.caret_blinking()).then_some(self.caret_deadline),
        ];
        let now = Instant::now();
        let timeout = deadlines
            .into_iter()
            .flatten()
            .min()
            .map(|deadline| deadline.saturating_duration_since(now));
        poll(&mut fds, timeout)?;

        // Persist what changed since the save was armed. Without this the only
        // save is at shutdown, so a kill or a crash would drop the whole
        // session's edits.
        if self.autosave_due.is_some_and(|due| Instant::now() >= due) {
            self.shell.tick_autosave();
            self.autosave_due = None;
        }

        // Blink the caret. Off-focus, or once a run is spent, this settles it
        // shown and costs nothing until a key or a click in a field wakes it.
        if !self.shell.ui.caret_blinking() || Instant::now() >= self.caret_deadline {
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
            // Out of self for the loop, so handle can borrow the rest of it.
            let mut msg = std::mem::take(&mut self.incoming);
            while self.conn.next_message(&mut msg) {
                self.handle(&msg)?;
            }
            self.incoming = msg;
        }

        // Later launches, which hand over whatever their launcher told them and
        // leave the window to us.
        if fds[3].is_ready() {
            while let Some(token) = self.handed_over() {
                self.counts.handovers += 1;
                self.raise(&token);
            }
        }

        // UI and worker messages.
        let mut batch = Vec::new();
        self.msg_rx.drain(|m| batch.push(m));
        self.dispatch(batch);

        let mut worker = Vec::new();
        self.evt_rx.drain(|e| worker.push(e));
        self.shell.dispatch_worker(worker);

        // After the worker events, since those are what bring the columns the
        // fit is owed to.
        self.tick_refit();

        // Meter animation: decay, then fold in the newest peaks. Peaks that
        // woke the loop step the meters at once; from then on they step on
        // their own deadline until every bar is at rest again.
        let now = Instant::now();
        if fds[4].is_ready() && self.shell.wake_meters(now) {
            self.shell.ui.dirty.mark_meters();
        }
        if self.shell.tick_meters(now) {
            self.shell.ui.dirty.mark_meters();
        }

        // The knob's ring eases in and out, and the fit button's press mark
        // eases away, so both keep painting for as long as they are moving.
        if self.shell.tick_fades(now) {
            self.shell.ui.dirty.mark_full();
        }

        // Arm a save on the first edit the disk does not have yet.
        if self.autosave_due.is_none() && self.shell.unsaved() {
            self.autosave_due = Some(now + AUTOSAVE_DELAY);
        }

        // Paced by the frame callback. Changes that land while one is out,
        // configures included, wait for its answer and go out as one frame, and
        // a hidden window, which the compositor stops answering, paints nothing.
        if self.frame_callback == 0 && self.shell.ui.dirty.needs_paint() {
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
            fitted: self.shell.is_fitted(w),
        });
    }

    /// Press the restore button, if the window is maximized and so shows one.
    /// Returns whether it was pressed.
    pub fn restore(&mut self) -> bool {
        if !self.shell.ui.maximized {
            return false;
        }
        self.window_action(WindowAction::ToggleMaximize);
        true
    }

    /// What this run saw of the compositor it ran against.
    pub fn facts(&self) -> Facts {
        Facts {
            compositor: self.compositor != 0,
            shm: self.shm != 0,
            wm_base: self.wm_base != 0,
            seat: self.seat != 0,
            decoration: self.decoration_mgr != 0,
            activation: self.activation != 0,
            fractional: self.fractional_mgr != 0,
            viewport: self.viewporter != 0,
            size: (self.width, self.height),
            normal: self.normal_size,
            states: self.states,
            chrome: self.shell.ui.chrome,
            scale: self.scale,
            buffer_scale: self.buffer_scale,
            buffer: self.buffer_dims,
            counts: self.counts,
        }
    }

    /// Persist geometry and flush a final save.
    pub fn shutdown(&mut self) {
        let (w, h) = self.normal_size;
        self.shell
            .shutdown(w.max(0) as u32, h.max(0) as u32, self.shell.ui.maximized);
    }
}

/// What one run against a compositor saw: which globals it offered, where the
/// window ended up, and how much of the exchange happened.
///
/// The [`fmt::Display`] output is the format the probe prints and the
/// compositor tests parse, so it is a contract between the two: one fact per
/// line, every key present on every run, and presence written as 1 or 0 rather
/// than an object id, which moves with bind order.
pub struct Facts {
    compositor: bool,
    shm: bool,
    wm_base: bool,
    seat: bool,
    decoration: bool,
    activation: bool,
    fractional: bool,
    viewport: bool,
    size: (i32, i32),
    normal: (i32, i32),
    states: ToplevelStates,
    chrome: Chrome,
    scale: f32,
    buffer_scale: i32,
    buffer: (i32, i32),
    counts: Counts,
}

impl fmt::Display for Facts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bit = |present: bool| u8::from(present);
        writeln!(
            f,
            "probe globals compositor={} shm={} wm_base={} seat={} \
             decoration={} activation={} fractional={} viewport={}",
            bit(self.compositor),
            bit(self.shm),
            bit(self.wm_base),
            bit(self.seat),
            bit(self.decoration),
            bit(self.activation),
            bit(self.fractional),
            bit(self.viewport),
        )?;
        writeln!(
            f,
            "probe window {}x{} normal={}x{} maximized={} tiled={} chrome={}",
            self.size.0,
            self.size.1,
            self.normal.0,
            self.normal.1,
            self.states.maximized,
            self.states.tiled,
            match self.chrome {
                Chrome::Server => "server",
                Chrome::Client => "client",
                Chrome::Toolkit => "toolkit",
            },
        )?;
        writeln!(
            f,
            "probe scale factor={} buffer_scale={} buffer={}x{}",
            self.scale, self.buffer_scale, self.buffer.0, self.buffer.1,
        )?;
        writeln!(
            f,
            "probe counts configures={} frames={} callbacks={} tokens={} handovers={}",
            self.counts.configures,
            self.counts.frames,
            self.counts.callbacks,
            self.counts.tokens,
            self.counts.handovers,
        )
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

/// The size a toplevel configure gives the window. A side left at zero is the
/// window's to pick, and it picks the same side of its normal size, which is
/// how a restore from maximized gets back the size from before. Every side
/// still goes through the minimum, since a compositor may configure straight
/// past the one the window declared.
fn configured_size(w: i32, h: i32, normal: (i32, i32), show_sidebar: bool) -> (i32, i32) {
    let side = |configured: i32, normal: i32| if configured > 0 { configured } else { normal };
    layout::at_least_minimum(side(w, normal.0), side(h, normal.1), show_sidebar)
}

/// Read a configure's trailing state array, which is a run of u32 enum values.
fn toplevel_states(bytes: &[u8]) -> ToplevelStates {
    let mut out = ToplevelStates::default();
    for state in bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&c| u32::from_ne_bytes(c))
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

/// Whether to narrate the compositor exchange to stderr. The globals, the
/// window's size, and every key arrive over that exchange, so when the window
/// comes up wrong it is the only place the reason shows.
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

/// Who draws the titlebar before anything has been negotiated.
///
/// Server means there is a manager to ask, and the answer arrives later as a
/// decoration configure that may still hand the job back. Client means no
/// answer is coming: either the settings already refused server-side chrome, or
/// the compositor offers no manager, which the protocol reads as the client
/// decorating. Treating a missing manager as server-side would leave the window
/// with a titlebar from neither side, and so with no close, drag, or edges.
fn initial_chrome(decorations: Decorations, has_manager: bool) -> Chrome {
    if decorations == Decorations::Client || !has_manager {
        Chrome::Client
    } else {
        Chrome::Server
    }
}

/// The scale for a window shown on `entered`: the largest of those outputs'
/// scales, so one straddling a 1x and a 2x screen stays sharp on the denser.
/// An output with no scale reported, or none entered at all, counts as 1.
fn preferred_scale(entered: &[u32], scales: &HashMap<u32, i32>) -> i32 {
    entered
        .iter()
        .filter_map(|o| scales.get(o))
        .copied()
        .max()
        .unwrap_or(1)
        .max(1)
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
        app.tick(None)?;
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

    /// Outputs and the scales they reported.
    fn scales(pairs: &[(u32, i32)]) -> HashMap<u32, i32> {
        pairs.iter().copied().collect()
    }

    /// The debt a buffer carries is everything that changed while the other one
    /// was on screen. Getting this wrong shows the frame before last in
    /// whatever a partial repaint did not cover.
    #[test]
    fn a_buffer_owes_every_change_it_sat_out() {
        let mut buffers = [BufferSlot {
            owed: Owed::All,
            ..Default::default()
        }; 2];

        // Neither has held a frame, so the first two paints are whole ones
        // however little changed.
        assert_eq!(take_owed(&mut buffers, 0, Owed::All), Owed::All);
        assert_eq!(take_owed(&mut buffers, 1, Owed::Meters), Owed::All);

        // Both current now, so a meter step costs only the meters.
        assert_eq!(take_owed(&mut buffers, 0, Owed::Meters), Owed::Meters);
        assert_eq!(take_owed(&mut buffers, 1, Owed::Meters), Owed::Meters);

        // One full frame, and the buffer that missed it owes all of it again
        // even though the turn it lands on only stepped the meters.
        assert_eq!(take_owed(&mut buffers, 0, Owed::All), Owed::All);
        assert_eq!(take_owed(&mut buffers, 1, Owed::Meters), Owed::All);
        assert_eq!(take_owed(&mut buffers, 0, Owed::Meters), Owed::Meters);
    }

    /// A compositor offering no decoration manager is the case that reads as a
    /// broken window: nothing negotiates, so a server-side default means no
    /// titlebar from either side.
    #[test]
    fn no_decoration_manager_leaves_the_window_to_draw_its_own() {
        assert_eq!(
            initial_chrome(Decorations::Server, false),
            Chrome::Client,
            "with no manager the protocol says the client decorates"
        );
        assert_eq!(
            initial_chrome(Decorations::Server, true),
            Chrome::Server,
            "a manager is there to be asked"
        );
    }

    /// The setting refuses server-side chrome outright, so no decoration object
    /// is created even where one could be.
    #[test]
    fn asking_for_client_chrome_negotiates_nothing() {
        assert_eq!(initial_chrome(Decorations::Client, true), Chrome::Client);
        assert_eq!(initial_chrome(Decorations::Client, false), Chrome::Client);
    }

    #[test]
    fn a_window_on_no_output_yet_draws_at_one() {
        assert_eq!(preferred_scale(&[], &scales(&[(7, 2)])), 1);
    }

    #[test]
    fn a_window_takes_the_scale_of_the_output_it_is_on() {
        assert_eq!(preferred_scale(&[7], &scales(&[(7, 2), (8, 1)])), 2);
    }

    #[test]
    fn a_window_across_two_outputs_takes_the_denser() {
        assert_eq!(preferred_scale(&[8, 7], &scales(&[(7, 3), (8, 1)])), 3);
    }

    #[test]
    fn an_output_that_reported_no_scale_counts_as_one() {
        assert_eq!(preferred_scale(&[9], &scales(&[(7, 2)])), 1);
    }

    #[test]
    fn an_ordinary_window_owns_its_own_size() {
        let s = toplevel_states(&states(&[TOPLEVEL_STATE_ACTIVATED]));
        assert!(!s.maximized);
        assert!(!s.tiled);
        assert!(!s.compositor_sized(), "its size is the user's to keep");
    }

    /// Each side on its own: a side the configure gives is taken, a side it
    /// leaves at zero or below comes from the normal size, and both keep the
    /// minimum.
    #[test]
    fn a_side_left_at_zero_comes_from_the_normal_size() {
        let (min_w, min_h) = layout::minimum_size(false);
        let normal = (min_w + 200, min_h + 100);
        assert_eq!(configured_size(0, 0, normal, false), normal);
        assert_eq!(
            configured_size(min_w + 400, 0, normal, false),
            (min_w + 400, normal.1)
        );
        assert_eq!(
            configured_size(-5, min_h + 300, normal, false),
            (normal.0, min_h + 300)
        );
        assert_eq!(configured_size(1, 1, normal, false), (min_w, min_h));
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

    /// Facts as a compositor with everything would leave them.
    fn facts() -> Facts {
        Facts {
            compositor: true,
            shm: true,
            wm_base: true,
            seat: true,
            decoration: true,
            activation: true,
            fractional: true,
            viewport: true,
            size: (638, 692),
            normal: (560, 720),
            states: ToplevelStates {
                maximized: false,
                tiled: true,
            },
            chrome: Chrome::Server,
            scale: 1.5,
            buffer_scale: 1,
            buffer: (957, 1038),
            counts: Counts {
                configures: 2,
                frames: 3,
                callbacks: 2,
                tokens: 1,
                handovers: 1,
            },
        }
    }

    #[test]
    fn the_probe_report_is_four_lines_of_key_values() {
        // The compositor tests parse this, so the shape is the contract: one
        // fact per line, every key on every run.
        let lines: Vec<String> = facts().to_string().lines().map(str::to_string).collect();
        assert_eq!(
            lines,
            vec![
                "probe globals compositor=1 shm=1 wm_base=1 seat=1 decoration=1 activation=1 \
                 fractional=1 viewport=1",
                "probe window 638x692 normal=560x720 maximized=false tiled=true chrome=server",
                "probe scale factor=1.5 buffer_scale=1 buffer=957x1038",
                "probe counts configures=2 frames=3 callbacks=2 tokens=1 handovers=1",
            ],
        );
    }

    #[test]
    fn a_global_the_compositor_never_offered_reads_as_zero() {
        let bare = Facts {
            seat: false,
            decoration: false,
            activation: false,
            fractional: false,
            chrome: Chrome::Client,
            ..facts()
        };
        let printed = bare.to_string();
        assert!(
            printed.contains("seat=0 decoration=0 activation=0 fractional=0 viewport=1"),
            "absence is a zero rather than a missing key: {printed}"
        );
        assert!(printed.contains("chrome=client"));
    }

    #[test]
    fn unknown_states_are_ignored_rather_than_guessed_at() {
        // Later protocol versions add states; none of them should be taken to
        // mean the compositor sized the window.
        let s = toplevel_states(&states(&[3, 9, 99]));
        assert!(!s.compositor_sized());
    }
}
