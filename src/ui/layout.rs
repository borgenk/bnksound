//! Layout types: stable row ids, hit targets, and the metric constants the
//! projection lays the mixer out with.
//!
//! The geometry projection (snapshot to rectangles) builds on these. They are
//! kept apart from the drawing so input mapping and hit testing can name every
//! interactive element without touching pixels.

use crate::domain::{MAX_VOLUME, Section};
use crate::render::primitives::Rect;
use crate::ui::{Chrome, Focus, UiState};
use crate::view::snapshot::{DeviceRowView, ModalView, SinkOption, ViewSnapshot};

/// Stable identity of a mixer row across refreshes. Device rows key on their
/// PipeWire node id; app rows key on the grouping key (collapsed group) or the
/// member stream id (expanded sub-row).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RowId {
    Sink(u32),
    Source(u32),
    AppGroup(String),
    AppMember(u32),
}

/// A window edge or corner, for custom-decoration resize handles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Something the pointer can land on. Layout produces a rectangle for each;
/// input maps a click on one, plus the event, into a state message or a
/// transient UI change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HitTarget {
    // Section toolbar.
    SectionFilter(Section),
    MuteAll,
    ResetTargets,
    // Profile selector and its menu.
    ProfileSelector,
    ProfileApply(String),
    ProfileCreate,
    // Device rows (sink or source).
    DeviceDefault(RowId),
    // Rows in general (device or app, collapsed group or member).
    RowMute(RowId),
    RowSlider(RowId),
    // App rows.
    AppExpand(RowId),
    /// A per-app output target button. `sink` None is the autoroute "A" button.
    AppTarget {
        row: RowId,
        sink: Option<u32>,
    },
    /// The strip's horizontal scrollbar, present only while the columns run
    /// wider than the window.
    StripScrollbar,
    // Command palette.
    PaletteInput,
    PaletteRow(usize),
    // Modal dialog.
    ModalInput,
    ModalConfirm,
    ModalCancel,
    /// The dimmed area around an open overlay. It swallows presses that would
    /// otherwise reach the mixer behind it, and dismisses the overlay.
    Backdrop,
    // Client-side decorations, present only when the shell draws its own chrome.
    TitlebarDrag,
    WindowMinimize,
    WindowMaximize,
    WindowClose,
    ResizeEdge(ResizeEdge),
}

/// Fixed pixel metrics of the mixer body, in logical (pre-scale) pixels. The
/// values match the GTK reference layout's column and row sizing.
pub mod metrics {
    /// Width of one device or app column.
    pub const COLUMN_WIDTH: i32 = 106;
    /// Height of a column's header block (icon, name, subtitle).
    pub const COLUMN_HEADER_HEIGHT: i32 = 46;
    /// Square side of an application icon.
    pub const ICON_SIZE: i32 = 26;
    /// Height reserved for the icon block above a name.
    pub const ICON_BOX_HEIGHT: i32 = 28;
    /// Height a column keeps when the strip is shorter than one, which only
    /// happens when a window manager forces a size past [`super::minimum_size`].
    /// The column is clipped rather than squashed to nothing.
    pub const SLIDER_TRACK_HEIGHT: i32 = 140;
    /// Shortest the meter and fader get before the window stops shrinking.
    /// Short enough for the window to tuck into a corner of the screen, long
    /// enough to still aim at.
    pub const MIN_FADER_HEIGHT: i32 = 130;
    /// Gap between the target/expand picker row and the mute button. Wider
    /// than the gap between the pins, which is what sets the mute apart from
    /// the choices above it.
    pub const PICKER_MUTE_GAP: i32 = 9;
    /// Gap between the target pins themselves. Tighter than the gap to the
    /// mute button, so the pins read as one set of linked choices and the mute
    /// reads as something else sitting below them.
    pub const PICK_STACK_GAP: i32 = 2;
    // That reading only survives while the two gaps stay apart, so the build
    // stops rather than let them be levelled out.
    const _: () = assert!(PICK_STACK_GAP < PICKER_MUTE_GAP);
    /// Section-toolbar action button width (mute-all, reset).
    pub const ACTION_BTN_WIDTH: i32 = 32;
    /// Height of one action-bar or picker button.
    pub const ACTION_BTN_HEIGHT: i32 = 20;
    /// Width of the left action bar the buttons are centered in.
    pub const SIDEBAR_WIDTH: i32 = 48;
    /// Gap between two stacked action-bar buttons.
    pub const ACTION_GAP: i32 = 4;
    /// Gap separating the filter cluster from the action cluster.
    pub const ACTION_CLUSTER_GAP: i32 = 15;
    /// The action bar's own vertical padding.
    pub const SIDEBAR_PAD: i32 = 12;
    /// Width of the slider trough, matching the meter's segment pitch.
    pub const SLIDER_WIDTH: i32 = 16;
    /// Diameter of the slider knob. It overhangs the trough on both sides.
    pub const KNOB_SIZE: i32 = 28;
    /// Side of a small square picker button (target pins, mute).
    pub const PICK_SIZE: i32 = 22;
    /// Bottom margin shared by the meter, the slider, and the mute button, so
    /// all three end on the same line.
    pub const COLUMN_BOTTOM: i32 = 12;
    /// Height of the strip carrying the profile selector.
    pub const PROFILE_STRIP_HEIGHT: i32 = 34;
    /// Width of the profile selector chip.
    pub const PROFILE_CHIP_WIDTH: i32 = 140;
    /// Inset from the strip's left edge to the chip. The chip sits nearer the
    /// window edge than the columns do, so it reads as part of the chrome.
    pub const PROFILE_CHIP_PAD: i32 = 4;
    /// Cubic volume above which the slider fill switches to the warning color.
    pub const VOLUME_WARNING_THRESHOLD: f32 = 1.10;
    /// Inset from a text field's left edge to its first glyph. Drawing and
    /// caret hit-testing both measure from here, so they cannot drift apart.
    pub const FIELD_PAD: i32 = 8;
    /// Inset from a text field's top edge to the text's top.
    pub const FIELD_TEXT_TOP: i32 = 7;
    /// Point size of editable field text, shared by the drawing and the caret.
    pub const FIELD_TEXT_SIZE: f32 = 13.0;
    /// Pitch of one command-palette row. Layout stacks rows on it and the
    /// wheel turns pixels into rows with it.
    pub const PALETTE_ROW_H: i32 = 30;
    /// Height of the client-drawn titlebar.
    pub const TITLEBAR_HEIGHT: i32 = 34;
    /// Square side of a window button (minimize, maximize, close).
    pub const WINDOW_BTN: i32 = 28;
    /// Grab thickness of a resize edge; a corner grab is twice this.
    pub const RESIZE_GRAB: i32 = 6;
}

/// A hit rectangle and what it targets.
pub struct Hit {
    pub rect: Rect,
    pub target: HitTarget,
}

/// The geometry of one column's slider.
pub struct SliderGeom {
    /// The full vertical track the thumb travels.
    pub track: Rect,
    /// The lit portion from the thumb down to the track bottom.
    pub fill: Rect,
    /// The draggable thumb.
    pub thumb: Rect,
    /// Y of the unity-gain (cubic 1.0) reference notch.
    pub unity_y: i32,
}

/// The geometry of one device or app column.
///
/// The meter and the slider run the column's full body height side by side,
/// centered as a pair. The small square buttons ride over that pair's right
/// edge, so resizing them cannot shift the slider off centre.
pub struct ColumnGeom {
    pub id: RowId,
    pub rect: Rect,
    /// App icon; empty for device rows.
    pub icon: Rect,
    /// The device form heading, or the app name for app rows.
    pub name: Rect,
    /// The device name under its form heading; empty for app rows.
    pub sub: Rect,
    pub slider: SliderGeom,
    pub meter: Rect,
    pub mute: Rect,
    /// Target buttons as (rect, sink id), stacked bottom-up above the mute
    /// button; None is the autoroute button. Empty for device rows.
    pub targets: Vec<(Rect, Option<u32>)>,
    /// The expand toggle, one slot above the topmost target pin. Empty unless
    /// the row is an app group with more than one member.
    pub expand: Rect,
    /// The hairline rule closing the column on its right.
    pub separator: Rect,
    pub is_app: bool,
}

/// One row of the profile dropdown: the profile a click applies.
pub struct ProfileRowGeom {
    pub name: String,
    pub rect: Rect,
    pub active: bool,
}

/// The dropped-open profile selector.
pub struct ProfileMenuGeom {
    pub panel: Rect,
    pub rows: Vec<ProfileRowGeom>,
    /// The rule between the profiles and the row that makes a new one.
    pub separator: Rect,
    pub create: Rect,
}

/// The window's own titlebar, laid out only when the shell draws its chrome.
pub struct TitlebarGeom {
    pub bar: Rect,
    pub title: Rect,
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
}

