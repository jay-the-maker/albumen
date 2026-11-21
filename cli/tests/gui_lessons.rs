//! Round-1 lessons from the GUI, kept as checks instead of notes.
//!
//! 1. No state is ever written inside `--library`, whatever the page does.
//! 2. The page names no other company or product.
//! 3. Every date the page shows or receives is ISO 8601.
//! 4. A symlink or a control-character name in the library is skipped and
//!    counted; it never turns the grid into 0 photos.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use sha2::Digest as _;

struct Gui(Child);
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(lib: &Path, work: &Path) -> (Gui, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .arg("gui")
        .arg("--library")
        .arg(lib)
        .args(["--no-open", "--port", "0", "--work"])
        .arg(work)
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

/// Status and body of one request. `body` is sent with a Content-Length.
pub fn call(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nX-Albumen: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let code = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body)
}

fn get(addr: &str, path: &str) -> (u16, String) {
    call(addr, "GET", path, "")
}

/// Path, kind, size, mtime, mode and SHA-256 of every entry, symlinks not
/// followed. Equal before and after means nothing in the tree changed.
fn fingerprint(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let m = std::fs::symlink_metadata(e.path()).unwrap();
            let kind = if m.file_type().is_symlink() {
                format!("link->{}", std::fs::read_link(e.path()).unwrap().display())
            } else if m.is_dir() {
                stack.push(e.path());
                "dir".to_string()
            } else {
                format!(
                    "{:x}",
                    sha2::Sha256::digest(std::fs::read(e.path()).unwrap())
                )
            };
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions());
            #[cfg(not(unix))]
            let mode = 0;
            out.push(format!(
                "{:?} {kind} {} {:?} {mode}",
                e.path(),
                m.len(),
                m.modified().unwrap()
            ));
        }
    }
    out.sort();
    out
}

fn write_jpeg(path: &Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode_image(&img)
        .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A small library with two events, a symlink and a control-character name.
fn library(root: &Path) -> std::path::PathBuf {
    let lib = root.join("lib");
    for ev in ["2015 06 Reunion", "2015 07 Beach"] {
        std::fs::create_dir_all(lib.join(ev)).unwrap();
        for i in 0..3 {
            write_jpeg(&lib.join(ev).join(format!("IMG_{i}.jpg")), 64, 48);
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            lib.join("2015 06 Reunion/IMG_0.jpg"),
            lib.join("2015 07 Beach/link.jpg"),
        )
        .unwrap();
        write_jpeg(&lib.join("2015 07 Beach/bad\u{7}name.jpg"), 64, 48);
    }
    lib
}

/// Every endpoint the page calls, GET and POST, leaves the library as it
/// was: same entries, bytes, times and modes. State goes to --work only.
#[test]
fn no_state_is_written_inside_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let work = dir.path().join("work");
    let before = fingerprint(&lib);
    let (_gui, addr) = start(&lib, &work);
    for path in [
        "/",
        "/api/library",
        "/api/status",
        "/api/overlay",
        "/thumb?p=2015%2006%20Reunion%2FIMG_0.jpg",
        "/img?p=2015%2007%20Beach%2FIMG_1.jpg",
    ] {
        let (code, _) = get(&addr, path);
        assert!(code == 200 || code == 404, "{path}: {code}");
    }
    for (path, body) in overlay_writes() {
        let (code, text) = call(&addr, "POST", path, body);
        assert!(code == 200 || code == 404, "{path} {body}: {code} {text}");
    }
    assert_eq!(fingerprint(&lib), before, "the library is unchanged");
    assert!(work.join("thumbs").exists(), "previews went to --work");
}

/// Every edit the page can send. Each new write path is added here so this
/// test covers it.
fn overlay_writes() -> Vec<(&'static str, &'static str)> {
    vec![]
}

