// SPDX-License-Identifier: Apache-2.0

//! Real-size `copy_verify` throughput against a raw durable write.
//!
//! The files are generated here. They are not fixtures and they are not
//! committed. Each one is a deterministic JPEG-like byte string of a fixed
//! length. The import clock is only `copy_verify`. Generation, the source
//! re-read, and the raw write are outside that clock.
//!
//! Each file is hashed once, while its bytes are written, then the
//! destination is read back and hashed again. Files run on a bounded worker
//! pool so that hash work overlaps I/O. Each lane reads and hashes the next
//! file, writes and syncs the current one, and reads back and directory-syncs
//! the one before, on three threads. Each lane reuses its read buffers. The pool is `available_parallelism`
//! capped at [`MAX_IMPORT_WORKERS`]. The raw write stays sequential. It uses
//! the same durability calls as one file of the import: `open_new`,
//! `File::sync_all`, and `dest::sync_dir` after every file. On macOS that
//! `sync_all` is `F_FULLFSYNC`. A plain `fsync` sample is also recorded
//! there, and the ratio uses `F_FULLFSYNC`, which is what the import uses.
//! The raw rate is the median of samples taken before and after the import.
//! The raw write does not hash and it does not preserve a source mtime. The
//! ratio is import MB/s divided by that raw MB/s. 1 MB is 1,000,000 bytes.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Instant, SystemTime};

