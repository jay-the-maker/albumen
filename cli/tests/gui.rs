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
    assert!(library.contains("\"files\":19"), "{library}");
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
    assert!(summary.starts_with("4 backups: "), "{summary}");
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
        card.contains("19 items, all match what was backed up"),
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
        "One state per backup folder.",
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
    // One shared fixture for every screen (R5): the IMG_0003 RAW + JPEG
    // pair (2015), HEIC only (2018), Oct 3 with a dateless JPEG ("pier",
    // dated by its file name) and a lone RAW, and
    // the undated CR3 and NEF last.
    assert_eq!(
        events,
        [
            ("Sep 3, 2015".to_string(), 1),
            ("Apr 1, 2018".to_string(), 1),
            ("Oct 3, 2026".to_string(), 5),
            ("Oct 4, 2026".to_string(), 2),
            ("Oct 11, 2026".to_string(), 4),
            ("Oct 18, 2026".to_string(), 3),
            ("No date".to_string(), 2),
        ]
    );
    let first = &library["images"][0];
    assert_eq!(first["taken_from"], "photo metadata", "{first}");
    assert!(first["taken"].as_str().unwrap().starts_with("2026-10-"));
    let oct3 = library["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == "Oct 3, 2026")
        .unwrap();
    let key = oct3["key"].as_str().unwrap();
    assert!(key.ends_with(".png") || key.ends_with(".jpg"), "{oct3}");
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
        if page.contains(&format!("id=\"{id}\"")) {
            continue;
        }
        // Built by the script (b.id='grid-more'): allowed only if the script
        // creates it and every lookup checks it exists before using it.
        if page.contains(&format!(".id='{id}'")) {
            for (at, _) in page.match_indices(&format!("$('{id}')")) {
                // From the lookup to the end of its function.
                let body = &page[at..];
                let line = &body[..body.find("\nfunction ").unwrap_or(body.len())];
                let var = page[..at]
                    .rsplit_once("const ")
                    .and_then(|(_, v)| v.split_once('=').map(|(v, _)| v.trim()))
                    .unwrap_or("");
                assert!(
                    !var.is_empty()
                        && (line.contains(&format!("!{var}")) || line.contains(&format!("{var}?"))),
                    "script-built #{id} is used without an existence check: {line}"
                );
            }
            continue;
        }
        missing.push(id.to_string());
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
    // The grid has one delegated click handler (e7ac85b). Inside it, the
    // first thing after the selection is set must be infoPhoto(): nothing
    // that can throw (highlight, redraw) may run before Info is updated.
    let start = page
        .find("$('grid').addEventListener('click',")
        .expect("the grid's photo click handler");
    let handler = &page[start..start + page[start..].find("});").unwrap()];
    let select = handler
        .find("selected=f.dataset.p;")
        .expect("click sets the selection");
    let after = &handler[select + "selected=f.dataset.p;".len()..];
    assert!(
        after.starts_with("infoPhoto();"),
        "photo click must call infoPhoto() before anything else: {handler}"
    );
    let before = &handler[..select];
    for risky in ["classList", "draw(", "querySelector('figure.sel')"] {
        assert!(
            !before.contains(risky),
            "{risky} runs before the selection and infoPhoto(): {handler}"
        );
    }
    assert!(!after.contains("draw("), "a click must not redraw the grid");
}

/// Taste gate v1 FIX-2: photos the browser cannot show get a placeholder
/// tile, not a hidden or broken image.
#[test]
fn heic_and_raw_get_placeholder_tiles() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    assert!(page.contains("Can't preview this file here. Your original is kept unchanged."));
    assert!(page.contains("m.preview!==false"));
    let (_, library) = get(&addr, "/api/library");
    let library: serde_json::Value = serde_json::from_str(&library).unwrap();
    let no_preview: Vec<&str> = library["images"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["preview"] == false)
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        no_preview,
        [
            "2026-10-03 reef.dng",
            "IMG_0002.heic",
            "IMG_0101.cr3",
            "IMG_0102.nef"
        ]
    );
}

