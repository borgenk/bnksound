//! The frame painter: draw a snapshot, its projected layout, and the retained
//! UI state into a pixel buffer.
//!
//! This is the one pixel-producing path both shells share. It reads the
//! snapshot, the layout geometry, the meter animation, and the theme, and draws
//! through the painter; it commits nothing and knows no window system.

use std::fmt::Write;
use std::path::Path;

use crate::platform::arena::ArrayString;
use crate::render::buffer::Color;
use crate::render::image::{IconCache, draw_icon};
use crate::render::primitives::{Painter, Rect};
use crate::render::text::{Font, TextStyle};
use crate::ui::layout::{ColumnGeom, HitTarget, Layout, RowId, SliderGeom};
use crate::ui::meter::{self, Tier};
use crate::ui::theme::Palette;
use crate::ui::{Focus, UiState};
use crate::view::snapshot::{SinkOption, ViewSnapshot};

const NAME_SIZE: f32 = 13.0;
/// All-caps device form heading, spread apart as in the reference.
const TYPE_SIZE: f32 = 12.0;
const TYPE_TRACKING: f32 = 1.0;
/// Device name under its form heading, and the app name in an app column.
const DEVICE_NAME_SIZE: f32 = 10.0;
/// Longest name a column shows, counted after any suffix. Cutting it here is
/// what leaves a gap at both edges of the column.
const MAX_NAME_CHARS: usize = 14;
/// A capped name, with room for every character being a four-byte one.
type NameText = ArrayString<{ MAX_NAME_CHARS * 4 }>;
/// Small pick-button label.
const PICK_SIZE_PT: f32 = 12.0;
/// The profile chip's label, a step up from a pick button.
const PROFILE_SIZE: f32 = 13.0;
/// Inset from the chip's edge to its label.
const PROFILE_PAD_X: i32 = 12;
/// A profile dropdown row: its label size, inset, and corner.
const MENU_SIZE: f32 = 12.0;
const MENU_PAD_X: i32 = 10;
const MENU_ROW_RADIUS: i32 = 3;
/// Volume percentage on the knob: quiet detail, not a headline.
const KNOB_VALUE_SIZE: f32 = 9.0;
const SUB_SIZE: f32 = 11.0;
const ICON_RADIUS: i32 = 6;
/// Corner radius shared by every small button and the profile chip.
const PICK_RADIUS: i32 = 4;

/// What one column needs drawn, unified across device and app rows.
struct RowDraw<'a> {
    label: &'a str,
    /// Device form heading ("SPEAKER", "HEADSET"); empty for app rows.
    sublabel: &'a str,
    icon_key: &'a str,
    /// The resolved icon file for app rows; None falls back to the tinted tile.
    icon_path: Option<&'a Path>,
    /// Cubic gain, for the percent readout.
    cubic: f32,
    /// The sink an app row is pinned to; None is autoroute.
    target_sink: Option<u32>,
    muted: bool,
    warning: bool,
    tombstoned: bool,
    is_default: bool,
    is_app: bool,
    member_count: usize,
    is_expanded: bool,
    can_expand: bool,
}

/// What a column draw needs beyond the column and its row: the retained UI
/// state, the text and theme resources, the sinks a target pin can name, and
/// the icon cache. Shared by every column in a frame.
struct ColumnCtx<'a> {
    ui: &'a UiState,
    font: &'a Font,
    palette: &'a Palette,
    sinks: &'a [SinkOption],
    icons: &'a mut IconCache,
}

/// Draw the whole frame.
pub fn paint_frame(
    p: &mut Painter,
    snapshot: &ViewSnapshot,
    ui: &UiState,
    layout: &Layout,
    font: &Font,
    palette: &Palette,
    icons: &mut IconCache,
) {
    let bounds = p.bounds();
    p.fill(bounds, palette.bg);

    if let Some(bar) = &layout.titlebar {
        paint_titlebar(p, bar, ui, palette);
    }
    paint_chrome(p, snapshot, layout, ui, font, palette);

    // Columns are clipped to the strip so scrolled content does not spill into
    // the toolbar or past the edges.
    {
        let mut cx = ColumnCtx {
            ui,
            font,
            palette,
            sinks: &snapshot.sink_options,
            icons,
        };
        let mut strip = p.clipped(layout.strip);
        for col in &layout.columns {
            // Looking a row up scans the snapshot, so a column scrolled out of
            // the strip or outside the damage is skipped before that cost.
            if !strip.intersects(col.rect) {
                continue;
            }
            if let Some(row) = row_draw(snapshot, &col.id) {
                paint_column(&mut strip, col, &row, &mut cx);
            }
        }
    }

    // Over the columns, in the air they leave along the bottom, and brighter
    // while the pointer is on it.
    if let Some(bar) = &layout.strip_scrollbar {
        let lit = ui.hover.as_ref() == Some(&crate::ui::layout::HitTarget::StripScrollbar)
            || matches!(ui.drag, Some(crate::ui::Drag::StripScroll { .. }));
        let ink = if lit {
            palette.wash_30
        } else {
            palette.wash_20
        };
        p.rounded_rect(bar.slider, bar.slider.h / 2, ink);
    }

    if let Some(menu) = &layout.profile_menu {
        paint_profile_menu(p, menu, ui, font, palette);
    }

    if let Some(geom) = &layout.palette {
        paint_palette(p, snapshot, ui, geom, layout.content, font, palette);
    }
    if let (Some(view), Some(geom)) = (&snapshot.modal, &layout.modal) {
        paint_modal(p, view, ui, geom, layout.content, font, palette);
    }

    // The border goes on last so nothing draws over it.
    if ui.settings.show_window_border {
        p.stroke_rect(layout.window, 1, palette.border);
    }
}

/// Repaint only what a meter step can have changed.
///
/// Each of the layout's meter rectangles gets the whole frame painted through a
/// clip, so what lands inside one is what a full repaint would have left there,
/// including whatever the slider, its ring, or an overlay puts on top. Pixels
/// outside every rectangle keep what the buffer already held, which makes this
/// correct only on a buffer already holding the rest of that frame.
pub fn paint_meters(
    p: &mut Painter,
    snapshot: &ViewSnapshot,
    ui: &UiState,
    layout: &Layout,
    font: &Font,
    palette: &Palette,
    icons: &mut IconCache,
) {
    for rect in layout.meter_damage() {
        paint_frame(
            &mut p.clipped(rect),
            snapshot,
            ui,
            layout,
            font,
            palette,
            icons,
        );
    }
}