use albumen_ingest::dest::{self, open_new};
use albumen_ingest::{
    copy_phases, copy_verify_files_pipelined, enable_copy_profile_thread, reset_copy_profile,
    sha256_hex, CopyPhases,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Files in the CI run. A smoke run may pass a smaller count to
/// [`measure_throughput`] directly. The CLI refuses a smaller count unless
/// `ALBUMEN_THROUGHPUT_ALLOW_SMALL=1`.
pub const DEFAULT_THROUGHPUT_FILES: u64 = 1_000;
/// About 5 MB, using the 1,000,000-byte megabyte from the performance budgets.
pub const DEFAULT_THROUGHPUT_BYTES: u64 = 5_000_000;
/// Product target for import MB/s divided by raw MB/s. CI prints the ratio
/// and does not fail below this. See `docs/performance-budgets.md`.
pub const THROUGHPUT_RATIO_FLOOR: f64 = 0.80;
/// The earlier import CPU target: at most this percent of the machine while
/// 1,000 photos import. Now printed as context next to CPU seconds per GB,
/// because it moves with disk speed. The definition is
/// [`ThroughputReport::cpu_percent`].
pub const CPU_PERCENT_TARGET: f64 = 10.0;

/// Scored team target for CPU seconds per GB imported (10^9 bytes; copy,
/// both SHA-256 passes, fsync, and read-back included). Report only. The
/// derivation is in docs/performance-budgets.md.
pub const CPU_SECONDS_PER_GB_TARGET: f64 = 2.0;
/// Team regression gate for normalized CPU seconds per GB imported: the
/// import throughput job fails above it on any leg.
pub const CPU_SECONDS_PER_GB_GATE: f64 = 3.0;
/// Reference one-thread in-memory SHA-256 rate the hash phases are
/// normalized to, in MB/s.
pub const SHA256_REFERENCE_MB_S: f64 = 1800.0;
/// Upper bound on import workers. The pool is also limited by the CPU count
/// and by the number of files.
pub const MAX_IMPORT_WORKERS: usize = 8;
/// Half the number of raw samples, and of import samples.
///
/// The run takes `2 * RAW_SAMPLES_PER_SIDE` rounds. Each round is one raw
/// sample and then one import, so the raw and import medians see the same
/// disk and cache state over the run (ABAB). One sample on a warm or cold
/// disk does not move either median.
pub const RAW_SAMPLES_PER_SIDE: usize = 3;

const JPEG_PREFIX: &[u8] = &[
    0xFF, 0xD8, // SOI
    0xFF, 0xE0, // APP0
    0x00, 0x10, // segment length 16
    b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
];
const JPEG_EOI: &[u8] = &[0xFF, 0xD9];

#[derive(Debug, Clone, PartialEq)]
pub struct ThroughputReport {
    pub leg: String,
    pub files: u64,
    pub bytes_each: u64,
    pub bytes: u64,
    pub import_seconds: f64,
    pub import_mb_s: f64,
    pub import_items_s: f64,
    pub raw_seconds: f64,
    pub raw_mb_s: f64,
    /// Durability primitive shared by the import and [`Self::raw_mb_s`].
    ///
    /// `File::sync_all` is `fsync` on Linux, `F_FULLFSYNC` on macOS, and
    /// `FlushFileBuffers` on Windows.
    pub raw_sync: String,
    /// Median of a plain `fsync` raw write. On Linux this is the same call
    /// as [`Self::raw_mb_s`]. On macOS it is the other primitive. On Windows
    /// there is no `fsync`, so this is absent.
    pub raw_fsync_mb_s: Option<f64>,
    /// Median of an `F_FULLFSYNC` raw write. Present on macOS, where it is
    /// the honest raw rate. Absent where the OS has no `F_FULLFSYNC`.
    pub raw_fullfsync_mb_s: Option<f64>,
    /// Honest raw samples, before the import and then after it.
    pub raw_samples: Vec<f64>,
    pub phases: CopyPhases,
    pub ratio: f64,
    /// Ceiling under the import's durability: the same number of workers
    /// as the import, each writing files from a warm buffer with
    /// `File::sync_all` on every file and `dest::sync_dir` after every file.
    /// No source read, no hash, no read-back. Median of the samples.
    pub ceiling_mb_s: f64,
    pub ceiling_samples: Vec<f64>,
    /// [`Self::ceiling_mb_s`] divided by [`Self::raw_mb_s`]. The import
    /// ratio cannot reach this with the same syncs.
    pub ceiling_ratio: f64,
    /// Like [`Self::ceiling_mb_s`], but each file's bytes are first read
    /// from its generated source file. Still no hash and no read-back.
    pub copy_ceiling_mb_s: f64,
    pub copy_ceiling_samples: Vec<f64>,
    /// [`Self::copy_ceiling_mb_s`] divided by [`Self::raw_mb_s`].
    pub copy_ceiling_ratio: f64,
    pub count_in: u64,
    pub count_out: u64,
    pub mismatched: u64,
    pub source_changed: u64,
    pub workers: u64,
    /// Process CPU time (user plus system, all threads) spent during the
    /// timed import, in seconds.
    pub cpu_seconds: f64,
    /// Logical cores, from `std::thread::available_parallelism`.
    pub cores: u64,
    /// `cpu_seconds / (import_seconds * cores) * 100`: the share of the whole
    /// machine the import used while it ran. 100 means every core was busy
    /// for the whole import.
    pub cpu_percent: f64,
    /// `cpu_seconds * 1000 / files`: CPU seconds per 1,000 imported photos.
    pub cpu_seconds_per_1000: f64,
    /// Read-back threads behind the import's writer lanes.
    pub verifiers: u64,
    /// Interleaved rounds. Each round takes one raw sample and then one
    /// import sample, so both see the same disk and cache over the run.
    pub rounds: u64,
    /// One import rate per round. [`Self::import_mb_s`] is their median.
    pub import_samples: Vec<f64>,
    /// Share of source pages in the page cache just before each sample,
    /// from `mincore`. `None` where it cannot be read (Windows).
    pub resident_before_raw: Vec<Option<f64>>,
    pub resident_before_import: Vec<Option<f64>>,
    pub resident_before_copy_ceiling: Vec<Option<f64>>,
    /// `cpu_seconds / (bytes / 10^9)`: CPU seconds per GB imported, per
    /// round. Unlike `cpu_percent`, it does not move with disk speed.
    pub cpu_seconds_per_gb: f64,
    /// [`Self::cpu_seconds_per_gb`] with only the hash and read-back hash
    /// phases rescaled to a [`SHA256_REFERENCE_MB_S`] runner: their thread
    /// CPU times `sha256_mb_s / 1800`. Everything else stays as measured.
    /// The gate and the scored target use this number.
    pub cpu_seconds_per_gb_normalized: f64,
    /// The CPU has the SHA-256 instructions `sha2` checks for (SHA-NI on
    /// x86_64, SHA2 on aarch64).
    pub sha_hw: bool,
    /// In-memory SHA-256 rate on one thread, for the same `sha2` build.
    pub sha256_mb_s: f64,
}

impl ThroughputReport {
    /// 100 percent means every generated file was verified into the
    /// destination, no digest disagreed, and every source byte and mtime
    /// still matches the snapshot taken before the import.
    pub fn reconciled(&self) -> bool {
        self.files > 0
            && self.count_in == self.files
            && self.count_out == self.files
            && self.mismatched == 0
            && self.source_changed == 0
            && self.bytes == self.files.saturating_mul(self.bytes_each)
    }

    pub fn reconciled_percent(&self) -> f64 {
        if self.files == 0 {
            return 0.0;
        }
        if self.reconciled() {
            return 100.0;
        }
        let bad = self.mismatched.saturating_add(self.source_changed);
        let good = self.files.saturating_sub(bad.min(self.files));
        (good as f64) * 100.0 / (self.files as f64)
    }

    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "leg": self.leg,
            "files": self.files,
            "bytes_each": self.bytes_each,
            "bytes": self.bytes,
            "import_seconds": self.import_seconds,
            "import_mb_s": self.import_mb_s,
            "import_items_s": self.import_items_s,
            "raw_seconds": self.raw_seconds,
            "raw_mb_s": self.raw_mb_s,
            "raw_sync": self.raw_sync,
            "raw_fsync_mb_s": self.raw_fsync_mb_s,
            "raw_fullfsync_mb_s": self.raw_fullfsync_mb_s,
            "raw_samples": self.raw_samples,
            "phase_ms": phase_ms(&self.phases),
            "phase_share": phase_share(&self.phases),
            "ratio": self.ratio,
            "ceiling_mb_s": self.ceiling_mb_s,
            "ceiling_samples": self.ceiling_samples,
            "ceiling_ratio": self.ceiling_ratio,
            "copy_ceiling_mb_s": self.copy_ceiling_mb_s,
            "copy_ceiling_samples": self.copy_ceiling_samples,
            "copy_ceiling_ratio": self.copy_ceiling_ratio,
            "count_in": self.count_in,
            "count_out": self.count_out,
            "mismatched": self.mismatched,
            "source_changed": self.source_changed,
            "workers": self.workers,
            "verifiers": self.verifiers,
            "comparison": "abab",
            "rounds": self.rounds,
            "import_samples": self.import_samples,
            "source_resident": {
                "before_raw": self.resident_before_raw,
                "before_import": self.resident_before_import,
                "before_copy_ceiling": self.resident_before_copy_ceiling,
            },
            "reconciled_percent": self.reconciled_percent(),
            "cpu_seconds": self.cpu_seconds,
            "cores": self.cores,
            "cpu_percent": self.cpu_percent,
            "cpu_seconds_per_1000": self.cpu_seconds_per_1000,
            "cpu_percent_target": CPU_PERCENT_TARGET,
            "cpu_percent_met": self.cpu_percent <= CPU_PERCENT_TARGET,
        });
        // A second step keeps the json! macro under its recursion limit.
        value["cpu_seconds_per_gb"] = json!(self.cpu_seconds_per_gb);
        value["cpu_seconds_per_gb_target"] = json!(CPU_SECONDS_PER_GB_TARGET);
        value["cpu_seconds_per_gb_gate"] = json!(CPU_SECONDS_PER_GB_GATE);
        value["cpu_seconds_per_gb_normalized"] = json!(self.cpu_seconds_per_gb_normalized);
        value["sha256_reference_mb_s"] = json!(SHA256_REFERENCE_MB_S);
        value["cpu_seconds_per_gb_gate_passed"] =
            json!(cpu_gate_passes(self.cpu_seconds_per_gb_normalized));
        value["cpu_seconds_per_gb_met"] =
            json!(self.cpu_seconds_per_gb_normalized <= CPU_SECONDS_PER_GB_TARGET);
        value["phase_cpu_ms_per_round"] = phase_cpu_ms(self);
        value["sha_hw"] = json!(self.sha_hw);
        value["sha256_mb_s"] = json!(self.sha256_mb_s);
        value
    }
}

