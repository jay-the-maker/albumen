// SPDX-License-Identifier: Apache-2.0

//! M6: one saved path index shared by more than one writer.
//!
//! The GUI opens `LibraryIndex` afresh for each listing; the search tool
//! keeps one open and refreshes it per query. Both save the same
//! `<sha256(root)>.idx`. These tests run both patterns at once on synthetic
//! trees, kill writers mid-save, damage the file, and check that every saved
//! file parses, no save goes backwards, and nothing is left behind.
//!
//! Sizes and counts come from the environment so the same tests give the
//! acceptance runs: `ALBUMEN_M6_FILES` (default 2000), `ALBUMEN_M6_ITERS`
//! (default 200), `ALBUMEN_M6_KILLS` (default 40), `ALBUMEN_M6_SEED`,
//! `ALBUMEN_M6_READ_BARS=1` (fail X2 unless read p95 <= 20 ms and max <= 100 ms).
//! Every randomized test prints its seed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use albumen_ingest::index::{
    check_saved_file, newest_slot, read_saved_file, slot_files, LibraryIndex, CRASH_ENV,
};

const CHILD_ENV: &str = "ALBUMEN_M6_CHILD";

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn seed() -> u64 {
    env_num(
        "ALBUMEN_M6_SEED",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
            | 1,
    )
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A synthetic library: `files` empty .jpg files over 50 directories.
fn make_library(root: &Path, files: u64) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    for i in 0..files {
        let dir = format!("d{:02}", i % 50);
        std::fs::create_dir_all(root.join(&dir)).unwrap();
        let rel = format!("{dir}/img{i:07}.jpg");
        std::fs::write(root.join(&rel), b"").unwrap();
        all.insert(rel);
    }
    all
}

fn index_file(index: &LibraryIndex) -> PathBuf {
    index.file().unwrap().to_path_buf()
}

fn partials(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial-"))
        .collect()
}

fn sorted(paths: Vec<String>) -> BTreeSet<String> {
    paths.into_iter().collect()
}

/// Add or remove one file in a random directory.
fn mutate(root: &Path, all: &mut BTreeSet<String>, rng: &mut Rng, serial: &mut u64) {
    if rng.below(2) == 0 && !all.is_empty() {
        let pick = rng.below(all.len() as u64) as usize;
        let rel = all.iter().nth(pick).unwrap().clone();
        std::fs::remove_file(root.join(&rel)).unwrap();
        all.remove(&rel);
    } else {
        *serial += 1;
        let rel = format!("d{:02}/new{:07}.jpg", rng.below(50), serial);
        std::fs::write(root.join(&rel), b"").unwrap();
        all.insert(rel);
    }
}

