//! M6 X4 with the real binary: while another program holds the library
//! index's writer lock, `albumen gui` still answers every request at once
//! from the last good listing, shows one quiet "pending" flag on
//! `/api/index`, and saves once the lock is free. Synthetic files only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Gui(Child);
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

/// GET `path`; the status, the body and how long the answer took.
fn get(addr: &str, path: &str) -> (u16, String, Duration) {
    let began = Instant::now();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let took = began.elapsed();
    let code = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body, took)
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap()
}

fn wait_ready(addr: &str) -> serde_json::Value {
    for _ in 0..400 {
        let (code, body, _) = get(addr, "/api/library");
        assert_eq!(code, 200);
        let lib = json(&body);
        if lib["status"] == "ready" {
            return lib;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("library never ready");
}

fn paths(lib: &serde_json::Value) -> Vec<String> {
    let mut p: Vec<String> = lib["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect();
    p.sort();
    p
}

#[test]
fn a_pending_index_save_never_blocks_a_gui_request() {
    let library = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for i in 0..40 {
        std::fs::write(library.path().join(format!("IMG_{i:04}.jpg")), b"synthetic").unwrap();
    }

    // First run saves the index (and creates its lock file).
    {
        let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
        assert_eq!(paths(&wait_ready(&addr)).len(), 40);
        let (_, body, _) = get(&addr, "/api/index");
        assert_eq!(json(&body)["pending"], false);
    }
    let lock_path = std::fs::read_dir(index.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().ends_with(".idx.lock"))
        .expect("the index's lock file");

    // Another program holds the writer lock; the library gains a folder.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();
    std::fs::create_dir(library.path().join("new")).unwrap();
    std::fs::write(library.path().join("new/IMG_9000.jpg"), b"synthetic").unwrap();

    let (_gui, addr) = start(library.path(), work.path(), index.path(), home.path());
    let began = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut requests = 0;
    let mut saw_pending = false;
    // The save waits about 2.5 s for the lock; ask all through it.
    while began.elapsed() < Duration::from_secs(6) {
        for path in ["/", "/api/library", "/api/index", "/api/status"] {
            let (code, body, took) = get(&addr, path);
            assert_eq!(code, 200, "{path}");
            slowest = slowest.max(took);
            requests += 1;
            if path == "/api/index" && json(&body)["pending"] == true {
                saw_pending = true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "M6 X4 pending-save requests={requests} slowest_ms={:.1} saw_pending={saw_pending}",
        slowest.as_secs_f64() * 1000.0
    );
    assert!(saw_pending, "/api/index never said a save was pending");
    assert!(
        slowest < Duration::from_millis(500),
        "a request waited {slowest:?} while the save was pending"
    );
    // The listing is whole while the save is owed.
    let lib = wait_ready(&addr);
    assert!(paths(&lib).contains(&"new/IMG_9000.jpg".to_string()));
    assert_eq!(paths(&lib).len(), 41);

    // Free the lock: the background retry saves and the flag clears.
    held.unlock().unwrap();
    drop(held);
    let freed = Instant::now();
    let mut cleared = false;
    while freed.elapsed() < Duration::from_secs(30) {
        let (_, body, _) = get(&addr, "/api/index");
        if json(&body)["pending"] == false {
            cleared = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "M6 X4 cleared={cleared} after_ms={}",
        freed.elapsed().as_millis()
    );
    assert!(cleared, "the pending save never went through");
    // And the page carries the one quiet line.
    let (_, page, _) = get(&addr, "/");
    assert!(page.contains(r#"<p id="index-note""#));
    assert!(page.contains("Updating the library index…"));
    assert!(page.contains("/api/index"));
}
