use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::gdk_pixbuf;
use gtk4::glib::SignalHandlerId;
use gtk4::prelude::*;

use crate::bus::Sender;
use crate::domain::{
    DeviceForm, MAX_VOLUME, SinkForm, SourceForm, Stream as AudioStream, linear_to_cubic,
};
use crate::state::Message;
use crate::ui::theme::Palette;
use crate::xdg::XdgInfo;

use super::meter::{SLIDER_TRACK_HEIGHT, build_meter};

/// What the pointer is hovering over, driving the bare-`m` mute shortcut.
/// Sink rows carry a node id; app rows carry a group key (mute fans out
/// to every collapsed member).
#[derive(Debug, Clone)]
pub(super) enum HoverTarget {
    Sink(u32),
    AppGroup(String),
}

/// Rendered logical size for sink/app icons.
const ICON_SIZE: i32 = 26;

/// Fixed icon-row height so varied app icon sizes don't shift the name
/// label below them off baseline. Slightly larger than `ICON_SIZE`.
const ICON_BOX_HEIGHT: i32 = 28;

/// Hard cap on name-label characters. Used as both the `max_width_chars`
/// hint and a string truncation in `update_row` (no ellipsis).
const MAX_NAME_CHARS: usize = 14;

/// Width of every column. Long sink-form labels ("HEADPHONES") overflow
/// and get clipped by `Overflow::Hidden`.
const COLUMN_WIDTH: i32 = 106;

/// Reserved header height (icon/type + name) above the slider. Fixed so
/// the percent readout lands at the same Y in sink and app columns.
/// Tuned to fit the taller (app) header.
const COLUMN_HEADER_HEIGHT: i32 = 46;

/// Left margin on right-column widgets. 0 lets the thumb's -6 px overflow
/// (see `.bnk-scale slider`) graze the column without overlap.
const SLIDER_COLUMN_INSET: i32 = 0;

/// Gap between the sink-target picker (A/H/S) and the mute button so the
/// target group reads as one cluster distinct from M.
const PICKER_MUTE_GAP: i32 = 6;

/// Pixels the button column slides into the meter+slider group's right
/// edge, trimming the visible gap left by the thumb's -6 px overflow.
/// Buttons sit on top in z-order, so the overlap is purely visual.
const BUTTONS_SLIDER_OVERLAP: i32 = 4;
/// Volume above which the slider turns its warning color. 1.10 (110%) is
/// just past unity gain, so routine 100% playback stays normal pink.
const VOLUME_WARNING_THRESHOLD: f32 = 1.10;

/// Top-of-column header. Apps show an icon; sinks show the `SinkForm` as
/// a bold all-caps type label.
pub(super) enum RowHeader {
    AppIcon(gtk::Box),
    DeviceType(gtk::Label),
}

pub(super) struct RowWidgets {
    pub(super) container: gtk::Box,
    /// Vertical separator after `container`, bundled with the row so
    /// insert/remove/reorder keep them together.
    pub(super) separator: gtk::Separator,
    pub(super) header: RowHeader,
    pub(super) name_label: gtk::Label,
    pub(super) percent_num_label: gtk::Label,
    pub(super) percent_sym_label: gtk::Label,
    /// Box wrapping the number + `%` labels above the slider. Held so its
    /// visibility tracks the `percent_above` setting.
    pub(super) percent_box: gtk::Box,
    /// The `%` label on the slider knob. Held so its visibility tracks
    /// the `percent_on_slider` setting.
    pub(super) knob_value: gtk::Label,
    pub(super) picker_container: Option<gtk::Box>,
    pub(super) scale: gtk::Scale,
    pub(super) scale_handler: SignalHandlerId,
    pub(super) mute_button: gtk::Button,
    /// Vertical segmented level meter left of the scale. Renders one bar
    /// per entry in `peaks`; the decay tick refills and fades `peaks`.
    /// Empty until the first peak (or for unmonitored nodes), rendering dim.
    pub(super) meter: gtk::DrawingArea,
    pub(super) peaks: Rc<RefCell<Vec<f32>>>,
}

pub(super) fn make_col_separator() -> gtk::Separator {
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.add_css_class("bnk-col-sep");
    sep
}

pub(super) fn build_sink_column(
    id: u32,
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    palette: Palette,
) -> RowWidgets {
    build_device_column(id, tx, hovered, Message::MakeDefault(id), palette)
}

