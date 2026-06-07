use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::cairo;
use gtk4::prelude::*;

use crate::domain::StreamKind;
use crate::meter::PeakPool;
use crate::state::App;
use crate::ui::Widgets;
use crate::ui::row::RowWidgets;
use crate::ui::theme::Palette;

/// Width of the segmented meter strip, in pixels (two stereo bars side-by-side).
const METER_WIDTH: i32 = 18;
/// Shared height of the slider track and the meter beside it; the tallest
/// item in a column, so it sets the window's minimum height.
pub(super) const SLIDER_TRACK_HEIGHT: i32 = 140;
/// Segments stacked vertically in each meter (~2 px each over the track height).
const METER_SEGMENTS: i32 = 72;
/// Bottom of the meter's dB scale, matching consumer level meters.
const METER_DB_FLOOR: f32 = -60.0;
/// Decay multiplier per tick: classic PPM ballistic (fast attack, slow
/// release), dropping the bar to ~10% in roughly 3 seconds.
pub(super) const PEAK_DECAY: f32 = 0.988;
/// How often `Widgets::decay_meters` should be called (16 ms ≈ 60 Hz).
pub const PEAK_DECAY_INTERVAL: Duration = Duration::from_millis(16);

/// Vertical segmented level meter. Returns the DrawingArea and the
/// `Vec` that holds the latest per-channel peaks (0.0 – 1.0+ each).
/// Mutate the vec via the `RefCell` and `queue_draw` to update visually.
pub(super) fn build_meter(palette: Palette) -> (gtk::DrawingArea, Rc<RefCell<Vec<f32>>>) {
    let peaks = Rc::new(RefCell::new(Vec::<f32>::new()));
    let area = gtk::DrawingArea::builder()
        .content_width(METER_WIDTH)
        .vexpand(true)
        .height_request(SLIDER_TRACK_HEIGHT)
        // Match the slider's thumb-overshoot margins so the meter doesn't
        // outrun the track. Tuned by eye against the thumb config.
        .margin_top(12)
        .margin_bottom(12)
        .build();

    let peaks_for_draw = peaks.clone();
    area.set_draw_func(move |_, cr, w, h| {
        let snap = peaks_for_draw.borrow();
        draw_meter(&palette, cr, w, h, &snap);
    });

    (area, peaks)
}

/// Paint one segmented bar per channel, side-by-side within `w` x `h`.
/// An empty peaks slice (no peak folded in yet, or worker hasn't
/// learned the format) renders a single dim bar across the full width.
fn draw_meter(palette: &Palette, cr: &cairo::Context, w: i32, h: i32, peaks: &[f32]) {
    let n_bars = peaks.len().max(1);
    let total_gap = (n_bars - 1) as f64;
    // 1 px between bars; min bar width 1 px so many channels still draw something.
    let bar_w = ((w as f64 - total_gap) / n_bars as f64).max(1.0);

    for i in 0..n_bars {
        let x = i as f64 * (bar_w + 1.0);
        let peak = peaks.get(i).copied().unwrap_or(0.0);
        draw_meter_bar(palette, cr, x, bar_w, h, peak);
    }
}