/// The command palette while it is open.
pub struct PaletteGeom {
    pub panel: Rect,
    pub input: Rect,
    /// One rectangle per row on screen, starting at `first_visible`.
    pub rows: Vec<Rect>,
    /// Index into the filtered list of the first row in `rows`, so a row's
    /// rectangle and its command still find each other.
    pub first_visible: usize,
    /// Track and slider, absent while the whole list fits.
    pub scrollbar: Option<ScrollbarGeom>,
    /// Where "no matching commands" is drawn when there are no rows.
    pub empty: Rect,
}

/// A vertical scrollbar: the lane it runs in and the slider riding on it.
pub struct ScrollbarGeom {
    pub track: Rect,
    pub slider: Rect,
}

/// An open create / rename / delete dialog.
pub struct ModalGeom {
    pub panel: Rect,
    pub title: Rect,
    pub body: Rect,
    /// The name field, absent on dialogs that only confirm.
    pub input: Option<Rect>,
    pub error: Rect,
    pub cancel: Rect,
    pub confirm: Rect,
}

/// The projected geometry of a whole frame: the toolbar, the scrollable strip
/// of columns, the overlays, and the flat list of hit rectangles.
pub struct Layout {
    /// The whole surface, including any client-drawn titlebar.
    pub window: Rect,
    /// The mixer body: the window minus a client titlebar, if there is one.
    pub content: Rect,
    /// The strip carrying the profile selector, absent when the window's own
    /// titlebar carries it instead.
    pub profile_strip: Option<Rect>,
    /// The left action bar, absent when the user has hidden it.
    pub sidebar: Option<Rect>,
    /// The scrollable column area.
    pub strip: Rect,
    pub columns: Vec<ColumnGeom>,
    pub hits: Vec<Hit>,
    /// The profile dropdown, present only while it is open.
    pub profile_menu: Option<ProfileMenuGeom>,
    /// The client-drawn titlebar, present only when the shell owns its chrome.
    pub titlebar: Option<TitlebarGeom>,
    pub palette: Option<PaletteGeom>,
    pub modal: Option<ModalGeom>,
    /// Largest horizontal scroll offset (0 when everything fits).
    pub scroll_max_x: i32,
    /// The strip's scrollbar, absent when every column fits.
    pub strip_scrollbar: Option<ScrollbarGeom>,
}

// Projection metrics (logical pixels).
const STRIP_PAD: i32 = 10;
/// The 1px rule between columns; the separator is drawn in it.
const COL_GAP: i32 = 1;
/// Inset from a column's left edge to its meter. The meter, fader, and button
/// stack sit at fixed offsets rather than being centred, which is what keeps
/// them lined up across columns of differing content.
const METER_INSET: i32 = 24;
/// Air between the fader and the button stack beside it.
const FADER_BUTTON_GAP: i32 = 9;
/// The strip's bottom padding, which is tighter than its top.
const STRIP_PAD_BOTTOM: i32 = 2;
const ICON_TOP: i32 = 8;
const NAME_H: i32 = 16;
const SLIDER_METER_GAP: i32 = 12;
/// Inset from a column's top to its heading.
const HEADER_TOP: i32 = 3;
/// Air between the header block and the top of the meter and fader.
const HEADER_GAP: i32 = 16;
/// Profile dropdown metrics.
const MENU_ROW_H: i32 = 26;
const MENU_PAD: i32 = 4;
/// The rule above the create row, a hairline like every other divider.
const MENU_SEP_H: i32 = 1;
const MENU_MIN_W: i32 = 200;
/// Overlay metrics: text field height, palette row pitch, modal panel width.
const FIELD_H: i32 = 30;
/// Palette height that is not rows: the search field with its margins above
/// the list, plus the panel's bottom padding.
const PALETTE_CHROME_H: i32 = 56;
/// Thickness of the lane a scrollbar runs in, slider plus the air either side.
const SCROLLBAR_LANE: i32 = 8;
const SCROLLBAR_INSET: i32 = 2;
/// Shortest a slider gets, so a long run still leaves something to grab.
const SCROLLBAR_MIN_LEN: i32 = 24;
const MODAL_W: i32 = 300;

/// Thumb-center y for a cubic gain within a vertical track.
fn y_for_cubic(track: Rect, cubic: f32) -> i32 {
    let t = (cubic / MAX_VOLUME).clamp(0.0, 1.0);
    track.bottom() - (track.h as f32 * t) as i32
}

/// The cubic gain a pointer y maps to within a vertical track. Inverse of
/// [`y_for_cubic`], for slider drag handling.
pub fn cubic_for_y(track: Rect, y: i32) -> f32 {
    let t = ((track.bottom() - y) as f32 / track.h.max(1) as f32).clamp(0.0, 1.0);
    t * MAX_VOLUME
}

/// The trough, its fill, and the round knob riding on the value.
///
/// The knob is wider than the trough and overhangs it on both sides, which is
/// what makes the fader read as a fader rather than a progress bar.
fn slider_geom(area: Rect, cubic: f32) -> SliderGeom {
    use metrics::KNOB_SIZE;
    let center = y_for_cubic(area, cubic).clamp(area.y, area.bottom());
    SliderGeom {
        track: area,
        fill: Rect::new(area.x, center, area.w, area.bottom() - center),
        thumb: Rect::new(
            area.x + (area.w - KNOB_SIZE) / 2,
            center - KNOB_SIZE / 2,
            KNOB_SIZE,
            KNOB_SIZE,
        ),
        unity_y: y_for_cubic(area, 1.0),
    }
}

/// What a column needs laid out. Bundled so the projection stays readable as
/// the per-row inputs grow.
pub struct ColumnSpec<'a> {
    pub id: RowId,
    pub cubic: f32,
    pub is_app: bool,
    pub can_expand: bool,
    /// Sinks the app target picker offers; empty for device rows.
    pub sinks: &'a [SinkOption],
}

/// Lay out one column into `rect`, which spans the full height of the strip.
fn column(spec: ColumnSpec, rect: Rect, hits: &mut Vec<Hit>) -> ColumnGeom {
    use metrics::*;
    let ColumnSpec {
        id,
        cubic,
        is_app,
        can_expand,
        sinks,
    } = spec;
    let x = rect.x;

    // Header: an icon over a name for apps, a form heading over the device name
    // for devices. Fixed height either way, so the two kinds line up.
    let icon = if is_app {
        Rect::new(
            x + (COLUMN_WIDTH - ICON_SIZE) / 2,
            rect.y + ICON_TOP,
            ICON_SIZE,
            ICON_SIZE,
        )
    } else {
        Rect::new(x, rect.y, 0, 0)
    };
    let (name, sub) = if is_app {
        (
            Rect::new(x, rect.y + ICON_BOX_HEIGHT + ICON_TOP, COLUMN_WIDTH, NAME_H),
            Rect::new(x, rect.y, 0, 0),
        )
    } else {
        (
            Rect::new(x, rect.y + HEADER_TOP, COLUMN_WIDTH, NAME_H),
            Rect::new(x, rect.y + HEADER_TOP + NAME_H, COLUMN_WIDTH, NAME_H),
        )
    };

    // Body: the meter and the trough side by side, centered as a pair, running
    // from under the header to the shared bottom margin.
    let body_top = rect.y + COLUMN_HEADER_HEIGHT + HEADER_GAP;
    let body_bottom = rect.bottom() - COLUMN_BOTTOM;
    let body_h = (body_bottom - body_top).max(1);
    let meter = Rect::new(x + METER_INSET, body_top, METER_W, body_h);
    let track = Rect::new(
        meter.right() + SLIDER_METER_GAP,
        body_top,
        SLIDER_WIDTH,
        body_h,
    );
    let slider = slider_geom(track, cubic);

    // The button stack rides over the pair's right edge, bottom-aligned: mute
    // last, target pins above it.
    let btn_x = (track.right() + FADER_BUTTON_GAP)
        .min(rect.right() - PICK_SIZE)
        .max(x);
    let mute = Rect::new(btn_x, body_bottom - PICK_SIZE, PICK_SIZE, PICK_SIZE);

    // The slider takes the whole trough; the knob's overhang is part of it.
    hits.push(Hit {
        rect: Rect::new(slider.thumb.x, track.y, slider.thumb.w, track.h),
        target: HitTarget::RowSlider(id.clone()),
    });

    let mut targets = Vec::new();
    let mut expand = Rect::new(x, rect.y, 0, 0);
    if is_app {
        // Autoroute first, then one pin per sink, stacked upward from the mute
        // button so the cluster reads bottom-up in the same order every time.
        let count = sinks.len() as i32 + 1;
        let mut by = mute.y - PICKER_MUTE_GAP - PICK_SIZE;
        for i in (0..count).rev() {
            let sink = if i == 0 {
                None
            } else {
                sinks.get((i - 1) as usize).map(|s| s.id)
            };
            let r = Rect::new(btn_x, by, PICK_SIZE, PICK_SIZE);
            targets.push((r, sink));
            hits.push(Hit {
                rect: r,
                target: HitTarget::AppTarget {
                    row: id.clone(),
                    sink,
                },
            });
            by -= PICK_SIZE + PICK_STACK_GAP;
        }
        targets.reverse();
        // The loop leaves `by` one slot above the topmost pin, which is where
        // the expand toggle goes so the cluster keeps reading bottom-up.
        if can_expand {
            expand = Rect::new(btn_x, by, PICK_SIZE, PICK_SIZE);
            hits.push(Hit {
                rect: expand,
                target: HitTarget::AppExpand(id.clone()),
            });
        }
    } else {
        // A device has no pin. Its caps heading is the make-default button, and
        // the accent it wears while default is that button's state.
        hits.push(Hit {
            rect: name,
            target: HitTarget::DeviceDefault(id.clone()),
        });
    }

    // Pushed after the pins so it wins where the two would overlap.
    hits.push(Hit {
        rect: mute,
        target: HitTarget::RowMute(id.clone()),
    });

    ColumnGeom {
        id,
        rect,
        icon,
        name,
        sub,
        slider,
        meter,
        mute,
        targets,
        expand,
        separator: Rect::new(rect.right(), rect.y, COL_GAP, rect.h),
        is_app,
    }
}

