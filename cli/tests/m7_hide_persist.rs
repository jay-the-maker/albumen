//! M7 (A8, "deleted photos came back"): hide and delete state lives only in
//! files (receiver `{name}.albumen-tombstone` sidecars and the family log).
//! Every listing must honour it after a scan, a rescan from the saved index
//! and an index rebuild:
//! - a receiver-hidden photo stays out of the GUI grid;
//! - a family hide hides the photo for that viewer only, and a family
//!   tombstone hides the object for every member;
//! - an unhide (tombstone rewritten to `"hidden":false`) shows it again;
//! - at scale (`ALBUMEN_M7_FILES`, for the garage PC and ubuntu CI) the
//!   same holds and, with `ALBUMEN_M7_BARS=1`, the M6 bars are met: first
//!   tile <= 2 s and gui child peak RSS <= 332 MiB.
//!
//! Synthetic files only; originals are checked bit-exact (bytes + mtime).

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use albumen_ingest::hides::hidden_paths;
use albumen_ingest::index::LibraryIndex;

/// PROVISIONAL, non-blocking (XO 00:18Z): one /api/found hide refresh after
/// the listing. Printed only until 3+ CI samples exist; never asserted.
const HIDE_POLL_BAR_MS: f64 = 150.0;

struct Gui(Child);
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn albumen(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn start(library: &Path, work: &Path, index: &Path, home: &Path) -> (Gui, String) {
    let (gui, addr, _) = start_pid(library, work, index, home);
    (gui, addr)
}

fn start_pid(library: &Path, work: &Path, index: &Path, home: &Path) -> (Gui, String, u32) {
    start_env(library, work, index, home, &[])
}

fn start_env(
    library: &Path,
    work: &Path,
    index: &Path,
    home: &Path,
    env: &[(&str, &str)],
) -> (Gui, String, u32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .envs(env.iter().copied())
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
    let pid = child.id();
    (Gui(child), addr.expect("gui prints its URL"), pid)
}

fn get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let code = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body)
}