/// Build an input (source) column. Same chrome as a sink; the type label
/// sets the default input instead of the default output.
pub(super) fn build_source_column(
    id: u32,
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    palette: Palette,
) -> RowWidgets {
    build_device_column(id, tx, hovered, Message::MakeDefaultSource(id), palette)
}

/// Build a device column (sink or source). The clickable type label
/// dispatches `make_default`; slider/mute/meter are identical for both
/// directions. Hover target is `Sink(id)` (just "this node id") either way.
fn build_device_column(
    id: u32,
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    make_default: Message,
    palette: Palette,
) -> RowWidgets {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Center)
        .width_request(COLUMN_WIDTH)
        .hexpand(false)
        .hexpand_set(true)
        // Clip overlong type-label text so a wide sink form (HEADPHONES)
        // doesn't widen the column past its siblings.
        .overflow(gtk::Overflow::Hidden)
        .build();
    attach_hover_tracker(&container, HoverTarget::Sink(id), hovered);

    let type_label = gtk::Label::new(None);
    type_label.set_xalign(0.5);
    type_label.set_single_line_mode(true);
    type_label.add_css_class("bnk-device-type");
    let gesture = gtk::GestureClick::new();
    let tx_g = tx.clone();
    gesture.connect_released(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        let _ = tx_g.send(make_default.clone());
    });
    type_label.add_controller(gesture);
    type_label.set_cursor_from_name(Some("pointer"));

    let name_label = single_line_name_label();
    name_label.add_css_class("bnk-device-name");
    // Positive margin pairs the name with the type heading; a negative
    // one would trip GTK's size-adjustment warning.
    name_label.set_margin_top(1);

    // Fixed-height header slot so the percent readout lands at the same Y
    // in sink and app columns. `vexpand(false) + vexpand_set(true)` is
    // load-bearing: without it the slot would fight the slider's vexpand
    // for leftover vertical space and blow up to hundreds of pixels.
    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .height_request(COLUMN_HEADER_HEIGHT)
        .vexpand(false)
        .vexpand_set(true)
        // Clip long text at the centered-header edges rather than the
        // outer column boundary.
        .overflow(gtk::Overflow::Hidden)
        .build();
    header_box.append(&type_label);
    header_box.append(&name_label);
    container.append(&header_box);

    let (
        slider_area,
        percent_num_label,
        percent_sym_label,
        percent_box,
        knob_value,
        scale,
        scale_handler,
        mute_button,
        meter,
        peaks,
    ) = build_slider_area(
        tx,
        move || Message::MuteToggled(id),
        move |v: f32| Message::VolumeChanged(id, v),
        None,
        palette,
    );
    container.append(&slider_area);

    RowWidgets {
        container,
        separator: make_col_separator(),
        header: RowHeader::DeviceType(type_label),
        name_label,
        percent_num_label,
        percent_sym_label,
        percent_box,
        knob_value,
        picker_container: None,
        scale,
        scale_handler,
        mute_button,
        meter,
        peaks,
    }
}

/// Build an application column. `hover_target` decides what the hover-mute
/// `m` shortcut dispatches; `mute_msg` / `volume_msg` build the slider/mute
/// messages. Group rows use `AppGroup` + Group* messages; member sub-rows
/// use `Sink(node_id)` + per-stream messages. `is_member` flips a CSS class
/// that washes sub-rows darker so they chunk under their parent.
pub(super) fn build_app_column(
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    hover_target: HoverTarget,
    is_member: bool,
    mute_msg: impl Fn() -> Message + 'static,
    volume_msg: impl Fn(f32) -> Message + 'static,
    palette: Palette,
) -> RowWidgets {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Center)
        .width_request(COLUMN_WIDTH)
        .hexpand(false)
        .hexpand_set(true)
        .overflow(gtk::Overflow::Hidden)
        .build();
    if is_member {
        container.add_css_class("bnk-app-member");
    }
    attach_hover_tracker(&container, hover_target, hovered);

    // Fixed height pins the icon row regardless of icon size, keeping the
    // name label below it on baseline across rows.
    let icon_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .height_request(ICON_BOX_HEIGHT)
        .build();

    let name_label = single_line_name_label();
    // Same device-name styling as sink columns.
    name_label.add_css_class("bnk-device-name");

    // Same fixed-height header slot as sink columns.
    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .height_request(COLUMN_HEADER_HEIGHT)
        .vexpand(false)
        .vexpand_set(true)
        .overflow(gtk::Overflow::Hidden)
        .build();
    header_box.append(&icon_box);
    header_box.append(&name_label);
    container.append(&header_box);

    // Picker stacks above the mute button, left-aligned at the shared
    // `SLIDER_COLUMN_INSET`. `margin_bottom` gaps it off the mute button.
    let picker_container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Start)
        .margin_start(SLIDER_COLUMN_INSET)
        .margin_bottom(PICKER_MUTE_GAP)
        .build();

    let (
        slider_area,
        percent_num_label,
        percent_sym_label,
        percent_box,
        knob_value,
        scale,
        scale_handler,
        mute_button,
        meter,
        peaks,
    ) = build_slider_area(tx, mute_msg, volume_msg, Some(&picker_container), palette);
    container.append(&slider_area);

    RowWidgets {
        container,
        separator: make_col_separator(),
        header: RowHeader::AppIcon(icon_box),
        name_label,
        percent_num_label,
        percent_sym_label,
        percent_box,
        knob_value,
        picker_container: Some(picker_container),
        scale,
        scale_handler,
        mute_button,
        meter,
        peaks,
    }
}

