//! Meter visual model: the decayed per-row peaks and the pure math that maps a
//! linear amplitude to a lit segment fraction and a color tier.
//!
//! The audio threads fold raw peaks into the shared pool; each step the shell
//! decays every bar and folds in the newest reading (decay first, so a fresh
//! reading lands at full height). Drawing reads the lit fraction and tier from
//! here, keeping the level scale (dB) distinct from the slider's gain curve.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::ui::layout::RowId;

/// Width of a segmented meter strip, in logical pixels.
pub const METER_WIDTH: i32 = 18;
/// Whole pixels from one cell to the next, its gap included.
///
/// The pitch is kept whole so the rungs have sharp edges. A meter's height
/// varies with the window, so a fixed number of cells would put them on a
/// fractional pitch, which can only be drawn by spreading the edges over
/// partial rows; at this size that reads as blur.
///
/// Whole pixels leave a choice between few thick rungs and many thin ones, and
/// the rungs are what the meter is read by, so they get the rows: three of cell
/// to one of gap. Fewer than that and the ladder thins out toward a grid of
/// lines, which is harder to judge a level from than a stack of blocks.
pub const CELL_PITCH: i32 = 4;
/// Rows of background between one cell and the next.
pub const CELL_GAP: i32 = 1;
/// Rows of ink in one cell.
pub const CELL_HEIGHT: i32 = CELL_PITCH - CELL_GAP;
// A gap as wide as the pitch would leave no cell to draw.
const _: () = assert!(CELL_GAP < CELL_PITCH);

/// How many cells a meter `height` rows tall shows, counting the top one the
/// strip cuts short.
pub fn segments_for(height: i32) -> i32 {
    let height = height.max(1);
    let whole = height / CELL_PITCH;
    if height % CELL_PITCH == 0 {
        whole
    } else {
        whole + 1
    }
}

/// The rows cell `from_bottom` covers in a strip `height` tall, as an offset
/// from the strip's top and a row count.
///
/// Cells stack from the bottom on the whole pitch, so cell 0 ends on the
/// strip's last row and every rung below the top one is [`CELL_HEIGHT`] tall.
/// The top cell runs to the strip's first row and takes whatever the pitch
/// leaves it, one to `CELL_PITCH` rows. That holds the ladder's top edge level
/// with the fader beside it at any window height, and grows it a row at a time
/// as the window grows.
pub fn cell_span(height: i32, from_bottom: i32) -> (i32, i32) {
    let bottom = height - from_bottom * CELL_PITCH;
    let top = if from_bottom >= segments_for(height) - 1 {
        0
    } else {
        bottom - CELL_HEIGHT
    };
    (top, (bottom - top).max(0))
}
/// Bars drawn before a stream has reported how many channels it carries.
/// Nearly everything is stereo, and a second bar appearing beside the first
/// once audio starts is more noticeable than one that was always there.
pub const ASSUMED_CHANNELS: usize = 2;
/// Bottom of the meter's dB scale, matching consumer level meters.
pub const METER_DB_FLOOR: f32 = -60.0;
/// The quietest peak the meter shows, METER_DB_FLOOR as a linear amplitude.
/// Anything below lights nothing, so it neither holds a bar up nor wakes the
/// loop to step one.
pub const METER_FLOOR: f32 = 0.001;
/// The share of its height a bar keeps for every PEAK_DECAY_INTERVAL that
/// passes: fast attack, slow release, dropping the bar to about 10% in roughly
/// 3 seconds.
pub const PEAK_DECAY: f32 = 0.988;
/// How often moving meters step, 16 ms being about 60 Hz. The decay follows
/// elapsed time, so this sets how often a playing window wakes, not how fast
/// the bars fall.
pub const PEAK_DECAY_INTERVAL: Duration = Duration::from_millis(16);
/// The longest gap between steps whose readings are still current. Moving bars
/// step every frame, so a longer gap means nothing stepped them, a hidden
/// window most likely.
const STALE_READINGS: Duration = Duration::from_millis(250);

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