/// The grid's paths once `/api/library` says ready.
fn grid(library: &Path, work: &Path, index: &Path, home: &Path) -> Vec<String> {
    let (_gui, addr) = start(library, work, index, home);
    for _ in 0..400 {
        let (code, body) = get(&addr, "/api/library");
        assert_eq!(code, 200);
        let lib: serde_json::Value = serde_json::from_str(&body).unwrap();
        if lib["status"] == "ready" {
            let mut p: Vec<String> = lib["images"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["path"].as_str().unwrap().to_string())
                .collect();
            p.sort();
            return p;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("library never ready");
}

fn line<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in:\n{text}"))
}

fn kept(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    paths
        .iter()
        .map(|path| {
            (
                path.clone(),
                fs::read(path).unwrap(),
                fs::metadata(path).unwrap().modified().unwrap(),
            )
        })
        .collect()
}

fn originals(lib: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
    let mut out: Vec<_> = ["IMG_0001.JPG", "IMG_0002.JPG"]
        .iter()
        .map(|name| {
            let path = lib.join(name);
            (
                name.to_string(),
                fs::read(&path).unwrap(),
                fs::metadata(&path).unwrap().modified().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_hidden_photo_stays_hidden_after_rescan_and_index_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("library");
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    let index_a = tmp.path().join("index-a");
    let index_b = tmp.path().join("index-b");
    for dir in [&lib, &work, &home, &index_a, &index_b] {
        fs::create_dir(dir).unwrap();
    }
    fs::write(lib.join("IMG_0001.JPG"), b"synthetic-m7-visible").unwrap();
    fs::write(lib.join("IMG_0002.JPG"), b"synthetic-m7-hidden").unwrap();

    let (ok, out) = albumen(&[
        "receive",
        "hide",
        "--library",
        lib.to_str().unwrap(),
        "--name",
        "IMG_0002.JPG",
        "--apply",
    ]);
    assert!(ok, "receive hide failed: {out}");
    let tombstone = fs::read_to_string(lib.join("IMG_0002.JPG.albumen-tombstone")).unwrap();
    assert!(tombstone.contains("\"hidden\":true"), "{tombstone}");
    let before = originals(&lib);

    let scan = grid(&lib, &work, &index_a, &home);
    let rescan = grid(&lib, &work, &index_a, &home);
    let rebuild = grid(&lib, &work, &index_b, &home);
    println!("M7 A8 scan={scan:?} rescan={rescan:?} rebuild={rebuild:?}");

    assert_eq!(originals(&lib), before, "an original changed");
    assert!(
        fs::read_to_string(lib.join("IMG_0002.JPG.albumen-tombstone"))
            .unwrap()
            .contains("\"hidden\":true"),
        "the tombstone changed"
    );
    let want = vec!["IMG_0001.JPG".to_string()];
    assert_eq!(scan, want, "first scan shows the hidden photo");
    assert_eq!(rescan, want, "rescan brought the hidden photo back");
    assert_eq!(rebuild, want, "index rebuild brought the hidden photo back");
}

#[test]
fn an_unhidden_photo_shows_again_after_rescan_and_index_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("library");
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    let index_a = tmp.path().join("index-a");
    let index_b = tmp.path().join("index-b");
    for dir in [&lib, &work, &home, &index_a, &index_b] {
        fs::create_dir(dir).unwrap();
    }
    fs::write(lib.join("IMG_0001.JPG"), b"synthetic-m7-visible").unwrap();
    fs::write(lib.join("IMG_0002.JPG"), b"synthetic-m7-hidden").unwrap();
    let lib_arg = lib.to_str().unwrap();
    for verb in ["hide", "unhide"] {
        let (ok, out) = albumen(&[
            "receive",
            verb,
            "--library",
            lib_arg,
            "--name",
            "IMG_0002.JPG",
            "--apply",
        ]);
        assert!(ok, "receive {verb} failed: {out}");
    }
    let tombstone = fs::read_to_string(lib.join("IMG_0002.JPG.albumen-tombstone")).unwrap();
    assert!(tombstone.contains("\"hidden\":false"), "{tombstone}");
    let before = originals(&lib);

    let scan = grid(&lib, &work, &index_a, &home);
    let rescan = grid(&lib, &work, &index_a, &home);
    let rebuild = grid(&lib, &work, &index_b, &home);
    println!("M7 unhide scan={scan:?} rescan={rescan:?} rebuild={rebuild:?}");

    assert_eq!(originals(&lib), before, "an original changed");
    let want = vec!["IMG_0001.JPG".to_string(), "IMG_0002.JPG".to_string()];
    assert_eq!(scan, want, "first scan hides an unhidden photo");
    assert_eq!(rescan, want, "rescan hides an unhidden photo");
    assert_eq!(rebuild, want, "index rebuild hides an unhidden photo");
}

/// The index's paths after open (scan or rescan), minus what the files say
/// is hidden for `viewer`.
fn visible(library: &Path, index: &Path, viewer: Option<&str>) -> Vec<String> {
    let paths = LibraryIndex::open(library, Some(index))
        .unwrap()
        .into_paths();
    let hidden = hidden_paths(library, &paths, viewer);
    assert!(hidden.errors.is_empty(), "{:?}", hidden.errors);
    paths
        .into_iter()
        .filter(|p| !hidden.paths.contains(p))
        .collect()
}

/// A two-member family library (ada admits bea) made with the CLI.
struct Family {
    library: PathBuf,
    key_a: PathBuf,
    key_b: PathBuf,
    member_a: String,
    member_b: String,
}

fn family(tmp: &Path) -> Family {
    let key_a = tmp.join("a.key");
    let key_b = tmp.join("b.key");
    let s = |p: &Path| p.to_str().unwrap().to_string();
    let (ok, text) = albumen(&["family", "keygen", "--key", &s(&key_a), "--apply"]);
    assert!(ok, "{text}");
    let member_a = line(&text, "member ").to_string();
    let (ok, text) = albumen(&["family", "keygen", "--key", &s(&key_b), "--apply"]);
    assert!(ok, "{text}");
    let member_b = line(&text, "member ").to_string();
    let parent = tmp.join("shared");
    fs::create_dir(&parent).unwrap();
    let (ok, text) = albumen(&[
        "family",
        "init",
        "--to",
        &s(&parent),
        "--key",
        &s(&key_a),
        "--name",
        "ada",
        "--apply",
    ]);
    assert!(ok, "{text}");
    let library = PathBuf::from(line(&text, "library "));
    let lib = s(&library);
    let (ok, text) = albumen(&[
        "family",
        "invite",
        "--library",
        &lib,
        "--key",
        &s(&key_a),
        "--member",
        &member_b,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let token = line(&text, "token ").to_string();
    let (ok, text) = albumen(&[
        "family",
        "join",
        "--library",
        &lib,
        "--key",
        &s(&key_b),
        "--token",
        &token,
        "--name",
        "bea",
        "--apply",
    ]);
    assert!(ok, "{text}");
    let (ok, text) = albumen(&[
        "family",
        "admit",
        "--library",
        &lib,
        "--key",
        &s(&key_a),
        "--member",
        &member_b,
        "--token",
        &token,
        "--apply",
    ]);
    assert!(ok, "{text}");

    Family {
        library,
        key_a,
        key_b,
        member_a,
        member_b,
    }
}

/// `family add` one synthetic file; returns its object path and sha256.
fn family_add(f: &Family, src: &Path) -> (String, String) {
    let (ok, text) = albumen(&[
        "family",
        "add",
        "--library",
        f.library.to_str().unwrap(),
        "--key",
        f.key_a.to_str().unwrap(),
        "--from",
        src.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    let sha = line(&text, "sha256 ").to_string();
    let r = format!("members/{}/objects/sha256/{}/{sha}", f.member_a, &sha[..2]);
    (r, sha)
}

/// `family hide` (as bea) or `family tombstone` (as ada) one object.
fn family_mark(f: &Family, verb: &str, sha: &str) {
    let key = if verb == "hide" { &f.key_b } else { &f.key_a };
    let (ok, text) = albumen(&[
        "family",
        verb,
        "--library",
        f.library.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--sha256",
        sha,
        "--apply",
    ]);
    assert!(ok, "{text}");
}

#[test]
fn a_family_hide_is_per_viewer_and_a_tombstone_is_for_everyone() {
    let tmp = tempfile::tempdir().unwrap();
    let f = family(tmp.path());
    let library = f.library.clone();
    let member_a = f.member_a.clone();
    let member_b = f.member_b.clone();
    let mut stored = Vec::new();
    let mut rel = Vec::new();
    for name in ["hidden-for-bea.jpg", "tombstoned.jpg", "plain.jpg"] {
        let src = tmp.path().join(name);
        fs::write(&src, format!("synthetic-m7-family-{name}")).unwrap();
        let (r, sha) = family_add(&f, &src);
        stored.push(library.join(&r));
        rel.push((r, sha));
    }
    family_mark(&f, "hide", &rel[0].1);
    family_mark(&f, "tombstone", &rel[1].1);
    let before = kept(&stored);

    let index_a = tempfile::tempdir().unwrap();
    let index_b = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    for (pass, index) in [
        ("scan", index_a.path()),
        ("rescan", index_a.path()),
        ("rebuild", index_b.path()),
    ] {
        for (who, viewer) in [("bea", member_b.as_str()), ("ada", member_a.as_str())] {
            let v = visible(&library, index, Some(viewer));
            let shows = |i: usize| v.contains(&rel[i].0);
            println!(
                "M7 family {pass} viewer={who} hidden_for_bea={} tombstoned={} plain={}",
                shows(0),
                shows(1),
                shows(2)
            );
            seen.push((pass, who, shows(0), shows(1), shows(2)));
        }
    }
    assert_eq!(kept(&stored), before, "a stored original changed");
    for (pass, who, hidden_for_bea, tombstoned, plain) in seen {
        assert!(plain, "{pass}: {who} does not see a plain photo");
        assert!(!tombstoned, "{pass}: {who} sees a tombstoned photo");
        assert_eq!(
            hidden_for_bea,
            who == "ada",
            "{pass}: {who} sees the photo bea hid = {hidden_for_bea}"
        );
    }
}

/// Peak RSS of one process, by pid, in kB. Linux: VmHWM. Windows:
/// PeakWorkingSet64 from the inbox PowerShell. Elsewhere: None.
fn peak_rss_kb(pid: u32) -> Option<u64> {
    if cfg!(target_os = "linux") {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
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

/// A scale or diagnostic test with its env var missing skips locally, but
/// FAILS in CI (`CI=true` or `GITHUB_ACTIONS=true`) so a skip never counts
/// as a pass there.
fn skip_or_fail_in_ci(what: &str, var: &str) {
    let ci = ["CI", "GITHUB_ACTIONS"].iter().any(|k| {
        std::env::var(k)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    if ci {
        panic!("{what}: {var} is unset or not a number in CI; a skip must not count as a pass");
    }
    println!("{what} skipped: set {var} (e.g. 380000)");
}

/// Scale run. Off unless `ALBUMEN_M7_FILES` is set (garage PC, ubuntu CI
/// via .github/m6-one.sh). The bars are asserted only with
/// `ALBUMEN_M7_BARS=1`: box timing is not evidence.
#[test]
fn hidden_photos_stay_hidden_at_scale() {
    let Some(files) = std::env::var("ALBUMEN_M7_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        skip_or_fail_in_ci("M7 scale", "ALBUMEN_M7_FILES");
        return;
    };
    let bars = std::env::var("ALBUMEN_M7_BARS").as_deref() == Ok("1");
    // The photos sit inside a family library (genesis.json, family hides
    // and tombstones), so every listing pays the family-log replay too.
    let tmp = tempfile::tempdir().unwrap();
    let fam = family(tmp.path());
    let marks = (files / 2000).max(2);
    for k in 0..marks {
        let src = tmp.path().join(format!("fam-{k}.jpg"));
        fs::write(&src, format!("synthetic-m7-scale-family-{k}")).unwrap();
        let (_, sha) = family_add(&fam, &src);
        family_mark(&fam, if k % 2 == 0 { "hide" } else { "tombstone" }, &sha);
    }
    let library = fam.library.as_path();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut hidden = Vec::new();
    for i in 0..files {
        let dir = format!("d{:02}", i % 50);
        fs::create_dir_all(library.join(&dir)).unwrap();
        let rel = format!("{dir}/IMG_{i:07}.jpg");
        fs::write(library.join(&rel), b"").unwrap();
        if i % 1000 == 7 {
            fs::write(
                library.join(format!("{rel}.albumen-tombstone")),
                format!("{{\"hidden\":true,\"reason\":\"hide\",\"stored_name\":\"{rel}\"}}\n"),
            )
            .unwrap();
            hidden.push(rel);
        }
    }
    let hidden_set: std::collections::HashSet<&str> = hidden.iter().map(String::as_str).collect();

    // COLD: no saved index. The first tile is the first one the page can
    // draw, streamed from /api/found during the first walk or from the
    // finished listing, whichever comes first.
    let began = Instant::now();
    let (_gui, addr, pid) = start_pid(library, work.path(), index.path(), home.path());
    let mut first_tile_ms = None;
    let mut found_polls = 0usize;
    let ready_ms = loop {
        assert!(began.elapsed() < Duration::from_secs(1800), "never ready");
        let f = found_page(&addr);
        found_polls += 1;
        let streamed = f["paths"].as_array().unwrap();
        for p in streamed {
            let p = p.as_str().unwrap();
            assert!(!hidden_set.contains(p), "first build streamed hidden {p}");
        }
        assert!(
            f["found"].as_u64().unwrap() as usize <= files - hidden.len(),
            "found counts hidden photos: {f}"
        );
        if first_tile_ms.is_none() && !streamed.is_empty() {
            first_tile_ms = Some(began.elapsed().as_secs_f64() * 1000.0);
        }
        let (code, body) = get(&addr, "/api/library?offset=0&limit=1");
        assert_eq!(code, 200);
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();
        if page["status"] == "ready" && !page["images"].as_array().unwrap().is_empty() {
            let ms = began.elapsed().as_secs_f64() * 1000.0;
            first_tile_ms.get_or_insert(ms);
            break ms;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let first_tile_ms = first_tile_ms.unwrap();
    // One /api/found after the listing: the hide-line refresh the page runs
    // every 10 s (tombstones + family replay). Provisional bar <= 150 ms
    // (XO 00:08Z): asserted with ALBUMEN_M7_BARS=1; binding after 3+ more
    // CI samples under it.
    let poll_began = Instant::now();
    let f = found_page(&addr);
    let hide_poll_ms = poll_began.elapsed().as_secs_f64() * 1000.0;
    println!("M7 HIDE_POLL_DIAG files={files} family_marks={marks} hide_poll_ms={hide_poll_ms:.1}");
    println!(
        "M7 HIDE_POLL_BAR PROVISIONAL non-blocking hide_poll_ms={hide_poll_ms:.1} \
         bar_ms={HIDE_POLL_BAR_MS} under={} (print only until 3+ CI samples)",
        hide_poll_ms <= HIDE_POLL_BAR_MS
    );
    let (_, body) = get(&addr, "/api/library");
    let lib: serde_json::Value = serde_json::from_str(&body).unwrap();
    let shown: std::collections::HashSet<&str> = lib["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    let hidden_shown = hidden.iter().filter(|h| shown.contains(h.as_str())).count();
    let rss = peak_rss_kb(pid);
    println!(
        "M6 M7 RESULT files={files} hidden={} shown={} hidden_shown={hidden_shown} \
         family_marks={marks} cold=1 found_polls={found_polls} \
         first_tile_ms={first_tile_ms:.1} ready_ms={ready_ms:.1} hide_poll_ms={hide_poll_ms:.1} \
         gui_child_peak_rss_kb={} bars={bars}",
        hidden.len(),
        shown.len(),
        rss.map(|r| r.to_string())
            .unwrap_or_else(|| "unmeasured".into()),
    );
    assert_eq!(hidden_shown, 0, "hidden photos shown at scale");
    assert_eq!(shown.len(), files - hidden.len());
    assert_eq!(f["found"].as_u64(), Some((files - hidden.len()) as u64));
    assert!(f["hide_note"].is_null(), "hide problem: {}", f["hide_note"]);
    if bars {
        assert!(
            first_tile_ms <= 2000.0,
            "first tile {first_tile_ms:.1} ms over 2 s"
        );
        let rss = rss.expect("peak RSS unmeasured on this OS");
        assert!(rss <= 339_968, "gui child peak RSS {rss} kB over 332 MiB");
    }
}

fn found_page(addr: &str) -> serde_json::Value {
    let (code, body) = get(addr, "/api/found");
    assert_eq!(code, 200);
    serde_json::from_str(&body).unwrap()
}

#[test]
fn a_hide_file_that_cant_be_read_hides_the_photo_and_says_so() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for name in ["IMG_0001.JPG", "IMG_0002.JPG"] {
        fs::write(library.path().join(name), b"").unwrap();
    }
    fs::write(library.path().join("IMG_0002.JPG.albumen-tombstone"), b"{").unwrap();
    for pass in ["first build", "rescan"] {
        let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
        let began = Instant::now();
        let lib = loop {
            let (_, body) = get(&addr, "/api/library");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            if v["status"] == "ready" {
                break v;
            }
            assert!(
                began.elapsed() < Duration::from_secs(60),
                "{pass}: never ready"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let paths: Vec<&str> = lib["images"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["IMG_0001.JPG"], "{pass}");
        let f = found_page(&addr);
        let note = f["hide_note"].as_str().unwrap_or_default();
        println!("M7 hide-note {pass}: {note}");
        assert!(
            note.contains("IMG_0002.JPG.albumen-tombstone"),
            "{pass}: {f}"
        );
        assert_eq!(f["found"], 1, "{pass}: {f}");
    }
}

#[test]
fn a_first_build_never_streams_a_hidden_tile() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let files = 4000usize;
    let mut hidden = std::collections::HashSet::new();
    for i in 0..files {
        let dir = format!("d{:02}", i % 20);
        fs::create_dir_all(library.path().join(&dir)).unwrap();
        let rel = format!("{dir}/IMG_{i:05}.jpg");
        fs::write(library.path().join(&rel), b"").unwrap();
        if i % 3 == 0 {
            fs::write(
                library.path().join(format!("{rel}.albumen-tombstone")),
                br#"{"hidden":true,"reason":"hide"}"#,
            )
            .unwrap();
            hidden.insert(rel);
        }
    }
    let visible_n = files - hidden.len();
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    let began = Instant::now();
    let mut polls = 0usize;
    let mut streamed_seen = 0usize;
    loop {
        let f = found_page(&addr);
        polls += 1;
        let paths = f["paths"].as_array().unwrap();
        streamed_seen = streamed_seen.max(paths.len());
        for p in paths {
            let p = p.as_str().unwrap();
            assert!(!hidden.contains(p), "poll {polls}: streamed hidden {p}");
        }
        let found = f["found"].as_u64().unwrap() as usize;
        assert!(
            found <= visible_n,
            "poll {polls}: found {found} > {visible_n}"
        );
        let (_, body) = get(&addr, "/api/library?offset=0&limit=600");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        for i in v["images"].as_array().unwrap() {
            let p = i["path"].as_str().unwrap();
            assert!(!hidden.contains(p), "poll {polls}: listed hidden {p}");
        }
        if v["status"] == "ready" {
            assert_eq!(v["total_images"].as_u64(), Some(visible_n as u64));
            assert_eq!(found_page(&addr)["found"].as_u64(), Some(visible_n as u64));
            break;
        }
        assert!(began.elapsed() < Duration::from_secs(120), "never ready");
    }
    println!("M7 first-build polls={polls} streamed_max={streamed_seen} visible={visible_n}");
}

/// Wait for the listing, then return /api/found.
fn ready_found(addr: &str) -> serde_json::Value {
    let began = Instant::now();
    loop {
        let (_, body) = get(addr, "/api/library?offset=0&limit=1");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        if v["status"] == "ready" {
            return found_page(addr);
        }
        assert!(began.elapsed() < Duration::from_secs(60), "never ready");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn a_family_log_read_error_hides_every_family_object_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let f = family(tmp.path());
    let mut objects = Vec::new();
    for name in ["one.jpg", "two.jpg"] {
        let src = tmp.path().join(name);
        fs::write(&src, format!("synthetic-m7-famlog-{name}")).unwrap();
        objects.push(family_add(&f, &src).0);
    }
    let index = tempfile::tempdir().unwrap();
    for viewer in [&f.member_a, &f.member_b] {
        let v = visible(&f.library, index.path(), Some(viewer));
        assert!(objects.iter().all(|o| v.contains(o)), "before: {v:?}");
    }
    // Make the family log unreadable (synthetic fixture in a temp dir): a
    // device's log directory becomes a plain file, so listing it fails.
    let mut files = Vec::new();
    files_under(&f.library.join("members"), &mut files);
    let log_dir = files
        .iter()
        .filter_map(|p| p.ancestors().find(|a| a.ends_with("log")))
        .next()
        .expect("no family log dir")
        .to_path_buf();
    fs::remove_dir_all(&log_dir).unwrap();
    fs::write(&log_dir, b"not a log directory").unwrap();
    for viewer in [Some(f.member_a.as_str()), Some(f.member_b.as_str()), None] {
        let h = hidden_paths(&f.library, &objects, viewer);
        for o in &objects {
            assert!(
                h.paths.contains(o),
                "{viewer:?} still sees {o} with a broken log"
            );
        }
        assert_eq!(h.errors.len(), 1, "{:?}", h.errors);
        assert!(
            h.errors[0].starts_with("Family photos hidden: can't read the family log "),
            "{:?}",
            h.errors
        );
    }
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let gui_index = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(&f.library, work.path(), gui_index.path(), home.path());
    let found = ready_found(&addr);
    let want = format!(
        "Family photos hidden: can't read the family log {}",
        f.library.join("members").display()
    );
    println!("M7 family-log note: {}", found["hide_note"]);
    assert_eq!(found["hide_note"].as_str(), Some(want.as_str()));
}

#[test]
fn the_hide_note_follows_a_hide_file_on_the_next_poll() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for name in ["IMG_0001.JPG", "IMG_0002.JPG"] {
        fs::write(library.path().join(name), b"").unwrap();
    }
    let tomb = library.path().join("IMG_0002.JPG.albumen-tombstone");
    fs::write(&tomb, br#"{"hidden":true}"#).unwrap();
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    assert!(ready_found(&addr)["hide_note"].is_null());
    fs::write(&tomb, b"{").unwrap();
    let broken = found_page(&addr);
    let note = broken["hide_note"].as_str().unwrap_or_default().to_string();
    println!("M7 hide-note after break: {note}");
    assert!(note.contains("IMG_0002.JPG.albumen-tombstone"), "{broken}");
    fs::write(&tomb, br#"{"hidden":true}"#).unwrap();
    let fixed = found_page(&addr);
    println!("M7 hide-note after fix: {}", fixed["hide_note"]);
    assert!(fixed["hide_note"].is_null(), "{fixed}");
}

enum Break {
    Corrupt,
    BadSignature,
    Mismatch,
    Delete,
}

/// bea hides one then two; break bea's entry for "hide one" so the view would
/// drop the later "hide two" too. Hides must fail closed for every viewer.
fn break_entry_before_a_later_hide(how: Break) {
    let tmp = tempfile::tempdir().unwrap();
    let f = family(tmp.path());
    let mut objects = Vec::new();
    let mut shas = Vec::new();
    for name in ["one.jpg", "two.jpg", "three.jpg"] {
        let src = tmp.path().join(name);
        fs::write(&src, format!("synthetic-m7-badentry-{name}")).unwrap();
        let (r, sha) = family_add(&f, &src);
        objects.push(r);
        shas.push(sha);
    }
    family_mark(&f, "hide", &shas[0]);
    family_mark(&f, "hide", &shas[1]);
    let index = tempfile::tempdir().unwrap();
    let v = visible(&f.library, index.path(), Some(&f.member_b));
    assert!(
        !v.contains(&objects[1]) && v.contains(&objects[2]),
        "before: {v:?}"
    );
    let mut files = Vec::new();
    files_under(&f.library.join("members").join(&f.member_b), &mut files);
    let mut logs: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| p.parent().is_some_and(|d| d.ends_with("log")))
        .collect();
    logs.sort_by_key(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
    });
    let bad = logs[logs.len() - 2].clone();
    let last = logs[logs.len() - 1].clone();
    // Synthetic temp fixture only.
    let prefix = match how {
        Break::Corrupt => {
            fs::write(&bad, b"{ not a signed entry").unwrap();
            "Family photos hidden: a family log entry can't be read: "
        }
        Break::BadSignature => {
            let mut v: serde_json::Value =
                serde_json::from_slice(&fs::read(&bad).unwrap()).unwrap();
            let sig = v["signature"].as_str().unwrap().to_string();
            let flip = if sig.ends_with('0') { '1' } else { '0' };
            v["signature"] = format!("{}{flip}", &sig[..sig.len() - 1]).into();
            fs::write(&bad, serde_json::to_vec(&v).unwrap()).unwrap();
            "Family photos hidden: a family log entry can't be read: "
        }
        Break::Mismatch => {
            fs::copy(&last, &bad).unwrap();
            "Family photos hidden: a family log entry can't be read: "
        }
        Break::Delete => {
            fs::remove_file(&bad).unwrap();
            "Family photos hidden: a family log entry is missing: "
        }
    };
    let want = format!("{prefix}{}", bad.display());
    for viewer in [Some(f.member_a.as_str()), Some(f.member_b.as_str()), None] {
        let h = hidden_paths(&f.library, &objects, viewer);
        for o in &objects {
            assert!(
                h.paths.contains(o),
                "{viewer:?} sees {o} with a broken log entry"
            );
        }
        assert_eq!(h.errors, std::slice::from_ref(&want), "{viewer:?}");
    }
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let gui_index = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(&f.library, work.path(), gui_index.path(), home.path());
    let found = ready_found(&addr);
    println!("M7 broken-entry note: {}", found["hide_note"]);
    assert_eq!(found["hide_note"].as_str(), Some(want.as_str()));
}

#[test]
fn a_bad_family_log_entry_before_a_later_hide_hides_every_family_object() {
    break_entry_before_a_later_hide(Break::Corrupt);
}

#[test]
fn a_missing_family_log_entry_before_a_later_hide_hides_every_family_object() {
    break_entry_before_a_later_hide(Break::Delete);
}

#[test]
fn a_family_log_entry_with_a_bad_signature_hides_every_family_object() {
    break_entry_before_a_later_hide(Break::BadSignature);
}

#[test]
fn a_family_log_entry_out_of_sequence_hides_every_family_object() {
    break_entry_before_a_later_hide(Break::Mismatch);
}

#[test]
fn a_filled_hole_clears_the_note_and_only_real_hides_stay_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let f = family(tmp.path());
    let mut objects = Vec::new();
    let mut shas = Vec::new();
    for name in ["one.jpg", "two.jpg", "three.jpg", "four.jpg"] {
        let src = tmp.path().join(name);
        fs::write(&src, format!("synthetic-m7-recover-{name}")).unwrap();
        let (r, sha) = family_add(&f, &src);
        objects.push(r);
        shas.push(sha);
    }
    // bea: hide one, hide two (the later hide), tombstone four (everyone).
    family_mark(&f, "hide", &shas[0]);
    family_mark(&f, "hide", &shas[1]);
    family_mark(&f, "tombstone", &shas[3]);
    let real = |viewer: Option<&str>| -> BTreeSet<String> {
        let mut want = BTreeSet::from([objects[3].clone()]);
        if viewer == Some(f.member_b.as_str()) {
            want.insert(objects[0].clone());
            want.insert(objects[1].clone());
        }
        want
    };
    let viewers = [Some(f.member_a.as_str()), Some(f.member_b.as_str()), None];
    for viewer in viewers {
        let h = hidden_paths(&f.library, &objects, viewer);
        assert!(h.errors.is_empty(), "{viewer:?}: {:?}", h.errors);
        assert_eq!(
            h.paths.iter().cloned().collect::<BTreeSet<String>>(),
            real(viewer),
            "{viewer:?} before the hole"
        );
    }
    // Hole: bea's "hide one" entry goes missing while later entries exist.
    let mut files = Vec::new();
    files_under(&f.library.join("members").join(&f.member_b), &mut files);
    let mut logs: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| p.parent().is_some_and(|d| d.ends_with("log")))
        .collect();
    logs.sort_by_key(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
    });
    let hole = logs[logs.len() - 2].clone();
    let saved = fs::read(&hole).unwrap();
    fs::remove_file(&hole).unwrap();
    let want_note = format!(
        "Family photos hidden: a family log entry is missing: {}",
        hole.display()
    );
    for viewer in viewers {
        let h = hidden_paths(&f.library, &objects, viewer);
        let all: BTreeSet<String> = objects.iter().cloned().collect();
        assert_eq!(
            h.paths.iter().cloned().collect::<BTreeSet<String>>(),
            all,
            "{viewer:?} with the hole"
        );
        assert_eq!(h.errors, std::slice::from_ref(&want_note), "{viewer:?}");
    }
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let gui_index = tempfile::tempdir().unwrap();
    let (gui, addr) = start(&f.library, work.path(), gui_index.path(), home.path());
    let found = ready_found(&addr);
    assert_eq!(
        found["hide_note"].as_str(),
        Some(want_note.as_str()),
        "{found}"
    );
    // Fill the hole (same bytes back; synthetic temp fixture).
    fs::write(&hole, &saved).unwrap();
    for viewer in viewers {
        let h = hidden_paths(&f.library, &objects, viewer);
        assert!(
            h.errors.is_empty(),
            "{viewer:?} after refill: {:?}",
            h.errors
        );
        assert_eq!(
            h.paths,
            real(viewer),
            "{viewer:?} after refill: a fail-closed hide stuck"
        );
    }
    // Next poll on the same GUI: the line clears.
    let after = found_page(&addr);
    println!(
        "M7 recover note with hole: {want_note}; after refill: {}",
        after["hide_note"]
    );
    assert!(
        after["hide_note"].is_null() || after["hide_note"] == "",
        "note stuck after refill: {after}"
    );
    drop(gui);
    // Next listing: a fresh GUI on the same files has no line, and the index
    // listing (scan, then rescan from the saved index) shows every family
    // photo the files don't hide for that viewer. The GUI grid itself lists
    // no family objects (extensionless sha256 names), so the listing check
    // uses the index path hides are applied on.
    let (_gui2, addr2) = start(&f.library, work.path(), gui_index.path(), home.path());
    let relisted = ready_found(&addr2);
    assert!(
        relisted["hide_note"].is_null() || relisted["hide_note"] == "",
        "note stuck at the next listing: {relisted}"
    );
    let index = tempfile::tempdir().unwrap();
    for pass in ["scan", "rescan"] {
        for viewer in viewers {
            let v = visible(&f.library, index.path(), viewer);
            for o in &objects {
                assert_eq!(
                    v.contains(o),
                    !real(viewer).contains(o),
                    "{pass} {viewer:?}: {o} after the hole was filled"
                );
            }
        }
    }
    println!("M7 recover listing ok for ada, bea, none");
}

/// M7 follow-up: a hide made after the first listing applies on the very
/// next GUI request, not only at the next listing. A new receiver tombstone
/// (beside a photo in a subfolder and at the root) is written after the
/// grid is ready; the next `/api/library` must not list the photo and
/// `/img` must not serve it. Synthetic files only; originals bit-exact.
#[test]
fn a_new_hide_applies_on_the_next_gui_request() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(library.path().join("sub")).unwrap();
    let names = ["IMG_0001.JPG", "IMG_0002.JPG", "sub/IMG_0003.JPG"];
    for name in names {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-next-{name}"),
        )
        .unwrap();
    }
    let originals: Vec<PathBuf> = names.iter().map(|n| library.path().join(n)).collect();
    let before = kept(&originals);
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    let images = |addr: &str| -> Vec<String> {
        let (code, body) = get(addr, "/api/library");
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ready", "{v}");
        let mut p: Vec<String> = v["images"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap().to_string())
            .collect();
        p.sort();
        assert_eq!(v["total_images"].as_u64(), Some(p.len() as u64), "{v}");
        p
    };
    ready_found(&addr);
    assert_eq!(images(&addr), names);
    assert_eq!(get(&addr, "/img?p=IMG_0002.JPG").0, 200);

    for (k, hide) in ["IMG_0002.JPG", "sub/IMG_0003.JPG"].iter().enumerate() {
        fs::write(
            library.path().join(format!("{hide}.albumen-tombstone")),
            br#"{"hidden":true,"reason":"hide"}"#,
        )
        .unwrap();
        // The very next request, no listing in between.
        let shown = images(&addr);
        println!("M7 next-request after hide {k}: {shown:?}");
        assert!(
            !shown.contains(&hide.to_string()),
            "next request still lists {hide}: {shown:?}"
        );
        let (code, _) = get(&addr, &format!("/img?p={hide}"));
        println!("M7 next-request /img {hide}: {code}");
        assert_eq!(code, 404, "next request still serves {hide}");
    }
    assert_eq!(images(&addr), ["IMG_0001.JPG"]);
    assert_eq!(get(&addr, "/img?p=IMG_0001.JPG").0, 200);
    assert_eq!(kept(&originals), before, "an original changed");
}

fn listed(addr: &str) -> Vec<String> {
    let (code, body) = get(addr, "/api/library");
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "ready", "{v}");
    let mut p: Vec<String> = v["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect();
    p.sort();
    p
}

/// /api/library's paths in the order served (not sorted), for the order
/// checks (XO 04:41Z (6)).
fn listed_raw(addr: &str) -> Vec<String> {
    let (code, body) = get(addr, "/api/library");
    parse_listed(code, &body)
}

/// The photo paths of an `/api/library` answer, in order.
fn parse_listed(code: u16, body: &str) -> Vec<String> {
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["status"], "ready", "{v}");
    v["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect()
}

/// Sets a file's or folder's mtime, as `rsync -t` would. Windows opens a
/// folder only with FILE_FLAG_BACKUP_SEMANTICS, and set_times needs write
/// access there.
fn set_mtime(path: &Path, t: SystemTime) {
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    };
    #[cfg(not(windows))]
    let f = fs::File::open(path).unwrap();
    f.set_times(fs::FileTimes::new().set_modified(t)).unwrap();
}

/// M7 mtime race (XO 00:18Z): a tombstone written in the same mtime tick as
/// the GUI's last hide check (made deterministic by putting the folder's
/// mtime back to what that check saw) must still reach the next request.
#[test]
fn a_hide_in_the_same_mtime_tick_still_applies_on_the_next_request() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sub = library.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for name in ["IMG_0001.JPG", "sub/IMG_0003.JPG"] {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-tick-{name}"),
        )
        .unwrap();
    }
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    assert_eq!(listed(&addr), ["IMG_0001.JPG", "sub/IMG_0003.JPG"]);
    let tick = fs::metadata(&sub).unwrap().modified().unwrap();
    fs::write(
        sub.join("IMG_0003.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    set_mtime(&sub, tick);
    let shown = listed(&addr);
    println!("M7 same-tick next request: {shown:?}");
    assert_eq!(shown, ["IMG_0001.JPG"], "same-tick hide missed");
    assert_eq!(get(&addr, "/img?p=sub/IMG_0003.JPG").0, 404);
}

/// A hidden video is never served, and a hide file beside it that can't be
/// read hides it (fail closed) and says so in the quiet line. Same for /img.
#[test]
fn hidden_and_unreadable_hides_404_video_and_img_with_a_note() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let vids = library.path().join("vids");
    fs::create_dir(&vids).unwrap();
    fs::write(library.path().join("IMG_0001.JPG"), b"synthetic-m7-vid-a").unwrap();
    fs::write(library.path().join("IMG_0002.JPG"), b"synthetic-m7-vid-b").unwrap();
    fs::write(vids.join("clip.MOV"), b"synthetic-m7-video").unwrap();
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    assert!(ready_found(&addr)["hide_note"].is_null());
    assert_eq!(get(&addr, "/video?p=vids/clip.MOV").0, 200);
    let tomb = vids.join("clip.MOV.albumen-tombstone");
    fs::write(&tomb, br#"{"hidden":true}"#).unwrap();
    let (code, _) = get(&addr, "/video?p=vids/clip.MOV");
    println!("M7 hidden video: {code}");
    assert_eq!(code, 404, "hidden video served");
    fs::write(&tomb, b"{").unwrap();
    let (code, _) = get(&addr, "/video?p=vids/clip.MOV");
    let note = found_page(&addr)["hide_note"].clone();
    println!("M7 unreadable video hide: {code} note={note}");
    assert_eq!(code, 404, "video with an unreadable hide file served");
    assert!(
        note.as_str()
            .unwrap_or_default()
            .contains("clip.MOV.albumen-tombstone"),
        "{note}"
    );
    fs::write(library.path().join("IMG_0002.JPG.albumen-tombstone"), b"{").unwrap();
    let (code, _) = get(&addr, "/img?p=IMG_0002.JPG");
    let note = found_page(&addr)["hide_note"].clone();
    println!("M7 unreadable img hide: {code} note={note}");
    assert_eq!(code, 404);
    assert!(
        note.as_str()
            .unwrap_or_default()
            .contains("IMG_0002.JPG.albumen-tombstone"),
        "{note}"
    );
}

/// A8 (XO 00:25Z): `rsync -t`/`-a` brings a hide in and puts the folder's
/// old mtime back. With folders last changed long ago (so the 2 s re-check
/// does not apply), a synced-in tombstone plus the folder mtime set back to
/// exactly what the GUI stamped must still hide the photo on the next request.
#[test]
fn an_rsync_style_hide_with_the_old_folder_mtime_restored_still_applies() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sub = library.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for name in ["sub/IMG_0001.JPG", "sub/IMG_0003.JPG"] {
        fs::write(library.path().join(name), format!("synthetic-m7-a8-{name}")).unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(3600);
    set_mtime(&sub, old);
    set_mtime(library.path(), old);
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    assert_eq!(listed(&addr), ["sub/IMG_0001.JPG", "sub/IMG_0003.JPG"]);
    let stamped = fs::metadata(&sub).unwrap().modified().unwrap();
    assert_eq!(stamped, old);
    fs::write(
        sub.join("IMG_0003.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    set_mtime(&sub, stamped);
    assert_eq!(fs::metadata(&sub).unwrap().modified().unwrap(), stamped);
    let shown = listed(&addr);
    println!("M7 A8 rsync-style next request: {shown:?}");
    assert_eq!(shown, ["sub/IMG_0001.JPG"], "rsync-style hide missed");
    assert_eq!(get(&addr, "/img?p=sub/IMG_0003.JPG").0, 404);
}

/// A tombstone rewritten in place at the same size (hidden false -> true),
/// with its own and its folder's old mtimes set back, as `rsync -a` can,
/// must still hide the photo on the next request, on every OS.
#[test]
fn a_same_size_tombstone_swap_with_old_mtimes_restored_still_applies() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sub = library.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for name in ["sub/IMG_0001.JPG", "sub/IMG_0003.JPG"] {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-swap-{name}"),
        )
        .unwrap();
    }
    let tomb = sub.join("IMG_0003.JPG.albumen-tombstone");
    let shown_body: &[u8] = br#"{"hidden":false,"reason":"hide"}"#;
    let hidden_body: &[u8] = br#"{"hidden":true ,"reason":"hide"}"#;
    assert_eq!(shown_body.len(), hidden_body.len());
    fs::write(&tomb, shown_body).unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for p in [tomb.as_path(), sub.as_path(), library.path()] {
        set_mtime(p, old);
    }
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    assert_eq!(listed(&addr), ["sub/IMG_0001.JPG", "sub/IMG_0003.JPG"]);
    fs::write(&tomb, hidden_body).unwrap();
    set_mtime(&tomb, old);
    set_mtime(&sub, old);
    assert_eq!(fs::metadata(&tomb).unwrap().len(), shown_body.len() as u64);
    let shown = listed(&addr);
    println!("M7 same-size swap next request: {shown:?}");
    assert_eq!(
        shown,
        ["sub/IMG_0001.JPG"],
        "same-size tombstone swap missed"
    );
    assert_eq!(get(&addr, "/img?p=sub/IMG_0003.JPG").0, 404);
}

#[test]
fn an_unchanged_request_reads_no_tombstone() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sub = library.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for name in [
        "sub/IMG_0001.JPG",
        "sub/IMG_0002.JPG",
        "sub/IMG_0003.JPG",
        "IMG_0004.JPG",
    ] {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-reads-{name}"),
        )
        .unwrap();
    }
    let hiding = sub.join("IMG_0002.JPG.albumen-tombstone");
    let showing = sub.join("IMG_0003.JPG.albumen-tombstone");
    fs::write(&hiding, br#"{"hidden":true,"reason":"hide"}"#).unwrap();
    fs::write(&showing, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for p in [
        hiding.as_path(),
        showing.as_path(),
        sub.as_path(),
        library.path(),
    ] {
        set_mtime(p, old);
    }
    // Setting the mtimes back changed every ctime to now, and the window
    // is measured from the later of the two (XO 02:53Z): let it pass.
    std::thread::sleep(Duration::from_millis(2100));
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    assert_eq!(
        listed(&addr),
        ["IMG_0004.JPG", "sub/IMG_0001.JPG", "sub/IMG_0003.JPG"]
    );
    let reads = |addr: &str| found_page(addr)["hide_tombstone_reads"].as_u64().unwrap();
    let before = reads(&addr);
    assert!(before >= 2, "counter never moved: {before}");
    for _ in 0..3 {
        assert_eq!(
            listed(&addr),
            ["IMG_0004.JPG", "sub/IMG_0001.JPG", "sub/IMG_0003.JPG"]
        );
        get(&addr, "/img?p=sub/IMG_0001.JPG");
        assert_eq!(get(&addr, "/img?p=sub/IMG_0002.JPG").0, 404);
    }
    let after = reads(&addr);
    println!(
        "M7 tombstone reads over 3 unchanged listings + 6 /img: {}",
        after - before
    );
    // With the portable key the change key of a non-hiding tombstone is a
    // content hash, which the counter counts (one per request); with ctime
    // (Unix) or ChangeTime (Windows) it is a stat.
    if found_page(&addr)["hide_key"] != "portable" {
        assert_eq!(after, before, "an unchanged request read tombstones");
    }
    // The counter is live: a new hide is read and applies on the next request.
    fs::write(
        sub.join("IMG_0001.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    assert_eq!(listed(&addr), ["IMG_0004.JPG", "sub/IMG_0003.JPG"]);
    assert!(reads(&addr) > after);
}

/// Whether the ALBUMEN_TEST_RELIST_* hooks are honoured by this build.
/// XO 05:06Z (a), PL push gate D1: the cap on each piece of inline hide work
/// in a request (keep_shown, the hidden/withheld set build): 40 ms at up to
/// 50k files, and 100 ms (inside the request max) above that.
const INLINE_CAP_MS: f64 = 40.0;
const INLINE_CAP_LARGE_MS: f64 = 100.0;
/// Library re-walks in the no-flicker run (exact): the first walk plus one
/// per grown listing (1 release + 20 syncs), XO 05:41Z (3).
const MAX_REWALKS: u64 = 22;

const HOOKS: bool = cfg!(any(debug_assertions, feature = "test-hooks"));

/// Wait until no folder is pending (every background re-list applied), so
/// counters read after it are final (XO 03:36Z (7): no race with a worker).
fn settled(addr: &str) -> serde_json::Value {
    let began = Instant::now();
    loop {
        let f = found_page(addr);
        if f["hide_pending_dirs"].as_u64() == Some(0) {
            return f;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "still pending: {f}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// XO 03:22Z (1): across a sync only the changed folder is listed again,
/// a new hide is hidden on the first request, a recreated folder is listed
/// again, and unchanged folders are listed 0 times. XO 03:36Z (7): each
/// count is read only once no folder is pending, and (with the test hooks)
/// every re-list is held 300 ms so the changed folder is really pending
/// while the first request is answered.
#[test]
fn only_changed_folders_are_listed_across_a_sync() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let write_dir = |d: &str| {
        let dir = library.path().join(d);
        fs::create_dir_all(&dir).unwrap();
        for n in 1..=2 {
            let p = dir.join(format!("IMG_000{n}.JPG"));
            fs::write(&p, format!("synthetic-m7-folders-{d}-{n}")).unwrap();
            set_mtime(&p, old);
        }
        set_mtime(&dir, old);
    };
    for d in ["a", "b", "c"] {
        write_dir(d);
    }
    set_mtime(library.path(), old);
    // Let every ctime (changed by the writes above) leave the window.
    std::thread::sleep(Duration::from_millis(2100));
    let (_gui, addr, _) = start_env(
        library.path(),
        work.path(),
        index.path(),
        home.path(),
        &[("ALBUMEN_TEST_RELIST_DELAY_MS", "300")],
    );
    ready_found(&addr);
    let all = [
        "a/IMG_0001.JPG",
        "a/IMG_0002.JPG",
        "b/IMG_0001.JPG",
        "b/IMG_0002.JPG",
        "c/IMG_0001.JPG",
        "c/IMG_0002.JPG",
    ];
    assert_eq!(listed(&addr), all);
    let dirs = |addr: &str| found_page(addr)["hide_dirs_listed"].as_u64().unwrap();
    // Settle: a folder first listed inside its window is listed once more.
    std::thread::sleep(Duration::from_millis(2100));
    listed(&addr);
    settled(&addr);
    let before = dirs(&addr);
    for _ in 0..3 {
        assert_eq!(listed(&addr), all);
    }
    settled(&addr);
    assert_eq!(dirs(&addr) - before, 0, "an unchanged folder was listed");
    // Sync: a hide lands in b only.
    fs::write(
        library.path().join("b/IMG_0002.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    let before = dirs(&addr);
    let hid_b = [
        "a/IMG_0001.JPG",
        "a/IMG_0002.JPG",
        "b/IMG_0001.JPG",
        "c/IMG_0001.JPG",
        "c/IMG_0002.JPG",
    ];
    assert_eq!(
        listed(&addr),
        hid_b,
        "the new hide was not hidden on the first request"
    );
    let pending = found_page(&addr)["hide_pending_dirs"].as_u64().unwrap();
    if HOOKS {
        assert_eq!(pending, 1, "b was not pending on the first request");
    }
    settled(&addr);
    let first = dirs(&addr) - before;
    // Wait out b's window: b may be listed once more, a and c never.
    std::thread::sleep(Duration::from_millis(2100));
    let before = dirs(&addr);
    assert_eq!(listed(&addr), hid_b);
    settled(&addr);
    let settle = dirs(&addr) - before;
    let before = dirs(&addr);
    for _ in 0..3 {
        assert_eq!(listed(&addr), hid_b);
    }
    settled(&addr);
    let steady = dirs(&addr) - before;
    println!(
        "M7 FOLDERS_LISTED sync_first={first} after_window={settle} unchanged={steady} pending_at_first={pending}"
    );
    assert_eq!(
        first, 1,
        "only the changed folder is listed on the first request"
    );
    assert!(settle <= 1, "unchanged folders were listed: {settle}");
    assert_eq!(steady, 0, "unchanged folders were listed");
    // Recreate c (same names; old mtimes put back where the change key is
    // the ctime, so only the key can tell) with a hide in it.
    let key = found_page(&addr)["hide_key"]
        .as_str()
        .unwrap_or("")
        .to_string();
    fs::remove_dir_all(library.path().join("c")).unwrap();
    write_dir("c");
    let tomb = library.path().join("c/IMG_0001.JPG.albumen-tombstone");
    fs::write(&tomb, br#"{"hidden":true,"reason":"hide"}"#).unwrap();
    if key != "portable" {
        set_mtime(&tomb, old);
        set_mtime(&library.path().join("c"), old);
    }
    let before = dirs(&addr);
    assert_eq!(
        listed(&addr),
        [
            "a/IMG_0001.JPG",
            "a/IMG_0002.JPG",
            "b/IMG_0001.JPG",
            "c/IMG_0002.JPG"
        ],
        "the recreated folder was not listed again"
    );
    settled(&addr);
    let recreated = dirs(&addr) - before;
    println!("M7 FOLDERS_LISTED recreated={recreated} key={key}");
    assert_eq!(recreated, 1, "only the recreated folder is listed");
}

/// XO 03:36Z (4): a folder held pending (each re-list delayed 1.5 s by
/// the test hook) hides no photo that was on screen and whose own
/// tombstone is absent or unchanged; only the photo whose tombstone is
/// new or changed is withheld, and it is back, following the page's own
/// poll, within 1 s of its folder being confirmed.
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn a_pending_folder_hides_no_visible_photo() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for d in ["a", "b"] {
        let dir = library.path().join(d);
        fs::create_dir_all(&dir).unwrap();
        for n in 1..=4 {
            let p = dir.join(format!("IMG_000{n}.JPG"));
            fs::write(&p, format!("synthetic-m7-pending-{d}-{n}")).unwrap();
            set_mtime(&p, old);
        }
    }
    // a/2 and a/4: a tombstone that says show (a/2 stays unchanged).
    for n in [2, 4] {
        fs::write(
            library
                .path()
                .join(format!("a/IMG_000{n}.JPG.albumen-tombstone")),
            br#"{"hidden":false,"reason":"unhide"}"#,
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(2100));
    let (_gui, addr, _) = start_env(
        library.path(),
        work.path(),
        index.path(),
        home.path(),
        &[("ALBUMEN_TEST_RELIST_DELAY_MS", "1500")],
    );
    ready_found(&addr);
    let all: Vec<String> = ["a", "b"]
        .iter()
        .flat_map(|d| (1..=4).map(move |n| format!("{d}/IMG_000{n}.JPG")))
        .collect();
    assert_eq!(listed(&addr), all);
    settled(&addr);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(listed(&addr), all);
    settled(&addr);
    // Sync into a: a new hide on a/3, a/4's show tombstone rewritten, a
    // file that is not a photo added.
    fs::write(
        library.path().join("a/IMG_0003.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    fs::write(
        library.path().join("a/IMG_0004.JPG.albumen-tombstone"),
        br#"{"hidden":false,"reason":"unhide again"}"#,
    )
    .unwrap();
    fs::write(library.path().join("a/notes.txt"), b"synthetic").unwrap();
    // Out of the same-tick window (2 s where it is coarse), so a is listed
    // once: what is measured is the pending time, not a second re-list.
    std::thread::sleep(Duration::from_millis(2100));
    let t = Instant::now();
    let first = listed(&addr);
    let f = found_page(&addr);
    assert_eq!(f["hide_pending_dirs"], 1, "a was not pending: {f}");
    let must = [
        "a/IMG_0001.JPG",
        "a/IMG_0002.JPG",
        "b/IMG_0001.JPG",
        "b/IMG_0004.JPG",
    ];
    for p in must {
        assert!(first.contains(&p.to_string()), "{p} blinked out: {first:?}");
    }
    assert!(
        !first.contains(&"a/IMG_0003.JPG".to_string()),
        "the new hide showed"
    );
    assert!(
        !first.contains(&"a/IMG_0004.JPG".to_string()),
        "a changed tombstone was trusted"
    );
    // While a is pending: every view still has them.
    let mut views = 1;
    let mut ver = f["hide_version"].as_u64().unwrap();
    let mut back = None;
    while back.is_none() {
        assert!(t.elapsed() < Duration::from_secs(10), "a/4 never came back");
        std::thread::sleep(Duration::from_millis(200));
        let f = found_page(&addr);
        let v = f["hide_version"].as_u64().unwrap();
        if v == ver && f["hide_pending_dirs"].as_u64() != Some(0) {
            let now = listed(&addr);
            views += 1;
            for p in must {
                assert!(
                    now.contains(&p.to_string()),
                    "{p} blinked out while a was pending"
                );
            }
            continue;
        }
        ver = v;
        let now = listed(&addr);
        views += 1;
        for p in must {
            assert!(now.contains(&p.to_string()), "{p} blinked out");
        }
        if now.contains(&"a/IMG_0004.JPG".to_string()) {
            back = Some(t.elapsed());
        }
    }
    let back = back.unwrap().as_secs_f64() * 1000.0;
    let f = settled(&addr);
    let confirmed = f["hide_withheld_last_ms"].as_f64().unwrap();
    println!(
        "M7 PENDING_KEEP views={views} back_after_ms={back:.0} withheld_ms={confirmed:.0} keep_stats={}",
        f["hide_keep_stats"]
    );
    assert!(
        back - confirmed <= 1000.0 && back <= 2500.0,
        "a/4 came back {back:.0} ms after the sync, {confirmed:.0} ms of it pending"
    );
    let mut want = all.clone();
    want.retain(|p| p != "a/IMG_0003.JPG");
    assert_eq!(listed(&addr), want);
}

/// XO 08:24Z (b): a hide that arrives while its folder is being listed in
/// the background (the walk thread's pre-filter after a sync started that
/// listing; held at ALBUMEN_TEST_RELIST_GATE, certain rather than timed)
/// applies on the very next request (A8, fail closed), and stays applied
/// once the stale listing lands.
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn a_hide_during_a_pending_listing_applies_on_the_next_request() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let dir = library.path().join("a");
    fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        let p = dir.join(format!("IMG_000{n}.JPG"));
        fs::write(&p, format!("synthetic-m7-prefilter-{n}")).unwrap();
        set_mtime(&p, old);
    }
    // a/2: a show tombstone, unchanged throughout (must not blink).
    fs::write(
        dir.join("IMG_0002.JPG.albumen-tombstone"),
        br#"{"hidden":false,"reason":"unhide"}"#,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let (_gui, addr, _) = start_env(
        library.path(),
        work.path(),
        index.path(),
        home.path(),
        &[("ALBUMEN_TEST_RELIST_GATE", "a")],
    );
    ready_found(&addr);
    let all = ["a/IMG_0001.JPG", "a/IMG_0002.JPG", "a/IMG_0003.JPG"];
    assert_eq!(listed(&addr), all);
    settled(&addr);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(listed(&addr), all);
    settled(&addr);
    // A sync adds a photo: the next request hands a to a worker, held at
    // the gate (its listing has not seen the hide below).
    let gate = library.path().join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    fs::write(dir.join("IMG_0004.JPG"), b"synthetic-m7-prefilter-4").unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let _ = listed(&addr);
    assert_eq!(
        found_page(&addr)["hide_pending_dirs"],
        1,
        "a was not pending"
    );
    // The hide arrives while a's listing is held.
    fs::write(
        dir.join("IMG_0001.JPG.albumen-tombstone"),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    let next = listed(&addr);
    println!("M7 A8 hide during pending listing: {next:?}");
    assert!(
        !next.contains(&"a/IMG_0001.JPG".to_string()),
        "the hide showed while a was pending: {next:?}"
    );
    for p in ["a/IMG_0002.JPG", "a/IMG_0003.JPG"] {
        assert!(next.contains(&p.to_string()), "{p} blinked out: {next:?}");
    }
    // The stale listing lands; the hide still applies after it.
    fs::remove_file(&gate).unwrap();
    let t = Instant::now();
    loop {
        let now = listed(&addr);
        assert!(
            !now.contains(&"a/IMG_0001.JPG".to_string()),
            "the hide showed after the stale listing: {now:?}"
        );
        if found_page(&addr)["hide_pending_dirs"] == 0
            && now.contains(&"a/IMG_0004.JPG".to_string())
        {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(15), "a never settled");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!listed(&addr).contains(&"a/IMG_0001.JPG".to_string()));
}

/// XO FRAGO 08:40Z D2 (A8): like the test above, but the hide is an
/// existing show tombstone rewritten in place while its folder is pending.
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn a_show_rewritten_in_place_to_hidden_during_a_pending_listing_applies_on_the_next_request() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let dir = library.path().join("a");
    fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        let p = dir.join(format!("IMG_000{n}.JPG"));
        fs::write(&p, format!("synthetic-m7-prefilter-{n}")).unwrap();
        set_mtime(&p, old);
    }
    // a/2: a show tombstone, later rewritten in place to hidden.
    fs::write(
        dir.join("IMG_0002.JPG.albumen-tombstone"),
        br#"{"hidden":false,"reason":"unhide"}"#,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let (_gui, addr, _) = start_env(
        library.path(),
        work.path(),
        index.path(),
        home.path(),
        &[("ALBUMEN_TEST_RELIST_GATE", "a")],
    );
    ready_found(&addr);
    let all = ["a/IMG_0001.JPG", "a/IMG_0002.JPG", "a/IMG_0003.JPG"];
    assert_eq!(listed(&addr), all);
    settled(&addr);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(listed(&addr), all);
    settled(&addr);
    // A sync adds a photo: the next request hands a to a worker, held at
    // the gate (its listing has not seen the hide below).
    let gate = library.path().join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    fs::write(dir.join("IMG_0004.JPG"), b"synthetic-m7-prefilter-4").unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let _ = listed(&addr);
    assert_eq!(
        found_page(&addr)["hide_pending_dirs"],
        1,
        "a was not pending"
    );
    // The hide arrives while a's listing is held.
    // XO FRAGO 08:40Z D2: the show tombstone of a/2 is rewritten in
    // place (same inode, no rename), so the folder's stamp does not move.
    let before = fs::metadata(&dir).unwrap().modified().unwrap();
    {
        use std::io::Write;
        let mut t = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(dir.join("IMG_0002.JPG.albumen-tombstone"))
            .unwrap();
        t.write_all(br#"{"hidden":true,"reason":"hide"}"#).unwrap();
    }
    println!(
        "M7 A8 in-place: folder stamp moved = {}",
        fs::metadata(&dir).unwrap().modified().unwrap() != before
    );
    let next = listed(&addr);
    println!("M7 A8 in-place hide during pending listing: {next:?}");
    assert!(
        !next.contains(&"a/IMG_0002.JPG".to_string()),
        "the hide showed while a was pending: {next:?}"
    );
    for p in ["a/IMG_0001.JPG", "a/IMG_0003.JPG"] {
        assert!(next.contains(&p.to_string()), "{p} blinked out: {next:?}");
    }
    // The stale listing lands; the hide still applies after it.
    fs::remove_file(&gate).unwrap();
    let t = Instant::now();
    loop {
        let now = listed(&addr);
        assert!(
            !now.contains(&"a/IMG_0002.JPG".to_string()),
            "the hide showed after the stale listing: {now:?}"
        );
        if found_page(&addr)["hide_pending_dirs"] == 0
            && now.contains(&"a/IMG_0004.JPG".to_string())
        {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(15), "a never settled");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!listed(&addr).contains(&"a/IMG_0002.JPG".to_string()));
}

/// XO 03:36Z (5): a re-list worker that panics is caught and logged; its
/// folder's photos are hidden meanwhile (fail closed) and the folder is
/// listed again, so it never stays pending.
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn a_relist_worker_that_panics_is_retried() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for d in ["a", "b"] {
        fs::create_dir_all(library.path().join(d)).unwrap();
        fs::write(library.path().join(format!("{d}/IMG_0001.JPG")), d).unwrap();
    }
    let (_gui, addr, _) = start_env(
        library.path(),
        work.path(),
        index.path(),
        home.path(),
        &[("ALBUMEN_TEST_RELIST_PANIC", "a")],
    );
    ready_found(&addr);
    let began = Instant::now();
    loop {
        let now = listed(&addr);
        assert!(now.contains(&"b/IMG_0001.JPG".to_string()));
        if now.contains(&"a/IMG_0001.JPG".to_string()) {
            break;
        }
        assert!(began.elapsed() < Duration::from_secs(10), "a stayed hidden");
        std::thread::sleep(Duration::from_millis(50));
    }
    let f = settled(&addr);
    println!(
        "M7 WORKER_PANIC panics={} back_after_ms={}",
        f["hide_worker_panics"],
        began.elapsed().as_millis()
    );
    assert_eq!(f["hide_worker_panics"], 1, "{f}");
}

/// XO 02:30Z (b): the change key is the platform's own (ctime on Unix,
/// ChangeTime via GetFileInformationByHandleEx on Windows), it was taken,
/// and no key failed (no silent fallback). The portable key only when a
/// test forces it.
#[test]
fn the_change_key_is_the_platform_key_with_no_fallback() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(library.path().join("sub")).unwrap();
    for name in ["sub/IMG_0001.JPG", "sub/IMG_0002.JPG"] {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-key-{name}"),
        )
        .unwrap();
    }
    fs::write(
        library.path().join("sub/IMG_0002.JPG.albumen-tombstone"),
        br#"{"hidden":false,"reason":"hide"}"#,
    )
    .unwrap();
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    ready_found(&addr);
    listed(&addr);
    let f = found_page(&addr);
    println!(
        "M7 change key: key={} changetime_keys={} key_failures={} same_tick_ms={} fs={}",
        f["hide_key"].as_str().unwrap_or("?"),
        f["hide_changetime_keys"],
        f["hide_key_failures"],
        f["hide_same_tick_ms"],
        f["hide_fs"].as_str().unwrap_or("?")
    );
    let forced = std::env::var("ALBUMEN_TEST_HIDE_KEY").is_ok_and(|v| v == "portable");
    let want = if forced {
        "portable"
    } else if cfg!(windows) {
        "changetime"
    } else if cfg!(unix) {
        "ctime"
    } else {
        "portable"
    };
    assert_eq!(f["hide_key"], want, "{f}");
    assert_eq!(f["hide_key_failures"], 0, "a change key failed: {f}");
    if cfg!(windows) && !forced {
        assert!(
            f["hide_changetime_keys"].as_u64().unwrap() > 0,
            "the ChangeTime path was never used: {f}"
        );
    } else {
        assert_eq!(f["hide_changetime_keys"], 0, "{f}");
    }
}

/// XO 02:30Z (a): the 50 ms same-tick window only on a filesystem known to
/// keep a fine ctime; a forced unknown or network type (statfs f_type hook)
/// falls back to 2 s, and a tombstone written 0.5 s ago is then read again
/// on every poll until it is 2 s old.
///
/// Needs the ALBUMEN_TEST_FS_MAGIC hook: a debug build, or a release build
/// with `--features test-hooks` (as CI runs it); plain release ignores it.
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn an_unknown_or_network_filesystem_falls_back_to_the_2s_window() {
    let library = tempfile::tempdir().unwrap();
    fs::write(library.path().join("IMG_0001.JPG"), b"synthetic-m7-fs-1").unwrap();
    fs::write(library.path().join("IMG_0002.JPG"), b"synthetic-m7-fs-2").unwrap();
    let tomb = library.path().join("IMG_0002.JPG.albumen-tombstone");
    fs::write(&tomb, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
    // Default: the real filesystem's window.
    {
        let (work, index, home) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
        ready_found(&addr);
        listed(&addr);
        let f = found_page(&addr);
        println!(
            "M7 fs window default: fs={} same_tick_ms={}",
            f["hide_fs"], f["hide_same_tick_ms"]
        );
        let fine = ["xfs", "btrfs", "tmpfs", "f2fs", "zfs", "apfs"];
        let want = if fine.contains(&f["hide_fs"].as_str().unwrap_or_default()) {
            50
        } else {
            2000
        };
        assert_eq!(f["hide_same_tick_ms"], want, "{f}");
    }
    for (magic, name) in [
        ("0x6969", "nfs"),
        // ext2/3/4 share 0xEF53 and may store whole seconds: coarse.
        ("0xef53", "ext2/3/4"),
        ("0x65735546", "fuse"),
        ("0x4d44", "msdos"),
        ("0x2011bab0", "exfat"),
        ("0x482b", "hfsplus"),
        ("0x1234", "unknown 0x1234"),
    ] {
        let (work, index, home) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (_gui, addr, _) = start_env(
            library.path(),
            work.path(),
            index.path(),
            home.path(),
            &[("ALBUMEN_TEST_FS_MAGIC", magic)],
        );
        ready_found(&addr);
        set_mtime(&tomb, SystemTime::now() - Duration::from_millis(500));
        listed(&addr);
        let f = found_page(&addr);
        assert_eq!(f["hide_same_tick_ms"], 2000, "{magic}: {f}");
        assert_eq!(f["hide_fs"], name, "{f}");
        if magic != "0x6969" {
            continue;
        }
        // Behaviour, not only the number: within 2 s of its mtime the
        // tombstone is untrusted and read again on each request.
        let reads = |addr: &str| found_page(addr)["hide_tombstone_reads"].as_u64().unwrap();
        let a = reads(&addr);
        listed(&addr);
        let b = reads(&addr);
        println!("M7 fs window nfs: reads within 2 s {a} -> {b}");
        assert!(
            b > a,
            "a 0.5 s old tombstone was trusted under the 2 s fallback"
        );
    }
}

/// XO 03:09Z (1): on ext a whole-second ctime keeps the 2 s window. The
/// ALBUMEN_TEST_COARSE_CTIME hook drops the ctime's ns (as ext2/3 or a
/// 128-byte-inode ext4 would) and ALBUMEN_TEST_FS_MAGIC forces ext, so this
/// runs on any Unix filesystem. A non-hiding tombstone changed just now is
/// read again on every request until 2 s have passed, then no more; with
/// its ns ctime kept (the control) it is trusted after the 50 ms window.
#[cfg(unix)]
#[test]
#[cfg_attr(
    not(any(debug_assertions, feature = "test-hooks")),
    ignore = "needs a debug build or --features test-hooks"
)]
fn a_whole_second_ctime_on_ext_keeps_the_2s_window() {
    if std::env::var("ALBUMEN_TEST_HIDE_KEY").is_ok_and(|v| v == "portable") {
        println!("M7 coarse ctime: skipped, the portable key has no ctime");
        return;
    }
    for coarse in [true, false] {
        let library = tempfile::tempdir().unwrap();
        let (work, index, home) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        fs::write(library.path().join("IMG_0001.JPG"), b"synthetic-m7-cc-1").unwrap();
        fs::write(library.path().join("IMG_0002.JPG"), b"synthetic-m7-cc-2").unwrap();
        let tomb = library.path().join("IMG_0002.JPG.albumen-tombstone");
        fs::write(&tomb, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
        let mut env = vec![("ALBUMEN_TEST_FS_MAGIC", "0xef53")];
        if coarse {
            env.push(("ALBUMEN_TEST_COARSE_CTIME", "1"));
        }
        let (_gui, addr, _) =
            start_env(library.path(), work.path(), index.path(), home.path(), &env);
        ready_found(&addr);
        assert_eq!(found_page(&addr)["hide_fs"], "ext2/3/4");
        // ctime = now (a whole second under the hook), mtime 0.5 s ago.
        set_mtime(&tomb, SystemTime::now() - Duration::from_millis(500));
        let t0 = std::time::Instant::now();
        listed(&addr);
        std::thread::sleep(Duration::from_millis(100));
        // Re-stamped now, past the 50 ms window from its ctime.
        listed(&addr);
        let reads = |addr: &str| found_page(addr)["hide_recheck_reads"].as_u64().unwrap();
        let a = reads(&addr);
        listed(&addr);
        listed(&addr);
        let b = reads(&addr);
        assert!(
            t0.elapsed() < Duration::from_millis(900),
            "too slow to test"
        );
        println!("M7 coarse ctime={coarse}: reads after +0.1 s over 2 requests {a} -> {b}");
        if !coarse {
            assert_eq!(b, a, "an ns ctime 0.1 s old was not trusted (50 ms window)");
            continue;
        }
        assert!(
            b >= a + 2,
            "a whole-second ctime was trusted within 2 s: {a} -> {b}"
        );
        std::thread::sleep(Duration::from_millis(2100).saturating_sub(t0.elapsed()));
        listed(&addr);
        let c = reads(&addr);
        listed(&addr);
        listed(&addr);
        let d = reads(&addr);
        println!("M7 coarse ctime: reads after 2.1 s over 2 requests {c} -> {d}");
        assert_eq!(d, c, "still untrusted after the 2 s window");
    }
}

/// XO 02:30Z (c): `/api/found` re-reads only changed tombstones, but stamps
/// every watched one each poll, so a tombstone corrupted in place (same
/// size, mtime set back) is in the note on the very next poll.
#[test]
fn a_corrupted_tombstone_is_in_the_note_within_one_poll() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for name in ["IMG_0001.JPG", "IMG_0002.JPG", "IMG_0003.JPG"] {
        fs::write(
            library.path().join(name),
            format!("synthetic-m7-note-{name}"),
        )
        .unwrap();
    }
    let good = br#"{"hidden":true,"reason":"hide"}"#;
    let tomb = library.path().join("IMG_0002.JPG.albumen-tombstone");
    let other = library.path().join("IMG_0003.JPG.albumen-tombstone");
    fs::write(&tomb, good).unwrap();
    fs::write(&other, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    for p in [tomb.as_path(), other.as_path(), library.path()] {
        set_mtime(p, old);
    }
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    assert!(ready_found(&addr)["hide_note"].is_null());
    // Past the coarsest same-tick window (the stamps were just taken).
    std::thread::sleep(Duration::from_millis(2100));
    let warm = found_page(&addr);
    let quiet = found_page(&addr);
    let reads = |f: &serde_json::Value| f["hide_tombstone_reads"].as_u64().unwrap();
    if quiet["hide_key"] != "portable" {
        assert_eq!(
            reads(&quiet),
            reads(&warm),
            "an unchanged poll read tombstones"
        );
    }
    let mut bad = good.to_vec();
    bad[0] = b'#';
    fs::write(&tomb, &bad).unwrap();
    set_mtime(&tomb, old);
    let polled = found_page(&addr);
    let note = polled["hide_note"].as_str().unwrap_or_default().to_string();
    println!(
        "M7 corrupted-tombstone note after one poll: {note} (reads {} -> {})",
        reads(&quiet),
        reads(&polled)
    );
    assert!(note.contains("IMG_0002.JPG.albumen-tombstone"), "{polled}");
    assert_eq!(
        reads(&polled) - reads(&quiet),
        if polled["hide_key"] == "portable" {
            2 + 1
        } else {
            1
        },
        "only the changed tombstone is read again: {polled}"
    );
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1]
}

/// CI diagnostic, print-only (XO 00:37Z): the per-request hide re-check
/// time and the /img p95 at `ALBUMEN_M7_RECHECK_FILES` files with 0, 1k and
/// 10k tombstones (`ALBUMEN_M7_RECHECK_TOMBSTONES`, cumulative, default
/// "0,1000,10000"). Off unless the files variable is set. Nothing is
/// asserted on timing; box timing is not evidence.
///
/// `recheck_*`: one `/api/library?offset=0&limit=1` right after a folder
/// in the library changed, so the request re-reads the hide state.
/// `img_*`: `/img` of visible photos once the state is cached (2.5 s after
/// the last change, past the 2 s same-tick window).
#[test]
fn m7_recheck_and_img_diagnostics() {
    let Some(files) = std::env::var("ALBUMEN_M7_RECHECK_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        skip_or_fail_in_ci("M7 RECHECK_DIAG", "ALBUMEN_M7_RECHECK_FILES");
        return;
    };
    let levels: Vec<usize> = std::env::var("ALBUMEN_M7_RECHECK_TOMBSTONES")
        .unwrap_or_else(|_| "0,1000,10000".into())
        .split(',')
        .map(|n| n.trim().parse().unwrap())
        .collect();
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let rel = |i: usize| format!("d{:02}/IMG_{i:07}.jpg", i % 50);
    for d in 0..50 {
        fs::create_dir_all(library.path().join(format!("d{d:02}"))).unwrap();
    }
    for i in 0..files {
        fs::write(
            library.path().join(rel(i)),
            format!("synthetic-m7-recheck-{i}"),
        )
        .unwrap();
    }
    let began = Instant::now();
    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    loop {
        let (_, body) = get(&addr, "/api/library?offset=0&limit=1");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        if v["status"] == "ready" {
            break;
        }
        assert!(began.elapsed() < Duration::from_secs(1800), "never ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ready_ms = began.elapsed().as_secs_f64() * 1000.0;
    println!("M7 RECHECK_DIAG_READY files={files} ready_ms={ready_ms:.1}");
    // Tombstones go on every k-th photo from the end, so the visible
    // photos /img samples (from the start) stay visible.
    // Only the second half is ever tombstoned (capped at files / 2).
    let half = files / 2;
    let step = (half / levels.iter().copied().max().unwrap_or(1).max(1)).max(1);
    let mut written = 0usize;
    let touch = library.path().join("d00").join(".m7-recheck-touch");
    for &n in &levels {
        let n = n.min(half);
        while written < n {
            let i = files - 1 - written * step;
            fs::write(
                library.path().join(format!("{}.albumen-tombstone", rel(i))),
                br#"{"hidden":true,"reason":"hide"}"#,
            )
            .unwrap();
            written += 1;
        }
        // 100 re-check samples per level (criterion 2 needs 100+); /img
        // has its own loop below with 200 samples.
        let synced = Instant::now();
        let mut recheck = Vec::new();
        let mut sync_window = Vec::new();
        // (ms, sample index, ms after the tombstone writes it started,
        // tombstones read, folders listed by that request)
        let mut slow: Vec<(f64, usize, f64, u64, u64)> = Vec::new();
        let counters = |addr: &str| {
            let f = found_page(addr);
            (
                f["hide_recheck_reads"].as_u64().unwrap(),
                f["hide_dirs_listed"].as_u64().unwrap(),
                f["hide_pending_dirs"].as_u64().unwrap(),
            )
        };
        let mut before = counters(&addr);
        // XO 03:22Z: from the sync until no folder is pending any more
        // (every changed folder confirmed and its photos back).
        let mut withheld: Option<(f64, usize)> = None;
        for k in 0..100 {
            if k % 2 == 0 {
                fs::write(&touch, b"x").unwrap();
            } else {
                fs::remove_file(&touch).unwrap();
            }
            let t = Instant::now();
            let (code, _) = get(&addr, "/api/library?offset=0&limit=1");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            recheck.push(ms);
            let at = synced.elapsed().as_secs_f64() * 1000.0 - ms;
            if synced.elapsed() < Duration::from_secs(2) {
                sync_window.push(ms);
            }
            let after = counters(&addr);
            if withheld.is_none() && after.2 == 0 {
                withheld = Some((synced.elapsed().as_secs_f64() * 1000.0, k));
            }
            slow.push((ms, k, at, after.0 - before.0, after.1 - before.1));
            before = after;
            assert_eq!(code, 200);
        }
        // Steady state (XO 02:30Z (a)): 100 more, all 2 s+ after the last
        // tombstone write (the "sync"), the touch folder still changing.
        let wait = Duration::from_millis(2100).saturating_sub(synced.elapsed());
        std::thread::sleep(wait);
        let mut steady = Vec::new();
        for k in 0..100 {
            if k % 2 == 0 {
                fs::write(&touch, b"x").unwrap();
            } else {
                let _ = fs::remove_file(&touch);
            }
            let t = Instant::now();
            let (code, _) = get(&addr, "/api/library?offset=0&limit=1");
            steady.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(code, 200);
        }
        let _ = fs::remove_file(&touch);
        // XO 03:36Z (3): how long a withheld photo stays off screen, as the
        // page sees it: a visible photo's show tombstone is rewritten (so it
        // is withheld until its folder is confirmed), one request is made,
        // then the page's own poll is followed (/api/found every 200 ms while
        // a folder is pending, the grid fetched when hide_version moves)
        // until /img serves the photo again. 20 samples, 20 folders.
        let mut ui = Vec::new();
        let mut withheld_k0 = 0;
        for s in 0..20usize {
            let p = rel(s + 50 * (s + 1));
            let tomb = library.path().join(format!("{p}.albumen-tombstone"));
            fs::write(&tomb, br#"{"hidden":false,"reason":"m7-ui"}"#).unwrap();
            let t = Instant::now();
            get(&addr, "/api/library?offset=0&limit=1");
            let mut ver = found_page(&addr)["hide_version"].as_u64().unwrap();
            if get(&addr, &format!("/img?p={p}")).0 != 200 {
                withheld_k0 += 1;
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    let f = found_page(&addr);
                    let v = f["hide_version"].as_u64().unwrap();
                    if v != ver {
                        ver = v;
                        get(&addr, "/api/library?offset=0&limit=1");
                        if get(&addr, &format!("/img?p={p}")).0 == 200 {
                            break;
                        }
                    }
                    assert!(t.elapsed() < Duration::from_secs(30), "{p} never came back");
                }
            }
            ui.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let ui_p95 = pct(&mut ui.clone(), 0.95);
        let ui_max = pct(&mut ui.clone(), 1.0);
        let f = found_page(&addr);
        println!(
            "M7 WITHHELD files={files} tombstones={written} key={} withheld_ms={} confirmed_at_k={} \
             withheld_max_ms={} withheld_p95_ms={} withheld_dirs={} ui_samples={} ui_withheld_at_k0={withheld_k0} \
             ui_p95_ms={ui_p95:.1} ui_max_ms={ui_max:.1} keep_stats={} keep_max_ms={} build_max_ms={} worker_panics={} redispatched={}",
            f["hide_key"].as_str().unwrap_or("?"),
            withheld.map_or("n/a".into(), |w| format!("{:.1}", w.0)),
            withheld.map_or("n/a".into(), |w| w.1.to_string()),
            f["hide_withheld_max_ms"],
            f["hide_withheld_p95_ms"],
            f["hide_withheld_dirs"],
            ui.len(),
            f["hide_keep_stats"],
            f["hide_keep_max_ms"],
            f["hide_build_max_ms"],
            f["hide_worker_panics"],
            f["hide_redispatched"],
        );
        // XO 04:41Z (2): what the page's poll costs (status, and the old
        // /api/found, which the page calls on its open path), bytes and ms
        // per call, 100 calls each (Product Lead 07:03Z: >= 100 samples).
        for path in ["/api/hide-status", "/api/found"] {
            let mut ms = Vec::new();
            let mut bytes = 0;
            for _ in 0..100 {
                let t = Instant::now();
                let (code, body) = get(&addr, path);
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(code, 200);
                bytes = body.len();
            }
            println!(
                "M7 POLL files={files} tombstones={written} samples={} path={path} bytes={bytes} p50_ms={:.2} p95_ms={:.2} max_ms={:.2}",
                ms.len(),
                pct(&mut ms.clone(), 0.5),
                pct(&mut ms.clone(), 0.95),
                pct(&mut ms.clone(), 1.0)
            );
            if path == "/api/hide-status" {
                assert!(bytes <= 256, "the status poll is not small: {bytes} bytes");
            }
        }
        // XO 04:41Z (5): the inline work a request may do has a stated cap.
        let keep = f["hide_keep_max_ms"].as_f64().unwrap();
        let build = f["hide_build_max_ms"].as_f64().unwrap();
        let cap = if files <= 50_000 {
            INLINE_CAP_MS
        } else {
            INLINE_CAP_LARGE_MS
        };
        println!("M7 INLINE files={files} keep_max_ms={keep} build_max_ms={build} cap_ms={cap}");
        assert!(
            keep <= cap && build <= cap,
            "inline work over {cap} ms: keep_shown {keep} ms, hidden-set build {build} ms"
        );
        assert!(
            ui_p95 <= 1000.0 && ui_max <= 1000.0,
            "a withheld photo stayed off screen too long: p95 {ui_p95:.1} ms, max {ui_max:.1} ms"
        );
        let wmax = f["hide_withheld_max_ms"].as_f64().unwrap();
        let wp95 = f["hide_withheld_p95_ms"].as_f64().unwrap();
        assert!(
            wp95 <= 1000.0 && wmax <= 1000.0,
            "a folder stayed pending too long: p95 {wp95} ms, max {wmax} ms"
        );
        let p95 = |v: &[f64]| {
            if v.is_empty() {
                "n/a".to_string()
            } else {
                format!("{:.1}", pct(&mut v.to_vec(), 0.95))
            }
        };
        println!(
            "M7 RECHECK_SPLIT files={files} tombstones={written} \
             sync2s_p95_ms={} sync2s_max_ms={} sync2s_samples={} steady_p95_ms={} steady_p50_ms={:.1} \
             steady_max_ms={:.1} steady_samples={}",
            p95(&sync_window),
            if sync_window.is_empty() {
                "n/a".to_string()
            } else {
                format!("{:.1}", pct(&mut sync_window.clone(), 1.0))
            },
            sync_window.len(),
            p95(&steady),
            pct(&mut steady.clone(), 0.5),
            pct(&mut steady.clone(), 1.0),
            steady.len()
        );
        // Where the slowest re-checks fall (k=0 is the first request after
        // this level's tombstone writes).
        let fmt = |(ms, k, at, r, l): &(f64, usize, f64, u64, u64)| {
            format!("{ms:.1}ms@k={k},+{at:.0}ms,reads={r},listed={l}")
        };
        let first = fmt(&slow[0]);
        slow.sort_by(|a, b| b.0.total_cmp(&a.0));
        let top: Vec<String> = std::iter::once(format!("first={first}"))
            .chain(slow.iter().take(3).map(fmt))
            .collect();
        println!(
            "M7 RECHECK_SLOWEST files={files} tombstones={written} {}",
            top.join(" ")
        );
        std::thread::sleep(Duration::from_millis(2500));
        get(&addr, "/api/library?offset=0&limit=1");
        let mut img = Vec::new();
        for k in 0..200 {
            let t = Instant::now();
            let (code, _) = get(&addr, &format!("/img?p={}", rel(k * 7 % half.max(1))));
            img.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(code, 200);
        }
        let (_, body) = get(&addr, "/api/library");
        let shown = serde_json::from_str::<serde_json::Value>(&body).unwrap()["images"]
            .as_array()
            .unwrap()
            .len();
        println!(
            "M7 RECHECK_DIAG files={files} tombstones={written} shown={shown} \
             recheck_p50_ms={:.1} recheck_p95_ms={:.1} recheck_max_ms={:.1} \
             img_p50_ms={:.1} img_p95_ms={:.1} img_max_ms={:.1} samples_recheck={} samples_img={}",
            pct(&mut recheck.clone(), 0.5),
            pct(&mut recheck.clone(), 0.95),
            pct(&mut recheck.clone(), 1.0),
            pct(&mut img.clone(), 0.5),
            pct(&mut img.clone(), 0.95),
            pct(&mut img.clone(), 1.0),
            recheck.len(),
            img.len(),
        );
        // Criterion 2 (planned, not asserted): binds only after 100+ CI
        // samples per level at 380k on Linux and Windows.
        println!(
            "M7 RECHECK_DIAG_PLANNED_BAR tombstones={written} recheck_p95_ms<=10 img_p95_ms<=20 \
             (pending 100 samples; print-only)"
        );
    }
}

/// `start_env` with the gui's stderr kept, for log-line assertions.
fn start_logged(
    library: &Path,
    work: &Path,
    index: &Path,
    home: &Path,
    env: &[(&str, &str)],
) -> (Gui, String, std::sync::Arc<std::sync::Mutex<String>>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .envs(env.iter().copied())
        .args(["gui", "--no-open", "--port", "0", "--library"])
        .arg(library)
        .arg("--work")
        .arg(work)
        .env("ALBUMEN_INDEX_DIR", index)
        .env("ALBUMEN_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let err = BufReader::new(child.stderr.take().unwrap());
    let sink = std::sync::Arc::clone(&log);
    std::thread::spawn(move || {
        for l in err.lines().map_while(Result::ok) {
            let mut s = sink.lock().unwrap();
            s.push_str(&l);
            s.push('\n');
        }
    });
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = None;
    for line in lines.by_ref().map_while(Result::ok) {
        if let Some(url) = line.strip_prefix("albumen gui: http://") {
            addr = Some(url.trim_end_matches('/').to_string());
            break;
        }
    }
    std::thread::spawn(move || for _ in lines.map_while(Result::ok) {});
    (Gui(child), addr.expect("gui prints its URL"), log)
}

/// Product Lead acceptance (doc 1Ak9gqDUQp4BfibMVlw4fd4qLw8CKUxPH, XO
/// 04:08Z): while a folder is pending after a sync no visible photo
/// disappears, a new hide never shows, new photos are withheld until the
/// folder is confirmed, and then show within 1 s. The folder is held
/// pending by the ALBUMEN_TEST_RELIST_GATE hook (certain, not timed); a
/// gate that is not honoured is a failure. Then 20 more syncs measure the
/// withheld-new-photo time (p95 and max <= 1 s, blocking).
fn no_flicker(variant: &str) {
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(HOOKS, "needs a debug build or --features test-hooks");
    }
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let lib = library.path();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let photo = |d: usize, n: usize| format!("d{d:02}/IMG_{n:04}.JPG");
    let tomb = |p: &str| lib.join(format!("{p}.albumen-tombstone"));
    // N = 5,000 photos in 50 folders. F = d00 (gated), G = d01 (the hides
    // whose removal is the sync that brings the new photos in).
    for d in 0..50 {
        fs::create_dir_all(lib.join(format!("d{d:02}"))).unwrap();
        for n in 0..100 {
            let p = lib.join(photo(d, n));
            fs::write(&p, format!("synthetic-m7-nf-{d}-{n}")).unwrap();
            set_mtime(&p, old);
        }
    }
    // H0: 250 hidden over 10 folders (d01..d10, 25 each).
    let mut h0 = Vec::new();
    for d in 1..=10 {
        for n in 0..25 {
            let p = photo(d, n);
            fs::write(tomb(&p), br#"{"hidden":true,"reason":"hide"}"#).unwrap();
            h0.push(p);
        }
    }
    // 50 showing tombstones, 25 of them in F.
    for d in [0, 20] {
        for n in 50..75 {
            fs::write(tomb(&photo(d, n)), br#"{"hidden":false,"reason":"unhide"}"#).unwrap();
        }
    }
    std::thread::sleep(Duration::from_millis(2100));
    let mut env = vec![("ALBUMEN_TEST_RELIST_GATE", "d00")];
    let h = photo(0, 10);
    let h_tomb = format!("{h}.albumen-tombstone");
    match variant {
        "coarse" if cfg!(unix) => {
            env.push(("ALBUMEN_TEST_FS_MAGIC", "0xef53"));
            env.push(("ALBUMEN_TEST_COARSE_CTIME", "1"));
        }
        "worker_error" => env.push(("ALBUMEN_TEST_TOMB_ERROR", &h_tomb)),
        "worker_panic" => {
            env.push(("ALBUMEN_TEST_RELIST_PANIC", "d00"));
            env.push(("ALBUMEN_TEST_RELIST_PANIC_AFTER_GATE", "1"));
        }
        _ => {}
    }
    let (_gui, addr, log) = start_logged(lib, work.path(), index.path(), home.path(), &env);
    let f0 = ready_found(&addr);
    let key = f0["hide_key"].as_str().unwrap_or("").to_string();
    let want = if cfg!(windows) { "changetime" } else { "ctime" };
    assert_eq!(key, want, "key does not match the platform");
    if variant == "coarse" {
        assert_eq!(
            f0["hide_same_tick_ms"], 2000,
            "coarse window not in force: {f0}"
        );
    }
    listed_raw(&addr);
    settled(&addr);
    std::thread::sleep(Duration::from_millis(2100));
    let v0 = listed_raw(&addr);
    settled(&addr);
    assert_eq!(v0.len(), 4750, "|V0|");
    let set = |v: &[String]| {
        v.iter()
            .cloned()
            .collect::<std::collections::HashSet<String>>()
    };
    let v0s = set(&v0);
    assert!(
        h0.iter().all(|p| !v0s.contains(p)),
        "V0 shows a hidden photo"
    );
    assert!(v0s.contains(&h));
    // Sync on F: 20 new photos, a new hide on h and an unrelated file. No
    // unhide and no scan: the confirmed listing alone brings the new photos
    // in (XO 04:41Z (1)).
    let gate = lib.join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    let p_new: Vec<String> = (0..20).map(|n| format!("d00/NEW_{n:04}.JPG")).collect();
    for p in &p_new {
        fs::write(lib.join(p), format!("synthetic-m7-nf-new-{p}")).unwrap();
    }
    fs::write(lib.join(&h_tomb), br#"{"hidden":true,"reason":"hide"}"#).unwrap();
    fs::write(lib.join("d00/notes.txt"), b"synthetic").unwrap();
    if variant != "coarse" {
        // Out of the same-tick window, so F is listed once (coarse: not).
        std::thread::sleep(Duration::from_millis(2100));
    }
    let unhidden = std::collections::HashSet::<String>::new();
    let mut disappearances = 0usize;
    let mut h_shown = 0usize;
    let mut times = Vec::new();
    let check = |r: &[String],
                 unhidden: &std::collections::HashSet<String>,
                 dis: &mut usize,
                 hs: &mut usize,
                 new_ok: bool| {
        let rs = set(r);
        *dis += v0.iter().filter(|p| **p != h && !rs.contains(*p)).count();
        *hs += usize::from(rs.contains(&h));
        assert!(
            h0.iter().all(|p| unhidden.contains(p) || !rs.contains(p)),
            "a hidden photo showed"
        );
        if !new_ok {
            assert!(
                p_new.iter().all(|p| !rs.contains(p)),
                "a new photo showed while pending"
            );
            // XO 04:41Z (6): the order too, not only the set: what shows
            // while pending is V0 in V0's order, minus what is withheld.
            let ord: Vec<&String> = v0.iter().filter(|p| rs.contains(*p)).collect();
            assert!(
                ord.len() == r.len() && ord.iter().zip(r).all(|(a, b)| *a == b),
                "the order changed while pending"
            );
        }
    };
    // End to end (request and the test's own parse), printed beside the
    // request-only time (XO 05:41Z (4)).
    let e2e = std::cell::RefCell::new(Vec::new());
    let timed = |addr: &str, times: &mut Vec<f64>| {
        // The request only (XO 05:06Z (d)): the test's own JSON parse of
        // the answer is not the server's time.
        let t = Instant::now();
        let (code, body) = get(addr, "/api/library");
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        let r = parse_listed(code, &body);
        e2e.borrow_mut().push(t.elapsed().as_secs_f64() * 1000.0);
        r
    };
    let r1 = timed(&addr, &mut times);
    check(&r1, &unhidden, &mut disappearances, &mut h_shown, false);
    let f = found_page(&addr);
    assert!(
        f["hide_pending_dirs"].as_u64().unwrap_or(0) >= 1,
        "the gate was not honoured (no folder pending): {f}"
    );
    // Pending: at least 20 requests at ~50 ms (2 s for the coarse window).
    let began = Instant::now();
    let mut k = 1;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let r = timed(&addr, &mut times);
        k += 1;
        check(&r, &unhidden, &mut disappearances, &mut h_shown, false);
        let f = found_page(&addr);
        assert!(
            f["hide_pending_dirs"].as_u64().unwrap_or(0) >= 1,
            "F stopped being pending before the release: {f}"
        );
        if k >= 20 && (variant != "coarse" || began.elapsed() > Duration::from_secs(2)) {
            break;
        }
    }
    // Release.
    fs::remove_file(&gate).unwrap();
    let t = Instant::now();
    let mut back = None;
    while back.is_none() {
        std::thread::sleep(Duration::from_millis(50));
        let r = timed(&addr, &mut times);
        let rs = set(&r);
        check(&r, &unhidden, &mut disappearances, &mut h_shown, true);
        if p_new.iter().all(|p| rs.contains(p)) {
            back = Some(t.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "new photos never showed"
        );
    }
    let back = back.unwrap();
    settled(&addr);
    let mut want_set = v0s.clone();
    want_set.remove(&h);
    want_set.extend(p_new.iter().cloned());
    let fin = listed_raw(&addr);
    assert_eq!(set(&fin), want_set, "final set");
    for _ in 0..5 {
        assert_eq!(listed_raw(&addr), fin, "the grid oscillated");
    }
    if variant == "worker_panic" {
        let l = log.lock().unwrap().clone();
        assert!(
            l.contains("hide: worker panicked; retrying d00"),
            "no panic log line: {l}"
        );
        assert!(found_page(&addr)["hide_worker_panics"].as_u64() >= Some(1));
    }
    if variant == "worker_error" {
        let note = found_page(&addr)["hide_note"].to_string();
        assert!(
            note.contains("test hook: unreadable"),
            "error not in the note: {note}"
        );
        assert!(
            log.lock().unwrap().contains("test hook: unreadable"),
            "error not on stderr"
        );
    }
    assert!(
        found_page(&addr)["hide_grew"].as_u64() >= Some(1),
        "the new photos came in some other way than the confirmed listing"
    );
    // 20 syncs: one new photo in F each (no unhide), timed from the
    // writes to the new photo in /api/library (polled at 200 ms, as the
    // page polls while a folder is pending).
    let mut withheld = Vec::new();
    let mut known = fin.clone();
    for r in 1..=20 {
        let np = format!("d00/NEW_R{r:03}.JPG");
        fs::write(lib.join(&np), format!("synthetic-m7-nf-{np}")).unwrap();
        let t = Instant::now();
        loop {
            let rr = timed(&addr, &mut times);
            let rs = set(&rr);
            let lost = known.iter().filter(|p| !rs.contains(*p)).count();
            disappearances += lost;
            h_shown += usize::from(rs.contains(&h));
            if rs.contains(&np) {
                withheld.push(t.elapsed().as_secs_f64() * 1000.0);
                known = rr;
                break;
            }
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "{np} never showed: {}",
                {
                    let mut f = found_page(&addr);
                    f["paths"] = serde_json::Value::Null;
                    f
                }
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let w_p95 = pct(&mut withheld.clone(), 0.95);
    let w_max = pct(&mut withheld.clone(), 1.0);
    // XO 05:06Z (d): library re-walks during the run (the generation). Each
    // of the 21 grown listings may start one; a re-list of a folder with a
    // hidden photo must not (it did on 387f91f, see the docs).
    let (_, st) = get(&addr, "/api/hide-status");
    let st: serde_json::Value = serde_json::from_str(&st).unwrap();
    let rewalks = st["generation"].as_u64().unwrap_or(0);
    // FRAGO 06:13Z (3): the cause of each re-walk, so an extra one on
    // another platform names its photo and tombstone state.
    let causes = found_page(&addr)["hide_rewalk_causes"].clone();
    println!("M7 NO_FLICKER_REWALK_CAUSES variant={variant} causes={causes}");
    println!("M7 NO_FLICKER_REWALKS variant={variant} generation={rewalks} max={MAX_REWALKS}");
    // XO 05:41Z (3): asserted, not only printed. One walk at the start and
    // at most one per grown listing (21); a re-list of the folder with the
    // hidden photo starting one would push this over.
    // XO 06:08Z (1): exactly, not at most (no headroom).
    assert_eq!(
        rewalks, MAX_REWALKS,
        "{rewalks} library re-walks (exactly {MAX_REWALKS})"
    );
    let r_p95 = pct(&mut times.clone(), 0.95);
    let r_max = pct(&mut times.clone(), 1.0);
    let e2e = e2e.into_inner();
    let e_p95 = pct(&mut e2e.clone(), 0.95);
    let e_max = pct(&mut e2e.clone(), 1.0);
    println!(
        "M7 NO_FLICKER variant={variant} key={key} requests={} disappearances={disappearances} \
         h_shown={h_shown} release_to_new_ms={back:.1} withheld_samples={} withheld_p95_ms={w_p95:.1} \
         withheld_max_ms={w_max:.1} recheck_p95_ms={r_p95:.1} recheck_max_ms={r_max:.1} \
         e2e_p95_ms={e_p95:.1} e2e_max_ms={e_max:.1}",
        times.len(),
        withheld.len()
    );
    assert_eq!(disappearances, 0, "a visible photo disappeared");
    assert_eq!(h_shown, 0, "the new hide showed");
    let f = found_page(&addr);
    let (keep, build) = (
        f["hide_keep_max_ms"].as_f64().unwrap(),
        f["hide_build_max_ms"].as_f64().unwrap(),
    );
    println!("M7 NO_FLICKER_INLINE variant={variant} keep_max_ms={keep} build_max_ms={build}");
    assert!(
        keep <= INLINE_CAP_MS && build <= INLINE_CAP_MS,
        "inline work: keep_shown {keep} ms, build {build} ms"
    );
    assert!(back <= 1000.0, "new photos {back:.1} ms after the release");
    assert!(
        w_p95 <= 1000.0 && w_max <= 1000.0,
        "withheld p95 {w_p95:.1} max {w_max:.1}"
    );
    assert!(
        r_p95 <= 10.0 && r_max <= 100.0,
        "requests p95 {r_p95:.1} max {r_max:.1}"
    );
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

/// A small synthetic library for the XO 04:41Z tests: `dirs` folders of
/// 20 photos each, old mtimes, out of the same-tick window on return.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn small_lib(lib: &Path, dirs: usize) -> Vec<String> {
    let old = SystemTime::now() - Duration::from_secs(3600);
    let mut all = Vec::new();
    for d in 0..dirs {
        fs::create_dir_all(lib.join(format!("d{d:02}"))).unwrap();
        for n in 0..20 {
            let rel = format!("d{d:02}/IMG_{n:04}.JPG");
            let p = lib.join(&rel);
            fs::write(&p, format!("synthetic-m7-r2-{d}-{n}")).unwrap();
            set_mtime(&p, old);
            all.push(rel);
        }
    }
    std::thread::sleep(Duration::from_millis(2100));
    all
}

#[cfg(any(debug_assertions, feature = "test-hooks"))]
struct Lib {
    _dirs: [tempfile::TempDir; 4],
    gui: Gui,
    addr: String,
    log: std::sync::Arc<std::sync::Mutex<String>>,
}

/// Start the gui on `lib` and wait until its listing is settled.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn open_small(dirs: usize, hidden: &[&str], env: &[(&str, &str)]) -> (Lib, Vec<String>) {
    let library = tempfile::tempdir().unwrap();
    let (work, index, home) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let all = small_lib(library.path(), dirs);
    for h in hidden {
        fs::write(
            library.path().join(format!("{h}.albumen-tombstone")),
            br#"{"hidden":true,"reason":"hide"}"#,
        )
        .unwrap();
    }
    if !hidden.is_empty() {
        std::thread::sleep(Duration::from_millis(2100));
    }
    let (gui, addr, log) =
        start_logged(library.path(), work.path(), index.path(), home.path(), env);
    ready_found(&addr);
    listed(&addr);
    settled(&addr);
    let l = Lib {
        _dirs: [library, work, index, home],
        gui,
        addr,
        log,
    };
    (l, all)
}

#[cfg(any(debug_assertions, feature = "test-hooks"))]
impl Lib {
    fn path(&self) -> &Path {
        self._dirs[0].path()
    }
    fn set(&self) -> std::collections::HashSet<String> {
        listed(&self.addr).into_iter().collect()
    }
    /// Poll /api/library every 100 ms until `ok`, at most `secs`.
    fn until(
        &self,
        secs: u64,
        what: &str,
        ok: impl Fn(&std::collections::HashSet<String>) -> bool,
    ) -> f64 {
        let t = Instant::now();
        loop {
            if ok(&self.set()) {
                return t.elapsed().as_secs_f64() * 1000.0;
            }
            assert!(t.elapsed() < Duration::from_secs(secs), "{what}: {}", {
                let mut f = found_page(&self.addr);
                f["paths"] = serde_json::Value::Null;
                f
            });
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// XO 04:41Z (1): photos synced into a folder show with no unhide, no scan
/// and no other user action, once that folder is confirmed.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_synced_in_photos_show_without_unhide_or_scan() {
    let (l, all) = open_small(4, &[], &[]);
    let _ = &l.gui;
    assert_eq!(l.set().len(), all.len());
    let new: Vec<String> = (0..5).map(|n| format!("d02/SYNC_{n:04}.JPG")).collect();
    for p in &new {
        fs::write(l.path().join(p), format!("synthetic-m7-r2-{p}")).unwrap();
    }
    let ms = l.until(5, "synced-in photos never showed", |s| {
        new.iter().all(|p| s.contains(p))
    });
    let f = settled(&l.addr);
    let mut want: std::collections::HashSet<String> = all.iter().cloned().collect();
    want.extend(new.iter().cloned());
    assert_eq!(l.set(), want, "final set");
    assert!(f["hide_grew"].as_u64() >= Some(1), "{f}");
    println!("M7 SYNCED_IN shown_ms={ms:.1} grew={}", f["hide_grew"]);
}

/// XO 04:41Z (7): a photo hidden, then unhidden while its folder is
/// pending, shows once the folder is confirmed (it was lost before 09fda5d).
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_unhide_while_pending_shows_the_photo() {
    unhide_while_pending(&[]);
}

/// FRAGO 06:29Z (4): the same with no growth path (a re-listed folder never
/// counts as grown), so only the unhide (the carried hard-then-withheld
/// set) can bring the photo back: without it, 'the unhidden photo never
/// showed'.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_unhide_while_pending_shows_the_photo_with_no_growth() {
    unhide_while_pending(&[("ALBUMEN_TEST_NO_GROW", "1")]);
}

#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn unhide_while_pending(extra: &[(&str, &str)]) {
    let u = "d01/IMG_0003.JPG";
    let mut env = vec![("ALBUMEN_TEST_RELIST_GATE", "d01")];
    env.extend_from_slice(extra);
    let (l, all) = open_small(4, &[u], &env);
    let _ = &l.gui;
    assert!(!l.set().contains(u), "u shown before the unhide");
    let gate = l.path().join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    fs::remove_file(l.path().join(format!("{u}.albumen-tombstone"))).unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    for _ in 0..10 {
        let s = l.set();
        assert!(!s.contains(u), "u shown before its folder was confirmed");
        assert!(
            found_page(&l.addr)["hide_pending_dirs"].as_u64() >= Some(1),
            "gate not honoured"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::remove_file(&gate).unwrap();
    let ms = l.until(5, "the unhidden photo never showed", |s| s.contains(u));
    settled(&l.addr);
    assert_eq!(l.set(), all.iter().cloned().collect(), "final set");
    let causes = found_page(&l.addr)["hide_rewalk_causes"].clone();
    println!("M7 UNHIDE_WHILE_PENDING shown_ms={ms:.1} causes={causes}");
    // XO 06:08Z (2): the photo must come back as an unhide (the carried
    // hard-then-withheld set), not only because its re-listed folder grew.
    assert!(
        causes.as_array().is_some_and(|c| c
            .iter()
            .any(|c| c == &serde_json::json!(format!("unhid {u}")))),
        "the unhide was not seen as one (only a grown folder brought it back): {causes}"
    );
    assert!(ms <= 1000.0, "unhidden photo {ms:.1} ms after the release");
}

/// FRAGO 06:29Z (3): a walk sent for a new photo that panics is retried,
/// so the photo still shows (the first walk after startup panics once).
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_a_new_photos_walk_that_panics_is_retried() {
    let (l, all) = open_small(4, &[], &[("ALBUMEN_TEST_PANIC_WALKS", "1")]);
    let _ = &l.gui;
    assert_eq!(l.set().len(), all.len());
    let p = "d03/SYNC_0001.JPG";
    fs::write(l.path().join(p), b"synthetic-m7-r6-walk-retry").unwrap();
    let ms = l.until(
        10,
        "the new photo never showed after its walk panicked",
        |s| s.contains(p),
    );
    // The timed retry (1 s after the panic) also runs; the photo may show
    // first by the re-list that sees it again.
    std::thread::sleep(Duration::from_millis(1500));
    let f = settled(&l.addr);
    let mut want: std::collections::HashSet<String> = all.iter().cloned().collect();
    want.insert(p.to_string());
    assert_eq!(l.set(), want, "final set");
    println!(
        "M7 WALK_RETRY shown_ms={ms:.1} failures={} retries={}",
        f["walk_failures"], f["walk_retries"]
    );
    assert_eq!(f["walk_failures"].as_u64(), Some(1), "{f}");
    assert!(f["walk_retries"].as_u64() >= Some(1), "{f}");
}

/// XO 04:41Z (3): a folder held past PENDING_LIMIT (10 s, a hung worker) is
/// logged and sent again; meanwhile nothing on screen goes, nothing new or
/// newly hidden shows; after the release both workers land without a
/// double apply (exact final set, stable).
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_pending_limit_sends_a_hung_folder_again() {
    let (l, all) = open_small(4, &[], &[("ALBUMEN_TEST_RELIST_GATE", "d00")]);
    let _ = &l.gui;
    let v0 = l.set();
    let gate = l.path().join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    let h = "d00/IMG_0005.JPG";
    fs::write(
        l.path().join(format!("{h}.albumen-tombstone")),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    let new = "d00/HUNG_0000.JPG";
    fs::write(l.path().join(new), b"synthetic-m7-r2-hung").unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let t = Instant::now();
    let mut lost = 0;
    while t.elapsed() < Duration::from_secs(12) {
        let s = l.set();
        lost += v0.iter().filter(|p| *p != h && !s.contains(*p)).count();
        assert!(!s.contains(h), "the new hide showed");
        assert!(!s.contains(new), "a new photo showed while pending");
        assert!(
            found_page(&l.addr)["hide_pending_dirs"].as_u64() >= Some(1),
            "gate not honoured"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(lost, 0, "a visible photo disappeared");
    let f = found_page(&l.addr);
    assert!(f["hide_redispatched"].as_u64() >= Some(1), "{f}");
    assert!(
        l.log
            .lock()
            .unwrap()
            .contains("hide: d00 still pending after"),
        "no PENDING_LIMIT log line"
    );
    fs::remove_file(&gate).unwrap();
    let ms = l.until(5, "the new photo never showed after the release", |s| {
        s.contains(new)
    });
    settled(&l.addr);
    let mut want: std::collections::HashSet<String> = all.iter().cloned().collect();
    want.remove(h);
    want.insert(new.to_string());
    let fin = listed_raw(&l.addr);
    assert_eq!(
        fin.iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>(),
        want,
        "final set"
    );
    assert_eq!(fin.len(), want.len(), "a photo listed twice");
    for _ in 0..5 {
        assert_eq!(listed_raw(&l.addr), fin, "the grid oscillated");
    }
    println!(
        "M7 PENDING_LIMIT redispatched={} release_to_new_ms={ms:.1}",
        f["hide_redispatched"]
    );
}

/// XO 04:41Z (3)/(4): a listing that panics on every try (3) fails closed
/// for anything new or newly hidden, keeps the photos on screen (per-photo
/// keep rule, no flicker), and is sent again after a backoff.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
#[test]
fn m7_exhausted_panics_keep_shown_photos_and_back_off() {
    let (l, all) = open_small(
        4,
        &[],
        &[
            ("ALBUMEN_TEST_RELIST_GATE", "d00"),
            ("ALBUMEN_TEST_RELIST_PANIC", "d00"),
            ("ALBUMEN_TEST_RELIST_PANIC_AFTER_GATE", "1"),
            ("ALBUMEN_TEST_RELIST_PANIC_TIMES", "3"),
        ],
    );
    let _ = &l.gui;
    let v0 = l.set();
    let gate = l.path().join(".albumen-test-gate");
    fs::write(&gate, b"").unwrap();
    let h = "d00/IMG_0007.JPG";
    fs::write(
        l.path().join(format!("{h}.albumen-tombstone")),
        br#"{"hidden":true,"reason":"hide"}"#,
    )
    .unwrap();
    let new = "d00/PANIC_0000.JPG";
    fs::write(l.path().join(new), b"synthetic-m7-r2-panic").unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    let s = l.set();
    assert!(!s.contains(h) && !s.contains(new));
    fs::remove_file(&gate).unwrap();
    // Until the three panics are in: nothing on screen goes.
    let t = Instant::now();
    let mut lost = 0;
    loop {
        let s = l.set();
        lost += v0.iter().filter(|p| *p != h && !s.contains(*p)).count();
        assert!(!s.contains(h), "the new hide showed");
        if found_page(&l.addr)["hide_exhausted"].as_u64() >= Some(1) {
            assert!(!s.contains(new), "a new photo showed from a failed listing");
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "never exhausted");
        std::thread::sleep(Duration::from_millis(20));
    }
    // During the backoff: still kept, still fail closed.
    let mut shown_new_at = None;
    let t = Instant::now();
    while shown_new_at.is_none() {
        let s = l.set();
        lost += v0.iter().filter(|p| *p != h && !s.contains(*p)).count();
        assert!(!s.contains(h), "the new hide showed");
        if s.contains(new) {
            shown_new_at = Some(t.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(t.elapsed() < Duration::from_secs(10), "never listed again");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(lost, 0, "a visible photo disappeared");
    let back = shown_new_at.unwrap();
    assert!(
        back >= 900.0,
        "sent again before the 1 s backoff ({back:.1} ms)"
    );
    settled(&l.addr);
    let log = l.log.lock().unwrap().clone();
    assert!(log.contains("listing d00 kept panicking"), "{log}");
    assert!(
        log.contains("listing d00 again after 1 failed tries"),
        "{log}"
    );
    assert_eq!(
        log.matches("hide: worker panicked; retrying d00").count(),
        3,
        "{log}"
    );
    let mut want: std::collections::HashSet<String> = all.iter().cloned().collect();
    want.remove(h);
    want.insert(new.to_string());
    assert_eq!(l.set(), want, "final set");
    let f = found_page(&l.addr);
    println!(
        "M7 EXHAUSTED panics={} exhausted={} relisted_after_ms={back:.1}",
        f["hide_worker_panics"], f["hide_exhausted"]
    );
}
