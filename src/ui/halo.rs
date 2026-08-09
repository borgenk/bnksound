//! The knob's hover ring, and how it fades.
//!
//! The ring is not switched on and off: it eases in while the pointer is over
//! a knob and eases back out when it leaves, so moving across a row of faders
//! does not flash. One row rises while another falls, which is why this holds a
//! value per row rather than a single lit one.
//!
//! Progress is driven by the clock rather than by a step per tick, so the fade
//! takes the same time however often the loop happens to wake.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::ui::layout::RowId;

/// How long the ring takes to reach full strength, and to fall back from it.
///
/// Longer than the stylesheet this came from asked for. At 120ms the ring was
/// there before the eye caught it moving, which made the fade pointless.
pub const FADE: Duration = Duration::from_millis(220);

/// Below this a ring is invisible, so its row is dropped rather than kept
/// animating toward a zero it has effectively reached.
const EPSILON: f32 = 0.001;

/// Per-row ring strength, 0 to 1.
#[derive(Default)]
pub struct HaloState {
    rows: HashMap<RowId, f32>,
    /// When the last advance ran, so the next one knows how much time passed.
    last: Option<Instant>,
}

impl HaloState {
    pub fn new() -> Self {
        HaloState::default()
    }

    /// Move every row toward its target: the lit row toward full, the rest
    /// toward nothing. Reports whether anything moved, so a still window can
    /// skip repainting.
    pub fn advance(&mut self, lit: Option<&RowId>, now: Instant) -> bool {
        let dt = match self.last.replace(now) {
            Some(last) => now.saturating_duration_since(last),
            // First run: nothing has had time to move yet.
            None => return self.sync_target(lit),
        };
        let step = dt.as_secs_f32() / FADE.as_secs_f32();
        let mut changed = self.sync_target(lit);

        self.rows.retain(|row, value| {
            let target = if Some(row) == lit { 1.0 } else { 0.0 };
            let next = if (*value - target).abs() <= EPSILON {
                target
            } else if target > *value {
                (*value + step).min(target)
            } else {
                (*value - step).max(target)
            };
            if next != *value {
                *value = next;
                changed = true;
            }
            // Keep a row while it still shows something or still has somewhere
            // to go. One that has finished falling is dropped.
            *value > EPSILON || target > 0.0
        });
        changed
    }

    /// Make sure the lit row is being tracked, so it has something to rise
    /// from. Reports whether it had to be added.
    fn sync_target(&mut self, lit: Option<&RowId>) -> bool {
        match lit {
            Some(row) if !self.rows.contains_key(row) => {
                self.rows.insert(row.clone(), 0.0);
                true
            }
            _ => false,
        }
    }

    /// How strongly `row`'s ring should draw, eased so it leaves quickly and
    /// settles gently rather than running at one speed.
    pub fn strength(&self, row: &RowId) -> f32 {
        let t = self.rows.get(row).copied().unwrap_or(0.0);
        ease_out(t)
    }
}

/// How long a press mark takes to fade out. Short: it is an acknowledgement,
/// not a state, and one that outstayed the press would read as one.
const MARK: Duration = Duration::from_millis(180);

/// A press acknowledgement: struck to full by the press, easing back to nothing
/// on its own clock.
///
/// The fit button moves as the window it resizes grows, so a press can leave the
/// pointer behind and take the hover with it. The mark says the press landed
/// whatever the pointer does afterwards.
#[derive(Default)]
pub struct PressMark {
    value: f32,
    last: Option<Instant>,
}

impl PressMark {
    /// Take the mark to full, where a press leaves it.
    pub fn strike(&mut self) {
        self.value = 1.0;
    }

    /// How strongly to draw it, eased so it leaves quickly and trails off.
    pub fn strength(&self) -> f32 {
        ease_out(self.value)
    }

    /// Ease toward nothing. Reports whether it moved, so a still window can
    /// skip repainting.
    pub fn advance(&mut self, now: Instant) -> bool {
        let last = self.last.replace(now);
        if self.value <= 0.0 {
            return false;
        }
        // First run after a strike: nothing has had time to move yet.
        let Some(last) = last else {
            return false;
        };
        let step = now.saturating_duration_since(last).as_secs_f32() / MARK.as_secs_f32();
        let next = (self.value - step).max(0.0);
        let moved = next != self.value;
        self.value = next;
        moved
    }
}