/// X2 and the concurrent-writer test: a GUI-style writer (fresh open per
/// listing) and a search-tool-style writer (one long-lived index, refreshed)
/// save the same file after every change, while a reader parses it in a
/// loop. Every saved file must parse, and after both writers have seen a
/// change the file must hold it: a stale writer never takes it back.
#[test]
fn gui_and_search_tool_share_one_index() {
    let seed = seed();
    let files = env_num("ALBUMEN_M6_FILES", 2000);
    let iters = env_num("ALBUMEN_M6_ITERS", 200);
    println!("M6 X2 seed={seed} files={files} iters={iters}");
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let mut all = make_library(library.path(), files);
    let mut rng = Rng(seed);
    let mut serial = 0;

    let began = Instant::now();
    let mut search = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    println!(
        "M6 X8 first save files={files} ms={:.1}",
        began.elapsed().as_secs_f64() * 1000.0
    );
    let file = index_file(&search);
    let bytes = std::fs::metadata(newest_slot(&file).unwrap())
        .unwrap()
        .len();
    println!(
        "M6 X8 index bytes={bytes} per_file={:.1}",
        bytes as f64 / files as f64
    );

    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let failures = Arc::new(AtomicU64::new(0));
    let max_read_us = Arc::new(AtomicU64::new(0));
    // Reads over 100 ms. Readers take no lock, so any slow read is the
    // reader's own parse and hash under CPU contention, not a writer.
    let slow_reads = Arc::new(AtomicU64::new(0));
    let first_failure = Arc::new(std::sync::Mutex::new(None::<String>));
    let reader = {
        let first_failure = first_failure.clone();
        let slow_reads = slow_reads.clone();
        let (stop, reads, failures, max_read_us, file) = (
            stop.clone(),
            reads.clone(),
            failures.clone(),
            max_read_us.clone(),
            file.clone(),
        );
        std::thread::spawn(move || {
            let mut times_us = Vec::new();
            // (start offset ms from the reader's start, UTC epoch ms, duration us)
            let mut slow_at = Vec::new();
            // On-CPU ns of the reader thread per check, from its schedstat.
            let mut cpu_ns_per_read: Vec<u64> = Vec::new();
            let start = Instant::now();
            let mut probe = ReadProbe::open();
            while !stop.load(Ordering::Relaxed) {
                let before = probe.sample();
                let t = Instant::now();
                let wall = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis());
                let checked = check_saved_file(&file);
                let us = t.elapsed().as_micros() as u64;
                let after = probe.sample();
                if let (Some(b), Some(a)) = (before, after) {
                    let cpu_ns = a.on_cpu_ns.saturating_sub(b.on_cpu_ns);
                    cpu_ns_per_read.push(cpu_ns);
                    if us > 20_000 {
                        println!(
                            "M6 X2 READ_OVER_20MS seed={seed} utc={} utc_epoch_ms={wall} read_ms={:.2} \
                             on_cpu_ms={:.2} runq_wait_ms={:.2} steal_ticks={} steal_ms_approx={}",
                            iso_utc_ms(wall),
                            us as f64 / 1000.0,
                            cpu_ns as f64 / 1e6,
                            a.runq_ns.saturating_sub(b.runq_ns) as f64 / 1e6,
                            a.steal_ticks.saturating_sub(b.steal_ticks),
                            a.steal_ticks.saturating_sub(b.steal_ticks) * 10,
                        );
                    }
                }
                times_us.push(us);
                max_read_us.fetch_max(us, Ordering::Relaxed);
                if us > 100_000 {
                    slow_reads.fetch_add(1, Ordering::Relaxed);
                    slow_at.push(((t - start).as_millis(), wall, us));
                }
                reads.fetch_add(1, Ordering::Relaxed);
                if let Err(err) = checked {
                    failures.fetch_add(1, Ordering::Relaxed);
                    first_failure.lock().unwrap().get_or_insert(err);
                }
            }
            cpu_ns_per_read.sort_unstable();
            if !cpu_ns_per_read.is_empty() {
                let n = cpu_ns_per_read.len();
                let mean = cpu_ns_per_read.iter().sum::<u64>() as f64 / n as f64 / 1e6;
                let p95 = cpu_ns_per_read[(n * 95 / 100).min(n - 1)] as f64 / 1e6;
                let max = cpu_ns_per_read[n - 1] as f64 / 1e6;
                println!(
                    "M6 X2 READ_CPU seed={seed} reads={n} on_cpu_mean_ms={mean:.3} on_cpu_p95_ms={p95:.3} on_cpu_max_ms={max:.2} probe_cost_us_per_read={:.1}",
                    probe.cost_us()
                );
            }
            (times_us, slow_at)
        })
    };

    let mut regressions = 0;
    let mut last_generation = 0;
    let mut merges = 0;
    for _ in 0..iters {
        for _ in 0..=rng.below(3) {
            mutate(library.path(), &mut all, &mut rng, &mut serial);
        }
        let gui_first = rng.below(2) == 0;
        let lib = library.path().to_path_buf();
        let idx = index_dir.path().to_path_buf();
        let gui = std::thread::spawn(move || {
            if !gui_first {
                std::thread::yield_now();
            }
            let index = LibraryIndex::open_with_progress(&lib, Some(&idx), &mut |_| {}).unwrap();
            (
                sorted(index.paths().to_vec()),
                index.save_error.clone(),
                index.merges,
            )
        });
        search.refresh().unwrap();
        assert_eq!(search.save_error, None);
        let (gui_paths, gui_error, gui_merges) = gui.join().unwrap();
        merges += gui_merges;
        assert_eq!(gui_error, None);
        assert_eq!(gui_paths, all, "GUI listing differs from the tree");
        assert_eq!(
            sorted(search.paths().to_vec()),
            all,
            "search listing differs"
        );
        let (generation, saved) = read_saved_file(&file).unwrap();
        if sorted(saved) != all {
            regressions += 1;
        }
        assert!(generation >= last_generation, "generation went backwards");
        last_generation = generation;
    }
    merges += search.merges;
    stop.store(true, Ordering::Relaxed);
    let (mut times_us, slow_at) = reader.join().unwrap();
    times_us.sort_unstable();
    let pct = |q: f64| {
        times_us
            .get(((times_us.len() as f64 * q) as usize).min(times_us.len().saturating_sub(1)))
            .map_or(0.0, |&us| us as f64 / 1000.0)
    };
    let (p50, p95, p99) = (pct(0.50), pct(0.95), pct(0.99));
    // Read-duration histogram, upper bounds in ms (last bucket is open).
    let bounds = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0];
    let mut buckets = vec![0u64; bounds.len() + 1];
    for &us in &times_us {
        let ms = us as f64 / 1000.0;
        buckets[bounds.iter().position(|&b| ms <= b).unwrap_or(bounds.len())] += 1;
    }
    let histogram: Vec<String> = bounds
        .iter()
        .map(|b| format!("le{b}"))
        .chain(std::iter::once("gt200".to_string()))
        .zip(&buckets)
        .map(|(k, n)| format!("{k}:{n}"))
        .collect();
    println!("M6 X2 HISTOGRAM seed={seed} {}", histogram.join(" "));
    for (offset_ms, wall_ms, us) in &slow_at {
        println!(
            "M6 X2 SLOW_READ seed={seed} at_ms={offset_ms} utc_epoch_ms={wall_ms} read_ms={:.2}",
            *us as f64 / 1000.0
        );
    }
    let fresh = LibraryIndex::open(library.path(), None).unwrap();
    let (_, saved) = read_saved_file(&file).unwrap();
    let byte_identical = saved == fresh.paths();
    let left = partials(index_dir.path());
    let max_ms = max_read_us.load(Ordering::Relaxed) as f64 / 1000.0;
    let slow = slow_reads.load(Ordering::Relaxed);
    // M6 read bars: p95 <= 20 ms and max <= 100 ms. Always printed; enforced
    // when ALBUMEN_M6_READ_BARS=1 (evidence runs), since shared CI runners
    // are not a latency benchmark.
    let bars_met = p95 <= 20.0 && max_ms <= 100.0;
    println!(
        "M6 X2 RESULT seed={seed} iters={iters} files={files} reads={} parse_failures={} \
         read_p50_ms={p50:.2} read_p95_ms={p95:.2} read_p99_ms={p99:.2} max_read_ms={max_ms:.2} \
         slow_reads_over_100ms={slow} read_bars_met={bars_met} \
         regressions={regressions} merges={merges} final_generation={last_generation} \
         equals_fresh_walk={byte_identical} stale_partials={} first_failure={:?}",
        reads.load(Ordering::Relaxed),
        failures.load(Ordering::Relaxed),
        left.len(),
        first_failure.lock().unwrap()
    );
    assert_eq!(failures.load(Ordering::Relaxed), 0);
    assert_eq!(regressions, 0);
    assert!(byte_identical);
    assert!(left.is_empty(), "{left:?}");
    if std::env::var("ALBUMEN_M6_READ_BARS").as_deref() == Ok("1") {
        assert!(
            bars_met,
            "read bars missed: p95={p95:.2} ms max={max_ms:.2} ms"
        );
    }
}

