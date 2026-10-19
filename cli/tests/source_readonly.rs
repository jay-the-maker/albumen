// SPDX-License-Identifier: Apache-2.0

//! Every `albumen` command leaves a source tree byte-for-byte identical.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
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

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn run(args: &[impl AsRef<std::ffi::OsStr>]) -> (bool, String) {
    let output = albumen().args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn stamp(path: &Path, bytes: &[u8], mtime: SystemTime) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    xattr::set(path, xattr_name(), b"trial").unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn plant(root: &Path) {
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_789);
    fs::create_dir_all(root.join("trip")).unwrap();
    xattr::set(root, xattr_name(), b"root").unwrap();
    xattr::set(root.join("trip"), xattr_name(), b"trip").unwrap();
    stamp(&root.join("a.txt"), b"alpha", mtime);
    stamp(&root.join("trip/IMG.jpg"), b"jpeg-bytes", mtime);
    stamp(&root.join("trip/IMG.xmp"), b"<xmp/>", mtime);
    stamp(&root.join("odd names/naïve, photo 😀.jpg"), b"odd", mtime);
}

fn capture(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    fn push(path: &Path, rel: &str, out: &mut Vec<Entry>) {
        let meta = fs::symlink_metadata(path).unwrap();
        let bytes = if meta.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        out.push(Entry {
            rel: rel.to_string(),
            is_dir: meta.is_dir(),
            bytes,
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
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
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

fn exercise(root: &Path, outside: &Path) {
    let (ok, text) = run(&[
        "export",
        "--from",
        root.to_str().unwrap(),
        "--to",
        outside.join("plan").to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(!outside.join("plan").exists(), "{text}");

    let (ok, text) = run(&[
        "export",
        "--from",
        root.to_str().unwrap(),
        "--to",
        outside.join("copy").to_str().unwrap(),
        "--manifest",
        outside.join("export.txt").to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    assert!(outside.join("copy").join("a.txt").is_file(), "{text}");

    let manifest = outside.join("scan.jsonl");
    let (ok, text) = run(&[
        "scan",
        root.to_str().unwrap(),
        "--manifest",
        manifest.to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        "verify-manifest",
        "--manifest",
        manifest.to_str().unwrap(),
        "--against",
        root.to_str().unwrap(),
    ]);
    assert!(ok, "{text}");

    let inside = root.join("inside");
    let (ok, text) = run(&[
        "export",
        "--from",
        root.to_str().unwrap(),
        "--to",
        inside.to_str().unwrap(),
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("inside"), "{text}");
    assert!(text.contains("read-only"), "{text}");
    assert!(!inside.exists(), "{text}");

    let (ok, text) = run(&[
        "import-takeout",
        "--from",
        root.to_str().unwrap(),
        "--to",
        outside.join("takeout").to_str().unwrap(),
        "--report",
        outside.join("takeout.jsonl").to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(!outside.join("takeout").exists(), "{text}");

    let (ok, text) = run(&[
        "import-icloud",
        "--from",
        root.to_str().unwrap(),
        "--to",
        outside.join("icloud").to_str().unwrap(),
        "--report",
        outside.join("icloud.jsonl").to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(!outside.join("icloud").exists(), "{text}");

    let (ok, text) = run(&[
        "import-takeout",
        "--from",
        root.to_str().unwrap(),
        "--to",
        outside.join("takeout-copy").to_str().unwrap(),
        "--report",
        outside.join("takeout-copy.jsonl").to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    assert!(outside.join("takeout-copy").join("trip/IMG.jpg").is_file());

    let inside_import = root.join("imported");
    let (ok, text) = run(&[
        "import-icloud",
        "--from",
        root.to_str().unwrap(),
        "--to",
        inside_import.to_str().unwrap(),
        "--report",
        outside.join("icloud-inside.jsonl").to_str().unwrap(),
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(!inside_import.exists(), "{text}");

    let nested = root.join("nested").join("out.jsonl");
    let (ok, text) = run(&[
        "scan",
        root.to_str().unwrap(),
        "--manifest",
        nested.to_str().unwrap(),
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("inside"), "{text}");
    assert!(!root.join("nested").exists(), "{text}");
}

fn prepare(read_only: bool) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    fs::create_dir(&root).unwrap();
    plant(&root);
    if read_only {
        lock_writes(&root);
    }
    (tmp, root)
}

#[test]
fn writable_source_is_unchanged_by_every_command() {
    let (tmp, root) = prepare(false);
    let before = capture(&root);
    exercise(&root, &tmp.path().join("out"));
    assert_eq!(capture(&root), before);
}

#[test]
fn read_only_source_is_unchanged_by_every_command() {
    let (tmp, root) = prepare(true);
    let before = capture(&root);
    exercise(&root, &tmp.path().join("out"));
    assert_eq!(capture(&root), before);
}

#[test]
fn symlink_destination_into_the_source_is_refused() {
    let (tmp, root) = prepare(false);
    let before = capture(&root);
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let (ok, text) = run(&[
        "export",
        "--from",
        root.to_str().unwrap(),
        "--to",
        link.to_str().unwrap(),
        "--execute",
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("inside"), "{text}");
    assert_eq!(capture(&root), before);
    let _ = File::open(root.join("a.txt")).unwrap();
}
