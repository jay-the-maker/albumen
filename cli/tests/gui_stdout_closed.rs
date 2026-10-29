// SPDX-License-Identifier: Apache-2.0

//! `albumen gui` keeps serving when nobody reads its stdout, e.g.
//! `albumen gui --demo | head -1`. Writing the banner to a closed pipe
//! (EPIPE) must not panic the server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn(port: u16, work: &std::path::Path, stdout: Stdio) -> Child {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args([
            "gui",
            "--demo",
            "--no-open",
            "--port",
            &port.to_string(),
            "--work",
        ])
        .arg(work)
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// GET / until it answers or 10 s pass. Returns the status line.
fn alive(port: u16) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            let raw = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
            let mut text = String::new();
            if s.write_all(raw.as_bytes()).is_ok() && s.read_to_string(&mut text).is_ok() {
                return text.lines().next().map(str::to_string);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Kill the child and return everything it wrote to stderr.
fn finish(mut child: Child) -> String {
    let _ = child.kill();
    let _ = child.wait();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    err
}

fn check(port: u16, child: Child) {
    std::thread::sleep(Duration::from_millis(300));
    let status = alive(port);
    let err = finish(child);
    assert!(!err.contains("panicked"), "gui panicked:\n{err}");
    let status = status.unwrap_or_else(|| panic!("gui stopped serving; stderr:\n{err}"));
    assert!(status.starts_with("HTTP/1.1 200"), "{status}\n{err}");
}

#[test]
fn gui_serves_when_stdout_is_closed_before_the_banner() {
    let work = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    check(port, spawn(port, work.path(), writer.into()));
}

#[test]
fn gui_serves_after_the_reader_hangs_up_on_the_first_line() {
    let work = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = spawn(port, work.path(), Stdio::piped());
    // Read up to and including the banner's first line, then hang up.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.starts_with("albumen gui: http://127.0.0.1:") {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "no banner");
    }
    drop(out);
    check(port, child);
}