/// D1: a writer that loaded the index before another saved merges that save
/// instead of replacing it.
#[test]
fn stale_writer_merges_instead_of_regressing() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let mut all = make_library(library.path(), 200);
    let mut stale = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let first = stale.generation().unwrap();
    std::fs::write(library.path().join("d01/only-b.jpg"), b"").unwrap();
    all.insert("d01/only-b.jpg".into());
    let b = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    assert!(b.generation().unwrap() > first);
    // The stale writer sees a different change and saves.
    std::fs::write(library.path().join("d02/only-a.jpg"), b"").unwrap();
    all.insert("d02/only-a.jpg".into());
    stale.refresh().unwrap();
    assert!(stale.merges >= 1, "the stale writer did not merge");
    let (_, saved) = read_saved_file(&index_file(&stale)).unwrap();
    assert_eq!(sorted(saved), all);
}

/// D3: a short or damaged file is refused and rebuilt, never read as a
/// smaller library.
#[test]
fn footer_refuses_short_and_damaged_files() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let all = make_library(library.path(), 300);
    let index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let file = newest_slot(&index_file(&index)).unwrap();
    let good = std::fs::read(&file).unwrap();
    let mut refused = 0;
    let step = (good.len() / 97).max(1);
    let mut cuts: Vec<usize> = (0..good.len()).step_by(step).collect();
    cuts.push(good.len() - 1);
    for cut in cuts {
        // Damage every slot: one good slot would (rightly) still be read.
        for slot in slot_files(&index_file(&index)) {
            std::fs::write(&slot, &good[..cut]).unwrap();
        }
        assert!(check_saved_file(&file).is_err(), "cut at {cut} accepted");
        assert!(check_saved_file(&index_file(&index)).is_err());
        let reopened = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        assert!(!reopened.loaded, "cut at {cut} was loaded");
        assert_eq!(sorted(reopened.paths().to_vec()), all);
        refused += 1;
    }
    let mut flipped = good.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0x01;
    std::fs::write(&file, &flipped).unwrap();
    assert!(check_saved_file(&file).is_err(), "flipped byte accepted");
    println!(
        "M6 D3 refused={} of {} damaged files",
        refused + 1,
        refused + 1
    );
}