/// Draw the window's own titlebar: the app name and the three window buttons.
fn paint_titlebar(
    p: &mut Painter,
    bar: &crate::ui::layout::TitlebarGeom,
    ui: &UiState,
    palette: &Palette,
) {
    use crate::ui::layout::HitTarget;
    if !p.intersects(bar.bar) {
        return;
    }
    p.fill(bar.bar, palette.titlebar);
    p.hline(bar.bar.x, bar.bar.bottom() - 1, bar.bar.w, palette.border);
    // The title's space belongs to the profile chip, which paint_chrome draws.
    // Two things in one strip would land on top of each other.

    // Close lights up red, the other two neutral, so the destructive one reads
    // as destructive before it is pressed. The glyphs are drawn rather than
    // typeset: the box-drawing characters they would need are missing from most
    // system fonts and come out as tofu.
    let buttons = [
        (bar.minimize, HitTarget::WindowMinimize, palette.wash_10),
        (bar.maximize, HitTarget::WindowMaximize, palette.wash_10),
        (bar.close, HitTarget::WindowClose, palette.danger_bg),
    ];
    for (rect, target, hover_bg) in buttons {
        if ui.hover.as_ref() == Some(&target) {
            p.rounded_rect(rect, PICK_RADIUS, hover_bg);
        }
        let fg = palette.text_subtle;
        // A 10px glyph box centred in the button.
        let g = rect.inset((rect.w - 10) / 2);
        match target {
            HitTarget::WindowMinimize => {
                p.line(g.x, g.y + g.h / 2, g.right(), g.y + g.h / 2, 1.5, fg);
            }
            HitTarget::WindowMaximize if ui.maximized => {
                // Restore: a square with a second one peeking out behind it.
                let back = Rect::new(g.x + 3, g.y, g.w - 3, g.h - 3);
                p.stroke_rect(back, 1, fg);
                let front = Rect::new(g.x, g.y + 3, g.w - 3, g.h - 3);
                p.fill(front, palette.titlebar);
                p.stroke_rect(front, 1, fg);
            }
            HitTarget::WindowMaximize => p.stroke_rect(g, 1, fg),
            _ => {
                p.line(g.x, g.y, g.right(), g.bottom(), 1.5, fg);
                p.line(g.right(), g.y, g.x, g.bottom(), 1.5, fg);
            }
        }
    }
}

/// The draw data for the row a column shows, read straight off the snapshot.
///
/// A map keyed by row id would answer this in one step, but building one per
/// frame is a heap allocation for a lookup over a few dozen rows that a scan
/// answers in less time than the map costs to fill.
fn row_draw<'a>(snapshot: &'a ViewSnapshot, id: &RowId) -> Option<RowDraw<'a>> {
    if let Some(d) = snapshot
        .sinks
        .iter()
        .chain(&snapshot.sources)
        .find(|d| d.id == *id)
    {
        return Some(RowDraw {
            label: &d.label,
            sublabel: &d.sublabel,
            icon_key: &d.label,
            icon_path: None,
            cubic: d.cubic,
            target_sink: None,
            muted: d.muted,
            warning: d.warning,
            tombstoned: false,
            is_default: d.is_default,
            is_app: false,
            member_count: 1,
            is_expanded: false,
            can_expand: false,
        });
    }
    let a = snapshot.app_rows.iter().find(|a| a.id == *id)?;
    Some(RowDraw {
        label: &a.label,
        sublabel: "",
        icon_key: &a.icon_key,
        icon_path: a.icon_path.as_deref(),
        cubic: a.cubic,
        target_sink: a.target_sink,
        muted: a.muted,
        warning: a.warning,
        tombstoned: a.tombstoned,
        is_default: false,
        is_app: true,
        member_count: a.member_count,
        is_expanded: a.is_expanded,
        can_expand: a.can_expand,
    })
}

/// Draw the left action bar and the profile strip: the chrome around the
/// columns, both laid out as small square picks.
fn paint_chrome(
    p: &mut Painter,
    snapshot: &ViewSnapshot,
    layout: &Layout,
    ui: &UiState,
    font: &Font,
    palette: &Palette,
) {
    if let Some(bar) = layout.profile_strip {
        p.fill(bar, palette.titlebar);
        p.hline(bar.x, bar.bottom() - 1, bar.w, palette.border);
    }
    // The action bar is a panel, not just a run of buttons: it carries the
    // surface colour so the filters read as chrome beside the mixer.
    if let Some(bar) = layout.sidebar {
        p.fill(bar, palette.surface);
    }

    use crate::ui::layout::HitTarget::*;
    for hit in &layout.hits {
        if !p.intersects(hit.rect) {
            continue;
        }
        let hovered = ui.hover.as_ref() == Some(&hit.target);
        match &hit.target {
            // Filters light up solid when their section is showing.
            SectionFilter(sec) => {
                let lit = snapshot.filter.shows(*sec);
                pick(
                    p,
                    hit.rect,
                    section_label(*sec),
                    font,
                    palette,
                    PickState {
                        lit,
                        hovered,
                        accent: false,
                    },
                );
            }
            MuteAll => pick(
                p,
                hit.rect,
                "M",
                font,
                palette,
                PickState {
                    lit: false,
                    hovered,
                    accent: false,
                },
            ),
            ResetTargets => pick(
                p,
                hit.rect,
                "R",
                font,
                palette,
                PickState {
                    lit: false,
                    hovered,
                    accent: false,
                },
            ),
            // The profile chip is a wider pick carrying the active name, with a
            // drawn caret marking it as a dropdown.
            ProfileSelector => {
                // A dropdown, not a toggle: the name it carries is the state,
                // so filling it would read as something switched on. The label
                // is left-aligned with the caret parked on the right, which is
                // what makes it read as a menu rather than a button.
                let label = snapshot.profile.active.as_deref().unwrap_or("Profiles");
                // No outline: the wash alone carries it, so it sits in the
                // chrome instead of reading as a button parked there. The caret
                // takes the label's own color rather than a dimmer one.
                let fg = palette.text;
                let r = hit.rect;
                p.rounded_rect(
                    r,
                    PICK_RADIUS,
                    if hovered {
                        palette.wash_8
                    } else {
                        palette.wash_4
                    },
                );
                let caret_w = 20;
                left_text(
                    p,
                    Rect::new(r.x + PROFILE_PAD_X, r.y, r.w - caret_w - PROFILE_PAD_X, r.h),
                    label,
                    font,
                    TextStyle::new(PROFILE_SIZE, palette.text),
                );
                let cx = r.right() - caret_w / 2;
                let cy = r.y + r.h / 2;
                p.triangle([(cx - 4, cy - 2), (cx + 4, cy - 2), (cx, cy + 3)], fg);
            }
            _ => continue,
        }
    }
}