/// Fail when reconciliation is short of 100 percent.
///
/// The 80 percent ratio is the product target in
/// [`THROUGHPUT_RATIO_FLOOR`]. It is not a failure here. The import reads
/// the destination back and fsyncs each directory entry. The raw write does
/// neither hash. On one machine the raw write moved between runs while the
/// import stayed put, so the ratio moved with the raw write. A later run
/// under 80 percent of a recorded per-leg ratio can be a regression gate.
/// That gate is not this 80 percent target.
pub fn throughput_problems(report: &ThroughputReport) -> Vec<String> {
    let mut problems = Vec::new();
    if !report.reconciled() {
        problems.push(format!(
            "reconciliation {}%: count_in {} count_out {} mismatched {} source_changed {}",
            report.reconciled_percent(),
            report.count_in,
            report.count_out,
            report.mismatched,
            report.source_changed
        ));
    }
    if !cpu_gate_passes(report.cpu_seconds_per_gb_normalized) {
        problems.push(format!(
            "import CPU {:.3} normalized CPU seconds per GB (raw {:.3}, sha256_mb_s {:.0}) is above the {:.1} regression gate",
            report.cpu_seconds_per_gb_normalized,
            report.cpu_seconds_per_gb,
            report.sha256_mb_s,
            CPU_SECONDS_PER_GB_GATE
        ));
    }
    problems
}

/// The CPU gate passes at or below [`CPU_SECONDS_PER_GB_GATE`], compared at
/// three decimals, as the job prints it.
pub fn cpu_gate_passes(cpu_seconds_per_gb: f64) -> bool {
    cpu_seconds_per_gb.is_finite()
        && (cpu_seconds_per_gb * 1000.0).round() <= (CPU_SECONDS_PER_GB_GATE * 1000.0).round()
}

pub fn ratio_passes(ratio: f64) -> bool {
    ratio.is_finite() && (ratio * 1000.0).round() >= (THROUGHPUT_RATIO_FLOOR * 1000.0).round()
}

pub fn measure_throughput(
    leg: &str,
    files: u64,
    bytes_each: u64,
) -> Result<ThroughputReport, String> {
    if files == 0 || bytes_each < (JPEG_PREFIX.len() + JPEG_EOI.len()) as u64 {
        return Err(
            "throughput needs at least one file large enough for a JPEG header".to_string(),
        );
    }
    let root = TempDir::new()?;
    let sources = root.path.join("sources");
    let dest = root.path.join("dest");
    fs::create_dir_all(&sources).map_err(|err| format!("create sources: {err}"))?;
    let (snaps, _unlock) = generate(&sources, files, bytes_each)?;

    let source_paths: Vec<PathBuf> = snaps.iter().map(|snap| snap.path.clone()).collect();
    let workers = import_lanes(files);
    let rounds = RAW_SAMPLES_PER_SIDE * 2;
    let mut raw_samples = Vec::with_capacity(rounds);
    let mut plain_samples = Vec::new();
    let mut import_samples = Vec::with_capacity(rounds);
    let mut ceiling_samples = Vec::with_capacity(RAW_SAMPLES_PER_SIDE);
    let mut copy_ceiling_samples = Vec::with_capacity(RAW_SAMPLES_PER_SIDE);
    let mut resident_before_raw = Vec::with_capacity(rounds);
    let mut resident_before_import = Vec::with_capacity(rounds);
    let mut resident_before_copy_ceiling = Vec::with_capacity(RAW_SAMPLES_PER_SIDE);
    let mut count_out = files;
    let mut mismatched = 0;
    reset_copy_profile();
    let mut cpu_seconds = 0.0;
    let mut import_seconds_total = 0.0;
    for round in 0..rounds {
        resident_before_raw.push(resident_share(&source_paths));
        sample_raw_once(
            &root.path.join("raw"),
            files,
            bytes_each,
            &mut raw_samples,
            &mut plain_samples,
        )?;

        resident_before_import.push(resident_share(&source_paths));
        fs::create_dir_all(&dest).map_err(|err| format!("create dest: {err}"))?;
        let cpu_before = process_cpu_time()?;
        let started = Instant::now();
        let (out, bad, imported_bytes) = import_parallel(&snaps, &dest, workers)?;
        let seconds = started.elapsed().as_secs_f64().max(1e-9);
        cpu_seconds += process_cpu_time()?.saturating_sub(cpu_before).as_secs_f64();
        import_seconds_total += seconds;
        import_samples.push((imported_bytes as f64) / 1_000_000.0 / seconds);
        // Every round must reconcile, so the report keeps the worst one.
        count_out = count_out.min(out);
        mismatched += bad;
        // The destination was created by this process for the measurement.
        // It is not a source. Removing it frees the disk for the next
        // sample. The source directory stays, read-only, until the temp
        // root drops.
        fs::remove_dir_all(&dest).map_err(|err| format!("remove measured destination: {err}"))?;

        if round % 2 == 0 {
            ceiling_samples.push(ceiling_sample(
                &root.path.join("ceiling"),
                files,
                bytes_each,
                workers,
            )?);
            resident_before_copy_ceiling.push(resident_share(&source_paths));
            copy_ceiling_samples.push(ceiling_sample_from(
                &root.path.join("copy-ceiling"),
                files,
                bytes_each,
                workers,
                Some(&source_paths),
            )?);
        }
    }
    let cores = thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1) as u64;
    let phases = copy_phases();
    let source_changed = sources_changed(&snaps)?;
    let ceiling_mb_s = median(&mut ceiling_samples.clone());
    let copy_ceiling_mb_s = median(&mut copy_ceiling_samples.clone());
    let expected_bytes = files.saturating_mul(bytes_each);
    let import_mb_s = median(&mut import_samples.clone());
    let import_seconds = (expected_bytes as f64) / 1_000_000.0 / import_mb_s.max(1e-9);
    let raw_mb_s = median(&mut raw_samples.clone());
    let (raw_fsync_mb_s, raw_fullfsync_mb_s) = raw_primitive_rates(raw_mb_s, &plain_samples);
    if !(raw_mb_s.is_finite() && raw_mb_s > 0.0 && import_mb_s.is_finite() && import_mb_s > 0.0) {
        return Err("import or raw throughput was not a positive number".to_string());
    }
    let gb = (expected_bytes.max(1) as f64) / 1e9;
    let sha_rate = sha256_mb_s();
    Ok(ThroughputReport {
        leg: leg.to_string(),
        files,
        bytes_each,
        bytes: expected_bytes,
        import_seconds,
        import_mb_s,
        import_items_s: (count_out as f64) / import_seconds,
        raw_seconds: (expected_bytes as f64) / 1_000_000.0 / raw_mb_s,
        raw_mb_s,
        raw_sync: IMPORT_FILE_SYNC.to_string(),
        raw_fsync_mb_s,
        raw_fullfsync_mb_s,
        raw_samples,
        phases,
        ratio: import_mb_s / raw_mb_s,
        ceiling_mb_s,
        ceiling_samples,
        ceiling_ratio: ceiling_mb_s / raw_mb_s,
        copy_ceiling_mb_s,
        copy_ceiling_samples,
        copy_ceiling_ratio: copy_ceiling_mb_s / raw_mb_s,
        count_in: files,
        count_out,
        mismatched,
        source_changed,
        workers: u64::try_from(workers).unwrap_or(0),
        verifiers: u64::try_from(albumen_ingest::pipelined_verifiers(workers)).unwrap_or(0),
        rounds: u64::try_from(rounds).unwrap_or(0),
        import_samples,
        resident_before_raw,
        resident_before_import,
        resident_before_copy_ceiling,
        cpu_seconds: cpu_seconds / rounds as f64,
        cores,
        cpu_percent: cpu_seconds / (import_seconds_total.max(1e-9) * cores as f64) * 100.0,
        cpu_seconds_per_1000: cpu_seconds / rounds as f64 * 1000.0 / (files as f64),
        cpu_seconds_per_gb: cpu_seconds / rounds as f64 / gb,
        sha_hw: sha_hw(),
        sha256_mb_s: sha_rate,
        cpu_seconds_per_gb_normalized: normalized_cpu_seconds_per_gb(
            cpu_seconds / rounds as f64 / gb,
            hash_cpu_ns(&phases) as f64 / 1e9 / rounds as f64 / gb,
            sha_rate,
        ),
    })
}