/// Paint a single segmented bar at horizontal offset `x` with width
/// `bar_w`. Segments below the current peak light up in neutral /
/// green / yellow / red tiers; the rest stay dim. The dB level scale here
/// is intentionally distinct from the slider's cubic gain curve.
fn draw_meter_bar(palette: &Palette, cr: &cairo::Context, x: f64, bar_w: f64, h: i32, peak: f32) {
    let segments = METER_SEGMENTS;
    // Sub-pixel gap keeps the cell:gap proportions readable at 2 px cells.
    let gap = 0.5;
    let seg_h = (h as f64 - gap * (segments as f64 - 1.0)) / segments as f64;
    // Map raw linear amplitude to a dB scale (METER_DB_FLOOR at the bottom,
    // 0 dB at the top) like a classic VU/PPM meter.
    let db = 20.0 * peak.max(1e-6).log10();
    let normalized = ((db - METER_DB_FLOOR) / -METER_DB_FLOOR).clamp(0.0, 1.0);
    // Fractional, not rounded, so the topmost segment fills proportionally
    // instead of snapping (which produced the stepping/flicker look).
    let lit_segments_f = normalized * segments as f32;
    // Tier thresholds: neutral up to 55%, green to 70%, yellow to 90%, red above.
    let green_threshold = (segments as f32 * 0.55) as i32;
    let yellow_threshold = (segments as f32 * 0.70) as i32;
    let red_threshold = (segments as f32 * 0.90) as i32;

    // The unlit "off" grid is one colour for every segment; resolve once.
    let (off_r, off_g, off_b, off_a) = palette.dim_grid.rgba_f64();

    for i in 0..segments {
        // Stack from the bottom up: segment 0 is quiet, last is clipping.
        let from_bottom = i;
        let y = h as f64 - (from_bottom as f64 + 1.0) * seg_h - from_bottom as f64 * gap;

        // Dim "off" background for the full segment (shares @bnk_dim_grid
        // with the slider trough so both read as the same "off" surface).
        cr.set_source_rgba(off_r, off_g, off_b, off_a);
        cr.rectangle(x, y, bar_w, seg_h);
        let _ = cr.fill();

        // Overlay the lit portion: coverage is the fraction of this segment
        // to fill (rising from its bottom).
        let coverage = (lit_segments_f - from_bottom as f32).clamp(0.0, 1.0) as f64;
        if coverage > 0.0 {
            let (r, g, b, a) = if from_bottom >= red_threshold {
                palette.meter_red.rgba_f64()
            } else if from_bottom >= yellow_threshold {
                palette.meter_amber.rgba_f64()
            } else if from_bottom >= green_threshold {
                palette.meter_green.rgba_f64()
            } else {
                // Calm gray-blue: clearly lit but quiet, so a meter at
                // normal level doesn't read as a constant alarm.
                palette.meter_neutral.rgba_f64()
            };
            let fill_h = seg_h * coverage;
            let fill_y = y + (seg_h - fill_h);
            cr.set_source_rgba(r, g, b, a);
            cr.rectangle(x, fill_y, bar_w, fill_h);
            let _ = cr.fill();
        }
    }
}

impl Widgets {
    /// Paint the peaks the audio threads folded into pool, then ease every bar
    /// down. Decay runs first so a fresh reading lands at full height this frame
    /// (it wins the max in apply_peak).
    pub fn pump_meters(&self, state: &App, pool: &PeakPool) {
        self.decay_meters();
        pool.drain(|node_id, peaks| self.apply_peak(state, node_id, peaks));
    }

    /// Apply fresh per-channel peak readings. Sinks feed at most one row; app
    /// streams feed their group row, plus the matching member sub-row when the
    /// group is expanded.
    pub fn apply_peak(&self, state: &App, node_id: u32, peaks: &[f32]) {
        let Some(s) = state.streams.get(&node_id) else {
            return;
        };
        match s.kind {
            StreamKind::Sink => {
                if let Some(row) = self.sink_rows.get(&node_id) {
                    Self::feed_meter(row, peaks);
                }
            }
            StreamKind::Source => {
                if let Some(row) = self.source_rows.get(&node_id) {
                    Self::feed_meter(row, peaks);
                }
            }
            StreamKind::Application => {
                // Routing is precomputed by app_group::render_plan.
                let Some(route) = self.app_peak_routes.get(&node_id) else {
                    return;
                };
                if let Some(row) = self.app_rows.get(route.group_key.as_ref()) {
                    Self::feed_meter(row, peaks);
                }
                if let Some(member_key) = route.member_key.as_deref()
                    && let Some(row) = self.app_rows.get(member_key)
                {
                    Self::feed_meter(row, peaks);
                }
            }
        }
    }

    /// Raise one row's per-channel bars to max(current, incoming) and queue a
    /// redraw, resizing on channel-count change.
    fn feed_meter(row: &RowWidgets, peaks: &[f32]) {
        let mut current = row.peaks.borrow_mut();
        if current.len() != peaks.len() {
            current.resize(peaks.len(), 0.0);
        }
        for (slot, &incoming) in current.iter_mut().zip(peaks.iter()) {
            if incoming > *slot {
                *slot = incoming;
            }
        }
        drop(current);
        row.meter.queue_draw();
    }

    /// Decay every meter's per-channel peaks by PEAK_DECAY and queue a redraw,
    /// easing the bars back to zero when a node goes quiet.
    pub fn decay_meters(&self) {
        for row in self
            .sink_rows
            .values()
            .chain(self.source_rows.values())
            .chain(self.app_rows.values())
        {
            let mut peaks = row.peaks.borrow_mut();
            for slot in peaks.iter_mut() {
                let next = *slot * PEAK_DECAY;
                *slot = if next < 0.001 { 0.0 } else { next };
            }
            drop(peaks);
            row.meter.queue_draw();
        }
    }
}
