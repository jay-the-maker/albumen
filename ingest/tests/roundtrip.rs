// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::fs::{self, FileTimes, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::{
    copy_verify, discover, exiftool_command, sha256_hex, BlobStore, IngestError, LocalStore,
    RcloneStore, S3Config, S3Store,
};
use serde_json::{Map, Value};

/// Tags that describe the filesystem location rather than the original.
/// FileModifyDate, FileName, and FileSize are not in this list: they must survive.
///
/// `FileCreateDate` is the name ExifTool uses on Windows for the creation time
/// it reads from the same stat field Unix reports as `FileInodeChangeDate`.
/// `ZoneIdentifier` is the NTFS `Zone.Identifier` stream. A byte copy does not
/// carry that stream. Neither tag is embedded EXIF, XMP, IPTC, GPS, or QuickTime.
const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FileCreateDate",
    "System:FilePermissions",
    "System:ZoneIdentifier",
];

struct Case {
    id: String,
    primary: String,
    companions: Vec<String>,
}

#[test]
fn corpus_checksums_match_the_published_sums() {
    let root = fixtures_dir();
    let sums = fs::read_to_string(root.join("SHA256SUMS")).expect("SHA256SUMS");
    let mut count = 0;
    for line in sums.lines().filter(|line| !line.is_empty()) {
        let (expected, name) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("bad SHA256SUMS line: {line}"));
        let actual = sha256_hex(&root.join(name)).expect("hash fixture");
        assert_eq!(actual, expected, "{name}");
        count += 1;
    }
    assert!(count >= 8, "fixture corpus is unexpectedly small");
}

#[test]
fn local_round_trip_preserves_bytes_metadata_mtime_and_filename() {
    for case in load_cases() {
        let stage = stage_case(&case);
        let once = tempfile::tempdir().expect("temp");
        let asset = discover(&stage.primary).expect("discover");
        assert_same_names(&case, &asset, &stage.dir);
        let report = copy_verify(&asset, &LocalStore::new(once.path())).expect("copy");
        let restored = tempfile::tempdir().expect("temp");
        let store = LocalStore::new(once.path());
        for file in &report.files {
            store
                .get_verified(&file.filename, restored.path())
                .expect("read back");
        }
        assert_preserved(&stage, restored.path(), &report);
    }
}

#[test]
fn s3_round_trip_preserves_bytes_metadata_mtime_and_filename() {
    if s3_unavailable() {
        return;
    }
    let mut config = S3Config::from_env().unwrap_or_else(|err| {
        panic!(
            "S3 round-trip requires MinIO and ALBUMEN_S3_* env vars ({err}). \
             Run `scripts/with-minio.sh cargo test --locked`."
        )
    });
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    config.prefix = format!("roundtrip-{}-{nanos}/", std::process::id());
    let store = S3Store::connect(config).expect("connect to S3-compatible endpoint");

    for case in load_cases() {
        let stage = stage_case(&case);
        let asset = discover(&stage.primary).expect("discover");
        assert_same_names(&case, &asset, &stage.dir);
        let report = copy_verify(&asset, &store)
            .unwrap_or_else(|err| panic!("S3 copy of {} failed: {err}", case.id));
        let restored = tempfile::tempdir().expect("temp");
        for file in &report.files {
            store
                .get_verified(&file.filename, restored.path())
                .unwrap_or_else(|err| panic!("S3 read-back of {} failed: {err}", file.filename));
        }
        assert_preserved(&stage, restored.path(), &report);
    }
}

const RCLONE_USER: &str = "albumen";
const RCLONE_PASS: &str = "albumen-ci-not-a-secret";

#[test]
fn rclone_sftp_webdav_and_b2_round_trip() {
    if s3_unavailable() {
        return;
    }
    let bin = rclone_bin();
    let sftp = start_serve("sftp", &bin);
    let sftp_store = RcloneStore::sftp(&bin, "127.0.0.1", sftp.port, RCLONE_USER, RCLONE_PASS)
        .expect("sftp store");
    round_trip_store(&sftp_store);
    drop(sftp);

    let webdav = start_serve("webdav", &bin);
    let webdav_store = RcloneStore::webdav(
        &bin,
        &format!("http://127.0.0.1:{}", webdav.port),
        RCLONE_USER,
        RCLONE_PASS,
    )
    .expect("webdav store");
    round_trip_store(&webdav_store);
    drop(webdav);

    let mut config = S3Config::from_env().unwrap_or_else(|err| {
        panic!(
            "B2's CI stand-in is MinIO through rclone ({err}). \
             Run `scripts/with-minio.sh cargo test --locked`."
        )
    });
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    config.prefix = format!("rclone-b2-{}-{nanos}/", std::process::id());
    let b2 = RcloneStore::s3(
        &bin,
        &config.endpoint,
        &config.bucket,
        &config.access_key,
        &config.secret_key,
        &config.prefix,
    )
    .unwrap_or_else(|err| panic!("rclone MinIO stand-in for B2 failed: {err}"));
    round_trip_store(&b2);
}