/// The fold behind MeterState::apply, for one row's bars.
fn fold_peaks(channels: &mut Vec<f32>, peaks: &[f32]) -> bool {
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

/// Per-row decayed peaks: the meter's retained visual state.
#[derive(Default)]
pub struct MeterState {
    rows: HashMap<RowId, Vec<f32>>,
    /// When the bars last stepped, so the next step decays them by the time
    /// that has passed since.
    last_step: Option<Instant>,
}

impl MeterState {
    pub fn new() -> Self {
        MeterState::default()
    }

    /// Ease every bar of every row toward zero by the time since the last
    /// step. Runs before the fresh readings are folded in. Reports whether any
    /// bar moved, so a silent window can skip repainting entirely.
    pub fn decay(&mut self, now: Instant) -> bool {
        let elapsed = self
            .last_step
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        let keep = PEAK_DECAY.powf(elapsed.as_secs_f32() / PEAK_DECAY_INTERVAL.as_secs_f32());
        let mut changed = false;
        for channels in self.rows.values_mut() {
            for v in channels.iter_mut() {
                let next = *v * keep;
                let next = if next < METER_FLOOR { 0.0 } else { next };
                if next != *v {
                    *v = next;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Whether the readings the pool holds now are older than they look.
    ///
    /// The pool keeps the loudest peak since its last drain. A step that comes
    /// after a long gap finds the loudest moment of the whole gap there rather
    /// than what is playing now. Bars at rest have no such gap to account for,
    /// because the first audible peak wakes a step at once.
    pub fn readings_stale(&self, now: Instant) -> bool {
        let moving = self.rows.values().flatten().any(|&v| v > 0.0);
        moving
            && self
                .last_step
                .is_some_and(|last| now.saturating_duration_since(last) > STALE_READINGS)
    }

    /// Fold fresh per-channel peaks into a row, keeping the louder of current
    /// and incoming. Resizes on a channel-count change. Reports whether any bar
    /// rose.
    pub fn apply(&mut self, row: &RowId, peaks: &[f32]) -> bool {
        // Only a row seen for the first time takes a copy of its id. An app
        // group's id owns its name, and copying it on every step would allocate
        // for a row that is already there.
        match self.rows.get_mut(row) {
            Some(channels) => fold_peaks(channels, peaks),
            None => {
                let mut channels = Vec::new();
                let changed = fold_peaks(&mut channels, peaks);
                self.rows.insert(row.clone(), channels);
                changed
            }
        }
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
    use crate::ui::meter::*;

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
        // whole pixels, which is what keeps the rungs sharp.
        for height in 1..800 {
            let n = segments_for(height);
            assert!(n >= 1, "a meter always has at least one cell");

            let (bottom_top, bottom_h) = cell_span(height, 0);
            assert_eq!(
                bottom_top + bottom_h,
                height,
                "the first cell ends on the strip's last row",
            );
            let (top_top, top_h) = cell_span(height, n - 1);
            assert_eq!(top_top, 0, "the last cell starts on the strip's first row");
            assert!(
                (1..=CELL_PITCH).contains(&top_h),
                "the top cell takes the leftover, got {top_h} at {height}",
            );

            // Everything under the top rung is one cell tall, on the pitch,
            // with the gap between one cell and the next.
            for i in 0..n - 1 {
                let (top, h) = cell_span(height, i);
                assert_eq!(h, CELL_HEIGHT, "cell {i} of {n} at height {height}");
                let (above_top, above_h) = cell_span(height, i + 1);
                assert_eq!(top - (above_top + above_h), CELL_GAP);
            }
        }
    }

    #[test]
    fn the_top_cell_grows_a_row_at_a_time_with_the_strip() {
        // A window resize moves the strip's top edge past the grid one row at a
        // time. The ladder follows it row by row rather than a cell at a time,
        // which is what keeps the meter from stepping while the window drags.
        for height in 1..800 {
            let short = cell_span(height, segments_for(height) - 1).1;
            let tall = cell_span(height + 1, segments_for(height + 1) - 1).1;
            let grew = tall == short + 1;
            // A cell that has taken the whole pitch splits, and the new rung
            // starts over at a single row.
            let split = short == CELL_PITCH && tall == 1;
            assert!(grew || split, "{height}: {short} -> {tall}");
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
        let start = Instant::now();
        m.decay(start);
        assert!(m.decay(start + PEAK_DECAY_INTERVAL));
        let after = m.channels(&row);
        assert!(after[0] < 0.5 && after[0] > 0.0);
        assert!(after[1] < 0.9 && after[1] > 0.0);
    }

    #[test]
    fn the_fall_follows_elapsed_time_not_the_number_of_steps() {
        // A window that steps twice as often, or one whose steps land off the
        // display's beat, has to show the same fall.
        let row = RowId::Sink(1);
        let start = Instant::now();
        let mut often = MeterState::new();
        let mut seldom = MeterState::new();
        for m in [&mut often, &mut seldom] {
            m.apply(&row, &[0.8]);
            m.decay(start);
        }
        for i in 1..=30 {
            often.decay(start + PEAK_DECAY_INTERVAL * i);
        }
        seldom.decay(start + PEAK_DECAY_INTERVAL * 30);

        let (a, b) = (often.channels(&row)[0], seldom.channels(&row)[0]);
        assert!(
            (a - b).abs() < 1e-5,
            "{a} after thirty steps, {b} after one"
        );
        let expected = 0.8 * PEAK_DECAY.powi(30);
        assert!((b - expected).abs() < 1e-5, "{b}, expected {expected}");
    }

    #[test]
    fn a_step_takes_no_time_on_the_first_turn() {
        // With no step before it there is no elapsed time to decay by, so a bar
        // that has just risen is drawn at the height it rose to.
        let mut m = MeterState::new();
        let row = RowId::Sink(1);
        m.apply(&row, &[0.5]);
        assert!(!m.decay(Instant::now()), "the first step moved a bar");
        assert_eq!(m.channels(&row), &[0.5]);
    }

    #[test]
    fn decay_snaps_a_bar_below_the_floor_to_zero() {
        let mut m = MeterState::new();
        let row = RowId::Source(2);
        m.apply(&row, &[METER_FLOOR / 2.0]);
        assert!(m.decay(Instant::now()));
        assert_eq!(m.channels(&row), &[0.0]);
    }

    #[test]
    fn the_floor_is_the_bottom_of_the_scale() {
        // The wake threshold and the drawn scale have to agree, or a peak too
        // quiet to light a cell would still keep the loop stepping.
        assert_eq!(lit_fraction(METER_FLOOR * 0.9), 0.0);
        assert!(lit_fraction(METER_FLOOR) < 1e-6);
        assert!(lit_fraction(METER_FLOOR * 1.2) > 0.0);
    }

    #[test]
    fn readings_after_a_long_gap_are_stale_while_the_bars_move() {
        let row = RowId::Sink(1);
        let start = Instant::now();
        let mut m = MeterState::new();
        m.apply(&row, &[0.5]);
        m.decay(start);

        let next_frame = start + PEAK_DECAY_INTERVAL;
        assert!(!m.readings_stale(next_frame), "a frame's gap is current");
        let shown_again = start + Duration::from_secs(60);
        assert!(
            m.readings_stale(shown_again),
            "a minute unstepped trusted the pool"
        );

        // Bars at rest were stepped until they settled, and the first peak
        // after that wakes a step at once, so what the pool holds is current.
        m.decay(shown_again);
        assert_eq!(m.channels(&row), &[0.0]);
        assert!(!m.readings_stale(shown_again + Duration::from_secs(60)));
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
