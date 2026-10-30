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

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use albumen_ingest::hides::hidden_paths;
use albumen_ingest::index::LibraryIndex;

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

#[test]
fn a_family_hide_is_per_viewer_and_a_tombstone_is_for_everyone() {
    let tmp = tempfile::tempdir().unwrap();
    let key_a = tmp.path().join("a.key");
    let key_b = tmp.path().join("b.key");
    let s = |p: &Path| p.to_str().unwrap().to_string();
    let (ok, text) = albumen(&["family", "keygen", "--key", &s(&key_a), "--apply"]);
    assert!(ok, "{text}");
    let member_a = line(&text, "member ").to_string();
    let (ok, text) = albumen(&["family", "keygen", "--key", &s(&key_b), "--apply"]);
    assert!(ok, "{text}");
    let member_b = line(&text, "member ").to_string();
    let parent = tmp.path().join("shared");
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

    let mut stored = Vec::new();
    let mut rel = Vec::new();
    for name in ["hidden-for-bea.jpg", "tombstoned.jpg", "plain.jpg"] {
        let src = tmp.path().join(name);
        fs::write(&src, format!("synthetic-m7-family-{name}")).unwrap();
        let (ok, text) = albumen(&[
            "family",
            "add",
            "--library",
            &lib,
            "--key",
            &s(&key_a),
            "--from",
            &s(&src),
            "--apply",
        ]);
        assert!(ok, "{text}");
        let sha = line(&text, "sha256 ").to_string();
        let r = format!("members/{member_a}/objects/sha256/{}/{sha}", &sha[..2]);
        stored.push(library.join(&r));
        rel.push((r, sha));
    }
    let (ok, text) = albumen(&[
        "family",
        "hide",
        "--library",
        &lib,
        "--key",
        &s(&key_b),
        "--sha256",
        &rel[0].1,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let (ok, text) = albumen(&[
        "family",
        "tombstone",
        "--library",
        &lib,
        "--key",
        &s(&key_a),
        "--sha256",
        &rel[1].1,
        "--apply",
    ]);
    assert!(ok, "{text}");
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
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut hidden = Vec::new();
    for i in 0..files {
        let dir = format!("d{:02}", i % 50);
        fs::create_dir_all(library.path().join(&dir)).unwrap();
        let rel = format!("{dir}/IMG_{i:07}.jpg");
        fs::write(library.path().join(&rel), b"").unwrap();
        if i % 1000 == 7 {
            fs::write(
                library.path().join(format!("{rel}.albumen-tombstone")),
                format!("{{\"hidden\":true,\"reason\":\"hide\",\"stored_name\":\"{rel}\"}}\n"),
            )
            .unwrap();
            hidden.push(rel);
        }
    }
    // A saved index, as after the first scan; the gui then rescans from it.
    LibraryIndex::open(library.path(), Some(index.path())).unwrap();

    let began = Instant::now();
    let (_gui, addr, pid) = start_pid(library.path(), work.path(), index.path(), home.path());
    let mut first_tile_ms = None;
    for _ in 0..4000 {
        let (code, body) = get(&addr, "/api/library?offset=0&limit=1");
        assert_eq!(code, 200);
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();
        if page["status"] == "ready" && !page["images"].as_array().unwrap().is_empty() {
            first_tile_ms = Some(began.elapsed().as_secs_f64() * 1000.0);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let first_tile_ms = first_tile_ms.expect("no first tile");
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
         first_tile_ms={first_tile_ms:.1} gui_child_peak_rss_kb={} bars={bars}",
        hidden.len(),
        shown.len(),
        rss.map(|r| r.to_string())
            .unwrap_or_else(|| "unmeasured".into()),
    );
    assert_eq!(hidden_shown, 0, "hidden photos shown at scale");
    assert_eq!(shown.len(), files - hidden.len());
    if bars {
        assert!(
            first_tile_ms <= 2000.0,
            "first tile {first_tile_ms:.1} ms over 2 s"
        );
        let rss = rss.expect("peak RSS unmeasured on this OS");
        assert!(rss <= 339_968, "gui child peak RSS {rss} kB over 332 MiB");
    }
}
