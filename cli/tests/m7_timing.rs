//! M7 timing tests (Product Lead r14 gate, option (b)): the tests whose
//! assertions are wall-clock bars (untrusted re-read <= 5 ms per request,
//! the no-flicker withheld / inline / request bars), in their own binary
//! run with `--test-threads=1` so other tests' CPU does not inflate them.
//! Bars and code are unchanged; the helpers are shared via m7/common.rs.
#![allow(dead_code, unused_imports)]

include!("m7/common.rs");

/// Product Lead 0926Z gate D2 / XO FRAGO 09:28Z (b): the production path,
/// no test hook. A real SHOW tombstone whose stamp can never be trusted
/// (mtime an hour in the future, as a sync from a skewed clock leaves it)
/// is read again on every request, by design: an untrusted show may have
/// been rewritten to a hide without a stamp change, so trusting it would
/// break A8. The cost is real, so it is measured and printed
/// (`M7 UNTRUSTED_REREADS`) for CI: 256 such tombstones, 10 requests,
/// reads per request and ms per request, beside a real HIDDEN tombstone
/// (also future-dated) that must stay hidden and is not re-read.
#[test]
fn an_untrusted_real_tombstone_reread_cost_is_measured() {
    const N: usize = 256;
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let future = SystemTime::now() + Duration::from_secs(3600);
    let mut want = Vec::new();
    for i in 0..=N {
        let name = format!("IMG_{i:04}.JPG");
        fs::write(
            library.path().join(&name),
            format!("synthetic-m7-untrusted-{name}"),
        )
        .unwrap();
        let tomb = library.path().join(format!("{name}.albumen-tombstone"));
        if i == 0 {
            fs::write(&tomb, br#"{"hidden":true,"reason":"hide"}"#).unwrap();
        } else {
            fs::write(&tomb, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
            want.push(name);
        }
        set_mtime(&tomb, future);
    }
    set_mtime(
        library.path(),
        SystemTime::now() - Duration::from_secs(3600),
    );
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(
        listed(&addr),
        want,
        "a hidden photo showed, or a shown one did not"
    );
    // One /api/found per snapshot: reads and age re-reads from the same
    // instant.
    let snap = || {
        let f = found_page(&addr);
        (
            f["hide_recheck_reads"].as_u64().unwrap(),
            f["hide_age_rereads"].as_u64().unwrap(),
        )
    };
    let w = Instant::now();
    let (r0, a0) = snap();
    let mut worst = 0.0f64;
    let t = Instant::now();
    for _ in 0..10 {
        let q = Instant::now();
        assert_eq!(
            listed(&addr),
            want,
            "a hidden photo showed, or a shown one did not"
        );
        worst = worst.max(q.elapsed().as_secs_f64() * 1000.0);
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 10.0;
    let (r1, a1) = snap();
    let window_ms = w.elapsed().as_millis() as u64;
    // The hidden tombstone's future mtime is not trusted, so the PL 14:36Z
    // bounded age (HID_REVERIFY, 500 ms) reads it again once per 500 ms by
    // design; CI 1471012 caught one such read (2561 vs 2560). Those are
    // counted apart and bounded by the window, never ignored.
    let age_reads = a1 - a0;
    let reads = r1 - r0 - age_reads;
    let key = found_page(&addr)["hide_key"]
        .as_str()
        .unwrap_or("?")
        .to_string();
    // Product Lead 0943Z D2: on Windows the key is ChangeTime, which a
    // future mtime cannot future-date, so the tombstones may be trusted
    // and 0 re-reads there measures nothing. It is printed as
    // measured=no, never as a pass.
    let measured = cfg!(unix);
    println!(
        "M7 UNTRUSTED_REREADS key={key} measured={} tombstones={N} requests=10 reads={reads} age_rereads={age_reads} \
         reads_per_request={:.1} per_request_ms={ms:.2} max_request_ms={worst:.2}",
        if measured { "yes" } else { "no" },
        reads as f64 / 10.0
    );
    if measured {
        // Exactly one re-read per untrusted SHOW per request: a lower bound
        // (they ARE re-read) and, with each show read once, an exact 0 for
        // the hidden one.
        assert!(
            age_reads <= window_ms / 500 + 1,
            "hidden tombstone re-read {age_reads} times by the 500 ms bounded age in {window_ms} ms"
        );
        assert_eq!(
            reads,
            10 * N as u64,
            "expected each of the {N} untrusted shows re-read once per request and \
             the hidden tombstone not at all"
        );
        // Product Lead 0951Z D2 / XO 09:53Z (3): the re-read cost bar,
        // p95 over the 10 requests (= the max at n=10) <= 5 ms at 256.
        // XO 11:34Z (e): ALBUMEN_M7_TIMING_PRINT_ONLY=1 (set only by the
        // print-only CPU-load step) prints a miss of this wall-clock bar
        // instead of failing; the binding m7_timing step never sets it. The
        // read count above stays asserted either way.
        let miss = format!(
            "untrusted re-read cost over the bar: max_request_ms={worst:.2} (bar 5 ms at {N})"
        );
        if std::env::var("ALBUMEN_M7_TIMING_PRINT_ONLY").as_deref() == Ok("1") {
            if worst > 5.0 {
                println!("M7 BAR_MISS_PRINT_ONLY {miss}");
            }
        } else {
            assert!(worst <= 5.0, "{miss}");
        }
    } else {
        assert!(
            reads <= 10 * N as u64,
            "the hidden tombstone was re-read too"
        );
    }
}
// The PL's five variants are ctime (Linux), ChangeTime (Windows), the coarse
// 2 s window, a worker error and a worker panic: four run on each platform
// (the key variant is compiled for its own platform only, so CI sees "4
// passed; 0 ignored" on both).
#[cfg(not(windows))]
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs test hooks"
)]
fn m7_no_flicker_ctime() {
    no_flicker("platform");
}
#[cfg(windows)]
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs test hooks"
)]
fn m7_no_flicker_changetime() {
    no_flicker("platform");
}
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs test hooks"
)]
fn m7_no_flicker_coarse_window() {
    no_flicker("coarse");
}
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs test hooks"
)]
fn m7_no_flicker_worker_error() {
    no_flicker("worker_error");
}
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs test hooks"
)]
fn m7_no_flicker_worker_panic() {
    no_flicker("worker_panic");
}
