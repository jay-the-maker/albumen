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
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut addr = None;
    for line in out.lines().map_while(Result::ok) {
        if let Some(url) = line.strip_prefix("albumen gui: http://") {
            addr = Some(url.trim_end_matches('/').to_string());
            break;
        }
    }
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
