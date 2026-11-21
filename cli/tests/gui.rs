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
    start_with(&["gui", "--demo"], work)
}

fn start_with(args: &[&str], work: &std::path::Path) -> (Gui, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .args(["--no-open", "--port", "0", "--work"])
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

/// Status, Content-Type, and raw body bytes.
fn get_bytes(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let code = head[9..12].parse().unwrap();
    let mime = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Type: "))
        .unwrap_or("")
        .to_string();
    (code, mime, raw[split + 4..].to_vec())
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

/// A listing of every file under `dir`: path, size, mtime, mode. Equal
/// before and after means nothing in the tree was written.
fn tree_state(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            }
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions());
            #[cfg(not(unix))]
            let mode = 0;
            out.push(format!(
                "{} {} {:?} {mode}",
                e.path().display(),
                m.len(),
                m.modified().unwrap()
            ));
        }
    }
    out.sort();
    out
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn write_jpeg(path: &std::path::Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
        .encode_image(&img)
        .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A real (non-demo) library: no demo banner flag, the folder name for the
/// title, folder and date for each photo, a size summary, and small JPEG
/// previews cached only in the work folder. The library is unchanged.
#[test]
fn real_library_groups_photos_and_serves_previews_from_the_work_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("2015");
    let event = lib.join("2015 06 Family Reunion");
    std::fs::create_dir_all(&event).unwrap();
    write_jpeg(&event.join("1443399020047.jpg"), 1600, 1200);
    write_jpeg(&lib.join("top.jpeg"), 800, 600);
    std::fs::write(lib.join("IMG_0001.HEIC"), b"heic bytes").unwrap();
    let before = tree_state(&lib);
    let work = dir.path().join("work");
    let (_gui, addr) = start_with(&["gui", "--library", lib.to_str().unwrap()], &work);

    let (code, page) = get(&addr, "/");
    assert_eq!(code, 200);
    assert!(page.contains("id=\"real-note\""));
    assert!(page.contains("<details class=\"dev\"><summary>Command details</summary><p class=\"runs\" id=\"runs-photos\">"));
    assert!(page.contains("i.loading='lazy'") && page.contains("'/thumb?p='"));
    assert!(
        page.contains("id=\"src\"")
            && page.contains("id=\"size\"")
            && page.contains("id=\"viewer\"")
    );
    for name in ["iphoto", "apple", "google", "icloud"] {
        // `-apple-system` is the CSS keyword for the system font, not a name.
        let text = page.to_lowercase().replace("-apple-system", "");
        assert!(!text.contains(name), "page names {name}");
    }

    let (code, body) = get(&addr, "/api/library");
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["demo"], false);
    assert_eq!(v["name"], "2015");
    assert_eq!(v["files"], 3);
    let images = v["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "HEIC is counted but not in the grid");
    let ev = images
        .iter()
        .find(|i| i["path"] == "2015 06 Family Reunion/1443399020047.jpg")
        .unwrap();
    assert_eq!(ev["folder"], "2015 06 Family Reunion");
    assert_eq!(ev["date_source"], "file", "no scan yet: file dates");
    assert!(ev["date"].as_str().unwrap().ends_with('Z'));
    let top = images.iter().find(|i| i["path"] == "top.jpeg").unwrap();
    assert_eq!(top["folder"], "");
    let original = std::fs::metadata(event.join("1443399020047.jpg"))
        .unwrap()
        .len();
    assert!(v["bytes"].as_u64().unwrap() > original);

    let q = "/thumb?p=2015%2006%20Family%20Reunion%2F1443399020047.jpg";
    let (code, mime, thumb) = get_bytes(&addr, q);
    assert_eq!(code, 200, "{}", String::from_utf8_lossy(&thumb));
    assert_eq!(mime, "image/jpeg");
    assert!(
        (thumb.len() as u64) < original,
        "preview is smaller than the original"
    );
    let (w, h) = image::load_from_memory(&thumb)
        .unwrap()
        .into_rgb8()
        .dimensions();
    assert_eq!((w, h), (480, 360));
    let cached: Vec<_> = tree_state(&work.join("thumbs"))
        .into_iter()
        .filter(|l| l.contains(".jpg "))
        .collect();
    assert_eq!(cached.len(), 1, "one preview cached in --work/thumbs");
    let (code, _, again) = get_bytes(&addr, q);
    assert_eq!(code, 200);
    assert_eq!(again, thumb, "second request is the cached preview");

    for bad in [
        "/thumb?p=..%2Fwork%2Fx.jpg",
        "/thumb?p=IMG_0001.HEIC",
        "/thumb?p=%2Fetc%2Fpasswd",
    ] {
        let (code, _) = get(&addr, bad);
        assert_eq!(code, 404, "{bad}");
    }
    let (code, _, full) = get_bytes(&addr, "/img?p=top.jpeg");
    assert_eq!(code, 200, "the viewer still gets the full original");
    assert_eq!(full, std::fs::read(lib.join("top.jpeg")).unwrap());

    assert_eq!(tree_state(&lib), before, "the library is unchanged");
}

/// The work folder, and so the preview cache, may not be inside the library.
#[test]
fn work_folder_inside_the_library_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    write_jpeg(&lib.join("a.jpg"), 64, 48);
    let before = tree_state(&lib);
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(["gui", "--library"])
        .arg(&lib)
        .args(["--no-open", "--port", "0", "--work"])
        .arg(lib.join("albumen-work"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!lib.join("albumen-work").exists());
    assert_eq!(tree_state(&lib), before);
}

#[test]
fn demo_library_says_it_is_the_demo() {
    let work = tempfile::tempdir().unwrap();
    let (_gui, addr) = start(work.path());
    let (_, body) = get(&addr, "/api/library");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["demo"], true);
    let (code, mime, _) = get_bytes(
        &addr,
        &format!("/thumb?p={}", pct(v["images"][0]["path"].as_str().unwrap())),
    );
    assert_eq!(code, 200);
    assert_eq!(mime, "image/png", "non-JPEG previews are the original");
}
