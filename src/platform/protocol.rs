//! Wayland object ids, request opcodes, and event opcodes for the interfaces
//! the shell uses. Only what bnksound needs is listed.

/// wl_display is always object id 1.
pub const WL_DISPLAY: u32 = 1;

// Request opcodes (client -> compositor).
pub mod req {
    pub const DISPLAY_SYNC: u16 = 0;
    pub const DISPLAY_GET_REGISTRY: u16 = 1;
    pub const REGISTRY_BIND: u16 = 0;
    pub const COMPOSITOR_CREATE_SURFACE: u16 = 0;
    pub const SHM_CREATE_POOL: u16 = 0;
    pub const SHM_POOL_CREATE_BUFFER: u16 = 0;
    pub const SHM_POOL_DESTROY: u16 = 1;
    pub const BUFFER_DESTROY: u16 = 0;
    pub const SURFACE_DESTROY: u16 = 0;
    pub const SURFACE_ATTACH: u16 = 1;
    pub const SURFACE_DAMAGE: u16 = 2;
    pub const SURFACE_FRAME: u16 = 3;
    pub const SURFACE_COMMIT: u16 = 6;
    pub const SURFACE_SET_BUFFER_SCALE: u16 = 8;
    pub const XDG_WM_BASE_PONG: u16 = 3;
    pub const XDG_WM_BASE_GET_XDG_SURFACE: u16 = 2;
    pub const XDG_SURFACE_GET_TOPLEVEL: u16 = 1;
    pub const XDG_SURFACE_SET_WINDOW_GEOMETRY: u16 = 3;
    pub const XDG_SURFACE_ACK_CONFIGURE: u16 = 4;
    pub const XDG_TOPLEVEL_SET_TITLE: u16 = 2;
    pub const XDG_TOPLEVEL_SET_APP_ID: u16 = 3;
    pub const XDG_TOPLEVEL_MOVE: u16 = 5;
    pub const XDG_TOPLEVEL_RESIZE: u16 = 6;
    pub const XDG_TOPLEVEL_SET_MIN_SIZE: u16 = 8;
    pub const XDG_TOPLEVEL_SET_MAXIMIZED: u16 = 9;
    pub const XDG_TOPLEVEL_UNSET_MAXIMIZED: u16 = 10;
    pub const XDG_TOPLEVEL_SET_MINIMIZED: u16 = 13;
    pub const FRACTIONAL_SCALE_MANAGER_GET_SCALE: u16 = 1;
    pub const VIEWPORTER_GET_VIEWPORT: u16 = 1;
    pub const VIEWPORT_SET_DESTINATION: u16 = 2;
    pub const DECORATION_MANAGER_GET_TOPLEVEL: u16 = 1;
    pub const DECORATION_SET_MODE: u16 = 1;
    pub const DATA_DEVICE_MANAGER_CREATE_SOURCE: u16 = 0;
    pub const DATA_DEVICE_MANAGER_GET_DEVICE: u16 = 1;
    pub const DATA_SOURCE_OFFER: u16 = 0;
    pub const DATA_SOURCE_DESTROY: u16 = 1;
    pub const DATA_DEVICE_SET_SELECTION: u16 = 1;
    pub const DATA_OFFER_RECEIVE: u16 = 1;
    pub const DATA_OFFER_DESTROY: u16 = 2;
    pub const CURSOR_SHAPE_MANAGER_GET_POINTER: u16 = 1;
    pub const CURSOR_SHAPE_DEVICE_SET_SHAPE: u16 = 1;
    pub const SEAT_GET_POINTER: u16 = 0;
    pub const SEAT_GET_KEYBOARD: u16 = 1;
    pub const ACTIVATION_GET_TOKEN: u16 = 1;
    pub const ACTIVATION_ACTIVATE: u16 = 2;
    pub const ACTIVATION_TOKEN_SET_APP_ID: u16 = 1;
    pub const ACTIVATION_TOKEN_SET_SURFACE: u16 = 2;
    pub const ACTIVATION_TOKEN_COMMIT: u16 = 3;
    pub const ACTIVATION_TOKEN_DESTROY: u16 = 4;
}