/// Thread CPU of the two SHA-256 phases, hash and read-back hash, summed.
fn hash_cpu_ns(phases: &albumen_ingest::CopyPhases) -> u64 {
    phases
        .cpu_named()
        .iter()
        .filter(|(name, _)| matches!(*name, "hash" | "readback_hash"))
        .fold(0u64, |sum, (_, ns)| sum.saturating_add(*ns))
}

/// `raw - hash + hash * sha256_mb_s / 1800`: the hash phases' CPU as it
/// would be on a runner whose one-thread SHA-256 runs at 1,800 MB/s. A
/// runner that hashes slower is not charged for its slower CPU; one that
/// hashes faster gets no credit for it. Without a usable rate the raw
/// number is returned.
pub fn normalized_cpu_seconds_per_gb(raw: f64, hash: f64, sha256_mb_s: f64) -> f64 {
    if !(sha256_mb_s.is_finite() && sha256_mb_s > 0.0 && hash.is_finite()) {
        return raw;
    }
    raw - hash + hash * sha256_mb_s / SHA256_REFERENCE_MB_S
}

/// User plus system CPU time of this process, all threads.
#[cfg(unix)]
fn process_cpu_time() -> Result<std::time::Duration, String> {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    Ok(std::time::Duration::new(
        u64::try_from(time.tv_sec).unwrap_or(0),
        u32::try_from(time.tv_nsec).unwrap_or(0),
    ))
}

#[cfg(windows)]
fn process_cpu_time() -> Result<std::time::Duration, String> {
    albumen_win::process_cpu_time().map_err(|err| format!("GetProcessTimes: {err}"))
}

/// Import lanes for this run: the worker pool on every system. Each lane
/// keeps one file sync in flight, and its prefetch and verify threads
/// overlap the read, hash, and read-back with it.
fn import_lanes(files: u64) -> usize {
    import_workers(files)
}

fn import_workers(files: u64) -> usize {
    let cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let files = usize::try_from(files).unwrap_or(usize::MAX);
    cpus.min(MAX_IMPORT_WORKERS).min(files).max(1)
}

fn import_parallel(snaps: &[Snap], dest: &Path, workers: usize) -> Result<(u64, u64, u64), String> {
    let count_out = AtomicU64::new(0);
    let mismatched = AtomicU64::new(0);
    let imported_bytes = AtomicU64::new(0);
    let error: Mutex<Option<String>> = Mutex::new(None);
    let sources: Vec<PathBuf> = snaps.iter().map(|snap| snap.path.clone()).collect();
    // The pipeline starts its own threads and copies this thread's profile
    // switch onto them.
    enable_copy_profile_thread();
    copy_verify_files_pipelined(&sources, dest, workers, |index, result| {
        let snap = &snaps[index];
        match result {
            Ok(stored) => {
                if stored.sha256 == snap.sha256
                    && stored.byte_len == snap.len
                    && stored.filename == snap.name
                {
                    count_out.fetch_add(1, Ordering::Relaxed);
                    imported_bytes.fetch_add(stored.byte_len, Ordering::Relaxed);
                } else {
                    mismatched.fetch_add(1, Ordering::Relaxed);
                }
                true
            }
            Err(err) => {
                record_error(&error, err.to_string());
                false
            }
        }
    });
    if let Some(err) = error
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
    {
        return Err(err);
    }
    Ok((
        count_out.load(Ordering::Relaxed),
        mismatched.load(Ordering::Relaxed),
        imported_bytes.load(Ordering::Relaxed),
    ))
}

fn record_error(error: &Mutex<Option<String>>, err: String) {
    let mut slot = error.lock().unwrap_or_else(|poison| poison.into_inner());
    if slot.is_none() {
        *slot = Some(err);
    }
}

struct Snap {
    name: String,
    path: PathBuf,
    sha256: String,
    len: u64,
    mtime: SystemTime,
}

fn generate(dir: &Path, files: u64, bytes_each: u64) -> Result<(Vec<Snap>, UnlockOnDrop), String> {
    let mut snaps = Vec::with_capacity(usize::try_from(files).unwrap_or(0));
    let mut unlock = UnlockOnDrop {
        saved: Vec::with_capacity(usize::try_from(files).unwrap_or(0)),
    };
    for index in 0..files {
        let name = format!("img-{index:04}.jpg");
        let path = dir.join(&name);
        let sha256 = write_jpeg(&path, index, bytes_each)?;
        let meta = fs::metadata(&path).map_err(|err| format!("stat {name}: {err}"))?;
        if meta.len() != bytes_each {
            return Err(format!("{name} is not {bytes_each} bytes"));
        }
        let original = meta.permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly)
            .map_err(|err| format!("mark {name} read-only: {err}"))?;
        unlock.saved.push((path.clone(), original));
        let meta = fs::metadata(&path).map_err(|err| format!("stat {name}: {err}"))?;
        let mtime = meta
            .modified()
            .map_err(|err| format!("mtime {}: {err}", path.display()))?;
        snaps.push(Snap {
            name,
            path,
            sha256,
            len: meta.len(),
            mtime,
        });
    }
    Ok((snaps, unlock))
}

