// SPDX-License-Identifier: Apache-2.0

//! Phase times for one profiled `copy_verify` run.
//!
//! The throughput harness turns this on for the import workers only. Other
//! threads stay off, so a parallel test does not add its copies to the
//! counters. The totals are sums across those workers. They can be longer
//! than the wall clock.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
}

static SOURCE_READ_NS: AtomicU64 = AtomicU64::new(0);
static HASH_NS: AtomicU64 = AtomicU64::new(0);
static WRITE_NS: AtomicU64 = AtomicU64::new(0);
static FILE_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static READBACK_READ_NS: AtomicU64 = AtomicU64::new(0);
static READBACK_HASH_NS: AtomicU64 = AtomicU64::new(0);
static DIR_SYNC_NS: AtomicU64 = AtomicU64::new(0);

/// Summed phase times. Nanoseconds are the unit so short files still add up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyPhases {
    pub source_read_ns: u64,
    pub hash_ns: u64,
    pub write_ns: u64,
    pub file_sync_ns: u64,
    pub readback_read_ns: u64,
    pub readback_hash_ns: u64,
    pub dir_sync_ns: u64,
}

impl CopyPhases {
    pub fn total_ns(self) -> u64 {
        self.source_read_ns
            .saturating_add(self.hash_ns)
            .saturating_add(self.write_ns)
            .saturating_add(self.file_sync_ns)
            .saturating_add(self.readback_read_ns)
            .saturating_add(self.readback_hash_ns)
            .saturating_add(self.dir_sync_ns)
    }

    /// This phase's share of [`CopyPhases::total_ns`]. Zero when nothing was timed.
    pub fn share(self, ns: u64) -> f64 {
        let total = self.total_ns();
        if total == 0 {
            0.0
        } else {
            (ns as f64) / (total as f64)
        }
    }
}

/// Clear the counters. Call this before the profiled workers start.
pub fn reset() {
    for slot in counters() {
        slot.store(0, Ordering::Relaxed);
    }
}

/// Read the counters. Safe to call after the profiled workers have joined.
pub fn snapshot() -> CopyPhases {
    CopyPhases {
        source_read_ns: SOURCE_READ_NS.load(Ordering::Relaxed),
        hash_ns: HASH_NS.load(Ordering::Relaxed),
        write_ns: WRITE_NS.load(Ordering::Relaxed),
        file_sync_ns: FILE_SYNC_NS.load(Ordering::Relaxed),
        readback_read_ns: READBACK_READ_NS.load(Ordering::Relaxed),
        readback_hash_ns: READBACK_HASH_NS.load(Ordering::Relaxed),
        dir_sync_ns: DIR_SYNC_NS.load(Ordering::Relaxed),
    }
}

pub fn enable_thread() {
    ON.with(|on| on.set(true));
}

/// Whether this thread records phase times. A pipeline copies this onto
/// the threads it starts.
pub(crate) fn thread_enabled() -> bool {
    ON.with(|on| on.get())
}

pub(crate) fn tic() -> Option<Instant> {
    if ON.with(|on| on.get()) {
        Some(Instant::now())
    } else {
        None
    }
}

pub(crate) fn add_source_read(start: Option<Instant>) {
    add(&SOURCE_READ_NS, start);
}

pub(crate) fn add_hash(start: Option<Instant>) {
    add(&HASH_NS, start);
}

pub(crate) fn add_write(start: Option<Instant>) {
    add(&WRITE_NS, start);
}

pub(crate) fn add_file_sync(start: Option<Instant>) {
    add(&FILE_SYNC_NS, start);
}

pub(crate) fn add_readback_read(start: Option<Instant>) {
    add(&READBACK_READ_NS, start);
}

pub(crate) fn add_readback_hash(start: Option<Instant>) {
    add(&READBACK_HASH_NS, start);
}

pub(crate) fn add_dir_sync(start: Option<Instant>) {
    add(&DIR_SYNC_NS, start);
}

fn add(slot: &AtomicU64, start: Option<Instant>) {
    let Some(start) = start else {
        return;
    };
    let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    slot.fetch_add(ns, Ordering::Relaxed);
}

fn counters() -> [&'static AtomicU64; 7] {
    [
        &SOURCE_READ_NS,
        &HASH_NS,
        &WRITE_NS,
        &FILE_SYNC_NS,
        &READBACK_READ_NS,
        &READBACK_HASH_NS,
        &DIR_SYNC_NS,
    ]
}
