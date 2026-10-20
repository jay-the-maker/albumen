// SPDX-License-Identifier: Apache-2.0

//! Sync is not backup, S3 target: copy A is a local library, copy B is an
//! s3:// prefix. A delete and a receiver tombstone on A, followed by
//! another export, leave every object in B in place: B still verifies
//! against its first manifest and gains no tombstone. The test snapshots
//! B's full object list (key, SHA-256 of the bytes, length) and every file
//! on A (bytes and mtime) around the second export: B's objects must be
//! unchanged with no new keys, and A must be untouched by the export. Runs against the
//! pinned local MinIO from scripts/with-minio.sh; no other network.
//! Synthetic bytes only.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn ok(args: &[&str]) -> String {
    let (ok, text) = run(args);
    assert!(ok, "albumen {}: {text}", args.join(" "));
    text
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stamp(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

/// Every object under the s3:// prefix: key -> (SHA-256 of bytes, length).
fn s3_snapshot(copy_b: &str) -> BTreeMap<String, (String, u64)> {
    use albumen_ingest::{ExportDest, S3Config, S3Store};
    let rest = copy_b.strip_prefix("s3://").unwrap();
    let (bucket, prefix) = rest.split_once('/').unwrap();
    let mut config = S3Config::from_env().expect("S3 env");
    config.bucket = bucket.to_string();
    config.prefix = prefix.to_string();
    let store = S3Store::connect(config).expect("connect MinIO");
    let mut out = BTreeMap::new();
    for key in store.list().expect("list copy B") {
        let hash = store
            .read_hash(&key)
            .expect("hash object")
            .unwrap_or_else(|| panic!("{key} listed but missing"));
        out.insert(key, hash);
    }
    out
}

/// Every file under a local root: relative path -> (bytes, mtime).
fn tree_snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let meta = fs::metadata(&path).unwrap();
                out.insert(rel, (fs::read(&path).unwrap(), meta.modified().unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Append a line to the GitHub Actions job summary when it is available,
/// so a skip is visible on the run page and not only in captured output.
fn record_in_job_summary(line: &str) {
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(path) {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }
}

#[test]
fn a_delete_or_tombstone_on_copy_a_never_reaches_the_s3_copy() {
    if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
        // Linux always has a pinned MinIO, so a skip there is a failure.
        if cfg!(target_os = "linux") {
            panic!("MinIO is missing on Linux; the S3 two-copy test must run here");
        }
        let line = format!(
            "SKIPPED two_copy_s3 on {}: ALBUMEN_S3_UNAVAILABLE=1 (no pinned MinIO for this runner)",
            std::env::consts::OS
        );
        eprintln!("{line}");
        record_in_job_summary(&line);
        return;
    }
    if std::env::var("ALBUMEN_S3_ENDPOINT").is_err() {
        panic!("S3 two-copy test requires MinIO. Run scripts/with-minio.sh cargo test --locked");
    }
    let bucket = std::env::var("ALBUMEN_S3_BUCKET").expect("ALBUMEN_S3_BUCKET");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let copy_b = format!("s3://{bucket}/two-copy-{}-{nanos}", std::process::id());

    let work = tempfile::tempdir().unwrap();
    let copy_a = work.path().join("copy-a");
    stamp(
        &copy_a.join("DCIM/IMG_0001.jpg"),
        b"synthetic-s3-original-1",
    );
    stamp(
        &copy_a.join("DCIM/IMG_0002.jpg"),
        b"synthetic-s3-original-2",
    );
    stamp(&copy_a.join("DCIM/IMG_0002.xmp"), b"<x:xmpmeta synthetic/>");
    let m1 = work.path().join("b1.txt");
    let m2 = work.path().join("b2.txt");

    ok(&[
        "export",
        "--from",
        p(&copy_a),
        "--to",
        &copy_b,
        "--manifest",
        p(&m1),
        "--execute",
    ]);
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&m1),
        "--against",
        &copy_b,
    ]);

    // On copy A: delete one original and tombstone another (receiver format).
    fs::remove_file(copy_a.join("DCIM/IMG_0001.jpg")).unwrap();
    fs::write(
        copy_a.join("DCIM/IMG_0002.jpg.albumen-tombstone"),
        "{\"hidden\":true,\"reason\":\"hide\",\"stored_name\":\"DCIM/IMG_0002.jpg\"}\n",
    )
    .unwrap();
    let b_before = s3_snapshot(&copy_b);
    let a_before = tree_snapshot(&copy_a);
    assert!(b_before.contains_key("DCIM/IMG_0001.jpg"), "{b_before:?}");
    ok(&[
        "export",
        "--from",
        p(&copy_a),
        "--to",
        &copy_b,
        "--manifest",
        p(&m2),
        "--execute",
    ]);

    // Copy B still holds every original from the first export, bit-exact,
    // and nothing it did not have before.
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&m1),
        "--against",
        &copy_b,
    ]);
    // Full object list of B: every key keeps its bytes, and the update adds
    // no key at all (the tombstone stays copy-local, IMG_0002 is unchanged).
    let b_after = s3_snapshot(&copy_b);
    for (key, before) in &b_before {
        assert_eq!(
            b_after.get(key),
            Some(before),
            "copy B object {key} changed or vanished"
        );
    }
    let added: Vec<_> = b_after
        .keys()
        .filter(|k| !b_before.contains_key(*k))
        .collect();
    assert!(
        added.is_empty(),
        "export added objects to copy B: {added:?}"
    );
    // Every file on copy A is byte- and mtime-identical after the export.
    assert_eq!(tree_snapshot(&copy_a), a_before, "export changed copy A");
    let first = fs::read_to_string(&m1).unwrap();
    assert!(first.contains("DCIM/IMG_0001.jpg"), "{first}");
    let second = fs::read_to_string(&m2).unwrap();
    assert!(!second.contains("albumen-tombstone"), "{second}");
    assert!(copy_a.join("DCIM/IMG_0002.jpg").is_file());
    record_in_job_summary(&format!(
        "RAN two_copy_s3 on {} against MinIO at {}",
        std::env::consts::OS,
        std::env::var("ALBUMEN_S3_ENDPOINT").unwrap_or_default()
    ));
}
