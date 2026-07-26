//! A process-wide allocation counter for the performance gate.
//!
//! It wraps the system allocator and bumps one relaxed atomic, so a scenario can
//! report how many allocations an operation cost beside its wall-clock time.
//! Allocation count is the signal a gate can trust: it is exactly reproducible
//! for a given code path, where a stopwatch swings with the machine.
//!
//! Installed as the global allocator only under the `perf-alloc` feature, which
//! `make perf` enables, because a global allocator taxes every allocation in the
//! process and the mixer should never pay for it. With the feature off the
//! counter still exists and nothing writes it, so [`snapshot`] reads zero and
//! [`ENABLED`] is what tells that from a measured zero.
//!
//! Only Rust allocations are counted. FreeType and HarfBuzz allocate on the C
//! side, invisible to a Rust global allocator, so a text-heavy scenario performs
//! more allocations than it reports. The count is a floor, and what it catches
//! is the churn this code introduces.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Whether the counting allocator is actually installed.
#[cfg(feature = "perf-alloc")]
pub const ENABLED: bool = true;
#[cfg(not(feature = "perf-alloc"))]
pub const ENABLED: bool = false;

/// Add-only within a scenario: reset zeroes it and allocation bumps it, so it
/// never underflows and repeats exactly for a given path and input.
static ALLOCS: AtomicU64 = AtomicU64::new(0);

/// Zero the counter before measuring.
pub fn reset() {
    ALLOCS.store(0, Relaxed);
}

/// Read the counter after measuring.
pub fn count() -> u64 {
    ALLOCS.load(Relaxed)
}

/// The allocator: every call forwarded to the system allocator, counted on the
/// way through.
#[cfg(feature = "perf-alloc")]
pub struct Counting;

// SAFETY: every method forwards to the system allocator unchanged, with the same
// pointer and layout it was given, and adds only a relaxed counter bump.
#[cfg(feature = "perf-alloc")]
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: the caller's contract for alloc carries over to System.
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: ptr came from this allocator with this layout.
        unsafe { std::alloc::System.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: the caller's contract for alloc_zeroed carries over to System.
        let ptr = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        // SAFETY: ptr came from this allocator with this layout.
        let new_ptr = unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // A grow is a fresh allocation's worth of work, so count it and let
            // a Vec growing in a loop show the churn it really is.
            ALLOCS.fetch_add(1, Relaxed);
        }
        new_ptr
    }
}