/// M5 AC2-AC4 HEIC extension, end to end: /api/library reports thumb "none"
/// for the HEIC, and /api/thumbs pages the content-keyed index that lives
/// under ALBUMEN_HOME, outside the library.
#[test]
fn heic_thumb_index_is_served_by_page_from_albumen_home() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(["gui", "--demo", "--no-open", "--port", "0", "--work"])
        .arg(work.path())
        .env("ALBUMEN_HOME", home.path())
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
    let _gui = Gui(child);
    let addr = addr.expect("gui prints its URL");
    // The first answer may be "indexing" (the walk runs in the background).
    let mut library = serde_json::Value::Null;
    for _ in 0..200 {
        let (code, body) = get(&addr, "/api/library");
        assert_eq!(code, 200);
        library = serde_json::from_str(&body).unwrap();
        if library["status"] == "ready" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(library["status"], "ready");
    let heic = library["images"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == "IMG_0002.heic")
        .unwrap()
        .clone();
    assert_eq!(heic["thumb"], "none");
    assert_eq!(heic["preview"], false);
    assert!(library["thumbs_generation"].is_u64());
    // The index is built in the background at start (F1); wait for it.
    let mut page = serde_json::Value::Null;
    for _ in 0..200 {
        let (code, body) = get(&addr, "/api/thumbs?offset=0&limit=10");
        assert_eq!(code, 200);
        page = serde_json::from_str(&body).unwrap();
        if !page["entries"].as_array().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // /api/library serves the last background walk; it catches up too.
    let mut generation = 0;
    for _ in 0..200 {
        let (_, library) = get(&addr, "/api/library");
        let library: serde_json::Value = serde_json::from_str(&library).unwrap();
        generation = library["thumbs_generation"].as_u64().unwrap();
        if generation >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(generation >= 1);
    let rows = page["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{page}");
    assert_eq!(rows[0]["path"], "IMG_0002.heic");
    assert_eq!(rows[0]["thumb"], "none");
    assert_eq!(rows[0]["key"].as_str().unwrap().len(), 64);
    let previews = home.path().join("previews");
    let lib_dir = std::fs::read_dir(&previews)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(lib_dir.join("thumbs/v1/index-v1.jsonl").is_file());
    let (_, past) = get(&addr, "/api/thumbs?offset=5&limit=10");
    assert!(past.contains("\"entries\":[]"), "{past}");
}

/// Attributes of an HTML start tag, e.g. `<button class="soon" disabled>`.
fn attrs(tag: &str) -> Vec<(String, String)> {
    let inner = tag.trim_start_matches('<').trim_end_matches('>');
    let mut out = Vec::new();
    let mut rest = inner.split_once(char::is_whitespace).map_or("", |x| x.1);
    while let Some(start) = rest.find(|c: char| !c.is_whitespace()) {
        rest = &rest[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let name = rest[..end].to_ascii_lowercase();
        rest = &rest[end..];
        let mut value = String::new();
        if let Some(r) = rest.strip_prefix('=') {
            let q = r.chars().next().unwrap();
            let close = r[1..].find(q).unwrap() + 1;
            value = r[1..close].to_string();
            rest = &r[close + 1..];
        }
        out.push((name, value));
    }
    out
}

/// Every `<button …>…</button>` in the LIBRARY source list: (attributes, text).
fn library_rows(page: &str) -> Vec<(Vec<(String, String)>, String)> {
    let aside = &page[page.find("<aside id=\"side\">").unwrap()..];
    let aside = &aside[..aside.find("<h2>BACKUPS</h2>").unwrap()];
    aside
        .split("<button")
        .skip(1)
        .map(|b| {
            let open = &b[..b.find('>').unwrap() + 1];
            let body = &b[open.len()..b.find("</button>").unwrap()];
            let mut text = String::new();
            let mut in_tag = false;
            for c in body.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => text.push(c),
                    _ => {}
                }
            }
            (attrs(&format!("<button{open}")), text)
        })
        .collect()
}

/// The words of a script with its `//` and `/* */` comments removed, split
/// on anything that is not a letter, digit or underscore.
fn script_words(script: &str) -> std::collections::BTreeSet<&str> {
    let mut code = Vec::new();
    let mut rest = script;
    while !rest.is_empty() {
        let line = rest.find("//");
        let block = rest.find("/*");
        let (cut, end) = match (line, block) {
            (Some(l), b) if b.is_none_or(|b| l < b) => {
                // `//` inside a URL string (http://) is not a comment.
                if rest[..l].ends_with(':') {
                    (l + 2, l + 2)
                } else {
                    (l, rest[l..].find('\n').map_or(rest.len(), |n| l + n))
                }
            }
            (_, Some(b)) => (b, rest[b..].find("*/").map_or(rest.len(), |n| b + n + 2)),
            _ => (rest.len(), rest.len()),
        };
        code.push(&rest[..cut]);
        rest = &rest[end..];
    }
    code.iter()
        .flat_map(|c| c.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')))
        .filter(|w| !w.is_empty())
        .collect()
}

/// FIX-4, structural (R4): Places and Faces are LIBRARY rows that carry the
/// `disabled` attribute and no `data-s`, there is no view for them, and the
/// script never refers to them, so nothing can open them.
#[test]
fn places_and_faces_are_disabled_not_built_yet_rows() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    let rows = library_rows(&page);
    let names: Vec<&str> = rows
        .iter()
        .map(|(_, t)| t.split("not built yet").next().unwrap().trim())
        .collect();
    assert_eq!(names, ["Events", "Photos", "Places", "Faces"]);
    for (a, text) in &rows[2..] {
        let has = |n: &str| a.iter().any(|(k, _)| k == n);
        assert!(has("disabled"), "{text} row is not disabled: {a:?}");
        assert!(
            !has("data-s") && !has("onclick"),
            "{text} row opens something"
        );
        assert!(text.contains("not built yet"), "{text}");
    }
    for (a, text) in &rows[..2] {
        assert!(!a.iter().any(|(k, _)| k == "disabled"), "{text}");
    }
    // No section, and not a word about them in the script.
    let lower = page.to_ascii_lowercase();
    let script = &lower[lower.rfind("<script>").unwrap()..];
    let words = script_words(script);
    for word in ["places", "faces"] {
        // Whole words in code and strings; comments and words such as
        // "replaces" do not count.
        assert!(!words.contains(word), "script refers to {word}");
        assert!(
            !lower.contains(&format!("id=\"{word}\"")),
            "{word} view exists"
        );
    }
}

/// R1: Info shows a camera time as written and says it has no zone; only a
/// time with an offset is converted to this computer's zone.
/// R2: Backup health shows the CLI's own stamps, local time with a numeric
/// offset ("UTC-7"), and uses the dark chrome. (Behaviour runs in scripts/gui-behaviour.mjs.)
#[test]
fn times_are_labelled_honestly_and_health_is_dark() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    assert!(page.contains("(camera clock, no time zone)"));
    assert!(!page.contains("camera time, no time zone recorded"));
    assert!(page.contains("const DARK_VIEWS=[...LIB_VIEWS,'health'];"));
    assert!(page.contains(".dark .card{"));
    assert!(page.contains("#info{overflow:auto;min-height:0;background:#2f3136;"));
    assert!(!page.contains("#info{overflow:auto;min-height:0;background:#fff"));
    assert!(page.contains("$('health-summary').textContent=st.summary||'';"));
    assert!(
        !page.contains("localStamps"),
        "health re-labels the CLI's times"
    );
    let (_, library) = get(&addr, "/api/library");
    let library: serde_json::Value = serde_json::from_str(&library).unwrap();
    for i in library["images"].as_array().unwrap() {
        if let Some(t) = i["taken"].as_str() {
            assert!(
                !t.ends_with('Z') && !t[10..].contains(['+', '-']),
                "an offset reached the page, so R1's camera-time label would be wrong: {t}"
            );
        }
    }
}

