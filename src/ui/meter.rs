//! Meter visual model: the decayed per-row peaks and the pure math that maps a
//! linear amplitude to a lit segment fraction and a color tier.
//!
//! The audio threads fold raw peaks into the shared pool; each frame the shell
//! decays every bar and folds in the newest reading (decay first, so a fresh
//! reading lands at full height). Drawing reads the lit fraction and tier from
//! here, keeping the level scale (dB) distinct from the slider's gain curve.

use std::collections::HashMap;
use std::time::Duration;

use crate::ui::layout::RowId;

/// Width of a segmented meter strip, in logical pixels.
pub const METER_WIDTH: i32 = 18;
/// Whole pixels from one cell to the next, its gap included.
///
/// The pitch is kept whole so every rung is the same size with sharp edges. A
/// meter's height varies with the window, so a fixed number of cells would put
/// them on a fractional pitch, which can only be drawn by spreading the edges
/// over partial rows; at this size that reads as blur.
///
/// Whole pixels leave a choice between few thick rungs and many thin ones, and
/// the rungs are what the meter is read by, so they get the rows: three of cell
/// to one of gap. Fewer than that and the ladder thins out toward a grid of
/// lines, which is harder to judge a level from than a stack of blocks.
pub const CELL_PITCH: i32 = 4;
/// Rows of background between one cell and the next.
pub const CELL_GAP: i32 = 1;
// A gap as wide as the pitch would leave no cell to draw.
const _: () = assert!(CELL_GAP < CELL_PITCH);

/// How many cells fit a meter `height` rows tall.
pub fn segments_for(height: i32) -> i32 {
    (height / CELL_PITCH).max(1)
}
/// Bars drawn before a stream has reported how many channels it carries.
/// Nearly everything is stereo, and a second bar appearing beside the first
/// once audio starts is more noticeable than one that was always there.
pub const ASSUMED_CHANNELS: usize = 2;
/// Bottom of the meter's dB scale, matching consumer level meters.
pub const METER_DB_FLOOR: f32 = -60.0;
/// Decay multiplier per tick: fast attack, slow release, dropping the bar to
/// about 10% in roughly 3 seconds.
pub const PEAK_DECAY: f32 = 0.988;
/// Meter tick interval (16 ms is about 60 Hz).
pub const PEAK_DECAY_INTERVAL: Duration = Duration::from_millis(16);
/// Below this the bar is treated as fully decayed and snapped to zero.
const DECAY_FLOOR: f32 = 0.001;

/// How many bars a row's meter shows. A row with no readings yet is drawn as
/// stereo rather than collapsed to one bar it would then have to grow out of.
pub fn bar_count(channels: usize) -> usize {
    if channels == 0 {
        ASSUMED_CHANNELS
    } else {
        channels
    }
}

/// The color tier of a lit segment. Drawing resolves it to a palette color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Neutral,
    Green,
    Amber,
    Red,
}

/// Fraction (0..=1) of the bar that should light for a linear peak, on the dB
/// scale from METER_DB_FLOOR at the bottom to 0 dB at the top.
pub fn lit_fraction(peak: f32) -> f32 {
    let db = 20.0 * peak.max(1e-6).log10();
    ((db - METER_DB_FLOOR) / -METER_DB_FLOOR).clamp(0.0, 1.0)
}

/// The tier of segment `from_bottom` (0 = quietest) of `total`. Neutral up to
/// 55%, green to 70%, amber to 90%, red above.
pub fn segment_tier(from_bottom: i32, total: i32) -> Tier {
    let green = (total as f32 * 0.55) as i32;
    let amber = (total as f32 * 0.70) as i32;
    let red = (total as f32 * 0.90) as i32;
    if from_bottom >= red {
        Tier::Red
    } else if from_bottom >= amber {
        Tier::Amber
    } else if from_bottom >= green {
        Tier::Green
    } else {
        Tier::Neutral
    }
}

/// How much of segment `from_bottom` fills for a lit fraction, 0..=1. The top
/// lit segment fills proportionally rather than snapping, which avoids stepping.
pub fn segment_coverage(lit: f32, total: i32, from_bottom: i32) -> f32 {
    let lit_segments = lit * total as f32;
    (lit_segments - from_bottom as f32).clamp(0.0, 1.0)
}

/// One decay step for a single bar value.
fn decayed(v: f32) -> f32 {
    let next = v * PEAK_DECAY;
    if next < DECAY_FLOOR { 0.0 } else { next }
}

/// Per-row decayed peaks: the meter's retained visual state.
#[derive(Default)]
pub struct MeterState {
    rows: HashMap<RowId, Vec<f32>>,
}

impl MeterState {
    pub fn new() -> Self {
        MeterState::default()
    }

