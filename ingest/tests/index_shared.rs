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
//! (default 200), `ALBUMEN_M6_KILLS` (default 40), `ALBUMEN_M6_SEED`.
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
    let first_failure = Arc::new(std::sync::Mutex::new(None::<String>));
    let reader = {
        let first_failure = first_failure.clone();
        let (stop, reads, failures, max_read_us, file) = (
            stop.clone(),
            reads.clone(),
            failures.clone(),
            max_read_us.clone(),
            file.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                let checked = check_saved_file(&file);
                max_read_us.fetch_max(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                reads.fetch_add(1, Ordering::Relaxed);
                if let Err(err) = checked {
                    failures.fetch_add(1, Ordering::Relaxed);
                    first_failure.lock().unwrap().get_or_insert(err);
                }
            }
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
    reader.join().unwrap();
    let fresh = LibraryIndex::open(library.path(), None).unwrap();
    let (_, saved) = read_saved_file(&file).unwrap();
    let byte_identical = saved == fresh.paths();
    let left = partials(index_dir.path());
    println!(
        "M6 X2 RESULT seed={seed} iters={iters} reads={} parse_failures={} max_read_ms={:.2} \
         regressions={regressions} merges={merges} final_generation={last_generation} \
         equals_fresh_walk={byte_identical} stale_partials={} first_failure={:?}",
        reads.load(Ordering::Relaxed),
        failures.load(Ordering::Relaxed),
        max_read_us.load(Ordering::Relaxed) as f64 / 1000.0,
        left.len(),
        first_failure.lock().unwrap()
    );
    assert_eq!(failures.load(Ordering::Relaxed), 0);
    assert_eq!(regressions, 0);
    assert!(byte_identical);
    assert!(left.is_empty(), "{left:?}");
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
