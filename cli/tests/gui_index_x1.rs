//! M6 X1 with the real binary: `albumen gui` answers /api/library pages
//! and search-tool-style readers read the saved index while one writer
//! saves it over and over. Counters: parse failures, fallback walks (no
//! complete saved index), partial results (a saved listing shorter or
//! longer than the writer's own count for that generation). GUI page
//! latency during writes is compared with a no-write baseline.
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
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
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

    // Baseline with the same two index readers running, so the only
    // difference between the two samples is the writer saving.
    let mut base = gui_pages(&addr, 300);
    let base_p50 = pct(&mut base, 0.50);
    let base_p95 = pct(&mut base, 0.95);
    let base_max = pct(&mut base, 1.0);
    // Diagnostic control (no bar): single requests spaced out like the
    // write-phase samples, with no writer running.
    let mut spaced: Vec<f64> = (0..300)
        .flat_map(|_| {
            std::thread::sleep(Duration::from_millis(5));
            gui_pages(&addr, 1)
        })
        .collect();
    let (spaced_p50, spaced_p95, spaced_max) = (
        pct(&mut spaced, 0.50),
        pct(&mut spaced, 0.95),
        pct(&mut spaced, 1.0),
    );
    let mut windows: Vec<(f64, f64)> = Vec::new();

    let mut during = Vec::new();
    let mut serial = 0u64;
    let mut save_errors = 0u64;
    for cycle in 0..cycles {
        serial += 1;
        let dir = format!("d{:02}", serial % 50);
        if cycle % 2 == 0 {
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
        if cycle % 4 == 0 {
            during.extend(gui_pages(&addr, 1));
        }
    }
    let more = gui_pages(&addr, 300usize.saturating_sub(during.len()));
    during.extend(more);
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
        "M6 X1 DIAG gui_base_max_ms={base_max:.2} gui_spaced_p50_ms={spaced_p50:.2} \
         gui_spaced_p95_ms={spaced_p95:.2} gui_spaced_max_ms={spaced_max:.2} gui_write_max_ms={write_max:.2} \
         gui_write_samples={} writer_refresh_p50_ms={rf50:.2} writer_refresh_max_ms={rfmax:.2} \
         slow_reads_over_50ms={} slow_reads_overlapping_a_writer_refresh={overlapping}",
        during.len(),
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
    let (_, saved) = read_saved_file(&file).unwrap();
    let equals_walk = saved.into_iter().collect::<BTreeSet<_>>() == all;
    println!(
        "M6 X1 RESULT files={files} cycles={cycles} saves_failed={save_errors} reads={reads} checked_against_writer={} \
         parse_failures={parse_failures} fallback_walks={fallbacks} partial_results={partials} \
         index_read_p50_ms={r50:.2} index_read_p95_ms={r95:.2} index_read_max_ms={rmax:.2} \
         gui_base_p50_ms={base_p50:.2} gui_base_p95_ms={base_p95:.2} \
         gui_write_p50_ms={p50:.2} gui_write_p95_ms={p95:.2} gui_p50_ratio={:.3} \
         first_listing_ms={first_ms:.1} final_equals_walk={equals_walk}",
        checked.load(Ordering::Relaxed),
        p50 / base_p50
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
        "/api/library p50 {p50:.2} ms more than 10% over the no-write {base_p50:.2} ms"
    );
    assert!(
        first_ms <= 2000.0,
        "first listing {first_ms:.1} ms over 2 s"
    );
}
