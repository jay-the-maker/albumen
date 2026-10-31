// Shared by the m7_hide_persist and m7_timing test binaries (included
// with include!, so items keep crate-root visibility).
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


fn found_page(addr: &str) -> serde_json::Value {
    let (code, body) = get(addr, "/api/found");
    assert_eq!(code, 200);
    serde_json::from_str(&body).unwrap()
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

















fn corrupted_tombstone_note_case(extra: usize, hold_open: bool) {
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
    for i in 0..extra {
        let photo = library.path().join(format!("IMG_X{i:04}.JPG"));
        fs::write(&photo, format!("synthetic-m7-note-x{i}")).unwrap();
        let t = library
            .path()
            .join(format!("IMG_X{i:04}.JPG.albumen-tombstone"));
        fs::write(&t, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
        set_mtime(&t, old);
    }
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
    // Held open: rewritten in place (no truncate, same length) and the
    // handle kept until after the poll.
    let _held = if hold_open {
        use std::io::{Seek, Write};
        let mut h = fs::OpenOptions::new().write(true).open(&tomb).unwrap();
        h.seek(std::io::SeekFrom::Start(0)).unwrap();
        h.write_all(&bad).unwrap();
        h.flush().unwrap();
        Some(h)
    } else {
        fs::write(&tomb, &bad).unwrap();
        set_mtime(&tomb, old);
        None
    };
    let polled = found_page(&addr);
    // XO 09:46Z: no tombstone stamp comes from a folder listing (NTFS
    // listings miss an open writer; Windows CI 8e83a30 had no note here).
    assert_eq!(
        polled["hide_dir_listed"].as_u64().unwrap_or(0),
        0,
        "a listing stamp was used"
    );
    let note = polled["hide_note"].as_str().unwrap_or_default().to_string();
    println!(
        "M7 corrupted-tombstone note after one poll: {note} (reads {} -> {})",
        reads(&quiet),
        reads(&polled)
    );
    assert!(note.contains("IMG_0002.JPG.albumen-tombstone"), "{polled}");
    // Product Lead 0951Z D3: visibility on the same poll, writer still
    // open in the held case: the corrupted (hidden) photo is not listed
    // and /img does not serve it.
    let shown = listed(&addr);
    assert!(
        !shown.iter().any(|p| p == "IMG_0002.JPG"),
        "the corrupted tombstone's photo was listed: {shown:?}"
    );
    assert!(shown.iter().any(|p| p == "IMG_0001.JPG"), "{shown:?}");
    assert_eq!(
        get(&addr, "/img?p=IMG_0002.JPG").0,
        404,
        "/img served the corrupted tombstone's photo"
    );
    assert_eq!(get(&addr, "/img?p=IMG_0001.JPG").0, 200);
    if hold_open {
        // Reads may include the writer-open retry; the note is the bar.
        return;
    }
    assert_eq!(
        reads(&polled) - reads(&quiet),
        if polled["hide_key"] == "portable" {
            extra as u64 + 2 + 1
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
        // FRAGO 09:53Z (4): the one panic is retried at once inside the
        // worker; the 1, 2, 4 .. 60 s backoff applies only after three
        // panics in a row (`hide_exhausted`), so it cannot delay the release.
        let f = found_page(&addr);
        assert_eq!(
            f["hide_exhausted"], 0,
            "backoff engaged: {}",
            f["hide_exhausted"]
        );
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
    // FRAGO 09:53Z (4): each sample and the backoff counters, so a slow
    // Windows run shows whether the time is spread or one outlier.
    {
        let f = found_page(&addr);
        let ws: Vec<String> = withheld.iter().map(|w| format!("{w:.0}")).collect();
        println!(
            "M7 NO_FLICKER_WITHHELD variant={variant} samples_ms={} exhausted={} redispatched={} worker_panics={}",
            ws.join(","),
            f["hide_exhausted"],
            f["hide_redispatched"],
            f["hide_worker_panics"]
        );
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
    {
        // XO 13:30Z (3): the unlocked-wait retakes and requests that came
        // with an older view than the state's (a 23rd re-walk's suspects).
        let f = found_page(&addr);
        println!(
            "M7 NO_FLICKER_RETAKE variant={variant} retake_stale={} gen_backwards={} walk_failures={}",
            f["hide_req_retake_stale"], f["hide_gen_backwards"], f["walk_failures"]
        );
    }
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
    // FRAGO 10:22Z (2): the slowest keep and build, with the thread's CPU,
    // involuntary switches and sub-phases, so a cap failure says why.
    let inline = format!(
        "keep=[{}] build=[{}]",
        f["hide_inline"]["keep"].as_str().unwrap_or("?"),
        f["hide_inline"]["build"].as_str().unwrap_or("?")
    );
    println!("M7 NO_FLICKER_INLINE variant={variant} keep_max_ms={keep} build_max_ms={build} {inline}");
    // XO 09:12Z / Product Lead 0912Z (4): per-request file touches and lock
    // wait, so one Windows run shows where the cost is.
    let r = &f["hide_req"];
    println!(
        "M7 NO_FLICKER_REQ variant={variant} requests={} tomb_io_total={} tomb_io_max={} \
         lock_wait_max_ms={} drain_max_ms={} total_max_ms={} tombstone_reads={} recheck_reads={} \
         keep_stats={} lock_hold={}",
        r["requests"],
        r["tomb_io_total"],
        r["tomb_io_max"],
        r["lock_wait_max_ms"],
        r["drain_max_ms"],
        r["total_max_ms"],
        f["hide_tombstone_reads"],
        f["hide_recheck_reads"],
        f["hide_keep_stats"],
        f["hide_lock_hold"],
    );
    assert!(
        keep <= INLINE_CAP_MS && build <= INLINE_CAP_MS,
        "inline work: keep_shown {keep} ms, build {build} ms; {inline}"
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