#[test]
fn product_lead_asks_after_fix_7_are_in_the_page() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, page) = get(&addr, "/");
    // 1. Inside an Event the sidebar lights Events.
    assert!(page.contains("const lit=s=>s=='photos'&&curEvent?'events':s;"));
    assert!(page.contains("b.dataset.s==lit(s)"));
    // 2. Info is a pane in the layout grid, not a floating overlay.
    assert!(page.contains(".app.with-info{grid-template-columns:200px 1fr 300px}"));
    assert!(!page.contains("#info{position:fixed"));
    let app = page.find("<div class=\"app\">").unwrap();
    let info = page.find("<div id=\"info\"").unwrap();
    let show = page.find("<div id=\"show\"").unwrap();
    assert!(app < info && info < show);
    assert!(
        page.contains("const z=o==0?'UTC':'UTC'+"),
        "times carry a numeric offset"
    );
    assert!(!page.contains("timeZoneName"), "no zone abbreviations");
    assert!(page.contains("' KB'") && page.contains("' MB'"));
    assert!(!page.contains("im.bytes+' bytes'"));
    assert!(page.contains("'RAW file: '+base(im.raw)"));
    // 4. Same banner words, no yellow.
    assert!(page.contains("DEMO-ONLY window. Synthetic sample images."));
    assert!(!page.contains("#fff4d6") && !page.contains("#6b5100"));
    // 5. Captions are stems.
    assert!(page.contains("figure(p,canShow(p)?stem(p):base(p))"));
    // 6. Smallest zoom: no captions, placeholder shows only the format.
    assert!(page.contains(".mini #grid figcaption{display:none}"));
    assert!(page.contains(".mini .ph .more{display:none}"));
    // 7. Search matches a pair's RAW name.
    assert!(page.contains("[p,m&&m.raw]"));
}

/// The health screen shows the same words as `albumen status` for a folder
/// with no backup record.
#[test]
fn not_backed_up_card_is_word_for_word_the_cli() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, status) = get(&addr, "/api/status");
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    let mut seen = 0;
    for d in status["destinations"].as_array().unwrap() {
        if d["state"] != "not backed up" {
            continue;
        }
        let dest = d["dest"].as_str().unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
            .args(["status", dest])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
        let cli = String::from_utf8_lossy(&out.stdout);
        let gui = d["text"].as_str().unwrap();
        assert!(cli.starts_with(gui), "gui:\n{gui}\ncli:\n{cli}");
        assert!(gui.contains("next: to back up this folder, run albumen backup --from "));
        assert_eq!(d["next"].as_str().unwrap(), gui.lines().last().unwrap());
        seen += 1;
    }
    assert!(seen >= 1);
}
