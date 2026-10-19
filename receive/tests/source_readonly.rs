// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` does not change a tree it is only planning against.
//!
//! Unix only: the proof records mode and extended attributes, and the
//! `xattr` crate does not build on Windows.

#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

fn stamp(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    xattr::set(path, xattr_name(), b"trial").unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(1_400_000_000, 9)),
        )
        .unwrap();
}

fn plant(root: &Path) {
    fs::create_dir_all(root.join("roll")).unwrap();
    xattr::set(root, xattr_name(), b"root").unwrap();
    stamp(&root.join("roll/a.jpg"), b"jpeg");
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

fn receive(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args(args)
        .output()
        .expect("run albumen-receive");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn exercise(root: &Path, other: &Path) {
    let (ok, text) = receive(&[
        "--to",
        root.to_str().unwrap(),
        "--user",
        "camera",
        "--password",
        "secret",
        "--webdav",
        "127.0.0.1:9",
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(text.contains("password: set"), "{text}");
    assert!(!text.contains("secret"), "{text}");

    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args([
            "--to",
            other.to_str().unwrap(),
            "--user",
            "camera",
            "--password",
            "secret",
            "--webdav",
            "127.0.0.1:0",
            "--apply",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn receiver");
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).expect("listen line");
    assert!(line.contains("webdav"), "{line}");
    let _ = child.kill();
    let _ = child.wait();
    assert!(other.is_dir());
}

fn run_case(read_only: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("archive");
    fs::create_dir(&root).unwrap();
    plant(&root);
    if read_only {
        lock_writes(&root);
    }
    let before = capture(&root);
    exercise(&root, &tmp.path().join("library"));
    assert_eq!(capture(&root), before);
}

#[test]
fn writable_archive_is_unchanged() {
    run_case(false);
}

#[test]
fn read_only_archive_is_unchanged() {
    run_case(true);
}