/// How a pick button reads: filled when lit, outlined in the accent when it is
/// the active choice, plain otherwise.
struct PickState {
    lit: bool,
    hovered: bool,
    accent: bool,
}

/// Draw one small pick button: a hairline-bordered box with a centered label.
/// A pick button's background and border, returning the colour its content
/// draws in. Split out so a button can hold drawn ink instead of a glyph.
fn pick_chrome(p: &mut Painter, rect: Rect, palette: &Palette, state: PickState) -> Color {
    let (border, fg) = if state.lit {
        p.rounded_rect(rect, PICK_RADIUS, palette.filter);
        (palette.filter, palette.on_filled)
    } else {
        if state.hovered {
            p.rounded_rect(rect, PICK_RADIUS, palette.wash_5);
        }
        if state.accent {
            (palette.accent, palette.accent)
        } else {
            (palette.wash_18, palette.text_subtle)
        }
    };
    p.rounded_stroke(rect, PICK_RADIUS, 1.0, border);
    fg
}

fn pick(
    p: &mut Painter,
    rect: Rect,
    label: &str,
    font: &Font,
    palette: &Palette,
    state: PickState,
) {
    let fg = pick_chrome(p, rect, palette, state);
    centered_text(p, rect, label, font, TextStyle::new(PICK_SIZE_PT, fg));
}

fn paint_column(p: &mut Painter, col: &ColumnGeom, row: &RowDraw, cx: &mut ColumnCtx) {
    // Expanded sub-rows get a quiet wash so a parent and its children read as
    // one app rather than as unrelated neighbours.
    if matches!(col.id, RowId::AppMember(_)) {
        p.fill(col.rect, cx.palette.wash_4);
    }

    // Header. Apps show an icon over their name; devices show their form as an
    // all-caps heading over the device name.
    if row.is_app {
        draw_icon(
            p,
            col.icon,
            row.icon_path,
            row.icon_key,
            cx.font,
            ICON_RADIUS,
            cx.icons,
        );
        let name_color = if row.tombstoned {
            cx.palette.text_muted
        } else {
            cx.palette.text_subtle
        };
        // Capping the name is what holds it clear of the column edges, and the
        // suffixes are inside the cap so a grouped or idle app cannot push the
        // name wider than a plain one.
        let mut label = NameText::new();
        let mut room = MAX_NAME_CHARS;
        room -= label.push_chars(row.label.chars(), room);
        if row.member_count > 1 && room > 0 {
            let mut suffix = NameText::new();
            let _ = write!(suffix, " ×{}", row.member_count);
            room -= label.push_chars(suffix.chars(), room);
        }
        if row.tombstoned && room > 0 {
            label.push_chars(" · idle".chars(), room);
        }
        centered_text(
            p,
            col.name,
            &label,
            cx.font,
            // An app name is styled as a device name, the same as it was when
            // both were one widget.
            TextStyle::new(DEVICE_NAME_SIZE, name_color),
        );
    } else {
        // The heading takes the accent while this is the default device, which
        // is how the default reads at a glance.
        let head_color = if row.is_default {
            cx.palette.accent
        } else {
            cx.palette.text_muted
        };
        centered_text(
            p,
            col.name,
            row.sublabel,
            cx.font,
            TextStyle::new(TYPE_SIZE, head_color)
                .tracked(TYPE_TRACKING)
                .bold(),
        );
        // Capped like an app name, so a long device name keeps the same gap at
        // the column edges rather than running into them.
        let mut name = NameText::new();
        name.push_chars(row.label.chars(), MAX_NAME_CHARS);
        centered_text(
            p,
            col.sub,
            &name,
            cx.font,
            TextStyle::new(DEVICE_NAME_SIZE, cx.palette.text_subtle),
        );
    }

    paint_meter(p, col.meter, &col.id, cx.ui, cx.palette);
    // The ring's strength is animated elsewhere, so this only reads it.
    paint_slider(
        p,
        &col.slider,
        row.warning,
        cx.ui.halo.strength(&col.id),
        cx.palette,
    );

    // The percent rides on the knob, quiet enough to read as detail.
    let mut percent = NameText::new();
    let _ = write!(percent, "{}", (row.cubic * 100.0).round() as i32);
    let readout_color = if row.warning {
        cx.palette.warning
    } else {
        cx.palette.text_muted
    };
    centered_text(
        p,
        col.slider.thumb,
        &percent,
        cx.font,
        TextStyle::new(KNOB_VALUE_SIZE, readout_color).bold(),
    );

    // Target pins for apps, a make-default pin for devices, then mute.
    let hover = cx.ui.hover.as_ref();
    if row.is_app {
        // The expand toggle tops the cluster, lit while the group is open.
        if row.can_expand {
            let fg = pick_chrome(
                p,
                col.expand,
                cx.palette,
                PickState {
                    lit: row.is_expanded,
                    hovered: matches!(hover, Some(HitTarget::AppExpand(r)) if *r == col.id),
                    accent: false,
                },
            );
            // Drawn rather than typeset: the triangles are absent from most UI
            // fonts and come out as tofu. Down means the group is open.
            let ax = col.expand.x + col.expand.w / 2;
            let ay = col.expand.y + col.expand.h / 2;
            let points = if row.is_expanded {
                [(ax - 4, ay - 2), (ax + 4, ay - 2), (ax, ay + 3)]
            } else {
                [(ax - 2, ay - 4), (ax - 2, ay + 4), (ax + 3, ay)]
            };
            p.triangle(points, fg);
        }
        for (rect, sink) in &col.targets {
            // Every case is one character: automatic, the sink's initial, or a
            // sink that is no longer there.
            let initial = match sink {
                None => 'A',
                Some(id) => cx
                    .sinks
                    .iter()
                    .find(|s| s.id == *id)
                    .map_or('?', |s| s.short),
            };
            let mut buf = [0u8; 4];
            let label = initial.encode_utf8(&mut buf);
            pick(
                p,
                *rect,
                label,
                cx.font,
                cx.palette,
                PickState {
                    lit: false,
                    hovered: matches!(
                        hover,
                        Some(HitTarget::AppTarget { row: r, sink: s })
                            if *r == col.id && *s == *sink
                    ),
                    accent: *sink == row.target_sink,
                },
            );
        }
    }
    if row.muted {
        p.rounded_rect(col.mute, PICK_RADIUS, cx.palette.warning);
        p.rounded_stroke(col.mute, PICK_RADIUS, 1.0, cx.palette.warning);
        centered_text(
            p,
            col.mute,
            "M",
            cx.font,
            TextStyle::new(PICK_SIZE_PT, cx.palette.on_filled),
        );
    } else {
        pick(
            p,
            col.mute,
            "M",
            cx.font,
            cx.palette,
            PickState {
                lit: false,
                hovered: matches!(hover, Some(HitTarget::RowMute(r)) if *r == col.id),
                accent: false,
            },
        );
    }

    // The hairline closing the column on its right.
    p.fill(col.separator, cx.palette.wash_6);
}