/// The meter strip width, re-exported from the meter module's metric.
use crate::ui::meter::METER_WIDTH as METER_W;

/// Project a snapshot into frame geometry for a window rectangle and the
/// current horizontal scroll offset.
///
/// Hits are pushed bottom-up, because [`Layout::hit`] scans them in reverse and
/// the topmost surface has to win: chrome and body first, then the profile
/// dropdown, then any overlay, and finally the resize edges, which stay grabbable
/// no matter what is open.
pub fn project(snapshot: &ViewSnapshot, ui: &UiState, window: Rect) -> Layout {
    let scroll_x = ui.scroll_x;
    let mut hits: Vec<Hit> = Vec::new();

    // Client-drawn chrome takes the top strip; the mixer body gets the rest.
    let (titlebar, content) = if ui.chrome == Chrome::Client {
        let bar = titlebar(window, &mut hits);
        let rest = Rect::new(window.x, bar.bar.bottom(), window.w, window.h - bar.bar.h);
        (Some(bar), rest)
    } else {
        (None, window)
    };

    // The profile selector gets its own strip across the top, unless a drawn
    // titlebar carries it or the toolkit hosts it as a widget of its own.
    let (profile_strip, body) = match ui.chrome {
        Chrome::Client | Chrome::Toolkit => (None, content),
        Chrome::Server => {
            let bar = Rect::new(
                content.x,
                content.y,
                content.w,
                metrics::PROFILE_STRIP_HEIGHT,
            );
            (
                Some(bar),
                Rect::new(content.x, bar.bottom(), content.w, content.h - bar.h),
            )
        }
    };
    // With the selector in the toolkit's header there is no chip to place, and
    // nothing here for a pointer to aim at.
    let prof = (ui.chrome != Chrome::Toolkit).then(|| {
        let chip = profile_chip(profile_strip.unwrap_or(content), titlebar.as_ref());
        hits.push(Hit {
            rect: chip,
            target: HitTarget::ProfileSelector,
        });
        chip
    });

    // The action bar takes a fixed strip down the left; the columns take what
    // is left of the body.
    let cfg = &ui.settings;
    let sidebar = cfg.show_sidebar.then(|| {
        let bar = Rect::new(body.x, body.y, metrics::SIDEBAR_WIDTH, body.h);
        action_bar(bar, cfg, &mut hits);
        bar
    });
    let strip = match sidebar {
        Some(bar) => Rect::new(bar.right(), body.y, body.w - bar.w, body.h),
        None => body,
    };

    // Every column spans the strip's full height, so the meter and slider run
    // its whole depth rather than sitting in a fixed-height block.
    let col_top = strip.y + STRIP_PAD;
    let col_h = (strip.h - STRIP_PAD - STRIP_PAD_BOTTOM).max(metrics::SLIDER_TRACK_HEIGHT);
    let mut x = strip.x - scroll_x;
    let mut columns = Vec::new();
    let mut place = |spec: ColumnSpec, x: &mut i32, columns: &mut Vec<ColumnGeom>| {
        let rect = Rect::new(*x, col_top, metrics::COLUMN_WIDTH, col_h);
        columns.push(column(spec, rect, &mut hits));
        *x += metrics::COLUMN_WIDTH + COL_GAP;
    };
    let device_spec = |row: &DeviceRowView| ColumnSpec {
        id: row.id.clone(),
        cubic: row.cubic,
        is_app: false,
        can_expand: false,
        sinks: &[],
    };
    if snapshot.show_sources {
        for row in &snapshot.sources {
            place(device_spec(row), &mut x, &mut columns);
        }
    }
    if snapshot.show_sinks {
        for row in &snapshot.sinks {
            place(device_spec(row), &mut x, &mut columns);
        }
    }
    if snapshot.show_apps {
        for row in &snapshot.app_rows {
            let spec = ColumnSpec {
                id: row.id.clone(),
                cubic: row.cubic,
                is_app: true,
                can_expand: row.can_expand,
                sinks: &snapshot.sink_options,
            };
            place(spec, &mut x, &mut columns);
        }
    }

    // Total content width to the right edge of the last column.
    let used = x + scroll_x - strip.x;
    let scroll_max_x = (used - strip.w + 2 * STRIP_PAD).max(0);
    let strip_scrollbar =
        (scroll_max_x > 0).then(|| strip_scrollbar(strip, scroll_x, scroll_max_x, &mut hits));

    let profile_menu = prof
        .filter(|_| ui.profile_menu_open)
        .map(|chip| profile_menu(snapshot, chip, &mut hits));

    // Overlays cover the body. Their backdrop hit goes down first so a press
    // that misses the panel dismisses instead of reaching the mixer behind it.
    let palette = snapshot
        .palette
        .open
        .then(|| palette(snapshot, content, &mut hits));
    let modal = snapshot
        .modal
        .as_ref()
        .map(|m| modal(m, content, &mut hits));

    // Resize edges last: a window stays resizable with an overlay open.
    if ui.chrome == Chrome::Client && !ui.maximized {
        resize_edges(window, &mut hits);
    }

    Layout {
        window,
        content,
        profile_strip,
        sidebar,
        strip,
        columns,
        hits,
        profile_menu,
        titlebar,
        palette,
        modal,
        scroll_max_x,
        strip_scrollbar,
    }
}

/// The strip's horizontal scrollbar, in the air the columns leave along the
/// bottom. Slider width is the visible share of the whole run of columns, and
/// its travel maps the scroll offset onto what that leaves.
fn strip_scrollbar(
    strip: Rect,
    scroll_x: i32,
    scroll_max_x: i32,
    hits: &mut Vec<Hit>,
) -> ScrollbarGeom {
    let track = Rect::new(
        strip.x + STRIP_PAD,
        strip.bottom() - SCROLLBAR_LANE,
        (strip.w - 2 * STRIP_PAD).max(1),
        SCROLLBAR_LANE,
    );
    let content_w = strip.w + scroll_max_x;
    let slider_w = ((track.w as i64 * strip.w as i64) / content_w.max(1) as i64) as i32;
    let slider_w = slider_w.clamp(SCROLLBAR_MIN_LEN.min(track.w), track.w);
    let travel = (track.w - slider_w) as f32;
    let progress = scroll_x as f32 / scroll_max_x as f32;
    let slider = Rect::new(
        track.x + (travel * progress).round() as i32,
        track.y + SCROLLBAR_INSET,
        slider_w,
        SCROLLBAR_LANE - 2 * SCROLLBAR_INSET,
    );
    // The whole lane takes the press, so a click beside the slider still lands
    // on the bar rather than the column behind it.
    hits.push(Hit {
        rect: track,
        target: HitTarget::StripScrollbar,
    });
    ScrollbarGeom { track, slider }
}

/// The smallest window that still shows a whole column: the top strip, the
/// padding around the columns, and a column's header, fader, and bottom margin.
/// Both shells hand this to the window manager, so both stop at the same place.
///
/// The top strip is the same height whether the window's chrome carries the
/// profile chip or a strip of its own does, so one number covers both.
pub fn minimum_size() -> (i32, i32) {
    use metrics::{
        COLUMN_BOTTOM, COLUMN_HEADER_HEIGHT, COLUMN_WIDTH, MIN_FADER_HEIGHT, TITLEBAR_HEIGHT,
    };
    let column = COLUMN_HEADER_HEIGHT + HEADER_GAP + MIN_FADER_HEIGHT + COLUMN_BOTTOM;
    (
        COLUMN_WIDTH + 40,
        TITLEBAR_HEIGHT + STRIP_PAD + column + STRIP_PAD_BOTTOM,
    )
}

/// A window size grown to [`minimum_size`]. The minimum a toplevel declares is
/// a hint a compositor may configure straight past, so every size handed to the
/// window goes through here rather than being taken at its word.
pub fn at_least_minimum(w: i32, h: i32) -> (i32, i32) {
    let (min_w, min_h) = minimum_size();
    (w.max(min_w), h.max(min_h))
}

