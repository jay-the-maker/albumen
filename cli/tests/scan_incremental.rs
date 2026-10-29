// SPDX-License-Identifier: Apache-2.0

//! Incremental scan (brief v5 item 5): touch 1% of a synthetic library, scan
//! again with `--previous`, and check that only the touched and new files were
//! hashed and that the manifest is byte-for-byte what a full scan writes.
//!
//! Files: `ALBUMEN_INCREMENTAL_FILES`, default 2000 locally. The 100k-file
//! number is recorded by the scale job (`scripts/scan-export-scale.py`).

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

fn files() -> usize {
    std::env::var("ALBUMEN_INCREMENTAL_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}

fn scan(source: &Path, manifest: &Path, home: &Path, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .env("ALBUMEN_HOME", home)
        .arg("scan")
        .arg(source)
        .arg("--manifest")
        .arg(manifest)
        .args(extra)
        .output()
        .expect("run albumen scan");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn counter(stdout: &str) -> (u64, u64) {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("hashed "))
        .unwrap_or_else(|| panic!("no counter line in {stdout}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    (words[1].parse().unwrap(), words[4].parse().unwrap())
}

#[test]
fn touching_one_percent_rehashes_only_those_and_matches_a_full_scan() {
    let n = files();
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("library");
    let home = tmp.path().join("home");
    let old = SystemTime::now() - Duration::from_secs(86_400);
    for i in 0..n {
        let dir = source.join(format!("roll{:02}", i % 20));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("IMG_{i:06}.jpg"));
        fs::write(&path, format!("synthetic {i}")).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let first = tmp.path().join("first.jsonl");
    scan(&source, &first, &home, &[]);

    let touched = n.div_ceil(100);
    for i in (0..n).step_by(n / touched).take(touched) {
        let path = source
            .join(format!("roll{:02}", i % 20))
            .join(format!("IMG_{i:06}.jpg"));
        fs::write(&path, format!("edited  {i}")).unwrap();
    }
    fs::write(source.join("roll00/NEW_0001.jpg"), b"new photo").unwrap();

    let incremental = tmp.path().join("incremental.jsonl");
    let prev = first.to_str().unwrap();
    let out = scan(&source, &incremental, &home, &["--previous", prev]);
    assert_eq!(
        counter(&out),
        (touched as u64 + 1, n as u64 - touched as u64),
        "{out}"
    );

    let full = tmp.path().join("full.jsonl");
    scan(&source, &full, &home, &[]);
    assert!(
        fs::read(&incremental).unwrap() == fs::read(&full).unwrap(),
        "incremental manifest differs from a full scan"
    );

    let forced = tmp.path().join("forced.jsonl");
    let out = scan(
        &source,
        &forced,
        &home,
        &["--previous", prev, "--rehash-all"],
    );
    assert_eq!(counter(&out), (n as u64 + 1, 0), "{out}");
    assert!(fs::read(&forced).unwrap() == fs::read(&full).unwrap());
}

#[test]
fn rehash_all_needs_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .arg("scan")
        .arg(tmp.path())
        .args(["--manifest", "/tmp/never-written.jsonl", "--rehash-all"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!Path::new("/tmp/never-written.jsonl").exists());
}