fn single_line_name_label() -> gtk::Label {
    let lbl = gtk::Label::new(None);
    lbl.set_xalign(0.5);
    lbl.set_single_line_mode(true);
    // Cap the natural width; `halign: Center` keeps the allocation at that
    // natural size instead of filling a header_box widened by a long type
    // label. The string is also truncated in `update_row`.
    lbl.set_max_width_chars(MAX_NAME_CHARS as i32);
    lbl.set_halign(gtk::Align::Center);
    lbl
}

/// Tuple returned by [`build_slider_area`]: container, percent number label,
/// percent `%` label, the percent readout box, the in-knob `%` label, the
/// scale and its handler, the mute button, the meter, and its peaks cell.
type SliderAreaParts = (
    gtk::Box,
    gtk::Label,
    gtk::Label,
    gtk::Box,
    gtk::Label,
    gtk::Scale,
    SignalHandlerId,
    gtk::Button,
    gtk::DrawingArea,
    Rc<RefCell<Vec<f32>>>,
);

/// Vertical centre (px from the top) of the slider thumb for `value` on the
/// inverted `0..MAX_VOLUME` range in a scale `h` px tall. Tuned against the
/// `.bnk-scale` thumb CSS; revisit if its min-height/margin change.
fn thumb_center_y(value: f64, h: i32) -> f64 {
    const THUMB_RADIUS: f64 = 16.0;
    let usable = (h as f64 - 2.0 * THUMB_RADIUS).max(0.0);
    let frac_from_top = 1.0 - value / MAX_VOLUME as f64;
    THUMB_RADIUS + frac_from_top * usable
}