/// Draw the fader: a rounded trough, the fill from the bottom up to the value,
/// the unity notch, and the round knob riding on top.
///
/// `halo` is how far the hover ring has faded in, 0 to 1.
fn paint_slider(p: &mut Painter, s: &SliderGeom, warning: bool, halo: f32, palette: &Palette) {
    const RADIUS: i32 = 3;
    p.rounded_rect(s.track, RADIUS, palette.dim_grid);
    let fill_color = if warning {
        palette.scale_fill_warning
    } else {
        palette.scale_fill
    };
    p.rounded_rect(s.fill, RADIUS, fill_color);

    // A short reference notch centered on the trough, so it reads as part of
    // the fader rather than a rule across the column.
    const NOTCH: i32 = 4;
    let cx = s.track.x + s.track.w / 2;
    p.hline(cx - NOTCH / 2, s.unity_y, NOTCH, palette.unity_notch);

    // A hard ring around the knob, drawn before it so the knob covers the
    // inner half and only the ring shows.
    //
    // It grows out of the knob's edge as it fades in, rather than fading in at
    // its full width. At this alpha the width is most of what the eye catches,
    // so a ring that only faded would read as appearing all at once.
    let halo = halo.clamp(0.0, 1.0);
    if halo > 0.0 {
        const HALO_WIDTH: i32 = 5;
        let width = (HALO_WIDTH as f32 * halo).round() as i32;
        if width > 0 {
            let ring = s.thumb.inset(-width);
            let alpha = (halo * 255.0).round() as u8;
            p.rounded_rect(ring, ring.h / 2, palette.knob_halo.scale_alpha(alpha));
        }
    }

    // Two stacked shadows lift the knob off the trough, the tighter one over a
    // wider, fainter one.
    knob_shadow(p, s.thumb, 1, 2, palette.shadow_strong);
    knob_shadow(p, s.thumb, 2, 4, palette.shadow_soft);

    p.rounded_rect(s.thumb, s.thumb.h / 2, palette.surface);
}

/// Lay a soft shadow under the knob: the same circle pushed down by `dy` and
/// spread over `blur` pixels.
///
/// Concentric circles at a fraction of the alpha each stack into a falloff.
/// At a 28px knob the difference from a real blur is not visible, and it costs
/// a handful of fills instead of a separate pass over the buffer.
fn knob_shadow(p: &mut Painter, thumb: Rect, dy: i32, blur: i32, color: Color) {
    let steps = blur.max(1);
    let step_alpha = (255 / steps).clamp(1, 255) as u8;
    for i in 0..steps {
        let grow = blur / 2 - i;
        let r = thumb.inset(-grow);
        let r = Rect::new(r.x, r.y + dy, r.w, r.h);
        p.rounded_rect(r, r.h / 2, color.scale_alpha(step_alpha));
    }
}

fn paint_meter(p: &mut Painter, area: Rect, id: &RowId, ui: &UiState, palette: &Palette) {
    let channels = ui.meters.channels(id);
    let n = meter::bar_count(channels.len()) as i32;
    let gap = 1;
    let bar_w = ((area.w - (n - 1) * gap) / n).max(1);
    let segs = meter::segments_for(area.h);
    let cell_h = (meter::CELL_PITCH - meter::CELL_GAP).max(1);

    for bar in 0..n {
        let bx = area.x + bar * (bar_w + gap);
        let peak = channels.get(bar as usize).copied().unwrap_or(0.0);
        let lit = meter::lit_fraction(peak);
        for i in 0..segs {
            // Whole rows throughout: the pitch divides the strip evenly, so
            // every cell is the same height and lands on a pixel boundary.
            let cell = Rect::new(
                bx,
                area.bottom() - i * meter::CELL_PITCH - cell_h,
                bar_w,
                cell_h,
            );
            p.fill(cell, palette.dim_grid);
            let cov = meter::segment_coverage(lit, segs, i);
            if cov > 0.0 {
                let color = tier_color(meter::segment_tier(i, segs), palette);
                // Only the topmost lit cell is partly covered. Dimming it by
                // how far the level reaches into it lets the tip travel
                // smoothly rather than jumping a whole cell at a time.
                let alpha = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
                p.fill(cell, color.scale_alpha(alpha));
            }
        }
    }
}

fn paint_palette(
    p: &mut Painter,
    snapshot: &ViewSnapshot,
    ui: &UiState,
    geom: &crate::ui::layout::PaletteGeom,
    content: Rect,
    font: &Font,
    palette: &Palette,
) {
    p.fill(content, palette.backdrop);
    // Panel and search field share one shade, so the field and the list below
    // it read as a single surface.
    p.rounded_rect(geom.panel, 8, palette.field_bg);
    p.rounded_stroke(geom.panel, 8, 1.0, palette.border);

    // An empty query shows the prompt in the muted color instead.
    let (query, qcolor) = if snapshot.palette.query.is_empty() {
        ("Search commands", palette.text_muted)
    } else {
        (snapshot.palette.query.as_str(), palette.text)
    };
    paint_field(p, geom.input, query, qcolor, font, palette);
    if ui.focus == Focus::Palette {
        draw_caret(p, geom.input, ui, font, palette);
    }

    let shown = &snapshot.palette.rows[geom.first_visible..geom.first_visible + geom.rows.len()];
    for (label, (offset, r)) in shown.iter().zip(geom.rows.iter().enumerate()) {
        let i = geom.first_visible + offset;
        if i == snapshot.palette.selected {
            p.rounded_rect(*r, 5, palette.wash_10);
        } else if ui.hover.as_ref() == Some(&crate::ui::layout::HitTarget::PaletteRow(i)) {
            p.rounded_rect(*r, 5, palette.wash_6);
        }
        paint_command_label(
            p,
            Rect::new(r.x + 8, r.y, r.w - 16, r.h),
            label,
            font,
            palette,
        );
    }
    if let Some(bar) = &geom.scrollbar {
        p.rounded_rect(bar.slider, bar.slider.w / 2, palette.wash_20);
    }
    if snapshot.palette.rows.is_empty() {
        left_text(
            p,
            geom.empty,
            "No matching commands",
            font,
            TextStyle::new(NAME_SIZE, palette.text_muted),
        );
    }
}

