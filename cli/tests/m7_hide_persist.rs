//! M7 step 1 (A8, "deleted photos came back"): a photo hidden with
//! `albumen receive hide` must stay out of the GUI grid after a rescan and
//! after an index rebuild. The hide lives only in the
//! `{name}.albumen-tombstone` sidecar; the M6 index has no hide field and the
//! grid does not read tombstones, so this test is expected to FAIL until
//! that is fixed. Synthetic files only; originals are checked bit-exact.
//! Correctness only, no timing.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

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
