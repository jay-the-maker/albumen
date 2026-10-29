// SPDX-License-Identifier: Apache-2.0

//! Every `albumen` command leaves a source tree byte-for-byte identical.
//!
//! Unix only: the proof records mode and extended attributes, and the
//! `xattr` crate does not build on Windows.

#![cfg(unix)]

use std::fs::{self, File, OpenOptions};
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

#[cfg(unix)]
fn xattr_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "albumen.trial"
    } else {
        "user.albumen-trial"
    }
}

fn meta_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
}

fn locked_mode(mode: u32) -> u32 {
    #[cfg(unix)]
    {
        mode & !0o222
    }
    #[cfg(windows)]
    {
        mode | 1
    }
}

fn apply_mode(path: &Path, mode: u32) {
    let mut perms = fs::symlink_metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(mode);
    }
    #[cfg(windows)]
    {
        perms.set_readonly(mode & 1 != 0);
    }
    fs::set_permissions(path, perms).unwrap();
}

fn mark_extra(path: &Path, bytes: &[u8]) {
    #[cfg(unix)]
    {
        xattr::set(path, xattr_name(), bytes).unwrap();
    }
    #[cfg(windows)]
    {
        let mut name = path.as_os_str().to_os_string();
        name.push(":albumen");
        fs::write(name, bytes).unwrap();
    }
}

fn read_extra(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        xattr::get(path, xattr_name()).unwrap().unwrap_or_default()
    }
    #[cfg(windows)]
    {
        let mut name = path.as_os_str().to_os_string();
        name.push(":albumen");
        fs::read(name).unwrap_or_default()
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
    mark_extra(path, b"trial");
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
    mark_extra(root, b"root");
    mark_extra(&root.join("trip"), b"trip");
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
            mode: meta_mode(&meta),
            xattr: read_extra(path),
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
        let mode = locked_mode(meta_mode(&fs::symlink_metadata(&path).unwrap()));
        apply_mode(&path, mode);
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

    let (ok, text) = run(&["ledger", "--library", root.to_str().unwrap()]);
    assert!(ok, "{text}");
    assert!(text.contains("objects:"), "{text}");
    assert!(text.contains("a.txt"), "{text}");
    assert!(text.contains("trip/IMG.jpg"), "{text}");

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
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&root, &link).unwrap();
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

/// Every entry under `root` (root included): kind, size, SHA-256 of the
/// bytes, mtime to the nanosecond, and full mode. Sorted, so a new or
/// removed file shows up as a listing difference.
fn digest(root: &Path) -> Vec<(String, bool, u64, String, i64, i64, u32)> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    capture(root)
        .into_iter()
        .map(|entry| {
            let meta = fs::symlink_metadata(root.join(&entry.rel)).unwrap();
            let sha = Sha256::digest(&entry.bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            (
                entry.rel,
                entry.is_dir,
                meta.size() * u64::from(!entry.is_dir),
                sha,
                meta.mtime(),
                meta.mtime_nsec(),
                meta.mode(),
            )
        })
        .collect()
}

/// Gives write permission back to the user on drop, so a failed assert
/// still lets the temp dir be cleaned up.
struct Unlock(PathBuf);
impl Drop for Unlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![self.0.clone()];
        while let Some(path) = stack.pop() {
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let mut perms = meta.permissions();
            perms.set_mode(meta.mode_bits() | 0o200);
            let _ = fs::set_permissions(&path, perms);
            if meta.is_dir() {
                if let Ok(dir) = fs::read_dir(&path) {
                    stack.extend(dir.flatten().map(|e| e.path()));
                }
            }
        }
    }
}

trait ModeBits {
    fn mode_bits(&self) -> u32;
}
impl ModeBits for fs::Metadata {
    fn mode_bits(&self) -> u32 {
        use std::os::unix::fs::MetadataExt;
        self.mode() & 0o7777
    }
}

#[test]
fn backup_twice_and_verify_never_write_the_source_even_when_locked() {
    let (tmp, root) = prepare(false);
    let _unlock = Unlock(root.clone());
    let mtime = UNIX_EPOCH + Duration::new(1_500_000_000, 987_654_321);
    // Extra shapes for backup: a hidden file, a read-only file, a deeper dir.
    stamp(&root.join(".hidden-note"), b"hidden", mtime);
    stamp(
        &root.join("trip/day 2/deep/IMG_0002.jpg"),
        b"deep-jpeg",
        mtime,
    );
    stamp(&root.join("trip/locked.jpg"), b"locked-jpeg", mtime);
    apply_mode(&root.join("trip/locked.jpg"), 0o444);
    let home = tmp.path().join("home");
    let dest = tmp.path().join("dest");
    let backup = |label: &str| {
        let out = albumen()
            .env("ALBUMEN_HOME", &home)
            .args(["backup", "--from"])
            .arg(&root)
            .arg("--to")
            .arg(&dest)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{label}: {text}");
        assert!(text.contains("backup ok:"), "{label}: {text}");
        text
    };
    let before = digest(&root);
    assert!(before.iter().any(|e| e.0 == ".hidden-note"));
    assert!(before.iter().any(|e| e.0 == "trip/day 2/deep/IMG_0002.jpg"));

    let first = backup("first backup");
    assert!(dest.join("trip/IMG.jpg").is_file(), "{first}");
    assert!(dest.join("trip/IMG.xmp").is_file(), "{first}");
    assert_eq!(digest(&root), before, "first backup changed the source");

    let second = backup("incremental backup");
    assert!(second.contains(" 0 copied"), "{second}");
    assert_eq!(
        digest(&root),
        before,
        "incremental backup changed the source"
    );

    // Lock every entry (chmod a-w): any write into the source now errors.
    lock_writes(&root);
    let locked = digest(&root);
    let third = backup("backup on a read-only source");
    assert!(third.contains(" 0 copied"), "{third}");
    let manifest = tmp.path().join("scan.jsonl");
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
    assert!(ok, "verify: {text}");
    let status = albumen()
        .env("ALBUMEN_HOME", &home)
        .arg("status")
        .arg(&dest)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    assert_eq!(
        digest(&root),
        locked,
        "a locked-source run changed the source"
    );
    // Same bytes, sizes, mtimes and listing as before; only the w bits differ.
    let strip = |v: Vec<(String, bool, u64, String, i64, i64, u32)>| {
        v.into_iter()
            .map(|mut e| {
                e.6 &= !0o222;
                e
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(locked), strip(before));
}