/// Draw a command row's label. The namespace it opens with ("profile: ")
/// names a group rather than the action, so it takes the muted color and the
/// action behind it keeps the full one.
fn paint_command_label(p: &mut Painter, rect: Rect, label: &str, font: &Font, palette: &Palette) {
    let action_style = TextStyle::new(NAME_SIZE, palette.text);
    let Some(split) = label.find(": ").map(|i| i + 2) else {
        left_text(p, rect, label, font, action_style);
        return;
    };
    let (namespace, action) = label.split_at(split);
    let namespace_style = TextStyle::new(NAME_SIZE, palette.text_muted);
    left_text(p, rect, namespace, font, namespace_style);
    let x = rect.x + font.text_width(namespace, namespace_style).round() as i32;
    left_text(
        p,
        Rect::new(x, rect.y, rect.right() - x, rect.h),
        action,
        font,
        action_style,
    );
}

/// Draw a text field: its rounded well and the text inside it.
fn paint_field(
    p: &mut Painter,
    field: Rect,
    text: &str,
    color: Color,
    font: &Font,
    palette: &Palette,
) {
    use crate::ui::layout::metrics::{FIELD_PAD, FIELD_TEXT_SIZE, FIELD_TEXT_TOP};
    p.rounded_rect(field, 5, palette.field_bg);
    let mut clipped = p.clipped(field);
    font.draw_text(
        &mut clipped,
        field.x + FIELD_PAD,
        field.y + FIELD_TEXT_TOP,
        text,
        TextStyle::new(FIELD_TEXT_SIZE, color),
        field.w - 2 * FIELD_PAD,
    );
}

fn paint_modal(
    p: &mut Painter,
    modal: &crate::view::snapshot::ModalView,
    ui: &UiState,
    geom: &crate::ui::layout::ModalGeom,
    content: Rect,
    font: &Font,
    palette: &Palette,
) {
    p.fill(content, palette.backdrop);
    p.rounded_rect(geom.panel, 8, palette.surface);
    p.rounded_stroke(geom.panel, 8, 1.0, palette.border);

    left_text(
        p,
        geom.title,
        &modal.title,
        font,
        TextStyle::new(NAME_SIZE, palette.text),
    );
    if !modal.body.is_empty() {
        left_text(
            p,
            geom.body,
            &modal.body,
            font,
            TextStyle::new(SUB_SIZE, palette.text_modal_body),
        );
    }
    if let Some(input) = geom.input {
        paint_field(p, input, &modal.input_value, palette.text, font, palette);
        if ui.focus == Focus::Modal {
            draw_caret(p, input, ui, font, palette);
        }
    }
    if let Some(err) = &modal.error {
        left_text(
            p,
            geom.error,
            err,
            font,
            TextStyle::new(SUB_SIZE, palette.warning),
        );
    }

    p.rounded_rect(geom.cancel, 5, palette.wash_8);
    centered_text(
        p,
        geom.cancel,
        "Cancel",
        font,
        TextStyle::new(SUB_SIZE, palette.text_subtle),
    );
    let cbg = if modal.destructive {
        palette.danger_bg
    } else {
        palette.cta_bg
    };
    p.rounded_rect(geom.confirm, 5, cbg);
    centered_text(
        p,
        geom.confirm,
        &modal.confirm_label,
        font,
        TextStyle::new(SUB_SIZE, palette.on_filled),
    );
}

/// Draw the dropped-open profile selector: one row per profile with its rename
/// and delete affordances, then the row that creates a new one.
fn paint_profile_menu(
    p: &mut Painter,
    menu: &crate::ui::layout::ProfileMenuGeom,
    ui: &UiState,
    font: &Font,
    palette: &Palette,
) {
    use crate::ui::layout::HitTarget;
    p.rounded_rect(menu.panel, 6, palette.surface);
    p.rounded_stroke(menu.panel, 6, 1.0, palette.border);

    let hovered = |target: &HitTarget| ui.hover.as_ref() == Some(target);

    for row in &menu.rows {
        // The active profile is named in the accent rather than sat on a
        // highlight, so the row under the pointer is the only lit one.
        if hovered(&HitTarget::ProfileApply(row.name.clone())) {
            p.rounded_rect(row.rect, MENU_ROW_RADIUS, palette.wash_8);
        }
        let name_color = if row.active {
            palette.accent
        } else {
            palette.text
        };
        let label = Rect::new(
            row.rect.x + MENU_PAD_X,
            row.rect.y,
            row.rect.w - MENU_PAD_X * 2,
            row.rect.h,
        );
        left_text(
            p,
            label,
            &row.name,
            font,
            TextStyle::new(MENU_SIZE, name_color),
        );
    }

    // A rule between picking a profile and making one.
    p.fill(menu.separator, palette.border);

    if hovered(&HitTarget::ProfileCreate) {
        p.rounded_rect(menu.create, MENU_ROW_RADIUS, palette.wash_8);
    }
    let create = Rect::new(
        menu.create.x + MENU_PAD_X,
        menu.create.y,
        menu.create.w - MENU_PAD_X * 2,
        menu.create.h,
    );
    left_text(
        p,
        create,
        "+ New profile",
        font,
        // Dimmer than a profile name: it is an action, not something to pick.
        TextStyle::new(MENU_SIZE, palette.text_idle),
    );
}

/// Draw text left-aligned and vertically centered, clipped to `rect`.
fn left_text(p: &mut Painter, rect: Rect, text: &str, font: &Font, style: TextStyle) {
    let text = &fit(text, font, style, rect.w);
    let th = font.text_height(style.size) as i32;
    let y = rect.y + (rect.h - th) / 2;
    let mut clipped = p.clipped(rect);
    font.draw_text(&mut clipped, rect.x, y, text, style, rect.w);
}

