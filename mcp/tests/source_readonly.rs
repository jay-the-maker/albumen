// SPDX-License-Identifier: Apache-2.0

//! Every MCP tool leaves the source it read byte-for-byte identical.
//!
//! Unix only: the proof records mode and extended attributes, and the
//! `xattr` crate does not build on Windows.

#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    rel: String,
    is_dir: bool,
    bytes: Vec<u8>,
    mtime: SystemTime,
    mode: u32,
    xattr: Vec<u8>,
}

fn xattr_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "albumen.trial"
    } else {
        "user.albumen-trial"
    }
}

struct Client {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Client {
    fn start(library: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"))
            .arg("--library")
            .arg(library)
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn albumen-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        write_message(
            &mut self.stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        let message = read_message(&mut self.stdout);
        assert_eq!(message["id"], id, "{message}");
        assert!(message.get("error").is_none(), "rpc error: {message}");
        message["result"].clone()
    }

    fn notify(&mut self, method: &str) {
        write_message(
            &mut self.stdin,
            &json!({"jsonrpc": "2.0", "method": method}),
        );
    }

    fn close(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait");
        assert!(status.success(), "server exited {status}");
    }
}

fn write_message(out: &mut impl Write, message: &Value) {
    let body = serde_json::to_vec(message).unwrap();
    write!(out, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    out.write_all(&body).unwrap();
    out.flush().unwrap();
}

fn read_message(input: &mut impl BufRead) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line).unwrap();
        assert!(read > 0, "server closed stdout");
        if line == "\n" || line == "\r\n" {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = length.expect("Content-Length");
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn handshake(client: &mut Client) {
    let init = client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "source-readonly", "version": "0.0.1"}
        }),
    );
    assert_eq!(init["protocolVersion"], "2024-11-05");
    client.notify("notifications/initialized");
}

fn tool_body(result: &Value) -> (bool, Value) {
    let is_error = result["isError"].as_bool().unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed = if is_error {
        Value::String(text.to_string())
    } else {
        serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
    };
    (is_error, parsed)
}

fn stamp(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    xattr::set(path, xattr_name(), b"trial").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_000, 11);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn plant_library(root: &Path) {
    fs::create_dir_all(root.join("trip")).unwrap();
    xattr::set(root, xattr_name(), b"root").unwrap();
    stamp(&root.join("trip/IMG.jpg"), b"jpeg-bytes");
    stamp(&root.join("trip/note.txt"), b"note");
}

fn plant_upload(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    xattr::set(root, xattr_name(), b"upload").unwrap();
    let path = root.join("photo.jpg");
    stamp(&path, b"still");
    stamp(&root.join("photo.xmp"), b"<xmp/>");
    path
}

fn capture(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    fn push(path: &Path, rel: &str, out: &mut Vec<Entry>) {
        let meta = fs::symlink_metadata(path).unwrap();
        out.push(Entry {
            rel: rel.to_string(),
            is_dir: meta.is_dir(),
            bytes: if meta.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            },
            mtime: meta.modified().unwrap(),
            mode: meta.mode(),
            xattr: xattr::get(path, xattr_name()).unwrap().unwrap_or_default(),
        });
    }
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<Entry>) {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let name = name.to_str().unwrap();
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            push(&path, &rel, out);
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                walk(&path, &rel, out);
            }
        }
    }
    push(root, "", &mut out);
    walk(root, "", &mut out);
    out
}

fn lock_writes(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(path) = stack.pop() {
        if fs::symlink_metadata(&path).unwrap().is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
        paths.push(path);
    }
    for path in paths {
        let mode = fs::symlink_metadata(&path).unwrap().mode() & !0o222;
        let mut perms = fs::symlink_metadata(&path).unwrap().permissions();
        perms.set_mode(mode);
        fs::set_permissions(&path, perms).unwrap();
    }
}

fn call(client: &mut Client, id: i64, name: &str, arguments: Value) -> (bool, Value) {
    tool_body(&client.request(
        id,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    ))
}

fn exercise(library: &Path, upload_from: &Path, outside: &Path, read_only_library: bool) {
    let mut reading = Client::start(library, &["--enable-export", "--enable-upload"]);
    handshake(&mut reading);
    let (err, body) = call(&mut reading, 2, "search", json!({"query": "IMG"}));
    assert!(!err, "{body}");
    let (err, body) = call(&mut reading, 3, "browse", json!({"prefix": "trip"}));
    assert!(!err, "{body}");
    let plan = outside.join("plan");
    let (err, body) = call(
        &mut reading,
        4,
        "export",
        json!({"to": plan.display().to_string()}),
    );
    assert!(!err, "{body}");
    assert_eq!(body["dry_run"], true, "{body}");
    assert!(!plan.exists(), "{body}");
    let (err, body) = call(
        &mut reading,
        5,
        "upload",
        json!({"path": upload_from.display().to_string()}),
    );
    assert!(!err, "{body}");
    assert_eq!(body["dry_run"], true, "{body}");
    let inside = library.join("inside");
    let (err, body) = call(
        &mut reading,
        6,
        "export",
        json!({"to": inside.display().to_string()}),
    );
    assert!(err, "{body}");
    assert!(body.as_str().unwrap().contains("inside"), "{body}");
    assert!(!inside.exists());
    reading.close();

    if read_only_library {
        return;
    }

    let mut writing = Client::start(library, &["--enable-export", "--enable-upload", "--apply"]);
    handshake(&mut writing);
    let copy = outside.join("copy");
    let (err, body) = call(
        &mut writing,
        2,
        "export",
        json!({
            "to": copy.display().to_string(),
            "manifest": outside.join("manifest.txt").display().to_string(),
        }),
    );
    assert!(!err, "{body}");
    assert!(copy.join("trip/IMG.jpg").is_file(), "{body}");
    let library_for_upload = outside.join("uploaded");
    fs::create_dir(&library_for_upload).unwrap();
    let mut uploader = Client::start(&library_for_upload, &["--enable-upload", "--apply"]);
    handshake(&mut uploader);
    let (err, body) = call(
        &mut uploader,
        2,
        "upload",
        json!({"path": upload_from.display().to_string()}),
    );
    assert!(!err, "{body}");
    assert!(library_for_upload.join("photo.jpg").is_file(), "{body}");
    uploader.close();
    writing.close();
}

fn run_case(read_only: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir(&library).unwrap();
    plant_library(&library);
    let upload_root = tmp.path().join("upload-src");
    let upload_from = plant_upload(&upload_root);
    if read_only {
        lock_writes(&library);
        lock_writes(&upload_root);
    }
    let library_before = capture(&library);
    let upload_before = capture(&upload_root);
    exercise(&library, &upload_from, &tmp.path().join("out"), read_only);
    assert_eq!(capture(&library), library_before);
    assert_eq!(capture(&upload_root), upload_before);
}

#[test]
fn writable_sources_are_unchanged_by_every_tool() {
    run_case(false);
}

#[test]
fn read_only_sources_are_unchanged_by_every_tool() {
    run_case(true);
}