/// Cubic ease-out: fast at the start, slow at the finish.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: u32) -> RowId {
        RowId::Sink(n)
    }

    /// Advance by a slice of the fade, as the loop would.
    fn tick(state: &mut HaloState, lit: Option<&RowId>, at: Instant) -> bool {
        state.advance(lit, at)
    }

    #[test]
    fn a_ring_rises_to_full_over_the_fade_and_no_sooner() {
        let mut state = HaloState::new();
        let start = Instant::now();
        let r = row(1);

        tick(&mut state, Some(&r), start);
        assert_eq!(state.strength(&r), 0.0, "it starts from nothing");

        // Halfway through the fade it is partway up, not there yet.
        tick(&mut state, Some(&r), start + FADE / 2);
        let mid = state.strength(&r);
        assert!(mid > 0.0 && mid < 1.0, "partway up, got {mid}");

        tick(&mut state, Some(&r), start + FADE * 2);
        assert_eq!(state.strength(&r), 1.0, "full once the fade has passed");
    }

    #[test]
    fn a_ring_falls_back_to_nothing_once_the_pointer_leaves() {
        let mut state = HaloState::new();
        let start = Instant::now();
        let r = row(1);

        tick(&mut state, Some(&r), start);
        tick(&mut state, Some(&r), start + FADE * 2);
        assert_eq!(state.strength(&r), 1.0);

        tick(&mut state, None, start + FADE * 2 + FADE / 2);
        let mid = state.strength(&r);
        assert!(mid > 0.0 && mid < 1.0, "partway down, got {mid}");

        tick(&mut state, None, start + FADE * 4);
        assert_eq!(state.strength(&r), 0.0, "gone once the fade has passed");
    }

    #[test]
    fn moving_between_knobs_crossfades_rather_than_snapping() {
        let mut state = HaloState::new();
        let start = Instant::now();
        let (a, b) = (row(1), row(2));

        tick(&mut state, Some(&a), start);
        tick(&mut state, Some(&a), start + FADE * 2);
        assert_eq!(state.strength(&a), 1.0);

        // The pointer moves to b: a has to still be visible on the way down.
        tick(&mut state, Some(&b), start + FADE * 2 + FADE / 4);
        assert!(state.strength(&a) > 0.0, "the old ring is still fading");
        assert!(state.strength(&b) > 0.0, "the new ring has started");
        assert!(
            state.strength(&a) > state.strength(&b),
            "the one being left is still the brighter of the two",
        );
    }

    #[test]
    fn a_finished_ring_stops_being_tracked() {
        let mut state = HaloState::new();
        let start = Instant::now();
        let r = row(1);

        tick(&mut state, Some(&r), start);
        tick(&mut state, Some(&r), start + FADE * 2);
        tick(&mut state, None, start + FADE * 4);
        assert!(
            state.rows.is_empty(),
            "a row at rest is dropped rather than animated forever",
        );
        assert_eq!(state.strength(&r), 0.0, "and still reads as nothing");
    }

    #[test]
    fn a_still_window_reports_no_movement() {
        let mut state = HaloState::new();
        let start = Instant::now();
        let r = row(1);

        tick(&mut state, Some(&r), start);
        tick(&mut state, Some(&r), start + FADE * 2);
        assert!(
            !tick(&mut state, Some(&r), start + FADE * 3),
            "a ring already at full is not a reason to repaint",
        );
        tick(&mut state, None, start + FADE * 5);
        assert!(
            !tick(&mut state, None, start + FADE * 6),
            "nor is an empty state",
        );
    }

    #[test]
    fn an_unstruck_mark_shows_nothing_and_asks_for_no_repaint() {
        let mut mark = PressMark::default();
        assert_eq!(mark.strength(), 0.0);
        let start = Instant::now();
        assert!(!mark.advance(start), "a still mark is not worth a frame");
        assert!(!mark.advance(start + MARK));
        assert_eq!(mark.strength(), 0.0);
    }

    #[test]
    fn a_struck_mark_shows_at_once_and_is_gone_by_the_end_of_the_fade() {
        let mut mark = PressMark::default();
        let start = Instant::now();
        // The clock starts running from the first advance after the strike, so
        // the press is at full for the frame it lands on.
        mark.advance(start);
        mark.strike();
        assert_eq!(
            mark.strength(),
            1.0,
            "the press shows on the frame it lands"
        );

        assert!(mark.advance(start + MARK / 2), "still moving");
        let mid = mark.strength();
        assert!(mid > 0.0 && mid < 1.0, "partway out, got {mid}");

        assert!(mark.advance(start + MARK));
        assert_eq!(mark.strength(), 0.0, "gone by the end of the fade");
        assert!(
            !mark.advance(start + MARK * 2),
            "and asks for nothing once it is gone",
        );
    }

    #[test]
    fn a_second_press_takes_the_mark_back_to_full() {
        let mut mark = PressMark::default();
        let start = Instant::now();
        mark.advance(start);
        mark.strike();
        mark.advance(start + MARK / 2);
        assert!(mark.strength() < 1.0);
        mark.strike();
        assert_eq!(mark.strength(), 1.0, "the second press reads as its own");
    }

    #[test]
    fn an_untracked_row_reads_as_nothing() {
        let state = HaloState::new();
        assert_eq!(state.strength(&row(9)), 0.0);
    }

    #[test]
    fn the_ease_runs_from_nothing_to_full_without_overshooting() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-1.0), 0.0, "clamped below");
        assert_eq!(ease_out(2.0), 1.0, "clamped above");
        // Eased out, so it is past halfway by the midpoint.
        assert!(ease_out(0.5) > 0.5);
        let mut prev = 0.0;
        for i in 0..=10 {
            let v = ease_out(i as f32 / 10.0);
            assert!(v >= prev, "never goes backwards");
            prev = v;
        }
    }
}
