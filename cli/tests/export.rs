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
fn export_without_apply_writes_nothing() {
    let fixtures = fixtures();
    let dest = tempfile::tempdir().unwrap();
    let dest_path = dest.path().join("out");
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        dest_path.as_os_str(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(text.contains("network: none"), "{text}");
    assert!(!dest_path.exists(), "{text}");
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        fixtures.as_os_str(),
        OsStr::new("--to"),
        OsStr::new("s3://bucket/prefix"),
        OsStr::new("--manifest"),
        dest.path().join("manifest.txt").as_os_str(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(text.contains("ALBUMEN_S3_ENDPOINT"), "{text}");
    assert!(!dest.path().join("manifest.txt").exists());
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
        OsStr::new("--apply"),
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
        OsStr::new("--apply"),
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
        OsStr::new("--apply"),
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
        OsStr::new("--apply"),
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
        OsStr::new("--execute"),
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
        OsStr::new("--execute"),
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

const RCLONE_USER: &str = "albumen";
const RCLONE_PASS: &str = "albumen-export-test";

fn rclone_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ALBUMEN_RCLONE") {
        let path = PathBuf::from(path);
        assert!(path.is_file(), "ALBUMEN_RCLONE={path:?} is not a file");
        return path;
    }
    let found = Command::new("rclone").arg("version").output();
    if found.map(|output| output.status.success()).unwrap_or(false) {
        return PathBuf::from("rclone");
    }
    panic!("rclone is not installed. Run scripts/install-rclone.sh.");
}

struct SftpServe {
    child: std::process::Child,
    port: u16,
    root: PathBuf,
    _keep: tempfile::TempDir,
}

impl Drop for SftpServe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve_sftp(bin: &Path) -> SftpServe {
    let keep = tempfile::tempdir().unwrap();
    let root = keep.path().join("data");
    fs::create_dir(&root).unwrap();
    let config = keep.path().join("rclone.conf");
    fs::write(&config, b"").unwrap();
    let log_path = keep.path().join("serve.log");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&config)
        .args(["serve", "sftp"])
        .arg(&root)
        .arg("--addr")
        .arg(format!("127.0.0.1:{port}"))
        .args(["--user", RCLONE_USER, "--pass", RCLONE_PASS])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(fs::File::create(&log_path).unwrap())
        .spawn()
        .expect("start rclone serve sftp");
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return SftpServe {
                child,
                port,
                root,
                _keep: keep,
            };
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "rclone serve sftp exited {status}\n{}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("rclone serve sftp did not listen on {port}");
}

fn run_rclone(bin: &Path, args: &[&OsStr]) -> (bool, String) {
    let output = albumen()
        .args(args)
        .env("ALBUMEN_RCLONE", bin)
        .env("ALBUMEN_RCLONE_PASSWORD", RCLONE_PASS)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn tree_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn export_and_verify_fixture_corpus_over_rclone_sftp() {
    let bin = rclone_bin();
    let serve = serve_sftp(&bin);
    let to = format!("sftp://{RCLONE_USER}@127.0.0.1:{}/export", serve.port);
    let fixtures = fixtures();
    let work = tempfile::tempdir().unwrap();
    let manifest = work.path().join("manifest.txt");
    let export = |manifest: &Path, from: &Path| {
        run_rclone(
            &bin,
            &[
                OsStr::new("export"),
                OsStr::new("--from"),
                from.as_os_str(),
                OsStr::new("--to"),
                OsStr::new(&to),
                OsStr::new("--manifest"),
                manifest.as_os_str(),
                OsStr::new("--apply"),
            ],
        )
    };
    let verify = |manifest: &Path| {
        run_rclone(
            &bin,
            &[
                OsStr::new("verify-manifest"),
                OsStr::new("--manifest"),
                manifest.as_os_str(),
                OsStr::new("--against"),
                OsStr::new(&to),
            ],
        )
    };

    let (ok, text) = export(&manifest, &fixtures);
    assert!(ok, "{text}");
    let (ok, text) = verify(&manifest);
    assert!(ok, "{text}");
    assert!(text.contains("verified"), "{text}");
    let exported = serve.root.join("export");
    let first = tree_snapshot(&exported);
    let data_files = first
        .iter()
        .filter(|(rel, _)| !rel.ends_with(".albumen-verify"))
        .count();
    let manifest_lines = fs::read_to_string(&manifest)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .count();
    assert_eq!(data_files, manifest_lines, "manifest and remote disagree");
    assert!(
        first.iter().any(|(rel, _)| rel.ends_with(".aae")),
        "sidecars were not exported"
    );

    // Same bytes again: nothing is rewritten.
    let mtimes: Vec<_> = first
        .iter()
        .map(|(rel, _)| {
            fs::metadata(exported.join(rel))
                .unwrap()
                .modified()
                .unwrap()
        })
        .collect();
    let (ok, text) = export(&work.path().join("again.txt"), &fixtures);
    assert!(ok, "{text}");
    assert_eq!(tree_snapshot(&exported), first);
    let after: Vec<_> = first
        .iter()
        .map(|(rel, _)| {
            fs::metadata(exported.join(rel))
                .unwrap()
                .modified()
                .unwrap()
        })
        .collect();
    assert_eq!(after, mtimes, "a second export rewrote objects");

    // Different bytes at an existing relative path fail and replace nothing.
    let (rel, _) = first
        .iter()
        .find(|(rel, _)| !rel.contains('/') && !rel.starts_with('.') && rel.ends_with(".jpg"))
        .or_else(|| {
            first
                .iter()
                .find(|(rel, _)| !rel.starts_with('.') && !rel.contains("/."))
        })
        .unwrap()
        .clone();
    let other = work.path().join("other");
    let target = other.join(&rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"synthetic bytes that differ from the fixture").unwrap();
    let (ok, text) = export(&work.path().join("conflict.txt"), &other);
    assert!(!ok, "conflicting export succeeded: {text}");
    assert_eq!(
        tree_snapshot(&exported),
        first,
        "conflict changed the remote"
    );
    let (ok, text) = verify(&manifest);
    assert!(ok, "{text}");
    source_unchanged(&fixtures);
}
