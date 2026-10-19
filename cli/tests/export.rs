// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn run(args: &[impl AsRef<OsStr>]) -> (bool, String) {
    let output = albumen().args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn source_unchanged(root: &Path) {
    let sums = fs::read_to_string(root.join("SHA256SUMS")).unwrap();
    let mut count = 0;
    for line in sums.lines().filter(|line| !line.is_empty()) {
        let (hash, name) = line.split_once("  ").unwrap();
        let path = root.join(name);
        let actual =
            albumen_ingest::sha256_hex(&path).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(actual, hash, "{name} changed in the source library");
        count += 1;
    }
    assert!(count >= 8);
}

#[test]
fn export_and_verify_local_fixture_corpus() {
    let fixtures = fixtures();
    source_unchanged(&fixtures);
    let dest = tempfile::tempdir().unwrap();
    let dest_path = dest.path();
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        dest_path.as_os_str(),
    ]);
    assert!(ok, "{text}");
    let manifest = dest_path.join("ALBUMEN-MANIFEST.txt");
    assert!(manifest.is_file());
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.as_os_str(),
        OsStr::new("--against"),
        dest_path.as_os_str(),
    ]);
    assert!(ok, "{text}");
    source_unchanged(&fixtures);

    let kept = fs::metadata(dest_path.join("aae/IMG_0008.jpg"))
        .unwrap()
        .modified()
        .unwrap();
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        dest_path.as_os_str(),
    ]);
    assert!(ok, "{text}");
    assert_eq!(
        fs::metadata(dest_path.join("aae/IMG_0008.jpg"))
            .unwrap()
            .modified()
            .unwrap(),
        kept
    );
    assert!(dest_path.join("aae/IMG_0008.aae").is_file());
    assert!(dest_path.join("motion-photo/IMG_0007.jpg").is_file());
    assert!(dest_path.join("raw-jpeg/IMG_0003.dng").is_file());
    assert!(dest_path.join("raw-jpeg/IMG_0003.jpg").is_file());

    fs::remove_file(dest_path.join("video/IMG_0006.mp4")).unwrap();
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        dest_path.as_os_str(),
    ]);
    assert!(ok, "{text}");
    assert!(dest_path.join("video/IMG_0006.mp4").is_file());
    assert_eq!(
        fs::metadata(dest_path.join("aae/IMG_0008.jpg"))
            .unwrap()
            .modified()
            .unwrap(),
        kept
    );

    let jpeg = dest_path.join("jpeg-gps/IMG_0001.jpg");
    let mut bytes = fs::read(&jpeg).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&jpeg, &bytes).unwrap();
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        dest_path.as_os_str(),
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("refusing to overwrite"));
    assert_eq!(fs::read(&jpeg).unwrap(), bytes);
    source_unchanged(&fixtures);
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.as_os_str(),
        OsStr::new("--against"),
        dest_path.as_os_str(),
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("manifest mismatch"));
}

#[test]
fn export_and_verify_s3_fixture_corpus() {
    if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
        eprintln!(
            "skipping S3: ALBUMEN_S3_UNAVAILABLE=1. Docker is not used. Local filesystem only."
        );
        return;
    }
    if std::env::var("ALBUMEN_S3_ENDPOINT").is_err() {
        panic!("S3 export requires MinIO. Run scripts/with-minio.sh cargo test --locked");
    }
    let bucket = std::env::var("ALBUMEN_S3_BUCKET").expect("ALBUMEN_S3_BUCKET");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("cli-export-{}-{nanos}", std::process::id());
    let to = format!("s3://{bucket}/{prefix}");
    let fixtures = fixtures();
    let manifest = tempfile::NamedTempFile::new().unwrap();
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        OsStr::new(&to),
        OsStr::new("--manifest"),
        manifest.path().as_os_str(),
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.path().as_os_str(),
        OsStr::new("--against"),
        OsStr::new(&to),
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        OsStr::new(&to),
        OsStr::new("--manifest"),
        manifest.path().as_os_str(),
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.path().as_os_str(),
        OsStr::new("--against"),
        OsStr::new(&to),
    ]);
    assert!(ok, "{text}");
    source_unchanged(&fixtures);
}