/// The first format (no generation, no footer) is still read.
#[test]
fn first_format_still_loads() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let all = make_library(library.path(), 120);
    let index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let file = index_file(&index);
    let text = std::fs::read_to_string(newest_slot(&file).unwrap()).unwrap();
    let v1: String = text
        .lines()
        .filter(|l| !l.starts_with("gen\t") && !l.starts_with("end\t"))
        .map(|l| {
            if l == "albumen-index/2" {
                "albumen-index/1\n".to_string()
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    // An earlier version saved only `<key>.idx`; put that file alone in a
    // fresh index directory.
    let old_dir = tempfile::tempdir().unwrap();
    std::fs::write(old_dir.path().join(file.file_name().unwrap()), v1).unwrap();
    let reopened = LibraryIndex::open(library.path(), Some(old_dir.path())).unwrap();
    assert!(reopened.loaded);
    assert_eq!(sorted(reopened.paths().to_vec()), all);
}

/// The child side of the kill and crash tests: when `ALBUMEN_M6_CHILD`
/// names a library and index dir, change the tree and save until killed.
/// Otherwise it returns at once.
#[test]
fn child_writer() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (lib, idx) = spec.split_once('|').unwrap();
    let (lib, idx) = (Path::new(lib), Path::new(idx));
    let mut index = LibraryIndex::open(lib, Some(idx)).unwrap();
    let mut n = 0u64;
    loop {
        n += 1;
        let rel = lib.join(format!("d{:02}/child{n}.jpg", n % 50));
        std::fs::write(&rel, b"").unwrap();
        index.refresh().unwrap();
    }
}

fn spawn_child(lib: &Path, idx: &Path, crash: Option<&str>) -> std::process::Child {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_writer", "--test-threads=1", "--quiet"])
        .env(CHILD_ENV, format!("{}|{}", lib.display(), idx.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match crash {
        Some(at) => cmd.env(CRASH_ENV, at),
        None => cmd.env_remove(CRASH_ENV),
    };
    cmd.spawn().unwrap()
}

/// X3: kill a writer at random points mid-save. The file is always the old
/// index or a new one, never torn, and a later save leaves no temp file.
#[test]
fn kill_mid_write_leaves_old_or_new() {
    let seed = seed();
    let kills = env_num("ALBUMEN_M6_KILLS", 40);
    let files = env_num("ALBUMEN_M6_FILES", 2000);
    println!("M6 X3 seed={seed} kills={kills} files={files}");
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    make_library(library.path(), files);
    let first = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let file = index_file(&first);
    drop(first);
    let mut rng = Rng(seed);
    let mut torn = 0;
    let mut stale_seen = 0;
    let mut last_generation = check_saved_file(&file).unwrap();
    let mut grew = 0;
    for _ in 0..kills {
        let mut child = spawn_child(library.path(), index_dir.path(), None);
        std::thread::sleep(Duration::from_millis(30 + rng.below(220)));
        child.kill().unwrap();
        child.wait().unwrap();
        match check_saved_file(&file) {
            Ok(generation) => {
                assert!(generation >= last_generation);
                if generation > last_generation {
                    grew += 1;
                }
                last_generation = generation;
            }
            Err(_) => torn += 1,
        }
        stale_seen += partials(index_dir.path()).len();
        let reopened = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        assert!(reopened.loaded, "a killed save forced a full walk");
    }
    let left = partials(index_dir.path());
    println!(
        "M6 X3 RESULT seed={seed} kills={kills} torn={torn} kills_after_a_new_save={grew} \
         partials_seen_after_kill={stale_seen} stale_partials_after_next_save={}",
        left.len()
    );
    assert_eq!(torn, 0);
    assert!(left.is_empty(), "{left:?}");
}

/// X3 crash hooks: abort before the rename (half a temp file on disk) and
/// right after it. Old or new index only; the next save clears the temp.
#[test]
fn crash_hooks_before_and_after_rename() {
    for at in ["before-rename", "after-rename"] {
        let library = tempfile::tempdir().unwrap();
        let index_dir = tempfile::tempdir().unwrap();
        let all = make_library(library.path(), 300);
        let first = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        let file = index_file(&first);
        let old = check_saved_file(&file).unwrap();
        drop(first);
        let status = spawn_child(library.path(), index_dir.path(), Some(at))
            .wait()
            .unwrap();
        assert!(!status.success(), "{at}: the child did not abort");
        let generation = check_saved_file(&file).unwrap();
        let left_by_crash = partials(index_dir.path()).len();
        match at {
            "before-rename" => {
                assert_eq!(generation, old);
                assert_eq!(left_by_crash, 1);
            }
            _ => assert_eq!(generation, old + 1),
        }
        let reopened = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        assert!(reopened.loaded);
        // The child added one file before aborting; the reopen lists it and saves.
        assert!(sorted(reopened.paths().to_vec()).is_superset(&all));
        let left = partials(index_dir.path());
        println!(
            "M6 X3 hook={at} old_gen={old} gen_after_crash={generation} \
             partials_after_crash={left_by_crash} partials_after_next_save={}",
            left.len()
        );
        assert!(left.is_empty(), "{at}: {left:?}");
    }
}

/// X7: another program holding the index without delete sharing (an
/// antivirus scan, a backup tool) delays the rename; the save is retried and
/// never lost.
#[cfg(windows)]
#[test]
fn held_handle_delays_but_never_loses_a_save() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    for hold_ms in [1000u64, 5000] {
        let library = tempfile::tempdir().unwrap();
        let index_dir = tempfile::tempdir().unwrap();
        let mut all = make_library(library.path(), 300);
        let mut index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        let file = index_file(&index);
        // A second save so both slots exist; then hold both.
        std::fs::write(library.path().join("d03/second.jpg"), b"").unwrap();
        all.insert("d03/second.jpg".into());
        std::thread::sleep(Duration::from_millis(2100));
        index.refresh().unwrap();
        let holder = {
            let held: Vec<_> = slot_files(&file)
                .iter()
                .map(|slot| {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .share_mode(FILE_SHARE_READ)
                        .open(slot)
                        .unwrap()
                })
                .collect();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(hold_ms));
                drop(held);
            })
        };
        std::fs::write(library.path().join("d03/held.jpg"), b"").unwrap();
        all.insert("d03/held.jpg".into());
        let t = Instant::now();
        index.refresh().unwrap();
        let first_ms = t.elapsed().as_millis();
        let first_error = index.save_error.clone();
        holder.join().unwrap();
        // Nothing changed since; a pending save still goes out.
        index.refresh().unwrap();
        let (_, saved) = read_saved_file(&file).unwrap();
        println!(
            "M6 X7 hold_ms={hold_ms} first_save_ms={first_ms} first_error={} retries={} \
             saved_after_release={}",
            first_error.is_some(),
            index.rename_retries,
            sorted(saved.clone()) == all
        );
        assert!(index.rename_retries > 0);
        assert_eq!(index.save_error, None);
        assert_eq!(sorted(saved), all, "a held handle lost a save");
        if hold_ms <= 1000 {
            assert_eq!(first_error, None, "a 1 s hold was not ridden out");
        }
    }
}