fn round_trip_store(store: &impl BlobStore) {
    let probe_dir = tempfile::tempdir().expect("probe");
    let probe = probe_dir.path().join("albumen-rclone-probe.bin");
    fs::write(&probe, b"probe-bytes-not-a-photo").expect("probe bytes");
    let file = OpenOptions::new().write(true).open(&probe).expect("probe");
    file.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700)),
    )
    .expect("probe mtime");
    drop(file);
    store
        .put_verified(&probe, "albumen-rclone-probe.bin")
        .expect("probe put");
    let again = store
        .put_verified(&probe, "albumen-rclone-probe.bin")
        .expect_err("identical bytes must not overwrite");
    assert!(
        matches!(again, IngestError::AlreadyExists { .. }),
        "{again}"
    );

    for case in load_cases() {
        let stage = stage_case(&case);
        let asset = discover(&stage.primary).expect("discover");
        assert_same_names(&case, &asset, &stage.dir);
        let report = copy_verify(&asset, store)
            .unwrap_or_else(|err| panic!("rclone copy of {} failed: {err}", case.id));
        let restored = tempfile::tempdir().expect("temp");
        for file in &report.files {
            store
                .get_verified(&file.filename, restored.path())
                .unwrap_or_else(|err| {
                    panic!("rclone read-back of {} failed: {err}", file.filename)
                });
        }
        assert_preserved(&stage, restored.path(), &report);
    }
}

struct RcloneServe {
    child: std::process::Child,
    port: u16,
    _keep: tempfile::TempDir,
}

impl Drop for RcloneServe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_serve(kind: &str, bin: &Path) -> RcloneServe {
    let keep = tempfile::tempdir().expect("serve dir");
    let root = keep.path().join("data");
    fs::create_dir(&root).expect("serve root");
    let log_path = keep.path().join("serve.log");
    let log = std::fs::File::create(&log_path).expect("serve log");
    let config = keep.path().join("rclone.conf");
    fs::write(&config, b"").expect("empty rclone config");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("port")
        .local_addr()
        .expect("addr")
        .port();
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&config)
        .arg("serve")
        .arg(kind)
        .arg(&root)
        .arg("--addr")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--user")
        .arg(RCLONE_USER)
        .arg("--pass")
        .arg(RCLONE_PASS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap_or_else(|err| panic!("failed to start rclone serve {kind}: {err}"));
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return RcloneServe {
                child,
                port,
                _keep: keep,
            };
        }
        if let Some(status) = child.try_wait().expect("serve status") {
            panic!(
                "rclone serve {kind} exited {status}\n{}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "rclone serve {kind} did not listen on {port}\n{}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );
}

fn s3_unavailable() -> bool {
    if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
        eprintln!(
            "skipping S3: ALBUMEN_S3_UNAVAILABLE=1. Docker is not used. Local filesystem only."
        );
        true
    } else {
        false
    }
}

fn rclone_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ALBUMEN_RCLONE") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "ALBUMEN_RCLONE={path:?} is not the rclone binary. scripts/install-rclone.sh installs the pinned release."
        );
        return path;
    }
    let found = Command::new("rclone").arg("version").output();
    if found.map(|output| output.status.success()).unwrap_or(false) {
        return PathBuf::from("rclone");
    }
    panic!(
        "rclone is not installed. Run scripts/install-rclone.sh, then \
         `scripts/with-minio.sh cargo test --locked`."
    );
}

struct Stage {
    dir: PathBuf,
    primary: PathBuf,
    _keep: tempfile::TempDir,
}

fn stage_case(case: &Case) -> Stage {
    let keep = tempfile::tempdir().expect("stage");
    let dir = keep.path().to_path_buf();
    let mut all = vec![case.primary.clone()];
    all.extend(case.companions.iter().cloned());
    for (index, relative) in all.iter().enumerate() {
        let source = fixtures_dir().join(relative);
        let name = source.file_name().expect("fixture name");
        let dest = dir.join(name);
        fs::copy(&source, &dest).expect("stage copy");
        let mtime = UNIX_EPOCH + Duration::new(1_400_000_000 + (index as u64) * 3_600, 123_456_700);
        let file = OpenOptions::new().write(true).open(&dest).expect("open");
        file.set_times(FileTimes::new().set_modified(mtime))
            .expect("set mtime");
    }
    let primary_name = Path::new(&case.primary).file_name().expect("primary name");
    Stage {
        primary: dir.join(primary_name),
        dir,
        _keep: keep,
    }
}

