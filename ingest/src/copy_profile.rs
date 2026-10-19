// SPDX-License-Identifier: Apache-2.0

//! Phase times for one profiled `copy_verify` run.
//!
//! The throughput harness turns this on for the import workers only. Other
//! threads stay off, so a parallel test does not add its copies to the
//! counters. The totals are sums across those workers. They can be longer
//! than the wall clock.
//!
//! Each phase records wall time and the calling thread's CPU time (user plus
//! system). CPU time comes from `CLOCK_THREAD_CPUTIME_ID` on Unix and
//! `GetThreadTimes` on Windows.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
enum Phase {
    Preflight = 0,
    CaseCheck,
    SourceRead,
    Hash,
    Write,
    Metadata,
    FileSync,
    ReadbackRead,
    ReadbackHash,
    DirSync,
}

const PHASES: usize = 10;

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
static WALL_NS: [AtomicU64; PHASES] = [ZERO; PHASES];
static CPU_NS: [AtomicU64; PHASES] = [ZERO; PHASES];

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
    /// Destination checks before the copy: source root, alias, exists.
    pub preflight_ns: u64,
    /// The case-collision listing of the destination directory.
    pub case_check_ns: u64,
    /// Named streams and setting the preserved mtime.
    pub metadata_ns: u64,
    /// Thread CPU time per phase, same order as [`CopyPhases::cpu_named`].
    pub cpu_ns: [u64; PHASES],
}

/// Phase names in [`CopyPhases::cpu_ns`] order.
pub const PHASE_NAMES: [&str; PHASES] = [
    "preflight",
    "case_check",
    "source_read",
    "hash",
    "write",
    "metadata",
    "file_sync",
    "readback_read",
    "readback_hash",
    "dir_sync",
];

impl CopyPhases {
    pub fn total_ns(self) -> u64 {
        self.source_read_ns
            .saturating_add(self.hash_ns)
            .saturating_add(self.write_ns)
            .saturating_add(self.file_sync_ns)
            .saturating_add(self.readback_read_ns)
            .saturating_add(self.readback_hash_ns)
            .saturating_add(self.dir_sync_ns)
            .saturating_add(self.preflight_ns)
            .saturating_add(self.case_check_ns)
            .saturating_add(self.metadata_ns)
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

    /// Thread CPU nanoseconds summed over every phase.
    pub fn cpu_total_ns(self) -> u64 {
        self.cpu_ns
            .iter()
            .fold(0u64, |sum, ns| sum.saturating_add(*ns))
    }

    /// `(name, thread CPU ns)` for each phase.
    pub fn cpu_named(self) -> [(&'static str, u64); PHASES] {
        let mut out = [("", 0u64); PHASES];
        for (slot, (name, ns)) in out.iter_mut().zip(PHASE_NAMES.iter().zip(self.cpu_ns)) {
            *slot = (name, ns);
        }
        out
    }
}

/// Clear the counters. Call this before the profiled workers start.
pub fn reset() {
    for slot in WALL_NS.iter().chain(CPU_NS.iter()) {
        slot.store(0, Ordering::Relaxed);
    }
}

/// Read the counters. Safe to call after the profiled workers have joined.
pub fn snapshot() -> CopyPhases {
    let wall = |phase: Phase| WALL_NS[phase as usize].load(Ordering::Relaxed);
    let mut cpu_ns = [0u64; PHASES];
    for (out, slot) in cpu_ns.iter_mut().zip(CPU_NS.iter()) {
        *out = slot.load(Ordering::Relaxed);
    }
    CopyPhases {
        source_read_ns: wall(Phase::SourceRead),
        hash_ns: wall(Phase::Hash),
        write_ns: wall(Phase::Write),
        file_sync_ns: wall(Phase::FileSync),
        readback_read_ns: wall(Phase::ReadbackRead),
        readback_hash_ns: wall(Phase::ReadbackHash),
        dir_sync_ns: wall(Phase::DirSync),
        preflight_ns: wall(Phase::Preflight),
        case_check_ns: wall(Phase::CaseCheck),
        metadata_ns: wall(Phase::Metadata),
        cpu_ns,
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

/// Start of one timed span: wall clock and thread CPU.
#[derive(Clone, Copy)]
pub(crate) struct Tic {
    wall: Instant,
    cpu_ns: u64,
}

pub(crate) fn tic() -> Option<Tic> {
    if ON.with(|on| on.get()) {
        Some(Tic {
            cpu_ns: thread_cpu_ns(),
            wall: Instant::now(),
        })
    } else {
        None
    }
}

fn thread_cpu_ns() -> u64 {
    albumen_win::thread_cpu_time()
        .map(|time| u64::try_from(time.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn add_preflight(start: Option<Tic>) {
    add(Phase::Preflight, start);
}

pub(crate) fn add_case_check(start: Option<Tic>) {
    add(Phase::CaseCheck, start);
}

pub(crate) fn add_source_read(start: Option<Tic>) {
    add(Phase::SourceRead, start);
}

pub(crate) fn add_hash(start: Option<Tic>) {
    add(Phase::Hash, start);
}

pub(crate) fn add_write(start: Option<Tic>) {
    add(Phase::Write, start);
}

pub(crate) fn add_metadata(start: Option<Tic>) {
    add(Phase::Metadata, start);
}

pub(crate) fn add_file_sync(start: Option<Tic>) {
    add(Phase::FileSync, start);
}

pub(crate) fn add_readback_read(start: Option<Tic>) {
    add(Phase::ReadbackRead, start);
}

pub(crate) fn add_readback_hash(start: Option<Tic>) {
    add(Phase::ReadbackHash, start);
}

pub(crate) fn add_dir_sync(start: Option<Tic>) {
    add(Phase::DirSync, start);
}

fn add(phase: Phase, start: Option<Tic>) {
    let Some(start) = start else {
        return;
    };
    let wall = u64::try_from(start.wall.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let cpu = thread_cpu_ns().saturating_sub(start.cpu_ns);
    WALL_NS[phase as usize].fetch_add(wall, Ordering::Relaxed);
    CPU_NS[phase as usize].fetch_add(cpu, Ordering::Relaxed);
}