/// A save that fails (here: another writer keeps the writers' lock past
/// every retry) is reported in `save_error`, the index still answers from
/// memory, and the next refresh saves it even though nothing changed since.
#[test]
fn pending_save_lands_on_next_refresh() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let mut all = make_library(library.path(), 300);
    let mut index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let file = index_file(&index);
    let mut lock_name = file.as_os_str().to_owned();
    lock_name.push(".lock");
    let other = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(lock_name))
        .unwrap();
    other.lock().unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    std::fs::write(library.path().join("d03/pending.jpg"), b"").unwrap();
    all.insert("d03/pending.jpg".into());
    index.refresh().unwrap();
    assert!(
        index.save_error.is_some(),
        "a kept lock did not fail the save"
    );
    assert!(index.paths().iter().any(|p| p == "d03/pending.jpg"));
    let (_, saved) = read_saved_file(&file).unwrap();
    assert!(!saved.iter().any(|p| p == "d03/pending.jpg"));
    other.unlock().unwrap();
    // Nothing changed since: the pending save still goes out.
    index.refresh().unwrap();
    assert_eq!(index.save_error, None);
    let (_, saved) = read_saved_file(&file).unwrap();
    assert_eq!(sorted(saved), all, "a pending save was lost");
}

/// D1 condition (b): a writers' lock held past every retry (another albumen
/// stuck mid-save, or any program holding `<key>.idx.lock`) never loses a
/// save. The refresh still returns the new listing, the save stays pending
/// (`save_error` set), and the next refresh after the lock is released
/// writes it, even though nothing in the library changed in between.
#[test]
fn a_stuck_lock_leaves_the_save_pending_and_it_is_retried() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let mut all = make_library(library.path(), 300);
    let mut index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let file = index_file(&index);
    let (before_generation, _) = read_saved_file(&file).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open({
            let mut name = file.as_os_str().to_owned();
            name.push(".lock");
            PathBuf::from(name)
        })
        .unwrap();
    lock.lock().unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    std::fs::write(library.path().join("d03/stuck.jpg"), b"").unwrap();
    all.insert("d03/stuck.jpg".into());
    let t = Instant::now();
    index.refresh().unwrap();
    let held_ms = t.elapsed().as_millis();
    assert!(
        index.save_error.is_some(),
        "a held lock must leave the save pending"
    );
    assert_eq!(
        sorted(index.paths().to_vec()),
        all,
        "the listing is still current"
    );
    let (generation, saved) = read_saved_file(&file).unwrap();
    assert_eq!(
        generation, before_generation,
        "nothing was written under the lock"
    );
    assert!(!sorted(saved).contains("d03/stuck.jpg"));
    // A second refresh while still held keeps it pending, not dropped.
    index.refresh().unwrap();
    assert!(index.save_error.is_some(), "still pending while held");
    lock.unlock().unwrap();
    drop(lock);
    // Nothing changed in the library; the pending save still goes out.
    index.refresh().unwrap();
    assert_eq!(index.save_error, None, "the retried save succeeds");
    let (after_generation, saved) = read_saved_file(&file).unwrap();
    assert!(after_generation > before_generation);
    assert_eq!(sorted(saved), all, "a stuck lock lost a save");
    println!(
        "M6 D1b held_refresh_ms={held_ms} generation {before_generation} -> {after_generation} \
         saved_after_release=true"
    );
}

