//! M6 X1 with the real binary: `albumen gui` answers /api/library pages
//! and search-tool-style readers read the saved index while one writer
//! saves it over and over. Counters: parse failures, fallback walks (no
//! complete saved index), partial results (a saved listing shorter or
//! longer than the writer's own count for that generation). GUI page
//! latency during writes is compared with a same-run, spaced, no-writer
//! control (Product Lead X1 ruling, 20:31Z): p50 within 10% of it, and
//! /api/library max under write <= 16.7 ms. XO 20:58Z: the base and
//! under-write windows are interleaved in rounds in the same run
//! (ALBUMEN_M6_ROUNDS, ALBUMEN_M6_SEED), and peak RSS <= 332 MiB is asserted
//! on the gui child process by its own pid (not the harness or rustc).
//! Synthetic files only.
//!
//! Env: ALBUMEN_M6_FILES (default 2000), ALBUMEN_M6_CYCLES (save/read
//! cycles, default 200). Spec bars: 10,000 cycles at 10k, 1,000 at 380k.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use albumen_ingest::index::{read_saved_file, LibraryIndex};

struct Gui(Child);
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(library: &Path, work: &Path, index: &Path, home: &Path) -> (Gui, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(["gui", "--no-open", "--port", "0", "--library"])
        .arg(library)
        .arg("--work")
        .arg(work)
        .env("ALBUMEN_INDEX_DIR", index)
        .env("ALBUMEN_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = None;
    for line in lines.by_ref().map_while(Result::ok) {
        if let Some(url) = line.strip_prefix("albumen gui: http://") {
            addr = Some(url.trim_end_matches('/').to_string());
            break;
        }
    }
    std::thread::spawn(move || for _ in lines.map_while(Result::ok) {});
    (Gui(child), addr.expect("gui prints its URL"))
}

/// GET `path`; the status, the body and how long the answer took.
fn get(addr: &str, path: &str) -> (u16, String, Duration) {
    let began = Instant::now();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let took = began.elapsed();
    let code = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body, took)
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap()
}