fn write_jpeg(path: &Path, index: u64, len: u64) -> Result<String, String> {
    let overhead = (JPEG_PREFIX.len() + JPEG_EOI.len()) as u64;
    if len < overhead {
        return Err(format!("file length {len} is shorter than the JPEG header"));
    }
    let mut file = File::create(path).map_err(|err| format!("create {}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(JPEG_PREFIX);
    file.write_all(JPEG_PREFIX)
        .map_err(|err| format!("write {}: {err}", path.display()))?;
    let mut left = len - overhead;
    let mut buf = vec![0u8; 1024 * 1024];
    while left > 0 {
        let n = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        fill(&mut buf[..n], index);
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|err| format!("write {}: {err}", path.display()))?;
        left -= n as u64;
    }
    hasher.update(JPEG_EOI);
    file.write_all(JPEG_EOI)
        .map_err(|err| format!("write {}: {err}", path.display()))?;
    drop(file);
    Ok(hex(&hasher.finalize()))
}

fn fill(buf: &mut [u8], index: u64) {
    let seed = index.to_le_bytes();
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = seed[i % 8]
            .wrapping_mul(31)
            .wrapping_add((i as u8).wrapping_mul(17))
            .wrapping_add(0x5A);
    }
}

fn sources_changed(snaps: &[Snap]) -> Result<u64, String> {
    let mut changed = 0u64;
    for snap in snaps {
        let meta =
            fs::metadata(&snap.path).map_err(|err| format!("re-stat {}: {err}", snap.name))?;
        let mtime = meta
            .modified()
            .map_err(|err| format!("re-stat mtime {}: {err}", snap.name))?;
        let digest = sha256_hex(&snap.path).map_err(|err| err.to_string())?;
        if meta.len() != snap.len || mtime != snap.mtime || digest != snap.sha256 {
            changed += 1;
        }
    }
    Ok(changed)
}

/// What `File::sync_all` does for the import on this OS.
///
/// The honest raw sample uses this same call. On macOS it is `F_FULLFSYNC`,
/// not `fsync`.
const IMPORT_FILE_SYNC: &str = {
    #[cfg(target_vendor = "apple")]
    {
        "F_FULLFSYNC"
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        "fsync"
    }
    #[cfg(windows)]
    {
        "FlushFileBuffers"
    }
};

#[derive(Clone, Copy)]
enum RawSync {
    /// `File::sync_all` on the file and `dest::sync_dir` on the directory.
    Import,
    /// Plain `fsync(2)` on the file and the directory. On Apple this is not
    /// what `File::sync_all` does.
    PosixFsync,
}

/// One honest raw sample and, on macOS, one plain `fsync` sample.
fn sample_raw_once(
    dir: &Path,
    files: u64,
    bytes_each: u64,
    honest: &mut Vec<f64>,
    plain: &mut Vec<f64>,
) -> Result<(), String> {
    honest.push(raw_sample(dir, files, bytes_each, RawSync::Import)?);
    if cfg!(target_vendor = "apple") {
        plain.push(raw_sample(dir, files, bytes_each, RawSync::PosixFsync)?);
    }
    Ok(())
}

/// Share of the source pages that are in the page cache, from `vmtouch`
/// in report mode. `vmtouch` with no flags only calls `mincore` on a
/// read-only mapping: it neither reads the files nor faults pages in, and
/// never writes. `None` when `vmtouch` is not installed or its output has
/// no page count, as on Windows.
fn resident_share(paths: &[PathBuf]) -> Option<f64> {
    let dir = paths.first()?.parent()?;
    let output = std::process::Command::new("vmtouch")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_vmtouch_resident(&String::from_utf8_lossy(&output.stdout))
}

/// `Resident Pages: 1220/2441  4M/9M  50%` gives 1220 / 2441.
fn parse_vmtouch_resident(text: &str) -> Option<f64> {
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Resident Pages:"))?;
    let (resident, total) = line.split_whitespace().next()?.split_once('/')?;
    let resident: f64 = resident.parse().ok()?;
    let total: f64 = total.parse().ok()?;
    (total > 0.0).then(|| resident / total)
}

fn raw_sample(dir: &Path, files: u64, bytes_each: u64, kind: RawSync) -> Result<f64, String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|err| format!("remove raw sample: {err}"))?;
    }
    let started = Instant::now();
    raw_write(dir, files, bytes_each, kind)?;
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    fs::remove_dir_all(dir).map_err(|err| format!("remove raw sample: {err}"))?;
    let mb_s = (files.saturating_mul(bytes_each) as f64) / 1_000_000.0 / seconds;
    if !mb_s.is_finite() || mb_s <= 0.0 {
        return Err("a raw sample was not a positive number".to_string());
    }
    Ok(mb_s)
}

fn raw_primitive_rates(honest: f64, plain: &[f64]) -> (Option<f64>, Option<f64>) {
    #[cfg(target_vendor = "apple")]
    {
        let mut plain = plain.to_vec();
        let plain_rate = median(&mut plain);
        (Some(plain_rate), Some(honest))
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        let _ = plain;
        (Some(honest), None)
    }
    #[cfg(windows)]
    {
        let _ = (honest, plain);
        (None, None)
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    }
}

fn phase_ms(phases: &CopyPhases) -> Value {
    json!({
        "source_read": ns_to_ms(phases.source_read_ns),
        "hash": ns_to_ms(phases.hash_ns),
        "write": ns_to_ms(phases.write_ns),
        "file_sync": ns_to_ms(phases.file_sync_ns),
        "readback_read": ns_to_ms(phases.readback_read_ns),
        "readback_hash": ns_to_ms(phases.readback_hash_ns),
        "dir_sync": ns_to_ms(phases.dir_sync_ns),
        "preflight": ns_to_ms(phases.preflight_ns),
        "metadata": ns_to_ms(phases.metadata_ns),
    })
}

fn phase_share(phases: &CopyPhases) -> Value {
    json!({
        "source_read": phases.share(phases.source_read_ns),
        "hash": phases.share(phases.hash_ns),
        "write": phases.share(phases.write_ns),
        "file_sync": phases.share(phases.file_sync_ns),
        "readback_read": phases.share(phases.readback_read_ns),
        "readback_hash": phases.share(phases.readback_hash_ns),
        "dir_sync": phases.share(phases.dir_sync_ns),
        "preflight": phases.share(phases.preflight_ns),
        "metadata": phases.share(phases.metadata_ns),
    })
}

