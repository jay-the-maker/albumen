//! `albumen gui --demo` serves only on 127.0.0.1 and refuses cross-site and traversal requests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

struct Gui(Child);
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(work: &std::path::Path) -> (Gui, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(["gui", "--demo", "--no-open", "--port", "0", "--work"])
        .arg(work)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Keep draining stdout for the child's whole life. The GUI prints more
    // lines after its URL; dropping the pipe early made those println!s hit
    // EPIPE and panic the server's main thread, so later requests saw a
    // connection reset (the old intermittent read_to_string panic).
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

fn request(addr: &str, raw: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let code = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body)
}

fn get(addr: &str, path: &str) -> (u16, String) {
    request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"),
    )
}

#[test]
fn demo_serves_page_and_four_honest_states() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    assert!(page.contains("DEMO-ONLY"));
    let (code, status) = get(&addr, "/api/status");
    assert_eq!(code, 200);
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    let states: Vec<&str> = status["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["not backed up", "stale", "damaged", "not backed up"]
    );
    let (code, library) = get(&addr, "/api/library");
    assert_eq!(code, 200);
    assert!(library.contains("\"files\":12"), "{library}");
}

#[test]
fn refuses_foreign_host_missing_header_and_traversal() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, _) = request(&addr, "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
    assert_eq!(code, 403, "DNS-rebinding host is refused");
    let (code, _) = request(
        &addr,
        &format!("POST /api/job?kind=scan HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n"),
    );
    assert_eq!(code, 403, "POST without X-Albumen is refused");
    for bad in [
        "/img?p=..%2F..%2Fetc%2Fpasswd",
        "/img?p=%2Fetc%2Fpasswd",
        "/api/proof?path=%2Fetc%2Fpasswd",
    ] {
        let (code, _) = get(&addr, bad);
        assert!(
            code == 404 || code == 400 || code == 403,
            "{bad} gave {code}"
        );
    }
    let (code, _) = request(
        &addr,
        &format!("POST /api/job?kind=delete HTTP/1.1\r\nHost: {addr}\r\nX-Albumen: 1\r\nContent-Length: 0\r\n\r\n"),
    );
    assert_eq!(code, 400, "unknown job kinds are refused");
}

#[test]
fn health_summary_and_next_match_the_cli_word_for_word() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, status) = get(&addr, "/api/status");
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    let dests: Vec<&str> = status["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["dest"].as_str().unwrap())
        .collect();
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .arg("status")
        .args(&dests)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let summary = status["summary"].as_str().unwrap();
    let next = status["summary_next"].as_str().unwrap();
    assert!(summary.starts_with("4 destinations: "), "{summary}");
    assert_eq!(lines[lines.len() - 2], summary);
    assert_eq!(lines[lines.len() - 1], next);
    assert_eq!(status["exit"].as_i64(), out.status.code().map(i64::from));
}

/// The tagline overclaims on health, and the GUI has no exit-0-only screen
/// that shows it, so it must appear nowhere: not on the page (all screens,
/// health included) and not in any API response, whatever the exit code.
#[test]
fn tagline_is_never_shown_on_health_or_after_a_nonzero_exit() {
    const TAGLINE: &str = "bit for bit, provably intact";
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    assert!(page.contains("<section id=\"health\""));
    assert!(!page.contains(TAGLINE), "page shows the tagline");
    let (_, status) = get(&addr, "/api/status");
    let json: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_ne!(json["exit"].as_i64(), Some(0), "demo health exits non-zero");
    assert!(!status.contains(TAGLINE), "health data carries the tagline");
}

#[test]
fn demo_banner_and_verify_failure_copy_are_scoped_not_overclaimed() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    assert!(page.contains(
        "DEMO-ONLY window. Synthetic sample images. Runs on this computer; the window listens on 127.0.0.1 only."
    ));
    assert!(
        page.contains("verify-manifest only read the backup; your photos folder was not opened")
    );
    let lower = page.to_lowercase();
    for bad in [
        "nothing leaves it",
        "your originals were not touched",
        "provably",
        "guaranteed",
        "never lose",
        "protected",
    ] {
        assert!(!lower.contains(bad), "page overclaims: {bad}");
    }
    let (_, status) = get(&addr, "/api/status");
    assert!(!status.contains("\"fresh\""), "status uses the CLI word ok");
    assert!(status.contains("status only read the backup; your photos folder was not opened"));
    assert!(!status.contains("your originals were not touched"));
}