fn assert_same_names(case: &Case, asset: &albumen_ingest::Asset, stage_dir: &Path) {
    let found: BTreeSet<_> = asset
        .files()
        .iter()
        .map(|file| file.path.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    let mut expected = BTreeSet::new();
    expected.insert(file_name(&case.primary));
    for companion in &case.companions {
        expected.insert(file_name(companion));
    }
    assert_eq!(found, expected, "discovery mismatch for {}", case.id);
    for name in &expected {
        assert!(stage_dir.join(name).is_file(), "missing staged {name}");
    }
}

fn assert_preserved(stage: &Stage, restored: &Path, report: &albumen_ingest::IngestReport) {
    let mut names: BTreeSet<_> = BTreeSet::new();
    for entry in fs::read_dir(&stage.dir).expect("read stage") {
        let entry = entry.expect("entry");
        if entry.path().is_file() {
            names.insert(entry.file_name().to_str().unwrap().to_string());
        }
    }
    assert_eq!(report.files.len(), names.len());
    for name in names {
        let source = stage.dir.join(&name);
        let dest = restored.join(&name);
        assert!(dest.is_file(), "missing restored {name}");
        assert_eq!(
            source.file_name(),
            dest.file_name(),
            "filename changed for {name}"
        );
        assert_eq!(
            sha256_hex(&source).unwrap(),
            sha256_hex(&dest).unwrap(),
            "sha256 mismatch for {name}"
        );
        let source_mtime = fs::metadata(&source).unwrap().modified().unwrap();
        let dest_mtime = fs::metadata(&dest).unwrap().modified().unwrap();
        assert_eq!(source_mtime, dest_mtime, "mtime mismatch for {name}");
        let stored = report
            .files
            .iter()
            .find(|file| file.filename == name)
            .unwrap_or_else(|| panic!("report missing {name}"));
        assert_eq!(stored.sha256, sha256_hex(&source).unwrap());
        assert_eq!(stored.mtime, source_mtime);
        assert_exif_eq(&source, &dest);
    }
}

fn assert_exif_eq(source: &Path, dest: &Path) {
    let left = exif_dump(source);
    let right = exif_dump(dest);
    if left != right {
        let diffs = differing_keys(&left, &right);
        panic!(
            "ExifTool metadata mismatch\n  source: {}\n  dest:   {}\n  changed: {diffs:?}\n--- source\n{}\n--- dest\n{}",
            source.display(),
            dest.display(),
            serde_json::to_string_pretty(&left).unwrap(),
            serde_json::to_string_pretty(&right).unwrap()
        );
    }
    assert_eq!(
        left.get("System:FileName"),
        right.get("System:FileName"),
        "filename tag"
    );
    assert_eq!(
        left.get("System:FileModifyDate"),
        right.get("System:FileModifyDate"),
        "mtime tag"
    );
}

fn exif_dump(path: &Path) -> Value {
    let output = exiftool_command()
        .args([
            "-j",
            "-G1",
            "-a",
            "-u",
            "-s",
            "-ee",
            "-api",
            "LargeFileSupport=1",
        ])
        .arg(path)
        .output()
        .expect("exiftool is required (package libimage-exiftool-perl)");
    assert!(
        output.status.success(),
        "exiftool failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut items: Vec<Value> = serde_json::from_slice(&output.stdout).expect("exiftool json");
    let mut value = items.pop().expect("one exif object");
    if let Some(map) = value.as_object_mut() {
        for key in VOLATILE_TAGS {
            map.remove(*key);
        }
    }
    value
}

fn differing_keys(left: &Value, right: &Value) -> Vec<String> {
    let empty = Map::new();
    let left = left.as_object().unwrap_or(&empty);
    let right = right.as_object().unwrap_or(&empty);
    let mut keys: BTreeSet<_> = left.keys().cloned().collect();
    keys.extend(right.keys().cloned());
    keys.into_iter()
        .filter(|key| left.get(key) != right.get(key))
        .collect()
}

fn load_cases() -> Vec<Case> {
    let path = fixtures_dir().join("manifest.json");
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).expect("manifest"))
        .expect("parse manifest");
    value["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|item| Case {
            id: item["id"].as_str().unwrap().to_string(),
            primary: item["primary"].as_str().unwrap().to_string(),
            companions: item["companions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect(),
        })
        .collect()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn file_name(relative: &str) -> String {
    Path::new(relative)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}