// Event opcodes (compositor -> client).
pub mod evt {
    pub const DISPLAY_ERROR: u16 = 0;
    pub const DISPLAY_DELETE_ID: u16 = 1;
    pub const REGISTRY_GLOBAL: u16 = 0;
    pub const REGISTRY_GLOBAL_REMOVE: u16 = 1;
    /// wl_surface.enter / .leave, naming an output the surface is shown on.
    pub const SURFACE_ENTER: u16 = 0;
    pub const SURFACE_LEAVE: u16 = 1;
    /// wl_output.scale, the integer scale of that output.
    pub const OUTPUT_SCALE: u16 = 3;
    pub const CALLBACK_DONE: u16 = 0;
    pub const SHM_FORMAT: u16 = 0;
    pub const BUFFER_RELEASE: u16 = 0;
    pub const XDG_WM_BASE_PING: u16 = 0;
    pub const XDG_SURFACE_CONFIGURE: u16 = 0;
    pub const XDG_TOPLEVEL_CONFIGURE: u16 = 0;
    pub const XDG_TOPLEVEL_CLOSE: u16 = 1;
    pub const DECORATION_CONFIGURE: u16 = 0;
    pub const SEAT_CAPABILITIES: u16 = 0;
    pub const DATA_DEVICE_SELECTION: u16 = 5;
    pub const DATA_SOURCE_SEND: u16 = 1;
    pub const DATA_SOURCE_CANCELLED: u16 = 2;
    pub const KEYBOARD_KEYMAP: u16 = 0;
    pub const KEYBOARD_ENTER: u16 = 1;
    pub const KEYBOARD_LEAVE: u16 = 2;
    pub const KEYBOARD_KEY: u16 = 3;
    pub const KEYBOARD_MODIFIERS: u16 = 4;
    pub const KEYBOARD_REPEAT_INFO: u16 = 5;
    pub const POINTER_ENTER: u16 = 0;
    pub const POINTER_LEAVE: u16 = 1;
    pub const POINTER_MOTION: u16 = 2;
    pub const POINTER_BUTTON: u16 = 3;
    pub const POINTER_AXIS: u16 = 4;
    pub const FRACTIONAL_PREFERRED_SCALE: u16 = 0;
    pub const ACTIVATION_TOKEN_DONE: u16 = 0;
}

/// xdg_toplevel states, as they appear in a configure's state array.
pub const TOPLEVEL_STATE_MAXIMIZED: u32 = 1;
pub const TOPLEVEL_STATE_FULLSCREEN: u32 = 2;
pub const TOPLEVEL_STATE_ACTIVATED: u32 = 4;
/// Edges the compositor has snapped the window against. A tiled window is not
/// maximized, but its size is the compositor's choice all the same.
pub const TOPLEVEL_STATE_TILED_LEFT: u32 = 5;
pub const TOPLEVEL_STATE_TILED_RIGHT: u32 = 6;
pub const TOPLEVEL_STATE_TILED_TOP: u32 = 7;
pub const TOPLEVEL_STATE_TILED_BOTTOM: u32 = 8;

/// xdg_toplevel resize edges. The bits are not a plain compass: left is 4 and
/// right is 8, so a corner is the sum of its two sides.
pub mod resize {
    pub const TOP: u32 = 1;
    pub const BOTTOM: u32 = 2;
    pub const LEFT: u32 = 4;
    pub const TOP_LEFT: u32 = 5;
    pub const BOTTOM_LEFT: u32 = 6;
    pub const RIGHT: u32 = 8;
    pub const TOP_RIGHT: u32 = 9;
    pub const BOTTOM_RIGHT: u32 = 10;
}

/// wp_fractional_scale_v1 reports the scale in 120ths of a logical pixel.
pub const FRACTIONAL_SCALE_DENOM: f32 = 120.0;

/// wl_seat capability bits.
pub const SEAT_CAP_POINTER: u32 = 1;
pub const SEAT_CAP_KEYBOARD: u32 = 2;

/// zxdg_toplevel_decoration_v1 modes.
pub const DECORATION_MODE_CLIENT_SIDE: u32 = 1;
pub const DECORATION_MODE_SERVER_SIDE: u32 = 2;

/// wp_cursor_shape_device_v1 shapes, in protocol enum order.
pub mod cursor {
    pub const DEFAULT: u32 = 1;
    pub const POINTER: u32 = 4;
    pub const TEXT: u32 = 9;
    pub const GRAB: u32 = 16;
    pub const GRABBING: u32 = 17;
    pub const EW_RESIZE: u32 = 26;
    pub const NS_RESIZE: u32 = 27;
    pub const NESW_RESIZE: u32 = 28;
    pub const NWSE_RESIZE: u32 = 29;
}

/// wl_shm ARGB8888 pixel format (premultiplied alpha, native byte order).
pub const SHM_FORMAT_ARGB8888: u32 = 0;

/// wl_pointer button state values.
pub const BUTTON_RELEASED: u32 = 0;
pub const BUTTON_PRESSED: u32 = 1;

/// Linux input-event codes for the mouse buttons.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