/// Draw the editor's selection band and caret inside a text field. Both measure
/// from the same origin the field's text is drawn at, so they land on it.
fn draw_caret(p: &mut Painter, field: Rect, ui: &UiState, font: &Font, palette: &Palette) {
    use crate::ui::layout::metrics::{FIELD_PAD, FIELD_TEXT_SIZE, FIELD_TEXT_TOP};
    let text = ui.editor.text();
    let x0 = field.x + FIELD_PAD;
    let y = field.y + FIELD_TEXT_TOP;
    let h = font.text_height(FIELD_TEXT_SIZE) as i32;
    let mut p = p.clipped(field);
    if let Some((start, end)) = ui.editor.selection() {
        let sx = font.x_at_char_offset(text, start, FIELD_TEXT_SIZE).round() as i32;
        let ex = font.x_at_char_offset(text, end, FIELD_TEXT_SIZE).round() as i32;
        p.fill(Rect::new(x0 + sx, y, (ex - sx).max(1), h), palette.wash_20);
    }
    if ui.caret_visible {
        let cx = font
            .x_at_char_offset(text, ui.editor.cursor(), FIELD_TEXT_SIZE)
            .round() as i32;
        p.fill(Rect::new(x0 + cx, y, 2, h), palette.text);
    }
}

fn tier_color(tier: Tier, palette: &Palette) -> Color {
    match tier {
        Tier::Neutral => palette.meter_neutral,
        Tier::Green => palette.meter_green,
        Tier::Amber => palette.meter_amber,
        Tier::Red => palette.meter_red,
    }
}

fn section_label(sec: crate::domain::Section) -> &'static str {
    match sec {
        crate::domain::Section::Inputs => "IN",
        crate::domain::Section::Outputs => "OUT",
        crate::domain::Section::Apps => "APP",
    }
}

/// Shorten `text` with an ellipsis until it fits `max_w`.
///
/// Text that already fits is borrowed and nothing is built. Only a run that
/// needs the ellipsis is copied, into a buffer sized past anything a column can
/// show, so a frame draws its labels without allocating.
fn fit<'a>(text: &'a str, font: &Font, style: TextStyle, max_w: i32) -> Fitted<'a> {
    // Advances are fractional, so the comparison stays in pixels rather than
    // truncating first, which would call a run almost a pixel too wide a fit.
    let max_w = max_w as f32;
    if font.text_width(text, style) <= max_w {
        return Fitted::Whole(text);
    }
    let ellipsis = '\u{2026}';
    let room = max_w - font.text_width("\u{2026}", style);
    let mut out = FittedText::new();
    out.push_str(font.truncate_to_width(text, style, room));
    out.push(ellipsis);
    Fitted::Cut(out)
}

/// How much room a shortened label can need. A column shows a few dozen
/// characters, so this is far past what any of them draws.
const FITTED_CAP: usize = 256;

type FittedText = ArrayString<FITTED_CAP>;

/// A label ready to draw: the original when it fits, otherwise the shortened
/// copy. Both read as a `&str`.
///
/// The variants differ in size by design. Holding the shortened text inline is
/// the whole point, and boxing it to even them out would put back the
/// allocation this exists to avoid. It lives on the stack for one draw call.
#[allow(clippy::large_enum_variant)]
enum Fitted<'a> {
    Whole(&'a str),
    Cut(FittedText),
}

impl std::ops::Deref for Fitted<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        match self {
            Fitted::Whole(s) => s,
            Fitted::Cut(s) => s,
        }
    }
}