/// Thread CPU per phase for one round, in milliseconds. The counters sum
/// every round's import threads.
fn phase_cpu_ms(report: &ThroughputReport) -> Value {
    let rounds = report.rounds.max(1) as f64;
    let mut map = serde_json::Map::new();
    for (name, ns) in report.phases.cpu_named() {
        map.insert(name.to_string(), json!(ns_to_ms(ns) / rounds));
    }
    Value::Object(map)
}

/// Thread CPU per phase for one round in milliseconds, then `untimed`: the
/// process CPU of the round that no phase counted (thread hand-offs, the
/// harness).
pub fn phase_cpu_line(report: &ThroughputReport) -> String {
    let rounds = report.rounds.max(1) as f64;
    let phases = report.phases;
    let mut parts: Vec<String> = phases
        .cpu_named()
        .iter()
        .map(|(name, ns)| format!("{name}:{:.0}", ns_to_ms(*ns) / rounds))
        .collect();
    let untimed = (report.cpu_seconds * 1000.0 - ns_to_ms(phases.cpu_total_ns()) / rounds).max(0.0);
    parts.push(format!("untimed:{untimed:.0}"));
    parts.join(",")
}

/// The same runtime check `sha2` 0.10 makes before it takes its hardware
/// path: `sha`, `sse2`, `ssse3`, and `sse4.1` on x86_64 (SHA-NI), and
/// `sha2` on aarch64. The aarch64 path exists only with the `asm` feature,
/// which `albumen-ingest` turns on for aarch64.
fn sha_hw() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sha")
            && std::arch::is_x86_feature_detected!("sse2")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("sse4.1")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Hash 256 MB already in memory on one thread. Software SHA-256 runs near
/// 200-500 MB/s; the hardware instructions run near 1,500-2,500 MB/s.
fn sha256_mb_s() -> f64 {
    let buf = vec![0x5au8; 8 * 1024 * 1024];
    let rounds = 32;
    let started = Instant::now();
    let mut hasher = Sha256::new();
    for _ in 0..rounds {
        hasher.update(&buf);
    }
    let digest = hasher.finalize();
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    std::hint::black_box(digest);
    (buf.len() * rounds) as f64 / 1_000_000.0 / seconds
}

/// Markdown scorecard for the CI step summary: CPU seconds per GB against
/// its team gate and scored team target, with `cpu_percent` as context.
pub fn cpu_scorecard(report: &ThroughputReport) -> String {
    let mark = |met: bool| if met { "met" } else { "not met" };
    format!(
        "| import CPU ({leg}) | value | team target | |\n|---|---|---|---|\n\
         | CPU seconds per GB, normalized to {sha_ref:.0} MB/s SHA-256 | {norm:.2} | ≤ {gb_target:.1} scored, ≤ {gb_gate:.1} gate (fails the job) | scored {gb_met}, gate {gate_met} |\n\
         | CPU seconds per GB, raw | {gb:.2} | | |\n\
         | cpu_percent (CPU ÷ (wall × {cores} cores)), context | {pct:.1}% | | |\n\
         | CPU seconds per 1,000 photos | {per_1000:.2} | | |\n\
         | hardware SHA-256 | {sha_hw} ({sha_mb_s:.0} MB/s) | | |\n",
        leg = report.leg,
        gb = report.cpu_seconds_per_gb,
        gb_target = CPU_SECONDS_PER_GB_TARGET,
        norm = report.cpu_seconds_per_gb_normalized,
        sha_ref = SHA256_REFERENCE_MB_S,
        gb_met = mark(report.cpu_seconds_per_gb_normalized <= CPU_SECONDS_PER_GB_TARGET),
        gb_gate = CPU_SECONDS_PER_GB_GATE,
        gate_met = mark(cpu_gate_passes(report.cpu_seconds_per_gb_normalized)),
        cores = report.cores,
        pct = report.cpu_percent,
        per_1000 = report.cpu_seconds_per_1000,
        sha_hw = report.sha_hw,
        sha_mb_s = report.sha256_mb_s,
    )
}

fn ns_to_ms(ns: u64) -> f64 {
    (ns as f64) / 1_000_000.0
}

pub fn optional_rate(rate: Option<f64>) -> String {
    match rate {
        Some(rate) => format!("{rate:.3}"),
        None => "none".to_string(),
    }
}

pub fn phase_share_line(phases: &CopyPhases) -> String {
    format!(
        "source_read:{:.3},hash:{:.3},write:{:.3},file_sync:{:.3},readback_read:{:.3},readback_hash:{:.3},dir_sync:{:.3},preflight:{:.3},metadata:{:.3}",
        phases.share(phases.source_read_ns),
        phases.share(phases.hash_ns),
        phases.share(phases.write_ns),
        phases.share(phases.file_sync_ns),
        phases.share(phases.readback_read_ns),
        phases.share(phases.readback_hash_ns),
        phases.share(phases.dir_sync_ns),
        phases.share(phases.preflight_ns),
        phases.share(phases.metadata_ns)
    )
}

/// Durable write of `files` new files, `bytes_each` bytes each.
///
/// `Import` uses `sync_all` then `sync_dir`, the durability `copy_verify`
/// uses for the bytes. This writer does not hash those bytes and does not
/// set a preserved mtime, so the ratio is the cost of verification on top
/// of the same fsyncs. `PosixFsync` is the comparison sample on macOS.
fn raw_write(dir: &Path, files: u64, bytes_each: u64, kind: RawSync) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("create raw dest: {err}"))?;
    let mut buf = vec![0u8; 1024 * 1024];
    fill(&mut buf, 0);
    for index in 0..files {
        let path = dir.join(format!("raw-{index:04}.bin"));
        let mut file = open_new(&path).map_err(|err| err.to_string())?;
        let mut left = bytes_each;
        while left > 0 {
            let n = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            file.write_all(&buf[..n])
                .map_err(|err| format!("raw write {}: {err}", path.display()))?;
            left -= n as u64;
        }
        sync_raw_file(&file, &path, kind)?;
        drop(file);
        sync_raw_dir(dir, kind)?;
    }
    Ok(())
}