/// A symlink and a control-character name are left out, counted and named.
/// The other photos are all still shown.
#[cfg(unix)]
#[test]
fn odd_entries_are_skipped_and_counted_never_an_empty_grid() {
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let (_gui, addr) = start(&lib, &dir.path().join("work"));
    let (code, body) = get(&addr, "/api/library");
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["images"].as_array().unwrap().len(), 6, "{body}");
    assert_eq!(v["skipped"]["count"], 2, "{body}");
    assert_eq!(v["skipped"]["by_reason"]["symlink"], 1);
    assert_eq!(v["skipped"]["by_reason"]["unsafe name"], 1);
    let shown: Vec<&str> = v["skipped"]["examples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(
        shown.contains(&"2015 07 Beach/bad\\u{7}name.jpg"),
        "{shown:?}"
    );
    let (_, page) = get(&addr, "/");
    assert!(
        page.contains("id=\"skip-note\""),
        "the page has a place to say so"
    );
}

/// Words that name another company or product. UI text stays about what
/// Albumen does, never framed against anyone.
const OTHER_NAMES: &[&str] = &[
    "iphoto",
    "apple",
    "google",
    "icloud",
    "picasa",
    "lightroom",
    "adobe",
    "microsoft",
    "windows photos",
    "amazon",
    "dropbox",
    "flickr",
    "facebook",
    "instagram",
    "frame.io",
    "takeout",
];

/// CSS font keywords that contain a name but are not UI text.
const FONT_KEYWORDS: &[&str] = &["-apple-system", "blinkmacsystemfont"];

/// Every file in src/gui: the page and the Rust that sends text to it.
fn ui_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    assert!(out.iter().any(|(n, _)| n == "page.html"));
    out
}

#[test]
fn ui_text_names_no_other_company_or_product() {
    for (file, text) in ui_sources() {
        let mut lower = text.to_lowercase();
        for k in FONT_KEYWORDS {
            lower = lower.replace(k, "");
        }
        for name in OTHER_NAMES {
            assert!(!lower.contains(name), "{file} names {name:?}");
        }
    }
}

/// The page formats dates only through its ISO helpers: no month-name
/// tables and no locale date formatting, which would print `Jun 20, 2015`
/// or `20/06/2015` depending on the computer.
#[test]
fn page_dates_are_iso_only() {
    let page = include_str!("../src/gui/page.html");
    for bad in [
        "toLocaleDateString",
        "toLocaleString",
        "toLocaleTimeString",
        "DateTimeFormat",
        "'Jan'",
        "\"Jan\"",
        "January",
    ] {
        assert!(!page.contains(bad), "page.html uses {bad}");
    }
    assert!(page.contains("function day(d){return d?d.slice(0,10):'';}"));
}

/// Dates the API sends are ISO 8601, with or without a trailing Z.
#[test]
fn api_dates_are_iso() {
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let (_gui, addr) = start(&lib, &dir.path().join("work"));
    let (_, body) = get(&addr, "/api/library");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let iso = |s: &str| {
        let b = s.as_bytes();
        (s.len() == 19 || (s.len() == 20 && s.ends_with('Z')))
            && b.iter().enumerate().take(19).all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                _ => c.is_ascii_digit(),
            })
    };
    for im in v["images"].as_array().unwrap() {
        for key in ["date", "mtime"] {
            let d = im[key].as_str().unwrap();
            assert!(iso(d), "{key} {d} is not ISO 8601");
        }
    }
}

/// Only these GUI files may call the write API (`albumen_ingest::dest`).
/// Everything else saves through them, so a new write path can't skip the
/// library check. Test modules are ignored.
const GUI_WRITERS: &[&str] = &["thumb.rs", "work.rs"];

#[test]
fn only_the_work_folder_modules_write() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || GUI_WRITERS.contains(&name.as_str()) || name == "demo.rs" {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (n, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("dest::"),
                "src/gui/{name}:{}: writes with dest:: directly; save through work::WorkDir instead",
                n + 1
            );
        }
    }
}
