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

/// Scale run. Off unless `ALBUMEN_M7_FILES` is set (garage PC, ubuntu CI
/// via .github/m6-one.sh). The bars are asserted only with
/// `ALBUMEN_M7_BARS=1`: box timing is not evidence.
#[test]
fn hidden_photos_stay_hidden_at_scale() {
    let Some(files) = std::env::var("ALBUMEN_M7_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        println!("M7 scale skipped: set ALBUMEN_M7_FILES (e.g. 380000)");
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

fn set_mtime(path: &Path, t: SystemTime) {
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(t))
        .unwrap();
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