fn wait_ready(addr: &str) -> serde_json::Value {
    for _ in 0..400 {
        let (code, body, _) = get(addr, "/api/library");
        assert_eq!(code, 200);
        let lib = json(&body);
        if lib["status"] == "ready" {
            return lib;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("library never ready");
}

/// Peak resident memory of one process, by its own pid, in kB (XO 20:58Z:
/// the 332 MiB budget is the product's, so X1 measures the gui child only,
/// not the test harness or rustc). Linux: VmHWM. Windows: PeakWorkingSet64
/// from the inbox PowerShell. Elsewhere: not measured (None).
fn peak_rss_kb(pid: u32) -> Option<u64> {
    if cfg!(target_os = "linux") {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    } else if cfg!(windows) {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!("(Get-Process -Id {pid}).PeakWorkingSet64"))
            .output()
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(bytes / 1024)
    } else {
        None
    }
}

/// Peak RSS bar for the gui child: 332 MiB.
const MAX_CHILD_RSS_KB: u64 = 339_968;

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

/// Time `n` GUI first-page requests (the grid's first 200 tiles).
fn gui_pages(addr: &str, n: usize) -> Vec<f64> {
    (0..n)
        .map(|_| {
            let (code, _, took) = get(addr, "/api/library?offset=0&limit=200");
            assert_eq!(code, 200);
            took.as_secs_f64() * 1000.0
        })
        .collect()
}

#[test]
fn readers_keep_answering_while_one_writer_saves() {
    let files = env_num("ALBUMEN_M6_FILES", 2000);
    let cycles = env_num("ALBUMEN_M6_CYCLES", 200);
    println!("M6 X1 files={files} cycles={cycles}");
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut all = BTreeSet::new();
    for i in 0..files {
        let dir = format!("d{:02}", i % 50);
        std::fs::create_dir_all(library.path().join(&dir)).unwrap();
        let rel = format!("{dir}/IMG_{i:07}.jpg");
        std::fs::write(library.path().join(&rel), b"").unwrap();
        all.insert(rel);
    }
    let mut writer = LibraryIndex::open(library.path(), Some(index.path())).unwrap();
    let file = writer.file().unwrap().to_path_buf();

    let began = Instant::now();
    let (gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    let first = wait_ready(&addr);
    let first_ms = began.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(first["images"].as_array().unwrap().len() as u64, files);
    println!("M6 X1 first_listing_ms={first_ms:.1} (saved index, gui start to ready)");

    // The writer's own count for each generation it saved.
    let counts: Arc<Mutex<HashMap<u64, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    {
        // What is on disk once the GUI is up (it may have saved too).
        let (g, p) = read_saved_file(&file).unwrap();
        counts.lock().unwrap().insert(g, p.len());
    }
    let checked = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let parse_failures = Arc::new(AtomicU64::new(0));
    let fallbacks = Arc::new(AtomicU64::new(0));
    let partials = Arc::new(AtomicU64::new(0));
    let read_ms = Arc::new(Mutex::new(Vec::new()));
    // Diagnostics only (no bar): when each slow read started, and when each
    // writer refresh (walk + save) ran, on one clock.
    let t0 = Instant::now();
    let slow_reads: Arc<Mutex<Vec<(f64, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (file, stop, counts) = (file.clone(), stop.clone(), counts.clone());
            let checked = checked.clone();
            let slow_reads = slow_reads.clone();
            let (reads, parse_failures, fallbacks, partials, read_ms) = (
                reads.clone(),
                parse_failures.clone(),
                fallbacks.clone(),
                partials.clone(),
                read_ms.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    let got = read_saved_file(&file);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    read_ms.lock().unwrap().push(ms);
                    if ms > 50.0 {
                        let at = (t - t0).as_secs_f64() * 1000.0;
                        slow_reads.lock().unwrap().push((at, ms));
                    }
                    reads.fetch_add(1, Ordering::Relaxed);
                    match got {
                        Ok((generation, paths)) => {
                            let want = counts.lock().unwrap().get(&generation).copied();
                            if let Some(want) = want {
                                checked.fetch_add(1, Ordering::Relaxed);
                                if want != paths.len() && partials.fetch_add(1, Ordering::Relaxed) < 5 {
                                        println!(
                                            "M6 X1 PARTIAL generation={generation} writer_count={want} read_count={}",
                                            paths.len()
                                        );
                                    }
                            }
                        }
                        Err(e) if e.contains("no complete saved index") => {
                            fallbacks.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            parse_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
        })
        .collect();

    // Diagnostic only (no bar): a warm back-to-back loop with the readers
    // running and no writer.
    let mut warm = gui_pages(&addr, 300);
    let (warm_p50, warm_max) = (pct(&mut warm, 0.50), pct(&mut warm, 1.0));
    // The X1 base (the bar) is the same-run, spaced, no-writer control
    // (59a96c4). XO 20:58Z: interleave it with the under-write windows so
    // drift and noise land on both sides. The run is split into `rounds`;
    // each round has one write window (cycles/rounds writer refreshes, a
    // GUI sample every 4th) and one base window (as many single samples,
    // spaced like that round's write samples, the same readers running, no
    // writer). ALBUMEN_M6_SEED picks which window goes first in each round
    // and where the writer's edits start.
    let rounds = env_num("ALBUMEN_M6_ROUNDS", 20).clamp(1, cycles.max(1));
    let seed = env_num("ALBUMEN_M6_SEED", 1);
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next_bit = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng & 1 == 1
    };
    println!("M6 X1 seed={seed} rounds={rounds}");
    let mut spaced: Vec<f64> = Vec::new();
    let mut windows: Vec<(f64, f64)> = Vec::new();
    let mut during = Vec::new();
    let mut serial = seed % 50;
    let mut save_errors = 0u64;
    let mut cycle = 0u64;
    let mut base_gap = Duration::from_millis(5);
    for round in 0..rounds {
        let round_cycles = cycles / rounds + u64::from(round < cycles % rounds);
        let base_first = next_bit();
        for phase in 0..2 {
            if (phase == 0) == base_first {
                let n = round_cycles.div_ceil(4).max(1);
                for _ in 0..n {
                    std::thread::sleep(base_gap);
                    spaced.extend(gui_pages(&addr, 1));
                }
                continue;
            }
            let w_began = Instant::now();
            let mut samples = 0u32;
            for _ in 0..round_cycles {
                serial += 1;
                let dir = format!("d{:02}", serial % 50);
                if cycle.is_multiple_of(2) {
                    let rel = format!("{dir}/new{serial:07}.jpg");
                    std::fs::write(library.path().join(&rel), b"").unwrap();
                    all.insert(rel);
                } else if let Some(rel) = all.iter().find(|r| r.starts_with(&dir)).cloned() {
                    std::fs::remove_file(library.path().join(&rel)).unwrap();
                    all.remove(&rel);
                }
                let w0 = (Instant::now() - t0).as_secs_f64() * 1000.0;
                writer.refresh().unwrap();
                windows.push((w0, (Instant::now() - t0).as_secs_f64() * 1000.0));
                if writer.save_error.is_some() {
                    save_errors += 1;
                }
                let g = writer.generation().unwrap();
                counts.lock().unwrap().insert(g, writer.paths().len());
                if cycle.is_multiple_of(4) {
                    during.extend(gui_pages(&addr, 1));
                    samples += 1;
                }
                cycle += 1;
            }
            if samples > 0 {
                base_gap = (w_began.elapsed() / samples).min(Duration::from_millis(100));
            }
        }
    }
    let (base_p50, base_p95, base_max) = (
        pct(&mut spaced, 0.50),
        pct(&mut spaced, 0.95),
        pct(&mut spaced, 1.0),
    );
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }
    let p50 = pct(&mut during, 0.50);
    let p95 = pct(&mut during, 0.95);
    let write_max = pct(&mut during, 1.0);
    let slow = slow_reads.lock().unwrap().clone();
    let overlapping = slow
        .iter()
        .filter(|(at, ms)| windows.iter().any(|(a, b)| *at < *b && at + ms > *a))
        .count();
    let mut refresh: Vec<f64> = windows.iter().map(|(a, b)| b - a).collect();
    let (rf50, rfmax) = (pct(&mut refresh, 0.50), pct(&mut refresh, 1.0));
    for (at, ms) in slow.iter().take(10) {
        println!("M6 X1 SLOW_READ at_ms={at:.1} took_ms={ms:.1}");
    }
    println!(
        "M6 X1 DIAG gui_warm_p50_ms={warm_p50:.2} gui_warm_max_ms={warm_max:.2} gui_base_max_ms={base_max:.2} \
         gui_write_samples={} gui_base_samples={} writer_refresh_p50_ms={rf50:.2} writer_refresh_max_ms={rfmax:.2} \
         slow_reads_over_50ms={} slow_reads_overlapping_a_writer_refresh={overlapping}",
        during.len(),
        spaced.len(),
        slow.len()
    );
    let mut rm = read_ms.lock().unwrap().clone();
    let (r50, r95, rmax) = (pct(&mut rm, 0.50), pct(&mut rm, 0.95), pct(&mut rm, 1.0));
    let (reads, parse_failures, fallbacks, partials) = (
        reads.load(Ordering::Relaxed),
        parse_failures.load(Ordering::Relaxed),
        fallbacks.load(Ordering::Relaxed),
        partials.load(Ordering::Relaxed),
    );
    // Read the child's peak while it is still alive.
    let child_peak = peak_rss_kb(gui.0.id());
    let child_peak_txt = child_peak.map_or("unmeasured".to_string(), |k| k.to_string());
    let (_, saved) = read_saved_file(&file).unwrap();
    let equals_walk = saved.into_iter().collect::<BTreeSet<_>>() == all;
    println!(
        "M6 X1 RESULT files={files} cycles={cycles} saves_failed={save_errors} reads={reads} checked_against_writer={} \
         parse_failures={parse_failures} fallback_walks={fallbacks} partial_results={partials} \
         index_read_p50_ms={r50:.2} index_read_p95_ms={r95:.2} index_read_max_ms={rmax:.2} \
         gui_base_p50_ms={base_p50:.2} gui_base_p95_ms={base_p95:.2} \
         gui_write_p50_ms={p50:.2} gui_write_p95_ms={p95:.2} gui_write_max_ms={write_max:.2} gui_p50_ratio={:.3} \
         first_listing_ms={first_ms:.1} final_equals_walk={equals_walk} \
         gui_child_peak_rss_kb={child_peak_txt} gui_child_pid={}",
        checked.load(Ordering::Relaxed),
        p50 / base_p50,
        gui.0.id()
    );
    assert_eq!(parse_failures, 0);
    assert_eq!(fallbacks, 0);
    assert_eq!(partials, 0);
    assert!(
        checked.load(Ordering::Relaxed) * 2 > reads,
        "too few reads checked against the writer"
    );
    assert_eq!(save_errors, 0);
    assert!(equals_walk, "the last save is not the library");
    assert!(p50 <= 500.0, "/api/library p50 {p50:.2} ms over 500 ms");
    assert!(
        p50 <= base_p50 * 1.10,
        "/api/library p50 {p50:.2} ms more than 10% over the spaced no-writer control {base_p50:.2} ms"
    );
    assert!(
        write_max <= 16.7,
        "/api/library max under write {write_max:.2} ms over 16.7 ms"
    );
    if let Some(kb) = child_peak {
        assert!(
            kb <= MAX_CHILD_RSS_KB,
            "gui child peak RSS {kb} kB over {MAX_CHILD_RSS_KB} kB (332 MiB)"
        );
    } else if cfg!(any(target_os = "linux", windows)) {
        panic!("could not read the gui child's peak RSS");
    }
    assert!(
        first_ms <= 2000.0,
        "first listing {first_ms:.1} ms over 2 s"
    );
}