/// After a GUI backup, the ok card says what status checked: the copies match
/// what was backed up. It does not claim anything about the originals.
#[test]
fn ok_card_says_match_what_was_backed_up_not_your_originals() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, status) = get(&addr, "/api/status");
    let json: serde_json::Value = serde_json::from_str(&status).unwrap();
    let dest = json["destinations"][0]["dest"]
        .as_str()
        .unwrap()
        .to_string();
    let enc: String = dest
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let (code, body) = request(
        &addr,
        &format!(
            "POST /api/job?kind=backup&dest={enc} HTTP/1.1\r\nHost: {addr}\r\nX-Albumen: 1\r\nContent-Length: 0\r\n\r\n"
        ),
    );
    assert_eq!(code, 200, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    let mut exit = serde_json::Value::Null;
    for _ in 0..300 {
        let (_, job) = get(&addr, &format!("/api/job?id={id}"));
        exit = serde_json::from_str::<serde_json::Value>(&job).unwrap()["exit"].clone();
        if !exit.is_null() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(exit.as_i64(), Some(0));
    let (_, status) = get(&addr, "/api/status");
    let json: serde_json::Value = serde_json::from_str(&status).unwrap();
    let card = json["destinations"][0]["text"].as_str().unwrap();
    assert_eq!(json["destinations"][0]["state"], "ok", "{card}");
    assert!(
        card.contains("12 items, all match what was backed up"),
        "{card}"
    );
    assert!(!status.contains("all match your originals"));
}

/// iPhoto-style layout: a source list instead of step tabs, a live zoom
/// slider, and CLI plumbing only under "Show details".
#[test]
fn page_is_a_source_list_library_with_plumbing_under_show_details() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    for needle in [
        "<aside id=\"side\">",
        "<h2>LIBRARY</h2>",
        "<h2>BACKUPS</h2>",
        "<button data-s=\"events\"><span>Events</span></button>",
        "<button data-s=\"photos\" class=\"on\"><span>Photos</span></button>",
        "<section id=\"photos\" class=\"view\">",
        "<section id=\"health\" class=\"view\" hidden>",
        "<input type=\"range\" id=\"zoom\"",
        "$('zoom').oninput=",
        "<summary>Show details</summary>",
        ">Info</button>",
        ">Slideshow</button>",
        ">Back up</button>",
        ">Check backup</button>",
        // Honest wording kept exactly.
        "Albumen only reads this folder. Nothing in it is changed, moved or deleted.",
        "Copies new and changed photos to a local or mounted folder, then checks every copy.",
        "One honest state per backup folder.",
        "Re-reads every backed-up copy and keeps a proof file you can open.",
        " items match, checked with SHA-256.",
        "--ok:#1f7a3f;--stale:#b26a00;--bad:#b3261e;--none:#6b7078;--accent:#2b5fd9",
    ] {
        assert!(page.contains(needle), "missing {needle}");
    }
    for gone in [
        "<nav>",
        "1 Choose photos",
        "Not verified (exit",
        ">Edit<",
        ">Share<",
        // Faces and Places only as disabled rows (FIX-4), never live views.
        "data-s=\"faces\"",
        "data-s=\"places\"",
    ] {
        assert!(!page.contains(gone), "still has {gone}");
    }
    // Every "runs:" line and the raw output sit inside a <details>.
    for (at, _) in page.match_indices("class=\"runs\"") {
        let open = page[..at].rfind("<details").expect("runs inside details");
        let close = page[..at].rfind("</details>");
        assert!(close.is_none_or(|c| c < open), "runs line outside details");
    }
    // No monospace font on the home (Photos) screen: only details use it.
    let style = &page[page.find("<style>").unwrap()..page.find("</style>").unwrap()];
    for rule in style.split('}').filter(|r| r.contains("monospace")) {
        assert!(
            rule.contains("details.more"),
            "monospace outside details: {rule}"
        );
    }
}