/// Where the profile chip sits: in the window's own titlebar when there is one,
/// otherwise at the left of its own strip.
fn profile_chip(bar: Rect, titlebar: Option<&TitlebarGeom>) -> Rect {
    use metrics::{PROFILE_CHIP_PAD, PROFILE_CHIP_WIDTH};
    let host = titlebar.map_or(bar, |t| t.title);
    let h = (host.h - 8).max(1);
    let w = PROFILE_CHIP_WIDTH.min((host.w - PROFILE_CHIP_PAD).max(0));
    Rect::new(host.x + PROFILE_CHIP_PAD, host.y + (host.h - h) / 2, w, h)
}

/// Push the left action bar's buttons: the section filters, then a gap, then
/// the mute-all and reset actions. Hidden buttons are not laid out at all, so
/// the ones that remain close the gap.
fn action_bar(bar: Rect, cfg: &crate::settings::Settings, hits: &mut Vec<Hit>) {
    use metrics::{ACTION_BTN_HEIGHT, ACTION_BTN_WIDTH, ACTION_CLUSTER_GAP, ACTION_GAP};
    let x = bar.x + (bar.w - ACTION_BTN_WIDTH) / 2;
    let mut y = bar.y + metrics::SIDEBAR_PAD;

    let mut any_filter = false;
    for (section, shown) in [
        (Section::Inputs, cfg.show_input_button),
        (Section::Outputs, cfg.show_output_button),
        (Section::Apps, cfg.show_apps_button),
    ] {
        if !shown {
            continue;
        }
        hits.push(Hit {
            rect: Rect::new(x, y, ACTION_BTN_WIDTH, ACTION_BTN_HEIGHT),
            target: HitTarget::SectionFilter(section),
        });
        y += ACTION_BTN_HEIGHT + ACTION_GAP;
        any_filter = true;
    }

    let actions = [
        (HitTarget::MuteAll, cfg.show_mute_button),
        (HitTarget::ResetTargets, cfg.show_reset_button),
    ];
    if any_filter && actions.iter().any(|(_, shown)| *shown) {
        y += ACTION_CLUSTER_GAP;
    }
    for (target, shown) in actions {
        if !shown {
            continue;
        }
        hits.push(Hit {
            rect: Rect::new(x, y, ACTION_BTN_WIDTH, ACTION_BTN_HEIGHT),
            target,
        });
        y += ACTION_BTN_HEIGHT + ACTION_GAP;
    }
}

/// Lay out the client-drawn titlebar: a drag strip with window buttons on the
/// right, and the title filling what is left.
fn titlebar(window: Rect, hits: &mut Vec<Hit>) -> TitlebarGeom {
    use metrics::{TITLEBAR_HEIGHT, WINDOW_BTN};
    let bar = Rect::new(window.x, window.y, window.w, TITLEBAR_HEIGHT);
    let pad = (TITLEBAR_HEIGHT - WINDOW_BTN) / 2;
    let y = bar.y + pad;

    // Right to left: close, maximize, minimize.
    let close = Rect::new(bar.right() - pad - WINDOW_BTN, y, WINDOW_BTN, WINDOW_BTN);
    let maximize = Rect::new(close.x - WINDOW_BTN, y, WINDOW_BTN, WINDOW_BTN);
    let minimize = Rect::new(maximize.x - WINDOW_BTN, y, WINDOW_BTN, WINDOW_BTN);
    let title = Rect::new(
        bar.x + STRIP_PAD,
        bar.y,
        minimize.x - bar.x - STRIP_PAD,
        bar.h,
    );

    // The drag strip goes down first so the buttons on top of it win.
    hits.push(Hit {
        rect: bar,
        target: HitTarget::TitlebarDrag,
    });
    for (rect, target) in [
        (minimize, HitTarget::WindowMinimize),
        (maximize, HitTarget::WindowMaximize),
        (close, HitTarget::WindowClose),
    ] {
        hits.push(Hit { rect, target });
    }

    TitlebarGeom {
        bar,
        title,
        minimize,
        maximize,
        close,
    }
}

/// Push the eight resize grabs around a window. Corners go down after edges so
/// they win where the two overlap.
fn resize_edges(window: Rect, hits: &mut Vec<Hit>) {
    use ResizeEdge::*;
    let g = metrics::RESIZE_GRAB;
    let c = g * 2;
    let (x, y, w, h) = (window.x, window.y, window.w, window.h);

    let edges = [
        (Rect::new(x, y, w, g), Top),
        (Rect::new(x, window.bottom() - g, w, g), Bottom),
        (Rect::new(x, y, g, h), Left),
        (Rect::new(window.right() - g, y, g, h), Right),
    ];
    let corners = [
        (Rect::new(x, y, c, c), TopLeft),
        (Rect::new(window.right() - c, y, c, c), TopRight),
        (Rect::new(x, window.bottom() - c, c, c), BottomLeft),
        (
            Rect::new(window.right() - c, window.bottom() - c, c, c),
            BottomRight,
        ),
    ];
    for (rect, edge) in edges.into_iter().chain(corners) {
        hits.push(Hit {
            rect,
            target: HitTarget::ResizeEdge(edge),
        });
    }
}

/// Lay out the command palette: a search field over a scrolling window of the
/// matching command rows.
fn palette(snapshot: &ViewSnapshot, content: Rect, hits: &mut Vec<Hit>) -> PaletteGeom {
    use metrics::PALETTE_ROW_H;
    hits.push(Hit {
        rect: content,
        target: HitTarget::Backdrop,
    });

    let w = (content.w - 80).clamp(120, 520);
    let x = content.x + (content.w - w) / 2;

    // The list shows a fixed number of rows and scrolls the rest, so a long
    // match list keeps the panel the same height as a short one. A window too
    // short for that many rows gets what fits.
    let total = snapshot.palette.rows.len();
    let room = ((content.h - 40 - PALETTE_CHROME_H) / PALETTE_ROW_H).max(1) as usize;
    let capacity = total.min(crate::command_palette::VISIBLE_ROWS).min(room);
    let first = crate::command_palette::scroll_into_view(
        snapshot.palette.scroll.min(total - capacity),
        snapshot.palette.selected,
        capacity,
    );

    let h = PALETTE_CHROME_H + capacity.max(1) as i32 * PALETTE_ROW_H;
    let panel = Rect::new(x, content.y + 40, w, h);
    let input = Rect::new(x + 10, panel.y + 8, w - 20, FIELD_H);
    hits.push(Hit {
        rect: input,
        target: HitTarget::PaletteInput,
    });

    // The scrollbar takes its lane out of the rows rather than sitting over
    // them, so a long label never runs under the slider.
    let scrolls = total > capacity;
    let row_w = if scrolls {
        w - 12 - SCROLLBAR_LANE
    } else {
        w - 12
    };

    let list_top = input.bottom() + 6;
    let mut y = list_top;
    let mut rows = Vec::with_capacity(capacity);
    for i in first..first + capacity {
        let r = Rect::new(x + 6, y, row_w, PALETTE_ROW_H - 2);
        hits.push(Hit {
            rect: r,
            target: HitTarget::PaletteRow(i),
        });
        rows.push(r);
        y += PALETTE_ROW_H;
    }

    let scrollbar = scrolls.then(|| {
        let track = Rect::new(
            panel.right() - 6 - SCROLLBAR_LANE,
            list_top,
            SCROLLBAR_LANE,
            capacity as i32 * PALETTE_ROW_H,
        );
        // Slider length is the visible fraction of the list, floored so it
        // stays grabbable, and its travel maps the scroll offset onto whatever
        // room that leaves.
        let shown = capacity as f32 / total as f32;
        let slider_h = ((track.h as f32 * shown).round() as i32).clamp(SCROLLBAR_MIN_LEN, track.h);
        let travel = (track.h - slider_h) as f32;
        let progress = first as f32 / (total - capacity) as f32;
        let slider = Rect::new(
            track.x + SCROLLBAR_INSET,
            track.y + (travel * progress).round() as i32,
            SCROLLBAR_LANE - 2 * SCROLLBAR_INSET,
            slider_h,
        );
        ScrollbarGeom { track, slider }
    });

    PaletteGeom {
        panel,
        input,
        rows,
        first_visible: first,
        scrollbar,
        empty: Rect::new(x + 14, y, w - 28, PALETTE_ROW_H),
    }
}

