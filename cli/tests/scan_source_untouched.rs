// SPDX-License-Identifier: Apache-2.0

//! `albumen scan` leaves the source and the folder that holds it unchanged:
//! same names, sizes, mtimes and SHA-256 before and after. The ledger goes
//! under ALBUMEN_HOME, or an explicit `--ledger` outside both.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use albumen_ingest::sha256_hex;

type Snapshot = BTreeMap<String, (bool, u64, Option<std::time::SystemTime>, Option<String>)>;

fn snapshot(root: &Path) -> Snapshot {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let hash = meta.is_file().then(|| sha256_hex(&path).unwrap());
            let mtime = (!meta.is_dir()).then(|| meta.modified().unwrap());
            out.insert(rel, (meta.is_dir(), meta.len(), mtime, hash));
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out
}

fn run(args: &[&str], home: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .env("ALBUMEN_HOME", home)
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

fn layout() -> (tempfile::TempDir, tempfile::TempDir) {
    let parent = tempfile::tempdir().unwrap();
    let source = parent.path().join("photos");
    fs::create_dir_all(source.join("roll")).unwrap();
    fs::write(source.join("a.jpg"), b"synthetic jpeg bytes").unwrap();
    fs::write(source.join("roll/b.dng"), b"synthetic raw bytes").unwrap();
    fs::write(parent.path().join("neighbour.txt"), b"beside the source").unwrap();
    let work = tempfile::tempdir().unwrap();
    (parent, work)
}

#[test]
fn scan_leaves_source_and_parent_identical() {
    let (parent, work) = layout();
    let source = parent.path().join("photos");
    let home = work.path().join("home");
    let manifest = work.path().join("out/manifest.jsonl");
    let before = snapshot(parent.path());
    let (ok, text) = run(
        &[
            "scan",
            source.to_str().unwrap(),
            "--manifest",
            manifest.to_str().unwrap(),
        ],
        &home,
    );
    assert!(ok, "{text}");
    assert_eq!(before, snapshot(parent.path()), "source or parent changed");
    assert!(!parent.path().join("photos.ledger").exists());
    let ledgers = home.join("ledgers");
    assert!(
        fs::read_dir(&ledgers).unwrap().next().is_some(),
        "no ledger under ALBUMEN_HOME"
    );
}

#[test]
fn scan_with_explicit_ledger_outside_leaves_source_identical() {
    let (parent, work) = layout();
    let source = parent.path().join("photos");
    let ledger = work.path().join("my-ledger");
    let before = snapshot(parent.path());
    let (ok, text) = run(
        &[
            "scan",
            source.to_str().unwrap(),
            "--manifest",
            work.path().join("m.jsonl").to_str().unwrap(),
            "--ledger",
            ledger.to_str().unwrap(),
        ],
        &work.path().join("home"),
    );
    assert!(ok, "{text}");
    assert_eq!(before, snapshot(parent.path()));
    assert!(ledger.join("analyses.jsonl").is_file());
}

#[test]
fn scan_refuses_a_ledger_inside_the_source_or_its_parent() {
    let (parent, work) = layout();
    let source = parent.path().join("photos");
    let before = snapshot(parent.path());
    for ledger in [source.join("ledger"), parent.path().join("photos.ledger")] {
        let (ok, text) = run(
            &[
                "scan",
                source.to_str().unwrap(),
                "--manifest",
                work.path().join("m.jsonl").to_str().unwrap(),
                "--ledger",
                ledger.to_str().unwrap(),
            ],
            &work.path().join("home"),
        );
        assert!(!ok, "accepted {}: {text}", ledger.display());
        assert!(text.contains("ledger directory is inside"), "{text}");
    }
    assert_eq!(before, snapshot(parent.path()));
}

#[test]
fn an_existing_sibling_ledger_is_read_and_kept() {
    let (parent, work) = layout();
    let source = parent.path().join("photos");
    let legacy = parent.path().join("photos.ledger");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(
        legacy.join("analyses.jsonl"),
        "{\"tool\":\"exiftool\",\"where\":\"on-device\",\"at\":\"2026-10-01T00:00:00Z\"}\n",
    )
    .unwrap();
    let home = work.path().join("home");
    let before = snapshot(parent.path());
    let (ok, text) = run(
        &[
            "scan",
            source.to_str().unwrap(),
            "--manifest",
            work.path().join("m.jsonl").to_str().unwrap(),
        ],
        &home,
    );
    assert!(ok, "{text}");
    assert_eq!(before, snapshot(parent.path()), "legacy ledger touched");
    let (ok, text) = run(
        &["ledger", "--library", source.to_str().unwrap(), "--json"],
        &home,
    );
    assert!(ok, "{text}");
    assert!(text.contains("2026-10-01T00:00:00Z"), "{text}");
}
