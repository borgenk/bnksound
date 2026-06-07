//! Fixed-capacity peak-meter storage shared between the realtime
//! PipeWire capture callbacks (the writers) and the GTK decay tick
//! (the reader).
//!
//! ```text
//!   RT audio thread(s)          GTK main thread (~60Hz tick)
//!   ┌────────────────┐          ┌─────────────────────────┐
//!   │ process cb      │          │ PeakPool::drain         │
//!   │  fold(&peaks)   │──┐    ┌─▶│  swap-read every live   │
//!   └────────────────┘  │    │  │  slot, feed the bars    │
//!   ┌────────────────┐  ▼    │  └─────────────────────────┘
//!   │ process cb      │ [ Slot; MAX_METERS ]
//!   │  fold(&peaks)   │──┘    │
//!   └────────────────┘       claim / release on monitor add / remove
//! ```
//!
//! Each monitored node owns one Slot for its capture stream's life.
//! The audio thread folds buffers in with an atomic max; the GTK thread
//! reads-and-clears to paint the bars. Peaks are |sample| (>= 0), so
//! the IEEE-754 bit pattern is monotonic and a raw-bits
//! AtomicU32::fetch_max is a correct float max in one instruction.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Channels tracked per node. Anything beyond collapses into channel 0.
pub const MAX_CHANNELS: usize = 8;

/// Nodes metered simultaneously. Past this a node gets no meter (logged
/// once) rather than panicking.
const MAX_METERS: usize = 64;

/// node.id sentinel for an unused slot. PipeWire never assigns
/// SPA_ID_INVALID (u32::MAX) to a real global, so it can't collide.
const FREE: u32 = u32::MAX;

struct Slot {
    /// The metered node's id, or FREE when the slot is unused.
    node_id: AtomicU32,
    /// Active channel count, set by the writer alongside chans.
    len: AtomicU8,
    /// Per-channel peak as f32 bits, folded with fetch_max.
    chans: [AtomicU32; MAX_CHANNELS],
}

impl Slot {
    const fn new() -> Self {
        Self {
            node_id: AtomicU32::new(FREE),
            len: AtomicU8::new(0),
            chans: [const { AtomicU32::new(0) }; MAX_CHANNELS],
        }
    }
}

/// Fixed pool of meter slots.
pub struct PeakPool {
    slots: [Slot; MAX_METERS],
}

impl PeakPool {
    pub fn new() -> Self {
        Self {
            slots: [const { Slot::new() }; MAX_METERS],
        }
    }

    /// Reserve a slot for node_id, returning an RAII handle that
    /// releases on drop. None when the pool is full.
    pub fn claim(self: &Arc<Self>, node_id: u32) -> Option<MeterSlot> {
        for (idx, slot) in self.slots.iter().enumerate() {
            // First thread to flip this slot from FREE owns it. CAS keeps
            // the pool correct even if claim ever runs off the PW thread.
            if slot
                .node_id
                .compare_exchange(FREE, node_id, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                slot.len.store(0, Ordering::Relaxed);
                for c in &slot.chans {
                    c.store(0, Ordering::Relaxed);
                }
                return Some(MeterSlot {
                    pool: Arc::clone(self),
                    idx,
                });
            }
        }
        None
    }

    /// Read-and-clear every live slot, invoking sink with the node id
    /// and its per-channel peaks. Clearing resets each interval so no
    /// transient is double-counted. Silent slots are skipped (the
    /// caller's decay step eases their bars down). Runs on the GTK thread.
    pub fn drain(&self, mut sink: impl FnMut(u32, &[f32])) {
        for slot in &self.slots {
            let node_id = slot.node_id.load(Ordering::Acquire);
            if node_id == FREE {
                continue;
            }
            let len = (slot.len.load(Ordering::Relaxed) as usize).min(MAX_CHANNELS);
            // Clear every channel regardless of len, so a shrinking
            // channel count can't strand a stale peak a later widening
            // would surface.
            let mut peaks = [0.0_f32; MAX_CHANNELS];
            let mut had_signal = false;
            for (ch, out) in peaks.iter_mut().enumerate() {
                let v = f32::from_bits(slot.chans[ch].swap(0, Ordering::Relaxed));
                if ch < len {
                    *out = v;
                    had_signal |= v > 0.0;
                }
            }
            if had_signal {
                sink(node_id, &peaks[..len]);
            }
        }
    }
}