/// Lay out a modal dialog: title, body, optional name field, error line, and the
/// cancel/confirm pair along the bottom.
fn modal(view: &ModalView, content: Rect, hits: &mut Vec<Hit>) -> ModalGeom {
    hits.push(Hit {
        rect: content,
        target: HitTarget::Backdrop,
    });

    let w = MODAL_W.min(content.w - 40);
    let h = if view.input_visible { 150 } else { 130 };
    let x = content.x + (content.w - w) / 2;
    let y = content.y + (content.h - h) / 2;
    let panel = Rect::new(x, y, w, h);

    let title = Rect::new(x + 16, y + 14, w - 32, 20);
    let mut cy = y + 40;
    let body = Rect::new(x + 16, cy, w - 32, 20);
    if !view.body.is_empty() {
        cy += 24;
    }
    let input = view.input_visible.then(|| {
        let r = Rect::new(x + 16, cy, w - 32, FIELD_H);
        hits.push(Hit {
            rect: r,
            target: HitTarget::ModalInput,
        });
        cy += 38;
        r
    });
    let error = Rect::new(x + 16, cy, w - 32, 20);

    let bh = 30;
    let by = y + h - bh - 12;
    let bw = (w - 40) / 2;
    let cancel = Rect::new(x + 16, by, bw, bh);
    let confirm = Rect::new(x + w - 16 - bw, by, bw, bh);
    hits.push(Hit {
        rect: cancel,
        target: HitTarget::ModalCancel,
    });
    hits.push(Hit {
        rect: confirm,
        target: HitTarget::ModalConfirm,
    });

    ModalGeom {
        panel,
        title,
        body,
        input,
        error,
        cancel,
        confirm,
    }
}

/// Lay the profile dropdown out under its chip and push its hit rectangles.
fn profile_menu(snapshot: &ViewSnapshot, chip: Rect, hits: &mut Vec<Hit>) -> ProfileMenuGeom {
    let width = chip.w.max(MENU_MIN_W);
    let count = snapshot.profile.rows.len() as i32;
    // Every profile, a rule, then the row that creates a new one.
    let height = MENU_PAD * 2 + (count + 1) * MENU_ROW_H + MENU_SEP_H;
    let panel = Rect::new(chip.x, chip.bottom() + MENU_PAD, width, height);

    let mut y = panel.y + MENU_PAD;
    let mut rows = Vec::with_capacity(snapshot.profile.rows.len());
    for row in &snapshot.profile.rows {
        // A whole row applies its profile. Renaming and deleting live in the
        // command palette, so there is nothing else here to aim at.
        let full = Rect::new(panel.x, y, width, MENU_ROW_H);
        hits.push(Hit {
            rect: full,
            target: HitTarget::ProfileApply(row.name.clone()),
        });

        rows.push(ProfileRowGeom {
            name: row.name.clone(),
            rect: full,
            active: row.active,
        });
        y += MENU_ROW_H;
    }

    // A rule sets making a profile apart from picking one.
    let separator = Rect::new(panel.x, y, width, MENU_SEP_H);
    y += MENU_SEP_H;

    let create = Rect::new(panel.x, y, width, MENU_ROW_H);
    hits.push(Hit {
        rect: create,
        target: HitTarget::ProfileCreate,
    });
    ProfileMenuGeom {
        panel,
        rows,
        separator,
        create,
    }
}