/// Events come from the capture time stored in each photo, oldest first.
#[test]
fn demo_photos_group_into_four_events_by_capture_date() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, library) = get(&addr, "/api/library");
    let library: serde_json::Value = serde_json::from_str(&library).unwrap();
    let events: Vec<(String, u64)> = library["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["title"].as_str().unwrap().to_string(),
                e["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            ("Oct 3, 2026".to_string(), 3),
            ("Oct 4, 2026".to_string(), 2),
            ("Oct 11, 2026".to_string(), 4),
            ("Oct 18, 2026".to_string(), 3),
        ]
    );
    let first = &library["images"][0];
    assert_eq!(first["taken_from"], "photo metadata", "{first}");
    assert!(first["taken"].as_str().unwrap().starts_with("2026-10-"));
    assert_eq!(library["events"][0]["key"], "2026-10-03 harbour.png");
}

/// Taste gate v1 FIX-1: every element the script looks up by id exists, so
/// draw() cannot throw on a null (the `curEvent-grid` typo left Events empty
/// and stopped Info updating).
#[test]
fn every_script_id_lookup_exists_in_the_page() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    let mut missing = Vec::new();
    for part in page.split("$('").skip(1) {
        let id = &part[..part.find('\'').unwrap()];
        if !page.contains(&format!("id=\"{id}\"")) {
            missing.push(id.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "ids looked up but not in the page: {missing:?}"
    );
    assert!(page.contains("$('event-grid').append(f)"));
    assert!(!page.contains("curEvent'"), "count label find-replace slip");
}

/// Taste gate v1 FIX-1b: clicking a photo refreshes Info before redrawing the
/// grid, so Info updates even if a later redraw step fails.
#[test]
fn clicking_a_photo_updates_info_first() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    assert!(
        page.contains("f.onclick=()=>{selected=p;infoPhoto();draw();}"),
        "photo click must call infoPhoto() before draw()"
    );
    let info = &page[page.find("function infoPhoto()").unwrap()..];
    let info = &info[..info.find('}').unwrap()];
    for id in ["i-name", "i-taken", "info-photo"] {
        assert!(
            info.contains(id) || page.contains(&format!("$('{id}')")),
            "{id}"
        );
    }
}

/// Taste gate v1 FIX-2: photos the browser cannot show get a placeholder
/// tile, not a hidden or broken image.
#[test]
fn heic_and_raw_get_placeholder_tiles() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    assert!(page.contains("preview not available yet"));
    assert!(page.contains("m.preview!==false"));
    let (_, library) = get(&addr, "/api/library");
    let library: serde_json::Value = serde_json::from_str(&library).unwrap();
    assert!(library["images"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["preview"] == true));
}

/// FIX-4: Places and Faces are listed under LIBRARY as disabled
/// 'not built yet' rows: no data-s, so no click opens a view.
#[test]
fn places_and_faces_are_disabled_not_built_yet_rows() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    for name in ["Places", "Faces"] {
        let row = format!(
            "<button class=\"soon\" disabled title=\"{name} is not built yet\"><span>{name}</span><small>not built yet</small></button>"
        );
        assert!(page.contains(&row), "missing {name} row");
        assert!(!page.contains(&format!("data-s=\"{}\"", name.to_lowercase())));
        assert!(!page.contains(&format!("<section id=\"{}\"", name.to_lowercase())));
    }
    let lib = page.find("<h2>LIBRARY</h2>").unwrap();
    let photos = page.find("<span>Photos</span>").unwrap();
    let places = page.find("<span>Places</span>").unwrap();
    let faces = page.find("<span>Faces</span>").unwrap();
    let backups = page.find("<h2>BACKUPS</h2>").unwrap();
    assert!(lib < photos && photos < places && places < faces && faces < backups);
}