/// One import-shaped raw sample on `workers` threads. Each file gets the
/// import's durability calls, `File::sync_all` then `dest::sync_dir`, and
/// nothing else.
fn ceiling_sample(dir: &Path, files: u64, bytes_each: u64, workers: usize) -> Result<f64, String> {
    ceiling_sample_from(dir, files, bytes_each, workers, None)
}

/// [`ceiling_sample`], optionally reading each file's bytes from
/// `sources` first. The sources are only opened for reading.
fn ceiling_sample_from(
    dir: &Path,
    files: u64,
    bytes_each: u64,
    workers: usize,
    sources: Option<&[PathBuf]>,
) -> Result<f64, String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|err| format!("remove ceiling sample: {err}"))?;
    }
    fs::create_dir_all(dir).map_err(|err| format!("create ceiling dest: {err}"))?;
    let next = AtomicU64::new(0);
    let error: Mutex<Option<String>> = Mutex::new(None);
    let started = Instant::now();
    thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                let mut buf = vec![0u8; 1024 * 1024];
                fill(&mut buf, 0);
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= files {
                        break;
                    }
                    let path = dir.join(format!("ceiling-{index:04}.bin"));
                    let result = (|| -> Result<(), String> {
                        let mut file = open_new(&path).map_err(|err| err.to_string())?;
                        if let Some(sources) = sources {
                            let source = &sources[usize::try_from(index).unwrap_or(0)];
                            let mut input = File::open(source)
                                .map_err(|err| format!("read {}: {err}", source.display()))?;
                            loop {
                                let n = input
                                    .read(&mut buf)
                                    .map_err(|err| format!("read {}: {err}", source.display()))?;
                                if n == 0 {
                                    break;
                                }
                                file.write_all(&buf[..n]).map_err(|err| {
                                    format!("ceiling write {}: {err}", path.display())
                                })?;
                            }
                            sync_raw_file(&file, &path, RawSync::Import)?;
                            drop(file);
                            return sync_raw_dir(dir, RawSync::Import);
                        }
                        let mut left = bytes_each;
                        while left > 0 {
                            let n =
                                usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
                            file.write_all(&buf[..n]).map_err(|err| {
                                format!("ceiling write {}: {err}", path.display())
                            })?;
                            left -= n as u64;
                        }
                        sync_raw_file(&file, &path, RawSync::Import)?;
                        drop(file);
                        sync_raw_dir(dir, RawSync::Import)
                    })();
                    if let Err(err) = result {
                        record_error(&error, err);
                        break;
                    }
                }
            });
        }
    });
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    if let Some(err) = error
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
    {
        return Err(err);
    }
    fs::remove_dir_all(dir).map_err(|err| format!("remove ceiling sample: {err}"))?;
    let mb_s = (files.saturating_mul(bytes_each) as f64) / 1_000_000.0 / seconds;
    if !mb_s.is_finite() || mb_s <= 0.0 {
        return Err("a ceiling sample was not a positive number".to_string());
    }
    Ok(mb_s)
}

fn sync_raw_file(file: &File, path: &Path, kind: RawSync) -> Result<(), String> {
    match kind {
        RawSync::Import => file
            .sync_all()
            .map_err(|err| format!("sync raw {}: {err}", path.display())),
        RawSync::PosixFsync => posix_fsync_file(file, path),
    }
}

fn sync_raw_dir(dir: &Path, kind: RawSync) -> Result<(), String> {
    match kind {
        RawSync::Import => dest::sync_dir(dir).map_err(|err| err.to_string()),
        RawSync::PosixFsync => posix_fsync_dir(dir),
    }
}

#[cfg(target_vendor = "apple")]
fn posix_fsync_file(file: &File, path: &Path) -> Result<(), String> {
    rustix::fs::fsync(file).map_err(|err| format!("fsync raw {}: {err}", path.display()))
}

#[cfg(not(target_vendor = "apple"))]
fn posix_fsync_file(file: &File, path: &Path) -> Result<(), String> {
    file.sync_all()
        .map_err(|err| format!("sync raw {}: {err}", path.display()))
}

#[cfg(target_vendor = "apple")]
fn posix_fsync_dir(dir: &Path) -> Result<(), String> {
    let file = File::open(dir).map_err(|err| format!("open raw dir {}: {err}", dir.display()))?;
    rustix::fs::fsync(&file).map_err(|err| format!("fsync raw dir {}: {err}", dir.display()))
}