impl Layout {
    /// The hit target at a point, topmost (last-pushed) first.
    pub fn hit(&self, x: i32, y: i32) -> Option<&HitTarget> {
        self.hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(x, y))
            .map(|h| &h.target)
    }

    /// The text field the focused overlay is editing, if one is open. A drag
    /// keeps mapping to it after the pointer leaves the rectangle.
    pub fn focused_field(&self, focus: Focus) -> Option<Rect> {
        match focus {
            Focus::Palette => self.palette.as_ref().map(|p| p.input),
            Focus::Modal => self.modal.as_ref().and_then(|m| m.input),
            Focus::Body => None,
        }
    }

    /// The rectangles a meter step can change.
    ///
    /// Meter levels reach the frame through one drawing call, a column's meter,
    /// so two frames that differ by a decay step differ only inside these. They
    /// are clipped to the strip because a scrolled column hangs past its edge,
    /// and one scrolled fully out contributes nothing.
    pub fn meter_damage(&self) -> impl Iterator<Item = Rect> + '_ {
        self.columns
            .iter()
            .map(|col| col.meter.intersect(self.strip))
            .filter(|r| !r.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use crate::state;
    use crate::view::snapshot::build_snapshot;

    fn snapshot_with_profiles(names: &[&str], active: Option<&str>) -> ViewSnapshot {
        let mut app = state::empty();
        // A fresh app always carries an implicit default profile; drop it so the
        // test asserts against exactly the profiles it set up.
        app.profiles.profiles.clear();
        for name in names {
            app.profiles.profiles.push(Profile {
                name: (*name).to_string(),
                ..Default::default()
            });
        }
        app.profiles.active = active.map(str::to_string);
        build_snapshot(&app, |_| None)
    }

    fn content() -> Rect {
        Rect::new(0, 0, 560, 720)
    }

    #[test]
    fn the_profile_menu_is_absent_until_it_is_opened() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());
        assert!(layout.profile_menu.is_none());
        assert!(
            !layout
                .hits
                .iter()
                .any(|h| matches!(h.target, HitTarget::ProfileApply(_))),
            "closed menu offers no apply targets"
        );
    }

    #[test]
    fn an_open_menu_lays_out_a_row_per_profile_plus_a_create_row() {
        let snap = snapshot_with_profiles(&["Gaming", "Music"], Some("Music"));
        let mut ui = UiState::new();
        ui.profile_menu_open = true;
        let layout = project(&snap, &ui, content());

        let menu = layout.profile_menu.as_ref().expect("menu is laid out");
        assert_eq!(menu.rows.len(), 2);
        assert_eq!(menu.rows[0].name, "Gaming");
        assert!(menu.rows[1].active, "the active profile is marked");
        // The create row sits below the last profile, past the rule.
        assert!(menu.create.y >= menu.rows[1].rect.bottom());
        // The panel encloses every row.
        for row in &menu.rows {
            assert!(row.rect.y >= menu.panel.y);
            assert!(row.rect.bottom() <= menu.panel.bottom());
        }
    }

    #[test]
    fn the_rule_separates_the_profiles_from_the_create_row() {
        let snap = snapshot_with_profiles(&["Gaming", "Work"], Some("Work"));
        let mut ui = UiState::new();
        ui.profile_menu_open = true;
        let layout = project(&snap, &ui, content());
        let menu = layout.profile_menu.as_ref().expect("menu");

        let last = menu.rows.last().expect("a profile row");
        assert!(
            menu.separator.y >= last.rect.bottom(),
            "the rule sits below the last profile",
        );
        assert!(
            menu.create.y >= menu.separator.bottom(),
            "the create row sits below the rule",
        );
        assert_eq!(menu.separator.w, menu.panel.w, "the rule spans the panel");
        assert!(menu.separator.h >= 1, "the rule is drawn");
        assert!(
            menu.separator.bottom() <= menu.panel.bottom(),
            "the rule stays inside the panel",
        );
    }

    #[test]
    fn menu_rows_hit_test_to_their_actions() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let mut ui = UiState::new();
        ui.profile_menu_open = true;
        let layout = project(&snap, &ui, content());
        let menu = layout.profile_menu.as_ref().expect("menu");
        let row = &menu.rows[0];

        let centre = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        // The whole row applies, including the right edge where the rename and
        // delete buttons used to sit.
        let (x, y) = centre(row.rect);
        assert_eq!(
            layout.hit(x, y),
            Some(&HitTarget::ProfileApply("Gaming".into()))
        );
        let right_edge = (row.rect.right() - 2, row.rect.y + row.rect.h / 2);
        assert_eq!(
            layout.hit(right_edge.0, right_edge.1),
            Some(&HitTarget::ProfileApply("Gaming".into())),
            "no affordance carves a hole out of the row",
        );
        let (x, y) = centre(menu.create);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::ProfileCreate));
    }

    /// A scene with a single default sink.
    fn mixer_scene() -> ViewSnapshot {
        let mut app = state::empty();
        app.streams.insert(
            1,
            crate::domain::Stream {
                name: "Speaker".into(),
                node_name: Some("node.1".into()),
                form: Some(crate::domain::DeviceForm::Output(
                    crate::domain::SinkForm::Speaker,
                )),
                is_default: true,
                ..crate::domain::sample_stream(1, crate::domain::StreamKind::Sink)
            },
        );
        build_snapshot(&app, |_| None)
    }

    /// A sink and an app stream, so an app column carries a pin per sink plus
    /// the autoroute pin.
    fn sink_and_app_scene() -> ViewSnapshot {
        let mut app = state::empty();
        for (id, kind, name) in [
            (1, crate::domain::StreamKind::Sink, "Speaker"),
            (7, crate::domain::StreamKind::Application, "Player"),
        ] {
            app.streams.insert(
                id,
                crate::domain::Stream {
                    name: name.into(),
                    node_name: Some(format!("node.{id}")),
                    is_default: kind == crate::domain::StreamKind::Sink,
                    ..crate::domain::sample_stream(id, kind)
                },
            );
        }
        build_snapshot(&app, |_| None)
    }

    /// The action bar is a vertical strip down the left, not a row across the
    /// top: its buttons stack, and the columns begin to its right.
    #[test]
    fn the_action_bar_stacks_down_the_left_and_the_columns_start_beside_it() {
        let snap = mixer_scene();
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());

        let bar = layout.sidebar.expect("the action bar is laid out");
        assert_eq!(bar.x, layout.content.x, "it is flush left");
        assert_eq!(bar.h, layout.content.h - bar.y + layout.content.y);
        assert_eq!(layout.strip.x, bar.right(), "columns begin beside it");

        let picks: Vec<Rect> = layout
            .hits
            .iter()
            .filter(|h| {
                matches!(
                    h.target,
                    HitTarget::SectionFilter(_) | HitTarget::MuteAll | HitTarget::ResetTargets
                )
            })
            .map(|h| h.rect)
            .collect();
        assert_eq!(picks.len(), 5, "three filters and two actions");
        for pair in picks.windows(2) {
            assert_eq!(pair[0].x, pair[1].x, "one aligned column of buttons");
            assert!(pair[1].y >= pair[0].bottom(), "stacked, not overlapping");
        }
        for r in &picks {
            assert!(r.x >= bar.x && r.right() <= bar.right(), "inside the bar");
        }
    }

    /// The profile chip lives in its own strip above the body, and the body
    /// starts below it.
    #[test]
    fn the_profile_chip_gets_a_strip_of_its_own() {
        let snap = mixer_scene();
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());

        let bar = layout.profile_strip.expect("the profile strip is laid out");
        assert_eq!(bar.y, layout.content.y);
        assert_eq!(bar.h, metrics::PROFILE_STRIP_HEIGHT);
        assert_eq!(layout.strip.y, bar.bottom(), "the body starts below it");

        let chip = layout
            .hits
            .iter()
            .find(|h| h.target == HitTarget::ProfileSelector)
            .expect("chip")
            .rect;
        assert!(chip.y >= bar.y && chip.bottom() <= bar.bottom());
        assert!(chip.x < bar.x + bar.w / 2, "it sits at the left");
    }

    /// The damage set is one rectangle per column, held inside the strip. A
    /// meter that spilled past it would repaint over the toolbar.
    #[test]
    fn meter_damage_is_every_column_meter_inside_the_strip() {
        let snap = mixer_scene();
        let ui = UiState::new();
        let layout = project(&snap, &ui, Rect::new(0, 0, 560, 720));
        assert!(!layout.columns.is_empty(), "the scene lays out no columns");

        let damage: Vec<Rect> = layout.meter_damage().collect();
        assert_eq!(damage.len(), layout.columns.len());
        for (rect, col) in damage.iter().zip(&layout.columns) {
            assert_eq!(*rect, col.meter.intersect(layout.strip));
            assert_eq!(*rect, rect.intersect(layout.strip), "inside the strip");
        }
    }

    /// A column scrolled out of sight contributes no damage, so a mixer with
    /// more columns than fit repaints only the ones on screen.
    #[test]
    fn meter_damage_drops_a_column_scrolled_out_of_the_strip() {
        let snap = mixer_scene();
        let mut ui = UiState::new();
        let layout = project(&snap, &ui, Rect::new(0, 0, 560, 720));
        let visible = layout.meter_damage().count();

        ui.scroll_x = layout.strip.w + metrics::COLUMN_WIDTH;
        let scrolled = project(&snap, &ui, Rect::new(0, 0, 560, 720));
        assert!(
            scrolled.meter_damage().count() < visible,
            "scrolling the strip past its columns still damages {visible} of them",
        );
    }

    /// The meter and the fader run the column's whole depth, so a taller window
    /// gives a longer throw rather than more empty space.
    #[test]
    fn the_meter_and_fader_fill_the_column_and_grow_with_the_window() {
        let snap = mixer_scene();
        let ui = UiState::new();
        let short = project(&snap, &ui, Rect::new(0, 0, 560, 500));
        let tall = project(&snap, &ui, Rect::new(0, 0, 560, 900));

        let col = |l: &Layout| -> (Rect, Rect) {
            let c = &l.columns[0];
            (c.meter, c.slider.track)
        };
        let (m_short, s_short) = col(&short);
        let (m_tall, s_tall) = col(&tall);

        assert_eq!(m_short.y, s_short.y, "meter and fader share a top");
        assert_eq!(m_short.bottom(), s_short.bottom(), "and a bottom");
        assert_eq!(m_short.w, crate::ui::meter::METER_WIDTH);
        assert_eq!(s_short.w, metrics::SLIDER_WIDTH);
        assert!(
            s_tall.h > s_short.h + 300,
            "400px more window gives a longer throw ({} vs {})",
            s_tall.h,
            s_short.h
        );
        assert!(m_tall.h == s_tall.h, "they stay the same length");
    }

    /// A device has no default pin: its caps heading is the button, which is
    /// why the heading takes the accent while it is the default.
    #[test]
    fn a_devices_heading_is_its_make_default_button() {
        let snap = mixer_scene();
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());
        let col = &layout.columns[0];

        let (x, y) = (col.name.x + col.name.w / 2, col.name.y + col.name.h / 2);
        assert_eq!(
            layout.hit(x, y),
            Some(&HitTarget::DeviceDefault(RowId::Sink(1)))
        );
        assert!(
            col.targets.is_empty(),
            "a device carries no target pins, only apps do"
        );
    }

    /// The mute button beats the target pins where they meet, so the bottom of
    /// the stack is never stolen by the pin above it.
    #[test]
    fn the_mute_button_wins_over_the_pin_stack() {
        let mut app = state::empty();
        app.streams.insert(
            7,
            crate::domain::Stream {
                name: "Player".into(),
                app_id: Some("com.example.Player".into()),
                node_name: Some("node.7".into()),
                ..crate::domain::sample_stream(7, crate::domain::StreamKind::Application)
            },
        );
        let snap = build_snapshot(&app, |_| None);
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());
        let col = &layout.columns[0];

        let (x, y) = (col.mute.x + col.mute.w / 2, col.mute.y + col.mute.h / 2);
        assert!(matches!(layout.hit(x, y), Some(HitTarget::RowMute(_))));
        // The pins stack upward, clear of the mute button.
        for (rect, _) in &col.targets {
            assert!(rect.bottom() <= col.mute.y, "a pin sits above the mute");
            assert_eq!(rect.x, col.mute.x, "one aligned stack");
        }
    }

    #[test]
    fn the_pins_sit_tighter_to_each_other_than_to_the_mute_button() {
        // The spacing is what says the pins are one set of linked choices and
        // the mute is not part of it, so the two gaps must stay distinct.
        let snap = sink_and_app_scene();
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());
        let col = layout
            .columns
            .iter()
            .find(|c| c.targets.len() >= 2)
            .expect("an app column with at least two pins");

        let mut pins: Vec<Rect> = col.targets.iter().map(|(r, _)| *r).collect();
        pins.sort_by_key(|r| r.y);
        for pair in pins.windows(2) {
            assert_eq!(
                pair[1].y - pair[0].bottom(),
                metrics::PICK_STACK_GAP,
                "pins are spaced by the stack gap",
            );
        }

        let lowest = pins.last().expect("a pin");
        assert_eq!(
            col.mute.y - lowest.bottom(),
            metrics::PICKER_MUTE_GAP,
            "the mute button sits a wider gap below the stack",
        );
    }

    #[test]
    fn server_side_chrome_leaves_the_whole_window_to_the_mixer() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());
        assert!(layout.titlebar.is_none());
        assert_eq!(layout.content, layout.window, "no strip is reserved");
        assert!(
            !layout
                .hits
                .iter()
                .any(|h| matches!(h.target, HitTarget::ResizeEdge(_) | HitTarget::TitlebarDrag)),
            "a server-decorated window grows no chrome targets"
        );
    }

    /// Under a toolkit that hosts the selector itself, the surface owes the
    /// frame neither a titlebar nor the strip the chip would otherwise need.
    #[test]
    fn toolkit_chrome_drops_the_profile_strip_along_with_the_titlebar() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Toolkit;
        let layout = project(&snap, &ui, content());

        assert!(layout.titlebar.is_none());
        assert!(
            layout.profile_strip.is_none(),
            "the toolkit's header carries the selector"
        );
        assert_eq!(
            layout.content, layout.window,
            "so the mixer gets the whole surface"
        );
        assert!(
            !layout
                .hits
                .iter()
                .any(|h| h.target == HitTarget::ProfileSelector),
            "and there is no painted chip to aim at"
        );
    }

    /// The strip exists only to host the chip, so it comes back with it.
    #[test]
    fn server_side_chrome_keeps_the_profile_strip() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());

        let strip = layout.profile_strip.expect("a strip of its own");
        assert_eq!(strip.h, metrics::PROFILE_STRIP_HEIGHT);
        assert!(
            layout
                .hits
                .iter()
                .any(|h| h.target == HitTarget::ProfileSelector),
            "with the chip in it"
        );
    }

    /// Opening the menu is pointer-driven through the chip, so a chip-less
    /// shell cannot end up painting a dropdown with nothing to hang it under.
    #[test]
    fn toolkit_chrome_paints_no_dropdown_even_if_the_flag_is_set() {
        let snap = snapshot_with_profiles(&["Gaming", "Work"], None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Toolkit;
        ui.profile_menu_open = true;
        let layout = project(&snap, &ui, content());

        assert!(layout.profile_menu.is_none());
        assert!(
            !layout
                .hits
                .iter()
                .any(|h| matches!(h.target, HitTarget::ProfileApply(_))),
        );
    }

    #[test]
    fn client_side_chrome_reserves_a_titlebar_and_pushes_the_body_down() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Client;
        let layout = project(&snap, &ui, content());

        let bar = layout.titlebar.as_ref().expect("titlebar is laid out");
        assert_eq!(bar.bar.h, metrics::TITLEBAR_HEIGHT);
        assert_eq!(
            layout.content.y,
            bar.bar.bottom(),
            "the body starts below it"
        );
        assert_eq!(
            layout.content.h,
            layout.window.h - bar.bar.h,
            "and the body is shorter by exactly the bar"
        );

        // The three buttons sit inside the bar, in order, clear of the title.
        assert!(bar.minimize.right() <= bar.maximize.x);
        assert!(bar.maximize.right() <= bar.close.x);
        assert!(bar.close.right() <= bar.bar.right());
        assert!(bar.title.right() <= bar.minimize.x);
        for r in [bar.minimize, bar.maximize, bar.close] {
            assert!(r.y >= bar.bar.y && r.bottom() <= bar.bar.bottom());
        }
    }

    /// The buttons sit on top of the drag strip, and the strip only claims what
    /// they leave over.
    #[test]
    fn titlebar_buttons_win_over_the_drag_strip() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Client;
        let layout = project(&snap, &ui, content());
        let bar = layout.titlebar.as_ref().expect("titlebar");

        let centre = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        for (rect, want) in [
            (bar.minimize, HitTarget::WindowMinimize),
            (bar.maximize, HitTarget::WindowMaximize),
            (bar.close, HitTarget::WindowClose),
        ] {
            let (x, y) = centre(rect);
            assert_eq!(layout.hit(x, y), Some(&want));
        }
        let (x, y) = centre(bar.title);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::TitlebarDrag));
    }

    #[test]
    fn resize_edges_exist_only_while_client_decorated_and_not_maximized() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let edges = |ui: &UiState| {
            project(&snap, ui, content())
                .hits
                .iter()
                .filter(|h| matches!(h.target, HitTarget::ResizeEdge(_)))
                .count()
        };
        let mut ui = UiState::new();
        assert_eq!(edges(&ui), 0, "the compositor handles server-side resizing");
        ui.chrome = Chrome::Client;
        assert_eq!(edges(&ui), 8, "four edges and four corners");
        ui.maximized = true;
        assert_eq!(edges(&ui), 0, "a maximized window has nothing to drag");
    }

    /// A corner has to beat the two edges it overlaps, or dragging it would
    /// resize in one axis only.
    #[test]
    fn corners_win_over_the_edges_they_overlap() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Client;
        let layout = project(&snap, &ui, content());
        let w = layout.window;
        use ResizeEdge::*;
        for (x, y, want) in [
            (w.x + 1, w.y + 1, TopLeft),
            (w.right() - 2, w.y + 1, TopRight),
            (w.x + 1, w.bottom() - 2, BottomLeft),
            (w.right() - 2, w.bottom() - 2, BottomRight),
        ] {
            assert_eq!(layout.hit(x, y), Some(&HitTarget::ResizeEdge(want)));
        }
        // Mid-edge is still the plain edge.
        let mid = w.y + w.h / 2;
        assert_eq!(layout.hit(w.x, mid), Some(&HitTarget::ResizeEdge(Left)));
    }

    /// An overlay covers the mixer, so nothing behind it may take a press, but
    /// the window must still be resizable from its edge.
    #[test]
    fn an_open_overlay_swallows_the_body_but_not_the_resize_edges() {
        let mut app = state::empty();
        app.palette = Some(state::CommandPalette::default());
        let snap = build_snapshot(&app, |_| None);
        let mut ui = UiState::new();
        ui.chrome = Chrome::Client;
        let layout = project(&snap, &ui, content());

        let geom = layout.palette.as_ref().expect("palette is laid out");
        // The field and the panel belong to the overlay.
        let (x, y) = (geom.input.x + 4, geom.input.y + geom.input.h / 2);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::PaletteInput));
        // Well inside the body, clear of the panel: the backdrop, not a column.
        let deep = layout.content.bottom() - metrics::RESIZE_GRAB - 2;
        assert_eq!(
            layout.hit(layout.content.w / 2, deep),
            Some(&HitTarget::Backdrop)
        );
        // The very edge still resizes.
        assert!(matches!(
            layout.hit(layout.window.w / 2, layout.window.bottom() - 1),
            Some(HitTarget::ResizeEdge(_))
        ));
    }

    #[test]
    fn a_modal_lays_out_its_field_and_buttons_and_claims_them() {
        let mut app = state::empty();
        state::update(
            &mut app,
            state::Message::OpenCreateProfileModal,
            &mut Vec::new(),
        );
        let snap = build_snapshot(&app, |_| None);
        let ui = UiState::new();
        let layout = project(&snap, &ui, content());

        let geom = layout.modal.as_ref().expect("modal is laid out");
        let input = geom.input.expect("the create dialog has a name field");
        let centre = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        let (x, y) = centre(input);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::ModalInput));
        let (x, y) = centre(geom.cancel);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::ModalCancel));
        let (x, y) = centre(geom.confirm);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::ModalConfirm));
        // Everything sits inside the panel, and the buttons do not overlap.
        for r in [geom.title, input, geom.cancel, geom.confirm] {
            assert!(r.x >= geom.panel.x && r.right() <= geom.panel.right());
            assert!(r.y >= geom.panel.y && r.bottom() <= geom.panel.bottom());
        }
        assert!(geom.cancel.right() <= geom.confirm.x);
    }

    #[test]
    fn hidden_toolbar_buttons_are_not_laid_out_and_the_rest_close_the_gap() {
        let snap = snapshot_with_profiles(&["Gaming"], None);
        let filters = |ui: &UiState| {
            project(&snap, ui, content())
                .hits
                .iter()
                .filter(|h| matches!(h.target, HitTarget::SectionFilter(_)))
                .count()
        };
        let ui = UiState::new();
        assert_eq!(filters(&ui), 3);

        let mut hidden = UiState::new();
        hidden.settings.show_input_button = false;
        assert_eq!(filters(&hidden), 2);
        // The two that remain start where the first one used to.
        let full = project(&snap, &ui, content());
        let trimmed = project(&snap, &hidden, content());
        let first = |l: &Layout| {
            l.hits
                .iter()
                .find(|h| matches!(h.target, HitTarget::SectionFilter(_)))
                .map(|h| h.rect.x)
        };
        assert_eq!(first(&full), first(&trimmed), "no hole is left behind");

        let mut off = UiState::new();
        off.settings.show_sidebar = false;
        assert_eq!(filters(&off), 0, "the whole toolbar can be hidden");
    }

    /// The dropdown floats over the column strip, so a click where they overlap
    /// belongs to the menu, not to whatever row is underneath.
    #[test]
    fn the_open_menu_takes_hits_from_the_columns_beneath_it() {
        let mut app = state::empty();
        app.profiles.profiles.clear();
        app.profiles.profiles.push(Profile {
            name: "Gaming".into(),
            ..Default::default()
        });
        // A sink column so there is something under the dropdown to steal from.
        app.streams.insert(
            1,
            crate::domain::Stream {
                name: "Speaker".into(),
                node_name: Some("node.1".into()),
                ..crate::domain::sample_stream(1, crate::domain::StreamKind::Sink)
            },
        );
        let snap = build_snapshot(&app, |_| None);

        let mut ui = UiState::new();
        ui.profile_menu_open = true;
        let layout = project(&snap, &ui, content());
        let menu = layout.profile_menu.as_ref().expect("menu");
        let row = &menu.rows[0];
        let (x, y) = (row.rect.x + 4, row.rect.y + row.rect.h / 2);

        assert_eq!(
            layout.hit(x, y),
            Some(&HitTarget::ProfileApply("Gaming".into())),
            "the overlay wins over the strip underneath"
        );
    }

    /// A mixer with `count` app columns, wide enough to overrun a narrow
    /// window.
    fn columns_snapshot(count: u32) -> ViewSnapshot {
        let mut app = state::empty();
        for id in 1..=count {
            app.streams.insert(
                id,
                crate::domain::Stream {
                    name: format!("App {id}"),
                    app_id: Some(format!("com.example.App{id}")),
                    node_name: Some(format!("node.{id}")),
                    ..crate::domain::sample_stream(id, crate::domain::StreamKind::Application)
                },
            );
        }
        build_snapshot(&app, |_| None)
    }

    /// A window too narrow for `count` columns, and the layout it projects to.
    fn narrow(count: u32, scroll_x: i32) -> Layout {
        let snap = columns_snapshot(count);
        let mut ui = UiState::new();
        ui.scroll_x = scroll_x;
        project(&snap, &ui, Rect::new(0, 0, 300, 400))
    }

    /// The minimum is only worth advertising if a column actually fits in it:
    /// header, fader, and the mute button under it, all inside the strip.
    #[test]
    fn a_window_at_the_minimum_size_still_holds_a_whole_column() {
        let (w, h) = minimum_size();
        let snap = columns_snapshot(1);
        let layout = project(&snap, &UiState::new(), Rect::new(0, 0, w, h));
        let col = layout.columns.first().expect("a column");

        assert!(
            col.rect.bottom() <= layout.strip.bottom(),
            "column {:?} runs past the strip {:?}",
            col.rect,
            layout.strip
        );
        assert!(
            col.slider.track.h >= metrics::MIN_FADER_HEIGHT,
            "fader is {} tall, under the {} the minimum promises",
            col.slider.track.h,
            metrics::MIN_FADER_HEIGHT
        );
        assert!(
            col.mute.bottom() <= col.rect.bottom(),
            "the mute button hangs out of its column"
        );
    }

    /// The declared minimum is a hint, so a compositor can hand over anything.
    /// What keeps the window off the floor is the size being grown on the way
    /// in, not the hint being honoured.
    #[test]
    fn a_size_under_the_minimum_is_grown_to_it() {
        let (min_w, min_h) = minimum_size();
        assert_eq!(at_least_minimum(1, 1), (min_w, min_h));
        assert_eq!(at_least_minimum(2000, 40), (2000, min_h));
        assert_eq!(at_least_minimum(40, 2000), (min_w, 2000));
        // A window already big enough is handed back untouched.
        assert_eq!(at_least_minimum(800, 600), (800, 600));
    }

    /// Both shells advertise the same minimum, and the fader is what sets it,
    /// so a change to one number moves the window's floor with it.
    #[test]
    fn the_minimum_height_is_the_chrome_plus_a_column() {
        let (_, h) = minimum_size();
        let taller = metrics::MIN_FADER_HEIGHT + 10;
        let snap = columns_snapshot(1);
        let layout = project(&snap, &UiState::new(), Rect::new(0, 0, 300, h + 10));
        let col = layout.columns.first().expect("a column");
        assert_eq!(
            col.slider.track.h, taller,
            "ten pixels of window are ten pixels of fader"
        );
    }

    #[test]
    fn the_strip_scrollbar_shows_only_once_the_columns_overrun_the_window() {
        let fits = project(
            &columns_snapshot(1),
            &UiState::new(),
            Rect::new(0, 0, 560, 400),
        );
        assert_eq!(fits.scroll_max_x, 0);
        assert!(
            fits.strip_scrollbar.is_none(),
            "columns that fit need no scrollbar"
        );

        let over = narrow(6, 0);
        assert!(over.scroll_max_x > 0);
        let bar = over.strip_scrollbar.as_ref().expect("scrollbar");
        assert!(
            bar.slider.w < bar.track.w,
            "the slider shows the visible share"
        );
        assert!(
            bar.track.bottom() <= over.strip.bottom(),
            "the bar rides inside the strip"
        );
    }

    #[test]
    fn the_strip_slider_runs_from_one_end_to_the_other() {
        let start = narrow(6, 0);
        let bar = start.strip_scrollbar.as_ref().expect("scrollbar");
        assert_eq!(bar.slider.x, bar.track.x);

        let end = narrow(6, start.scroll_max_x);
        let bar = end.strip_scrollbar.as_ref().expect("scrollbar");
        assert_eq!(
            bar.slider.right(),
            bar.track.right(),
            "scrolled to the last column, the slider is at the end"
        );
    }

    #[test]
    fn the_scrollbar_takes_presses_that_land_on_it() {
        let layout = narrow(6, 0);
        let bar = layout.strip_scrollbar.as_ref().expect("scrollbar");
        let (x, y) = (bar.slider.x + 2, bar.track.y + bar.track.h / 2);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::StripScrollbar));
    }

    /// A palette holding more commands than the list can show, scrolled to
    /// `scroll` with `selected` highlighted.
    fn scrolling_palette(scroll: usize, selected: usize) -> ViewSnapshot {
        let mut app = state::empty();
        app.profiles.profiles.clear();
        for i in 0..crate::command_palette::VISIBLE_ROWS + 6 {
            app.profiles.profiles.push(Profile {
                name: format!("p{i}"),
                ..Default::default()
            });
        }
        app.palette = Some(state::CommandPalette {
            scroll,
            selected,
            ..state::CommandPalette::default()
        });
        build_snapshot(&app, |_| None)
    }

    #[test]
    fn a_long_list_lays_out_one_window_of_rows_keyed_by_their_place_in_it() {
        let snap = scrolling_palette(4, 4);
        let layout = project(&snap, &UiState::new(), content());
        let palette = layout.palette.as_ref().expect("palette");

        assert!(snap.palette.rows.len() > crate::command_palette::VISIBLE_ROWS);
        assert_eq!(palette.rows.len(), crate::command_palette::VISIBLE_ROWS);
        assert_eq!(palette.first_visible, 4);

        // The first row on screen answers to its index in the whole list, not
        // its place in the window, so a click runs the command under it.
        let first = palette.rows[0];
        assert_eq!(
            layout.hit(first.x + 4, first.y + first.h / 2),
            Some(&HitTarget::PaletteRow(4))
        );
    }

    #[test]
    fn every_row_stays_inside_the_panel() {
        let snap = scrolling_palette(6, 6);
        let layout = project(&snap, &UiState::new(), content());
        let palette = layout.palette.as_ref().expect("palette");
        let panel = palette.panel;
        for r in &palette.rows {
            assert!(
                r.y >= panel.y && r.bottom() <= panel.bottom(),
                "row {r:?} escapes panel {panel:?}"
            );
        }
    }

    #[test]
    fn the_window_follows_a_selection_the_stored_scroll_left_behind() {
        // Scroll says the top of the list, selection says the bottom: the last
        // row wins, which is what a wrap from the first row asks for.
        let last = scrolling_palette(0, 0).palette.rows.len() - 1;
        let snap = scrolling_palette(0, last);
        let layout = project(&snap, &UiState::new(), content());
        let palette = layout.palette.as_ref().expect("palette");
        assert_eq!(
            palette.first_visible + palette.rows.len() - 1,
            last,
            "the window ends on the selected row"
        );
    }

    #[test]
    fn a_short_window_shows_fewer_rows_than_the_nominal_count() {
        let snap = scrolling_palette(0, 0);
        // Room for a handful of rows, no more.
        let layout = project(&snap, &UiState::new(), Rect::new(0, 0, 560, 300));
        let palette = layout.palette.as_ref().expect("palette");
        assert!(
            palette.rows.len() < crate::command_palette::VISIBLE_ROWS,
            "expected a narrowed window, got {}",
            palette.rows.len()
        );
        assert!(palette.panel.bottom() <= 300, "panel overflows the window");
    }

    #[test]
    fn the_scrollbar_shows_only_when_the_list_outgrows_its_window() {
        let mut app = state::empty();
        app.palette = Some(state::CommandPalette::default());
        // A fresh app offers a couple of commands, well inside one window.
        let short = build_snapshot(&app, |_| None);
        let layout = project(&short, &UiState::new(), content());
        assert!(
            layout
                .palette
                .as_ref()
                .expect("palette")
                .scrollbar
                .is_none(),
            "a list that fits needs no scrollbar"
        );

        let snap = scrolling_palette(0, 0);
        let layout = project(&snap, &UiState::new(), content());
        assert!(
            layout
                .palette
                .as_ref()
                .expect("palette")
                .scrollbar
                .is_some()
        );
    }

    #[test]
    fn the_slider_rides_the_track_from_top_to_bottom() {
        let total = scrolling_palette(0, 0).palette.rows.len();
        let max_scroll = total - crate::command_palette::VISIBLE_ROWS;

        let top = project(&scrolling_palette(0, 0), &UiState::new(), content());
        let top = top.palette.as_ref().expect("palette");
        let top = top.scrollbar.as_ref().expect("scrollbar");
        assert_eq!(top.slider.y, top.track.y, "unscrolled sits at the top");

        let end = project(
            &scrolling_palette(max_scroll, max_scroll),
            &UiState::new(),
            content(),
        );
        let end = end.palette.as_ref().expect("palette");
        let end = end.scrollbar.as_ref().expect("scrollbar");
        assert_eq!(
            end.slider.bottom(),
            end.track.bottom(),
            "scrolled to the end sits at the bottom"
        );
        assert!(
            end.slider.h < end.track.h,
            "the slider is shorter than its track"
        );
    }

    #[test]
    fn rows_give_up_the_scrollbar_lane_rather_than_run_under_it() {
        let snap = scrolling_palette(0, 0);
        let layout = project(&snap, &UiState::new(), content());
        let palette = layout.palette.as_ref().expect("palette");
        let bar = palette.scrollbar.as_ref().expect("scrollbar");
        for r in &palette.rows {
            assert!(
                r.right() <= bar.track.x,
                "row {r:?} runs into the scrollbar lane at {}",
                bar.track.x
            );
        }
    }
}