    /// Ease every bar of every row toward zero. Runs once per tick, before the
    /// fresh readings are folded in. Reports whether any bar moved, so a silent
    /// window can skip repainting entirely.
    pub fn decay(&mut self) -> bool {
        let mut changed = false;
        for channels in self.rows.values_mut() {
            for v in channels.iter_mut() {
                let next = decayed(*v);
                if next != *v {
                    *v = next;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Fold fresh per-channel peaks into a row, keeping the louder of current
    /// and incoming. Resizes on a channel-count change. Reports whether any bar
    /// rose.
    pub fn apply(&mut self, row: &RowId, peaks: &[f32]) -> bool {
        let channels = self.rows.entry(row.clone()).or_default();
        let mut changed = false;
        if channels.len() != peaks.len() {
            channels.resize(peaks.len(), 0.0);
            changed = true;
        }
        for (slot, &incoming) in channels.iter_mut().zip(peaks) {
            if incoming > *slot {
                *slot = incoming;
                changed = true;
            }
        }
        changed
    }

    /// The current per-channel peaks for a row, empty if it has none yet.
    pub fn channels(&self, row: &RowId) -> &[f32] {
        self.rows.get(row).map_or(&[], Vec::as_slice)
    }

    /// Drop rows the predicate rejects, freeing meter state for vanished rows.
    pub fn retain(&mut self, keep: impl Fn(&RowId) -> bool) {
        self.rows.retain(|row, _| keep(row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_fraction_spans_the_db_range() {
        assert_eq!(lit_fraction(0.0), 0.0); // silence
        assert!(lit_fraction(0.001) < 0.02); // ~ -60 dB, at the floor
        assert_eq!(lit_fraction(1.0), 1.0); // 0 dB, full
        assert_eq!(lit_fraction(2.0), 1.0); // above 0 dB clamps
        // Louder always lights at least as much.
        assert!(lit_fraction(0.5) > lit_fraction(0.1));
    }

    #[test]
    fn cells_tile_a_meter_without_a_fractional_pitch() {
        // Whatever height a window gives the strip, the cells divide it in
        // whole pixels, which is what keeps every rung the same and sharp.
        for height in [40, 80, 140, 141, 300, 721] {
            let n = segments_for(height);
            assert!(n >= 1, "a meter always has at least one cell");
            assert!(
                n * CELL_PITCH <= height,
                "{n} cells of {CELL_PITCH} must fit within {height}",
            );
            // The leftover is smaller than a cell, so nothing visible is lost.
            assert!(height - n * CELL_PITCH < CELL_PITCH);
        }
    }

    #[test]
    fn a_row_with_no_readings_is_drawn_as_stereo() {
        assert_eq!(bar_count(0), 2, "assumed rather than collapsed to one bar");
    }

    #[test]
    fn reported_channels_always_win_over_the_assumption() {
        assert_eq!(bar_count(1), 1, "a mono stream stays mono");
        assert_eq!(bar_count(2), 2);
        assert_eq!(bar_count(6), 6, "surround is drawn as it comes");
    }

    #[test]
    fn segment_tiers_step_up_toward_the_top() {
        let n = segments_for(144);
        assert_eq!(segment_tier(0, n), Tier::Neutral);
        assert_eq!(segment_tier(n - 1, n), Tier::Red);
        // Boundaries at 55 / 70 / 90 percent.
        assert_eq!(segment_tier((n as f32 * 0.55) as i32, n), Tier::Green);
        assert_eq!(segment_tier((n as f32 * 0.70) as i32, n), Tier::Amber);
        assert_eq!(segment_tier((n as f32 * 0.90) as i32, n), Tier::Red);
    }

    #[test]
    fn segment_coverage_fills_below_and_partial_at_the_edge() {
        // lit halfway: lower segments full, upper empty, one partial at the edge.
        let total = 10;
        let lit = 0.55; // 5.5 segments lit
        assert_eq!(segment_coverage(lit, total, 0), 1.0);
        assert_eq!(segment_coverage(lit, total, 4), 1.0);
        let edge = segment_coverage(lit, total, 5);
        assert!((edge - 0.5).abs() < 1e-6, "edge was {edge}");
        assert_eq!(segment_coverage(lit, total, 6), 0.0);
    }

    #[test]
    fn apply_keeps_the_louder_and_decay_eases_down() {
        let mut m = MeterState::new();
        let row = RowId::Sink(1);
        assert!(m.apply(&row, &[0.5, 0.2]));
        assert!(m.apply(&row, &[0.3, 0.9])); // max-fold per channel
        assert_eq!(m.channels(&row), &[0.5, 0.9]);
        assert!(m.decay());
        let after = m.channels(&row);
        assert!(after[0] < 0.5 && after[0] > 0.0);
        assert!(after[1] < 0.9 && after[1] > 0.0);
    }

    #[test]
    fn decay_snaps_a_tiny_value_to_zero() {
        let mut m = MeterState::new();
        let row = RowId::Source(2);
        m.apply(&row, &[DECAY_FLOOR / 2.0]);
        assert!(m.decay());
        assert_eq!(m.channels(&row), &[0.0]);
    }

    #[test]
    fn apply_resizes_on_channel_count_change() {
        let mut m = MeterState::new();
        let row = RowId::AppGroup("app:x".into());
        m.apply(&row, &[0.5, 0.5]);
        m.apply(&row, &[0.4]); // mono now

        assert_eq!(m.channels(&row).len(), 1);
    }

    #[test]
    fn retain_drops_vanished_rows() {
        let mut m = MeterState::new();
        m.apply(&RowId::Sink(1), &[0.5]);
        m.apply(&RowId::Sink(2), &[0.5]);
        m.retain(|r| matches!(r, RowId::Sink(1)));
        assert!(!m.channels(&RowId::Sink(1)).is_empty());
        assert!(m.channels(&RowId::Sink(2)).is_empty());
    }
}