/// One sample around a read: the reader thread's on-CPU and run-queue ns
/// (thread CPU clock; run-queue wait from `/proc/thread-self/schedstat`, tick-granular) and system-wide steal ticks (`/proc/stat`,
/// USER_HZ, usually 10 ms each). Both files are held open and re-read with
/// `pread`, so a sample is two small reads. Linux only; elsewhere `None`.
struct ReadProbe {
    #[cfg(target_os = "linux")]
    files: Option<(std::fs::File, std::fs::File)>,
    spent: Duration,
    samples: u64,
}

#[derive(Clone, Copy)]
struct ProbeSample {
    on_cpu_ns: u64,
    runq_ns: u64,
    steal_ticks: u64,
}

impl ReadProbe {
    fn open() -> Self {
        ReadProbe {
            #[cfg(target_os = "linux")]
            files: std::fs::File::open("/proc/thread-self/schedstat")
                .and_then(|s| Ok((s, std::fs::File::open("/proc/stat")?)))
                .ok(),
            spent: Duration::ZERO,
            samples: 0,
        }
    }

    fn sample(&mut self) -> Option<ProbeSample> {
        let t = Instant::now();
        let out = self.read();
        self.spent += t.elapsed();
        self.samples += 1;
        out
    }

    /// Mean cost of a sample, in microseconds. Two samples are taken per read.
    fn cost_us(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.spent.as_secs_f64() * 1e6 / self.samples as f64 * 2.0
    }