/// Locate the thumb node (CSS name `slider`) inside a `gtk::Scale`. Its
/// allocated bounds anchor the in-knob readout to the real thumb instead of
/// re-deriving GTK's drifting value->pixel mapping. `None` if GTK renames it.
fn slider_node(scale: &gtk::Scale) -> Option<gtk::Widget> {
    fn dfs(w: &gtk::Widget) -> Option<gtk::Widget> {
        let mut child = w.first_child();
        while let Some(c) = child {
            if c.css_name() == "slider" {
                return Some(c);
            }
            if let Some(found) = dfs(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    dfs(scale.upcast_ref::<gtk::Widget>())
}

/// Percent readout above a row of: meter, scale, and a right column with
/// the optional `picker` + mute button. `mute_msg` fires on mute click,
/// `volume_msg` fires continuously while dragging. Sink rows wire per-stream
/// variants; app rows wire the collapsed-group equivalents.
fn build_slider_area(
    tx: &Sender<Message>,
    mute_msg: impl Fn() -> Message + 'static,
    volume_msg: impl Fn(f32) -> Message + 'static,
    picker: Option<&gtk::Box>,
    palette: Palette,
) -> SliderAreaParts {
    // Overlay keeps the buttons out of layout flow: `meter_slider_group` is
    // the main child (self-centering), the buttons sit as an overlay child
    // positioned against its right edge. Overlay children don't measure, so
    // resizing the buttons can't drag the slider off center.
    let area = gtk::Overlay::new();

    let meter_slider_group = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        // Center meter + slider as a unit; buttons are positioned relative
        // to this group.
        .halign(gtk::Align::Center)
        .build();

    let side = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        // Hug the natural width; single-letter labels don't need the full
        // column, and hexpand would pad the right side for no reason.
        .hexpand(false)
        .build();

    // Number + `%` side by side, baseline-aligned ("75%"). Tabular figures
    // keep 99 -> 100 from reflowing the `%`. `halign: Center` lands it on
    // the same axis the meter+slider group centers on.
    let percent_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .halign(gtk::Align::Center)
        .build();
    let percent_num_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .valign(gtk::Align::Baseline)
        .build();
    percent_num_label.add_css_class("bnk-percent-num");
    let percent_sym_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .valign(gtk::Align::Baseline)
        .build();
    percent_sym_label.add_css_class("bnk-percent-sym");
    percent_sym_label.set_markup("<span size=\"medium\">%</span>");
    percent_box.append(&percent_num_label);
    percent_box.append(&percent_sym_label);

    let spacer = gtk::Box::builder().vexpand(true).build();
    let tx_m = tx.clone();
    let mute_button = pick_button("M", false, move || {
        let _ = tx_m.send(mute_msg());
    });
    mute_button.set_valign(gtk::Align::End);
    mute_button.set_halign(gtk::Align::Start);
    // Match the meter's 12 px bottom margin so the button, meter, and
    // slider track bottoms line up.
    mute_button.set_margin_bottom(12);
    // Shared inset so the mute button's left edge lines up with the picker.
    mute_button.set_margin_start(SLIDER_COLUMN_INSET);
    side.append(&spacer);
    if let Some(p) = picker {
        side.append(p);
    }
    side.append(&mute_button);

    let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, MAX_VOLUME as f64, 0.01);
    scale.add_css_class("bnk-scale");
    // Flip so 0 sits at the bottom and louder is higher.
    scale.set_inverted(true);
    scale.set_vexpand(true);
    // hexpand off + halign Start pins the trough left; otherwise it drifts
    // right and opens a gap before the buttons.
    scale.set_hexpand(false);
    scale.set_halign(gtk::Align::Start);
    scale.set_height_request(SLIDER_TRACK_HEIGHT);
    scale.set_draw_value(false);
    let tx_v = tx.clone();
    let scale_handler = scale.connect_value_changed(move |s| {
        let _ = tx_v.send(volume_msg(s.value() as f32));
    });

    // Unity-gain (cubic 1.0) reference drawn via Overlay+DrawingArea rather
    // than `Scale::add_mark`, which would grow a marks-gutter and shift
    // siblings. The overlay matches the scale's size.
    let scale_overlay = gtk::Overlay::new();
    scale_overlay.set_halign(gtk::Align::Start);
    scale_overlay.set_hexpand(false);
    scale_overlay.set_child(Some(&scale));
    let unity_tick = gtk::DrawingArea::new();
    unity_tick.set_can_target(false);
    let notch = palette.unity_notch.rgba_f64();
    unity_tick.set_draw_func(move |_, cr, w, h| {
        // `+ 0.5` lands the 1 px stroke on a pixel boundary so it stays crisp.
        let y = thumb_center_y(1.0, h).round() + 0.5;
        // Short notch centered on the trough so it reads as part of the slider.
        let notch_len = 4.0_f64;
        let cx = w as f64 / 2.0;
        let (nr, ng, nb, na) = notch;
        cr.set_source_rgba(nr, ng, nb, na);
        cr.set_line_width(1.0);
        cr.move_to(cx - notch_len / 2.0, y);
        cr.line_to(cx + notch_len / 2.0, y);
        let _ = cr.stroke();
    });
    scale_overlay.add_overlay(&unity_tick);

    // Volume percentage on the thumb. The thumb is GTK-internal, so a Label
    // rides as an overlay positioned on the thumb centre by
    // `get_child_position`. `set_can_target(false)` keeps drags reaching the
    // scale beneath.
    let knob_value = gtk::Label::new(None);
    knob_value.add_css_class("bnk-knob-value");
    knob_value.set_can_target(false);
    scale_overlay.add_overlay(&knob_value);

    // Centre the label on the thumb; other overlay children keep default
    // fill via the `None` return. Runs on every overlay allocation.
    let knob_for_pos = knob_value.clone();
    let scale_for_pos = scale.clone();
    scale_overlay.connect_get_child_position(move |overlay, child| {
        if child != knob_for_pos.upcast_ref::<gtk::Widget>() {
            return None;
        }
        let (_, nat_w, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
        let (_, nat_h, _, _) = child.measure(gtk::Orientation::Vertical, -1);
        // Centre on the actual thumb (already allocated by overlay-position
        // time), tracking the knob exactly. Fall back to the geometric model
        // only if the node can't be found.
        let (cx, cy) = slider_node(&scale_for_pos)
            .and_then(|slider| slider.compute_bounds(overlay))
            .map(|b| {
                (
                    (b.x() + b.width() / 2.0).round() as i32,
                    (b.y() + b.height() / 2.0).round() as i32,
                )
            })
            .unwrap_or_else(|| {
                (
                    overlay.width() / 2,
                    thumb_center_y(scale_for_pos.value(), overlay.height()).round() as i32,
                )
            });
        Some(gdk::Rectangle::new(
            cx - nat_w / 2,
            cy - nat_h / 2,
            nat_w,
            nat_h,
        ))
    });

    // Refresh text + warning tint and re-run positioning on every value
    // change; a same-width number wouldn't re-allocate the overlay, so
    // nudge it.
    let knob_for_upd = knob_value.clone();
    let overlay_for_upd = scale_overlay.clone();
    scale.connect_value_changed(move |s| {
        let value = s.value();
        knob_for_upd.set_text(&((value * 100.0).round() as i32).to_string());
        // Match the slider fill's threshold so knob and trough turn amber
        // together.
        if value >= VOLUME_WARNING_THRESHOLD as f64 {
            knob_for_upd.add_css_class("bnk-knob-value-warning");
        } else {
            knob_for_upd.remove_css_class("bnk-knob-value-warning");
        }
        overlay_for_upd.queue_allocate();
    });

    let (meter, peaks) = build_meter(palette);
    meter_slider_group.append(&meter);
    meter_slider_group.append(&scale_overlay);
    area.set_child(Some(&meter_slider_group));
    area.add_overlay(&side);

    // Position `side` `BUTTONS_SLIDER_OVERLAP` px inside the right edge of
    // the visible meter+slider content. The Overlay hands its main child the
    // full allocation, so the content's right edge is center + nat_w/2, not
    // the allocation's right. The overlap trims the gap left by the thumb's
    // -6 px overflow; buttons are on top in z-order, so it's purely visual.
    let main_for_pos = meter_slider_group.clone();
    area.connect_get_child_position(move |_, widget| {
        let main_alloc = main_for_pos.allocation();
        if main_alloc.width() == 0 {
            return None;
        }
        let (_, main_nat_w, _, _) = main_for_pos.measure(gtk::Orientation::Horizontal, -1);
        let (_, side_nat_w, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
        let content_right = main_alloc.x() + (main_alloc.width() + main_nat_w) / 2;
        Some(gdk::Rectangle::new(
            content_right - BUTTONS_SLIDER_OVERLAP,
            main_alloc.y(),
            side_nat_w,
            main_alloc.height(),
        ))
    });

    // Stack the percent readout above the inner row so the value heads the
    // strip and the right column stays reserved for action buttons.
    let wrapper = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    wrapper.append(&percent_box);
    wrapper.append(&area);

    (
        wrapper,
        percent_num_label,
        percent_sym_label,
        percent_box,
        knob_value,
        scale,
        scale_handler,
        mute_button,
        meter,
        peaks,
    )
}

/// Render one sink (output device) column. Devices are never collapsed, so
/// this takes the stream directly. Fallback label is the output catch-all.
pub(super) fn update_sink_row(row: &mut RowWidgets, s: &AudioStream) {
    update_device_row(row, s, SinkForm::Generic.display_label());
}

/// Render one source (input device) column. Mirror of [`update_sink_row`]
/// with the input catch-all fallback.
pub(super) fn update_source_row(row: &mut RowWidgets, s: &AudioStream) {
    update_device_row(row, s, SourceForm::Generic.display_label());
}

/// Shared device-row renderer. `fallback_label` is the heading shown before
/// the form resolves ("OUTPUT" for sinks, "INPUT" for sources).
fn update_device_row(row: &mut RowWidgets, s: &AudioStream, fallback_label: &str) {
    let cubic = linear_to_cubic(s.average_volume()).clamp(0.0, MAX_VOLUME);
    apply_slider_visuals(row, cubic);
    apply_name_label(row, s.display_name(), false, 1);

    match &row.header {
        RowHeader::DeviceType(type_label) => {
            let label = s
                .form
                .map(DeviceForm::display_label)
                .unwrap_or(fallback_label);
            type_label.set_text(label);
            crate::ui::set_swap_class(type_label, s.is_default, "bnk-active", "bnk-inactive");
        }
        RowHeader::AppIcon(_) => debug_assert!(false, "device row built with AppIcon header"),
    }
    apply_mute_visual(row, s.muted);
}

/// Aggregate state for a collapsed app row, materialised by
/// `AppRowGroup::to_info` so [`update_app_row`] never revisits `state.streams`.
pub(super) struct AppRowInfo<'a> {
    /// Stable row key for the Group* messages from the picker/mute/slider.
    pub key: &'a str,
    /// Owned because the label may be MPRIS-enriched (e.g. "Spotify · Track").
    pub display_name: String,
    pub xdg: Option<&'a XdgInfo>,
    /// Cubic master volume (max of member cubics), drives slider + readout.
    pub master_cubic: f32,
    /// True iff every member is muted (or the group is empty).
    pub all_muted: bool,
    /// `Some(name)` only when every member pins the same sink; mixed/no-pin
    /// leaves "A" autoroute as the active button.
    pub effective_target: Option<&'a str>,
    /// True iff every member is tombstoned; drives the dimmed "idle" name.
    pub all_tombstoned: bool,
    /// Stream count; > 1 surfaces a ×N badge in the name label.
    pub member_count: usize,
    /// Whether the row is expanded; drives the toggle glyph (`▸`/`▾`).
    pub is_expanded: bool,
}

/// Render one collapsed app column. Slider/mute/target/meter reflect
/// aggregate-of-members state; the picker dispatches Group* messages keyed
/// by [`AppRowInfo::key`] so one gesture fans out to every member.
pub(super) fn update_app_row(
    row: &mut RowWidgets,
    info: AppRowInfo<'_>,
    sinks: &[&AudioStream],
    tx: &Sender<Message>,
) {
    let cubic = info.master_cubic.clamp(0.0, MAX_VOLUME);
    apply_slider_visuals(row, cubic);
    apply_name_label(
        row,
        &info.display_name,
        info.all_tombstoned,
        info.member_count,
    );

    match &row.header {
        RowHeader::AppIcon(icon_box) => render_app_icon(icon_box, info.xdg),
        RowHeader::DeviceType(_) => debug_assert!(false, "app row built with DeviceType header"),
    }

    apply_mute_visual(row, info.all_muted);

    if let Some(picker) = row.picker_container.as_ref() {
        while let Some(c) = picker.first_child() {
            picker.remove(&c);
        }
        // Expand toggle as the first picker slot. Only shown when there's
        // something to expand into; the glyph flips with expand state.
        if info.member_count > 1 {
            let key_for_toggle = info.key.to_string();
            let label = if info.is_expanded {
                COLLAPSE_LABEL
            } else {
                EXPAND_LABEL
            };
            picker.append(&pick_button(label, info.is_expanded, {
                let tx_toggle = tx.clone();
                move || {
                    let _ = tx_toggle.send(Message::GroupToggleExpanded(key_for_toggle.clone()));
                }
            }));
        }
        let key_owned = info.key.to_string();
        append_sink_picker(
            picker,
            sinks,
            info.effective_target,
            {
                let tx_clear = tx.clone();
                let key = key_owned.clone();
                move || {
                    let _ = tx_clear.send(Message::GroupClearStreamTarget(key.clone()));
                }
            },
            {
                let tx_pin = tx.clone();
                move |sink_id| {
                    let _ = tx_pin.send(Message::GroupSetStreamTarget {
                        key: key_owned.clone(),
                        sink_id,
                    });
                }
            },
        );
    }
}

/// Render one expanded sub-row for a single member of an app group. Slider,
/// mute, and picker dispatch per-stream messages, behaving like a standalone
/// app except for the collapse button that folds the group back together.
pub(super) fn update_app_member_row(
    row: &mut RowWidgets,
    stream: &AudioStream,
    parent_key: &str,
    parent_xdg: Option<&XdgInfo>,
    tombstoned: bool,
    sinks: &[&AudioStream],
    tx: &Sender<Message>,
) {
    let cubic = linear_to_cubic(stream.average_volume()).clamp(0.0, MAX_VOLUME);
    apply_slider_visuals(row, cubic);

    // Sub-row label deliberately does NOT consult MPRIS: browsers expose one
    // MPRIS endpoint per instance, so every sub-row would show the same
    // title. Use media.name if distinctive, otherwise the node id.
    let label = stream
        .media_name
        .as_deref()
        .filter(|n| !n.is_empty() && *n != "Playback")
        .map(str::to_string)
        .unwrap_or_else(|| format!("Stream {}", stream.id));
    apply_name_label(row, &label, tombstoned, 1);

    match &row.header {
        // Inherit the parent group's icon so sub-rows chunk under one app.
        RowHeader::AppIcon(icon_box) => render_app_icon(icon_box, parent_xdg),
        RowHeader::DeviceType(_) => debug_assert!(false, "member row built with DeviceType header"),
    }

    apply_mute_visual(row, stream.muted);

    if let Some(picker) = row.picker_container.as_ref() {
        while let Some(c) = picker.first_child() {
            picker.remove(&c);
        }
        // Collapse button is always on a sub-row; any one folds the group.
        let parent_for_collapse = parent_key.to_string();
        picker.append(&pick_button(COLLAPSE_LABEL, false, {
            let tx_c = tx.clone();
            move || {
                let _ = tx_c.send(Message::GroupToggleExpanded(parent_for_collapse.clone()));
            }
        }));

        let app_id = stream.id;
        append_sink_picker(
            picker,
            sinks,
            stream.target_sink_name.as_deref(),
            {
                let tx_clear = tx.clone();
                move || {
                    let _ = tx_clear.send(Message::ClearStreamTarget(app_id));
                }
            },
            {
                let tx_pin = tx.clone();
                move |sink_id| {
                    let _ = tx_pin.send(Message::SetStreamTarget { app_id, sink_id });
                }
            },
        );
    }
}

/// Clear `icon_box` and render the app icon for `xdg`, or the
/// `applications-other-symbolic` fallback. Pixbuf loads at
/// `ICON_SIZE * scale_factor` device px for HiDPI crispness, then
/// `set_pixel_size` pins the logical size to `ICON_SIZE`.
fn render_app_icon(icon_box: &gtk::Box, xdg: Option<&XdgInfo>) {
    while let Some(c) = icon_box.first_child() {
        icon_box.remove(&c);
    }
    let scale_factor = icon_box.scale_factor().max(1);
    let target_px = ICON_SIZE * scale_factor;
    let img = match xdg.and_then(|x| x.icon_path.as_ref()) {
        Some(path) => {
            match gdk_pixbuf::Pixbuf::from_file_at_scale(path, target_px, target_px, true) {
                Ok(pb) => {
                    let texture = gdk::Texture::for_pixbuf(&pb);
                    gtk::Image::from_paintable(Some(&texture))
                }
                Err(_) => gtk::Image::from_icon_name("applications-other-symbolic"),
            }
        }
        None => gtk::Image::from_icon_name("applications-other-symbolic"),
    };
    img.set_pixel_size(ICON_SIZE);
    icon_box.append(&img);
}

/// Append the per-sink routing picker: an "A" autoroute (clear) button plus
/// one button per sink, labelled with the sink form's initial. Shown only
/// when at least two sinks exist. `effective_target` is the pinned sink's
/// `node.name` (or `None`); `on_clear` fires for "A", `on_set` for a sink id.
fn append_sink_picker(
    picker: &gtk::Box,
    sinks: &[&AudioStream],
    effective_target: Option<&str>,
    on_clear: impl Fn() + 'static,
    on_set: impl Fn(u32) + Clone + 'static,
) {
    let entries: Vec<(u32, &'static str, bool)> = sinks
        .iter()
        .filter_map(|sink| {
            let form = sink.form?;
            let active = sink.node_name.as_deref().is_some()
                && sink.node_name.as_deref() == effective_target;
            Some((sink.id, form.display_label(), active))
        })
        .collect();
    if entries.len() < 2 {
        return;
    }
    picker.append(&pick_button("A", effective_target.is_none(), on_clear));
    // Label with the first letter of the sink's form (already uppercase).
    for (sink_id, label, active) in entries {
        let abbrev: String = label.chars().take(1).collect();
        let on_set = on_set.clone();
        picker.append(&pick_button(&abbrev, active, move || on_set(sink_id)));
    }
}

/// Expand affordance on a collapsed multi-member app row.
const EXPAND_LABEL: &str = "▸";
/// Collapse affordance on every sub-row of an expanded group.
const COLLAPSE_LABEL: &str = "▾";

/// Push the cubic master value into the slider (signal blocked so it doesn't
/// loop back) and flip the above-unity-gain warning class.
fn apply_slider_visuals(row: &mut RowWidgets, cubic: f32) {
    row.scale.block_signal(&row.scale_handler);
    row.scale.set_value(cubic as f64);
    row.scale.unblock_signal(&row.scale_handler);

    // Past the threshold (110%) the slider and readout turn amber; routine
    // 100% use stays the normal pink.
    if cubic >= VOLUME_WARNING_THRESHOLD {
        row.scale.add_css_class("bnk-scale-warning");
        row.percent_num_label.add_css_class("bnk-percent-warning");
        row.percent_sym_label.add_css_class("bnk-percent-warning");
    } else {
        row.scale.remove_css_class("bnk-scale-warning");
        row.percent_num_label
            .remove_css_class("bnk-percent-warning");
        row.percent_sym_label
            .remove_css_class("bnk-percent-warning");
    }

    let percent = (cubic * 100.0).round() as i32;
    row.percent_num_label.set_markup(&format!(
        "<span size=\"xx-large\" weight=\"bold\" font_features=\"tnum\">{percent}</span>"
    ));
}

/// Stamp the name label. `tombstoned` appends `· idle` and dims; `member_count
/// > 1` appends `×N`. Result is truncated at [`MAX_NAME_CHARS`] (no ellipsis).
fn apply_name_label(row: &mut RowWidgets, display: &str, tombstoned: bool, member_count: usize) {
    let mut text = display.to_string();
    if member_count > 1 {
        text.push_str(&format!(" ×{member_count}"));
    }
    if tombstoned {
        text.push_str(" · idle");
    }
    let text: String = text.chars().take(MAX_NAME_CHARS).collect();
    row.name_label.set_text(&text);
    row.name_label.remove_css_class("bnk-tombstoned");
    if tombstoned {
        row.name_label.add_css_class("bnk-tombstoned");
    }
}

/// Toggle the mute button's active class.
fn apply_mute_visual(row: &mut RowWidgets, muted: bool) {
    if muted {
        row.mute_button.add_css_class("bnk-pick-active");
    } else {
        row.mute_button.remove_css_class("bnk-pick-active");
    }
}

/// Label-only picker button (sink-pin column and mute). Label truncated to
/// [`PICK_LABEL_MAX`] chars (no ellipsis; the column is too narrow).
fn pick_button<F: Fn() + 'static>(label: &str, active: bool, on_click: F) -> gtk::Button {
    let truncated: String = label.chars().take(PICK_LABEL_MAX).collect();
    let lbl = gtk::Label::new(Some(&truncated));
    let btn = gtk::Button::builder().child(&lbl).build();
    btn.add_css_class("bnk-pick");
    if active {
        btn.add_css_class("bnk-pick-active");
    }
    btn.connect_clicked(move |_| on_click());
    btn
}

const PICK_LABEL_MAX: usize = 8;

/// Stamp the column's hover identity into `hovered` on enter, clear on leave.
/// Drives the hover+`m` mute shortcut. Clears only if the cell still points
/// at this column, since B's enter fires before A's leave when sliding A->B.
fn attach_hover_tracker(
    container: &gtk::Box,
    target: HoverTarget,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
) {
    let motion = gtk::EventControllerMotion::new();
    let enter_target = target.clone();
    let hov_enter = hovered.clone();
    motion.connect_enter(move |_, _, _| {
        *hov_enter.borrow_mut() = Some(enter_target.clone());
    });
    let leave_target = target;
    let hov_leave = hovered.clone();
    motion.connect_leave(move |_| {
        let mut slot = hov_leave.borrow_mut();
        if matches!(
            (slot.as_ref(), &leave_target),
            (Some(HoverTarget::Sink(a)), HoverTarget::Sink(b)) if a == b,
        ) || matches!(
            (slot.as_ref(), &leave_target),
            (Some(HoverTarget::AppGroup(a)), HoverTarget::AppGroup(b)) if a == b,
        ) {
            *slot = None;
        }
    });
    container.add_controller(motion);
}
