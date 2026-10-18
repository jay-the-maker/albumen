// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::fs::{self, FileTimes, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::{copy_verify, discover, sha256_hex, BlobStore, LocalStore, S3Config, S3Store};
use serde_json::{Map, Value};

/// Tags that describe the filesystem location rather than the original.
/// FileModifyDate, FileName, and FileSize are not in this list: they must survive.
const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FilePermissions",
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
        let mtime = UNIX_EPOCH + Duration::new(1_400_000_000 + (index as u64) * 3_600, 123_456_789);
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
    let output = Command::new("exiftool")
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