    #[cfg(target_os = "linux")]
    fn read(&mut self) -> Option<ProbeSample> {
        use std::os::unix::fs::FileExt;
        let (sched, stat) = self.files.as_ref()?;
        let mut buf = [0u8; 256];
        let n = sched.read_at(&mut buf, 0).ok()?;
        let mut it = std::str::from_utf8(&buf[..n]).ok()?.split_whitespace();
        // schedstat's run time lags up to a tick for a running thread; the
        // thread CPU clock is exact.
        let _ = it.next()?;
        let on_cpu_ns = thread_cpu_ns()?;
        let runq_ns = it.next()?.parse().ok()?;
        // The first line of /proc/stat: "cpu  user nice system idle iowait irq softirq steal ...".
        let mut line = [0u8; 256];
        let n = stat.read_at(&mut line, 0).ok()?;
        let text = std::str::from_utf8(&line[..n]).ok()?;
        let steal_ticks = text
            .lines()
            .next()?
            .split_whitespace()
            .nth(8)?
            .parse()
            .ok()?;
        Some(ProbeSample {
            on_cpu_ns,
            runq_ns,
            steal_ticks,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn read(&mut self) -> Option<ProbeSample> {
        None
    }
}

/// This thread's CPU time, `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`.
#[cfg(target_os = "linux")]
fn thread_cpu_ns() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    extern "C" {
        fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
    }
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: a valid out pointer to a timespec-shaped struct (64-bit Linux).
    let rc = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (rc == 0).then(|| ts.sec as u64 * 1_000_000_000 + ts.nsec as u64)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for UTC epoch milliseconds.
fn iso_utc_ms(epoch_ms: u128) -> String {
    let ms = (epoch_ms % 1000) as u64;
    let secs = (epoch_ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[test]
fn iso_utc_ms_formats_known_instants() {
    assert_eq!(iso_utc_ms(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso_utc_ms(1_793_408_415_853), "2026-10-31T01:00:15.853Z");
    assert_eq!(iso_utc_ms(951_782_400_001), "2000-02-29T00:00:00.001Z");
}