/// Draw text horizontally centered in `rect`, vertically near its middle.
///
/// Text wider than the rectangle starts at its left edge and is cut to fit
/// rather than centered, which would push the overflow into the neighbouring
/// column. Drawing is clipped to the rectangle either way.
fn centered_text(p: &mut Painter, rect: Rect, text: &str, font: &Font, style: TextStyle) {
    let text = &fit(text, font, style, rect.w);
    let tw = font.text_width(text, style).round() as i32;
    let th = font.text_height(style.size) as i32;
    let x = if tw >= rect.w {
        rect.x
    } else {
        rect.x + (rect.w - tw) / 2
    };
    let y = rect.y + (rect.h - th) / 2;
    let mut clipped = p.clipped(rect);
    font.draw_text(&mut clipped, x, y, text, style, rect.w);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SinkForm, Stream, StreamKind};
    use crate::render::buffer::PixelBuffer;
    use crate::state;
    use crate::view::snapshot::build_snapshot;
    use std::path::Path;

    fn font() -> Font {
        Font::from_path(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/test-font.ttf"
        )))
        .expect("fixture font")
    }

    fn stream(id: u32, kind: StreamKind) -> Stream {
        Stream {
            name: format!("Device {id}"),
            node_name: Some(format!("node.{id}")),
            form: Some(crate::domain::DeviceForm::Output(SinkForm::Speaker)),
            is_default: id == 1,
            ..crate::domain::sample_stream(id, kind)
        }
    }

    fn scene() -> state::App {
        let mut a = state::empty();
        a.streams.insert(1, stream(1, StreamKind::Sink));
        a.streams.insert(2, stream(2, StreamKind::Source));
        let mut app = stream(3, StreamKind::Application);
        app.app_id = Some("com.example.Player".into());
        app.form = None;
        a.streams.insert(3, app);
        a
    }

    /// The same scene with a second stream under one app id, so the group's
    /// column can expand and carries the marker.
    fn expandable_scene() -> state::App {
        let mut a = scene();
        let mut extra = stream(4, StreamKind::Application);
        extra.app_id = Some("com.example.Player".into());
        extra.form = None;
        a.streams.insert(4, extra);
        a
    }

    fn render(app: &state::App, w: u32, h: u32) -> PixelBuffer {
        let f = font();
        let snap = build_snapshot(app, |_| None);
        let content = Rect::new(0, 0, w as i32, h as i32);
        let ui = UiState::new();
        let layout = crate::ui::layout::project(&snap, &ui, content);
        let mut buf = PixelBuffer::new(w, h);
        {
            let mut p = buf.painter();
            paint_frame(
                &mut p,
                &snap,
                &ui,
                &layout,
                &f,
                &Palette::dark(),
                &mut IconCache::new(),
            );
        }
        buf
    }

    /// The app column of a rendered scene, with its layout.
    fn app_column(app: &state::App) -> (crate::ui::layout::Layout, usize) {
        let snap = build_snapshot(app, |_| None);
        let ui = UiState::new();
        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, 560, 720));
        let at = layout
            .columns
            .iter()
            .position(|c| matches!(c.id, RowId::AppGroup(_)))
            .expect("an app column");
        (layout, at)
    }

    /// The toggle that opens a group's child columns sits one slot above the
    /// topmost target pin, in the same button column, and a click there reaches
    /// the expand target rather than a pin.
    #[test]
    fn an_expandable_group_gets_a_toggle_above_its_target_pins() {
        let app = expandable_scene();
        let (layout, at) = app_column(&app);
        let col = &layout.columns[at];
        let top_pin = col.targets.first().expect("an autoroute pin").0;

        assert_eq!(col.expand.x, top_pin.x, "same button column as the pins");
        assert!(
            col.expand.bottom() <= top_pin.y,
            "expand {:?} should sit above the topmost pin {top_pin:?}",
            col.expand
        );
        assert_eq!(
            layout.hit(
                col.expand.x + col.expand.w / 2,
                col.expand.y + col.expand.h / 2
            ),
            Some(&crate::ui::layout::HitTarget::AppExpand(col.id.clone())),
        );
    }

    /// A group of one has nothing to open, so it gets no toggle and the slot
    /// above its pins stays empty.
    #[test]
    fn a_single_stream_group_gets_no_toggle() {
        let app = scene();
        let (layout, at) = app_column(&app);
        let col = &layout.columns[at];
        assert!(col.expand.is_empty(), "got {:?}", col.expand);

        let top_pin = col.targets.first().expect("an autoroute pin").0;
        let above = top_pin.y - crate::ui::layout::metrics::PICK_STACK_GAP - 1;
        assert!(
            !matches!(
                layout.hit(top_pin.x + top_pin.w / 2, above),
                Some(crate::ui::layout::HitTarget::AppExpand(_))
            ),
            "nothing above the pins should expand"
        );
    }

    /// The marker is drawn, not typeset, because the triangles it would need
    /// are missing from common UI fonts. Ink inside the toggle catches it
    /// vanishing; the empty-slot comparison keeps chrome from faking a pass.
    #[test]
    fn the_toggle_carries_a_drawn_marker() {
        let bg = Palette::dark().bg.to_opaque_u32();
        let app = expandable_scene();
        let (layout, at) = app_column(&app);
        let expand = layout.columns[at].expand;
        let buf = render(&app, 560, 720);

        // Inside the border, so only the marker inks these rows.
        let inner = expand.inset(4);
        let mut ink = 0;
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                if buf.pixels()[y as usize * 560 + x as usize] != bg {
                    ink += 1;
                }
            }
        }
        assert!(ink > 0, "the toggle should carry a drawn marker");
    }

    /// The palette open over a list long enough to scroll, with the geometry
    /// the assertions need to know where to look.
    fn palette_frame() -> (PixelBuffer, crate::ui::layout::PaletteGeom) {
        let mut app = state::empty();
        app.profiles.profiles.clear();
        for i in 0..crate::command_palette::VISIBLE_ROWS + 6 {
            app.profiles.profiles.push(crate::profile::Profile {
                name: format!("p{i}"),
                ..Default::default()
            });
        }
        app.palette = Some(state::CommandPalette::default());

        let f = font();
        let snap = build_snapshot(&app, |_| None);
        let content = Rect::new(0, 0, 560, 720);
        let ui = UiState::new();
        let layout = crate::ui::layout::project(&snap, &ui, content);
        let mut buf = PixelBuffer::new(560, 720);
        {
            let mut p = buf.painter();
            paint_frame(
                &mut p,
                &snap,
                &ui,
                &layout,
                &f,
                &Palette::dark(),
                &mut IconCache::new(),
            );
        }
        (buf, layout.palette.expect("palette"))
    }

    /// The pixel at a point, as drawn.
    fn pixel_at(buf: &PixelBuffer, x: i32, y: i32) -> u32 {
        buf.pixels()[y as usize * buf.width() as usize + x as usize]
    }

    #[test]
    fn the_search_field_is_the_same_shade_as_the_panel_around_it() {
        let (buf, geom) = palette_frame();
        // Inside the field, then just above it in the panel's own padding.
        let inside = pixel_at(&buf, geom.input.right() - 3, geom.input.y + 3);
        let around = pixel_at(&buf, geom.input.right() - 3, geom.panel.y + 3);
        assert_eq!(
            inside, around,
            "the field should not read as a well cut into the panel"
        );
    }

    #[test]
    fn the_scrollbar_slider_draws_lighter_than_the_panel_behind_it() {
        let (buf, geom) = palette_frame();
        let bar = geom.scrollbar.as_ref().expect("scrollbar");
        let slider = pixel_at(
            &buf,
            bar.slider.x + bar.slider.w / 2,
            bar.slider.y + bar.slider.h / 2,
        );
        // Below the slider the track is empty, so the panel shows through.
        let empty = pixel_at(
            &buf,
            bar.slider.x + bar.slider.w / 2,
            bar.track.bottom() - 2,
        );
        assert_ne!(slider, empty, "the slider should be visible on the track");
        assert!(
            slider & 0xff > empty & 0xff,
            "the slider is a wash over the panel, so it lightens it"
        );
    }

    #[test]
    fn a_commands_namespace_is_drawn_dimmer_than_its_action() {
        let (buf, geom) = palette_frame();
        let row = geom.rows[1];
        // "profile: apply -> p0": the namespace runs to the colon, the action
        // follows it. Sample the darkest ink in each, which is the glyph body.
        let ink = |from: i32, to: i32| {
            let mut brightest = 0;
            for y in row.y..row.bottom() {
                for x in from..to {
                    brightest = brightest.max(pixel_at(&buf, x, y) & 0xff);
                }
            }
            brightest
        };
        let namespace = ink(row.x + 8, row.x + 44);
        let action = ink(row.x + 60, row.x + 140);
        assert!(
            namespace < action,
            "namespace ink {namespace} should be dimmer than action ink {action}"
        );
    }

    #[test]
    fn a_frame_draws_something_over_the_background() {
        let buf = render(&scene(), 560, 720);
        let bg = Palette::dark().bg.to_opaque_u32();
        let non_bg = buf.pixels().iter().filter(|&&p| p != bg).count();
        assert!(
            non_bg > 1000,
            "expected a drawn frame, {non_bg} non-bg pixels"
        );
    }

    /// Render with the hover ring at a given strength on the first column.
    fn render_with_halo(app: &state::App, strength: f32) -> PixelBuffer {
        use crate::ui::halo::FADE;
        use std::time::Instant;

        let f = font();
        let snap = build_snapshot(app, |_| None);
        let content = Rect::new(0, 0, 560, 720);
        let mut ui = UiState::new();
        let layout = crate::ui::layout::project(&snap, &ui, content);
        let row = layout.columns.first().map(|c| c.id.clone());

        // Drive the fade the way a shell's tick would, far enough to reach the
        // strength asked for.
        let start = Instant::now();
        ui.halo.advance(row.as_ref(), start);
        if strength > 0.0 {
            ui.halo
                .advance(row.as_ref(), start + FADE.mul_f32(strength));
        }

        let mut buf = PixelBuffer::new(560, 720);
        {
            let mut p = buf.painter();
            paint_frame(
                &mut p,
                &snap,
                &ui,
                &layout,
                &f,
                &Palette::dark(),
                &mut IconCache::new(),
            );
        }
        buf
    }

    #[test]
    fn the_hover_ring_draws_and_grows_with_the_fade() {
        // The ring is behind an animation, so nothing else here would notice
        // if it stopped being drawn at all.
        let app = scene();
        let cold = render_with_halo(&app, 0.0);
        let partway = render_with_halo(&app, 0.3);
        let full = render_with_halo(&app, 1.0);

        // The ring covers the same pixels at every strength and only differs in
        // weight, so this measures how far they moved, not how many did.
        let weight = |a: &PixelBuffer, b: &PixelBuffer| -> u64 {
            a.pixels()
                .iter()
                .zip(b.pixels())
                .map(|(x, y)| {
                    let channel = |v: u32, s: u32| ((v >> s) & 0xff) as i64;
                    (0..3)
                        .map(|i| (channel(*x, i * 8) - channel(*y, i * 8)).unsigned_abs())
                        .sum::<u64>()
                })
                .sum()
        };

        let mid = weight(&cold, &partway);
        let end = weight(&cold, &full);
        assert!(mid > 0, "a ring partway through the fade is visible");
        assert!(
            end > mid,
            "a fuller ring sits heavier than a fainter one ({end} vs {mid})",
        );

        // The ring grows out of the knob rather than fading in at full width,
        // which is most of what makes the fade visible at this alpha.
        let covered = |a: &PixelBuffer, b: &PixelBuffer| {
            a.pixels()
                .iter()
                .zip(b.pixels())
                .filter(|(x, y)| x != y)
                .count()
        };
        assert!(
            covered(&cold, &full) > covered(&cold, &partway),
            "a fuller ring reaches further out than a fainter one",
        );
    }

    #[test]
    fn identical_state_paints_identically() {
        let app = scene();
        let a = render(&app, 560, 720);
        let b = render(&app, 560, 720);
        assert_eq!(a.pixels(), b.pixels(), "rendering is deterministic");
    }

    /// Paint a frame, step the meters, and repaint through the damage gate.
    /// Returns the gated buffer beside the full repaint it has to match, and
    /// the layout both were painted from.
    fn gated_and_full(
        app: &state::App,
        mut ui: UiState,
    ) -> (PixelBuffer, PixelBuffer, crate::ui::layout::Layout) {
        let (f, palette) = (font(), Palette::dark());
        let (w, h) = (560, 720);
        let snap = build_snapshot(app, |_| None);

        let rows: Vec<_> = snap.meter_routes.values().flatten().cloned().collect();
        assert!(!rows.is_empty(), "the scene routes no meters to draw");
        for row in &rows {
            ui.meters.apply(row, &[0.15, 0.1]);
        }

        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, w, h));
        assert!(
            layout.meter_damage().next().is_some(),
            "the layout offers no meter rectangles to repaint",
        );

        let mut gated = PixelBuffer::new(w as u32, h as u32);
        paint_frame(
            &mut gated.painter(),
            &snap,
            &ui,
            &layout,
            &f,
            &palette,
            &mut IconCache::new(),
        );

        // One tick: every bar decays, then the newest peaks fold in. That pair
        // is the only thing a meter step does to the frame.
        assert!(ui.meters.decay(), "the decay moved nothing");
        for row in &rows {
            assert!(
                ui.meters.apply(row, &[0.95, 0.8]),
                "the peaks moved nothing"
            );
        }

        let mut full = PixelBuffer::new(w as u32, h as u32);
        paint_frame(
            &mut full.painter(),
            &snap,
            &ui,
            &layout,
            &f,
            &palette,
            &mut IconCache::new(),
        );
        assert!(
            gated.pixels() != full.pixels(),
            "the step changed nothing, so there is nothing to catch",
        );

        paint_meters(
            &mut gated.painter(),
            &snap,
            &ui,
            &layout,
            &f,
            &palette,
            &mut IconCache::new(),
        );
        (gated, full, layout)
    }

    /// The index of the first pixel two buffers disagree on.
    fn first_difference(a: &PixelBuffer, b: &PixelBuffer) -> Option<(i32, i32)> {
        let w = a.width() as i32;
        a.pixels()
            .iter()
            .zip(b.pixels())
            .position(|(x, y)| x != y)
            .map(|i| (i as i32 % w, i as i32 / w))
    }

    /// The claim the shell bets a frame on: repainting only the meter
    /// rectangles over the frame before leaves the pixels a full repaint would.
    /// If these ever part, a window shows a mixer that has moved on.
    #[test]
    fn a_meters_repaint_matches_a_full_one() {
        let (gated, full, _) = gated_and_full(&scene(), UiState::new());
        assert_eq!(first_difference(&gated, &full), None);
    }

    /// The same with the profile menu open, which hangs over the top of the
    /// columns. A meter it half covers is the case that would show a gate
    /// painting straight through an overlay.
    #[test]
    fn a_meters_repaint_keeps_an_overlay_on_top() {
        let mut app = scene();
        for i in 0..6 {
            app.profiles.profiles.push(crate::profile::Profile {
                name: format!("p{i}"),
                ..Default::default()
            });
        }
        let ui = UiState {
            profile_menu_open: true,
            ..UiState::new()
        };
        let (gated, full, layout) = gated_and_full(&app, ui);

        let panel = layout
            .profile_menu
            .as_ref()
            .expect("the menu is open")
            .panel;
        assert!(
            layout
                .meter_damage()
                .any(|r| !r.intersect(panel).is_empty()),
            "the menu hangs over no meter, so it covers nothing to get wrong",
        );
        assert_eq!(first_difference(&gated, &full), None);
    }

    #[test]
    fn empty_state_is_just_chrome() {
        // No streams: background and toolbar only, no columns.
        let buf = render(&state::empty(), 560, 720);
        let bg = Palette::dark().bg.to_opaque_u32();
        let titlebar = Palette::dark().titlebar.to_opaque_u32();
        // Toolbar band exists; the strip below is mostly background.
        let strip_start = 40 * 560;
        let strip_bg = buf.pixels()[strip_start..]
            .iter()
            .filter(|&&p| p == bg)
            .count();
        assert!(strip_bg > 560 * 600, "strip should be mostly background");
        assert!(buf.pixels()[..strip_start].contains(&titlebar));
    }
}