impl Default for PeakPool {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII handle to one claimed slot; dropping returns it to the pool.
/// PipeWire never fires the process callback after teardown, so the
/// realtime writer and this drop never race.
pub struct MeterSlot {
    pool: Arc<PeakPool>,
    idx: usize,
}

impl MeterSlot {
    /// Fold one buffer's per-channel peaks into the slot with an atomic
    /// max. Runs on the realtime audio thread.
    /// peaks must be non-negative; see the module note on bit-pattern
    /// fetch_max.
    pub fn fold(&self, peaks: &[f32]) {
        let slot = &self.pool.slots[self.idx];
        let len = peaks.len().min(MAX_CHANNELS);
        slot.len.store(len as u8, Ordering::Relaxed);
        for (ch, &p) in peaks[..len].iter().enumerate() {
            slot.chans[ch].fetch_max(p.to_bits(), Ordering::Relaxed);
        }
    }
}

impl Drop for MeterSlot {
    fn drop(&mut self) {
        self.pool.slots[self.idx]
            .node_id
            .store(FREE, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect drain output into an owned list for assertions.
    fn snapshot(pool: &PeakPool) -> Vec<(u32, Vec<f32>)> {
        let mut out = Vec::new();
        pool.drain(|node_id, peaks| out.push((node_id, peaks.to_vec())));
        out
    }

    #[test]
    fn empty_pool_drains_nothing() {
        let pool = Arc::new(PeakPool::new());
        assert!(snapshot(&pool).is_empty());
    }

    #[test]
    fn fold_then_drain_returns_peaks_then_goes_silent() {
        let pool = Arc::new(PeakPool::new());
        let slot = pool.claim(7).expect("pool has room");

        slot.fold(&[0.25, 0.75]);
        assert_eq!(snapshot(&pool), vec![(7, vec![0.25, 0.75])]);

        // Cleared by the first drain, so a second drain finds it silent.
        assert!(snapshot(&pool).is_empty());
    }

    #[test]
    fn fold_keeps_the_per_channel_max_regardless_of_order() {
        let pool = Arc::new(PeakPool::new());
        let slot = pool.claim(1).expect("room");

        // Bit-pattern fetch_max must be a real float max either order.
        slot.fold(&[0.9]);
        slot.fold(&[0.1]);
        assert_eq!(snapshot(&pool), vec![(1, vec![0.9])]);

        slot.fold(&[0.1]);
        slot.fold(&[0.9]);
        assert_eq!(snapshot(&pool), vec![(1, vec![0.9])]);
    }

    #[test]
    fn fold_truncates_to_max_channels() {
        let pool = Arc::new(PeakPool::new());
        let slot = pool.claim(2).expect("room");

        let many: Vec<f32> = (0..16).map(|i| i as f32 * 0.01).collect();
        slot.fold(&many);
        let snap = snapshot(&pool);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1.len(), MAX_CHANNELS);
    }

    #[test]
    fn dropping_a_slot_releases_it_and_hides_the_node() {
        let pool = Arc::new(PeakPool::new());
        let slot = pool.claim(42).expect("room");
        slot.fold(&[0.5]);
        drop(slot);

        // Released slot is no longer drained, and its peak is gone.
        assert!(snapshot(&pool).is_empty());

        // The freed capacity is reusable.
        let reused = pool.claim(99).expect("room after release");
        reused.fold(&[0.3]);
        assert_eq!(snapshot(&pool), vec![(99, vec![0.3])]);
    }

    #[test]
    fn claim_returns_none_when_full_then_recovers() {
        let pool = Arc::new(PeakPool::new());
        let held: Vec<MeterSlot> = (0..MAX_METERS as u32)
            .map(|id| pool.claim(id).expect("within capacity"))
            .collect();

        // One past capacity: the overflow node gets no meter.
        assert!(pool.claim(MAX_METERS as u32).is_none());

        // Free one and the pool accepts again.
        drop(held.into_iter().next().expect("non-empty"));
        assert!(pool.claim(1000).is_some());
    }

    #[test]
    fn multiple_live_nodes_drain_independently() {
        let pool = Arc::new(PeakPool::new());
        let a = pool.claim(10).expect("room");
        let b = pool.claim(20).expect("room");
        a.fold(&[0.4]);
        b.fold(&[0.6, 0.2]);

        let mut snap = snapshot(&pool);
        snap.sort_by_key(|(id, _)| *id);
        assert_eq!(snap, vec![(10, vec![0.4]), (20, vec![0.6, 0.2])]);
    }
}