#[cfg(not(target_vendor = "apple"))]
fn posix_fsync_dir(dir: &Path) -> Result<(), String> {
    dest::sync_dir(dir).map_err(|err| err.to_string())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// `set_readonly(true)` lets the re-read prove the import did not need write
/// access. Clearing it afterwards lets the temp directory drop on Windows,
/// where a read-only file is not removed.
struct UnlockOnDrop {
    saved: Vec<(PathBuf, fs::Permissions)>,
}

impl Drop for UnlockOnDrop {
    fn drop(&mut self) {
        for (path, perms) in &self.saved {
            let _ = fs::set_permissions(path, perms.clone());
        }
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| err.to_string())?
            .as_nanos();
        let parent = std::env::var_os("ALBUMEN_THROUGHPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = parent.join(format!("albumen-throughput-{nanos}-{}", std::process::id()));
        fs::create_dir_all(&path).map_err(|err| format!("create temp dir: {err}"))?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing_rescales_only_the_hash_phases() {
        // 2026-10-19, #119 ubuntu-24.04: raw 3.188, hash phases 10.574 s
        // per 5 GB round, SHA-256 1,337 MB/s.
        let hash = 10.574 / 5.0;
        let norm = normalized_cpu_seconds_per_gb(3.188, hash, 1337.0);
        assert!((norm - (3.188 - hash + hash * 1337.0 / 1800.0)).abs() < 1e-12);
        assert!(norm < 3.0, "{norm}");
        // At the reference rate nothing moves.
        assert_eq!(normalized_cpu_seconds_per_gb(2.5, 1.0, 1800.0), 2.5);
        // A faster runner is charged up to the reference.
        assert!(normalized_cpu_seconds_per_gb(2.5, 1.0, 2400.0) > 2.5);
        // Without a rate the raw number stands.
        assert_eq!(normalized_cpu_seconds_per_gb(2.5, 1.0, 0.0), 2.5);
    }

    #[test]
    fn the_cpu_gate_passes_the_measured_legs_and_fails_above_three() {
        // #112 on 2026-10-19: ubuntu-24.04, ubuntu-26.04, windows-2025, macos-14.
        for measured in [1.863, 2.790, 2.932, 1.935] {
            assert!(cpu_gate_passes(measured), "{measured}");
        }
        assert!(cpu_gate_passes(3.0));
        assert!(cpu_gate_passes(3.0004));
        assert!(!cpu_gate_passes(3.001));
        assert!(!cpu_gate_passes(f64::NAN));
    }

    #[test]
    fn vmtouch_resident_pages_parse() {
        let text = "           Files: 1000\n     Directories: 1\n  Resident Pages: 1220/2441  4M/9M  50%\n         Elapsed: 0.01 seconds\n";
        assert_eq!(parse_vmtouch_resident(text), Some(1220.0 / 2441.0));
        assert_eq!(parse_vmtouch_resident("Resident Pages: 0/0  0/0"), None);
        assert_eq!(parse_vmtouch_resident("nothing"), None);
    }

    #[test]
    fn ratio_gate_includes_exactly_eighty_percent() {
        assert!(ratio_passes(0.80));
        assert!(ratio_passes(0.7995));
        assert!(!ratio_passes(0.7994));
        assert!(!ratio_passes(f64::NAN));
    }

    #[test]
    fn defaults_are_a_thousand_files_of_about_five_megabytes() {
        const _: () = {
            assert!(DEFAULT_THROUGHPUT_FILES >= 1_000);
            assert!(DEFAULT_THROUGHPUT_BYTES >= 4_000_000);
            assert!(DEFAULT_THROUGHPUT_BYTES <= 6_000_000);
        };
        assert_eq!(THROUGHPUT_RATIO_FLOOR, 0.80);
    }

    #[test]
    fn synthetic_jpeg_has_the_header_the_length_and_a_stable_digest() {
        let root = TempDir::new().unwrap();
        let path = root.path.join("img-0001.jpg");
        let first = write_jpeg(&path, 1, 4096).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 4096);
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
        let again = write_jpeg(&root.path.join("img-0001-again.jpg"), 1, 4096).unwrap();
        assert_eq!(first, again);
        let other = write_jpeg(&root.path.join("img-0002.jpg"), 2, 4096).unwrap();
        assert_ne!(first, other);
    }

    #[test]
    fn a_ratio_under_eighty_percent_is_not_a_reconciliation_failure() {
        let report = ThroughputReport {
            leg: "test".to_string(),
            files: 1_000,
            bytes_each: 5_000_000,
            bytes: 5_000_000_000,
            import_seconds: 18.0,
            import_mb_s: 280.0,
            import_items_s: 56.0,
            raw_seconds: 8.0,
            raw_mb_s: 625.0,
            raw_sync: "fsync".to_string(),
            raw_fsync_mb_s: Some(625.0),
            raw_fullfsync_mb_s: None,
            raw_samples: vec![600.0, 625.0, 650.0],
            phases: CopyPhases::default(),
            ratio: 0.448,
            ceiling_mb_s: 600.0,
            ceiling_samples: vec![600.0],
            ceiling_ratio: 0.96,
            copy_ceiling_mb_s: 500.0,
            copy_ceiling_samples: vec![500.0],
            copy_ceiling_ratio: 0.8,
            count_in: 1_000,
            count_out: 1_000,
            mismatched: 0,
            source_changed: 0,
            workers: 1,
            cpu_seconds: 4.0,
            cores: 4,
            cpu_percent: 5.6,
            cpu_seconds_per_1000: 4.0,
            cpu_seconds_per_gb: 0.8,
            cpu_seconds_per_gb_normalized: 0.8,
            sha_hw: true,
            sha256_mb_s: 2000.0,
            verifiers: 2,
            rounds: 6,
            import_samples: vec![480.0],
            resident_before_raw: vec![Some(1.0)],
            resident_before_import: vec![Some(0.5)],
            resident_before_copy_ceiling: vec![None],
        };
        assert!(report.reconciled());
        assert!(!ratio_passes(report.ratio));
        assert!(throughput_problems(&report).is_empty());
        let mut missed = report.clone();
        missed.source_changed = 1;
        assert!(!missed.reconciled());
        assert!(!throughput_problems(&missed).is_empty());
    }

    #[test]
    fn two_small_files_reconcile_and_leave_the_source_unchanged() {
        let report = measure_throughput("test", 2, 8192).unwrap();
        assert!(report.cpu_seconds >= 0.0 && report.cpu_percent.is_finite());
        assert!(report.cores >= 1);
        assert!(report.reconciled(), "{report:?}");
        assert_eq!(report.reconciled_percent(), 100.0);
        assert!(report.ratio.is_finite() && report.ratio > 0.0);
        assert_eq!(report.bytes, 2 * 8192);
        assert!(report.workers >= 1);
        assert!(report.workers <= 2);
        assert_eq!(report.raw_sync, IMPORT_FILE_SYNC);
        assert_eq!(report.raw_samples.len(), RAW_SAMPLES_PER_SIDE * 2);
        assert_eq!(report.import_samples.len(), RAW_SAMPLES_PER_SIDE * 2);
        assert_eq!(
            report.resident_before_import.len(),
            RAW_SAMPLES_PER_SIDE * 2
        );
        assert_eq!(
            report.resident_before_copy_ceiling.len(),
            RAW_SAMPLES_PER_SIDE
        );
        for share in report.resident_before_import.iter().flatten() {
            assert!((0.0..=1.0).contains(share));
        }
        assert!(report.phases.total_ns() > 0, "{:?}", report.phases);
        #[cfg(target_vendor = "apple")]
        {
            assert_eq!(report.raw_sync, "F_FULLFSYNC");
            assert_eq!(report.raw_fullfsync_mb_s, Some(report.raw_mb_s));
            assert!(report.raw_fsync_mb_s.unwrap() > 0.0);
        }
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            assert_eq!(report.raw_sync, "fsync");
            assert_eq!(report.raw_fsync_mb_s, Some(report.raw_mb_s));
            assert_eq!(report.raw_fullfsync_mb_s, None);
        }
    }

    #[test]
    fn raw_median_uses_the_middle_of_an_even_sample() {
        assert_eq!(median(&mut [1.0, 4.0, 2.0, 9.0]), 3.0);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    }

    #[test]
    fn worker_pool_follows_the_cpu_count_and_the_cap() {
        assert_eq!(import_workers(1), 1);
        let pool = import_workers(10_000);
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(pool, cpus.min(MAX_IMPORT_WORKERS));
        assert!(pool <= MAX_IMPORT_WORKERS);
        assert!(pool >= 1);
    }
}
