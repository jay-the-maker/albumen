// SPDX-License-Identifier: Apache-2.0

//! Round-trip of permissively licensed public fixtures.
//!
//! The bytes are downloaded at test time and checked against the SHA-256 pins
//! in `tests/fixtures/LICENSES.md`. A Live Photo pair and an IPTC block are
//! not in that set; the license file says why.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use albumen_ingest::{
    copy_verify, discover, exiftool_command, export_tree, sha256_hex, BlobStore, DirectoryTree,
    LocalStore, SourceRoot,
};
use serde_json::{Map, Value};

/// Filesystem identity. Embedded EXIF, XMP, IPTC, GPS, MakerNotes, and
/// QuickTime stay in the comparison. `FileModifyDate` and `FileName` stay too.
const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FileCreateDate",
    "System:FilePermissions",
    "System:ZoneIdentifier",
];

struct Fixture {
    /// Basename the download keeps, so the round-trip checks that name.
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
    /// ExifTool `-G1 -s` key prefixes that this file must contain.
    groups: &'static [&'static str],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        filename: "exif-gps-date.heic",
        url: "https://heic.fyi/samples/exif-gps-date.heic",
        sha256: "01fd9fa28d09885d8482d01a5b36d88dc89fde3b7cfeaca50b70f8314d2c63d1",
        groups: &["XMP-", "GPS:"],
    },
    Fixture {
        filename: "IMG_1361.DNG",
        url: "https://raw.pixls.us/getfile.php/4264/raw/IMG_1361.DNG",
        sha256: "e91e77a4533ed7cce551d83330676ea5c47dd5e55fb38adda7819366afdbdfc2",
        // ExifTool reports Apple maker notes in the `Apple` group.
        groups: &["Apple:", "XMP-", "GPS:"],
    },
];

#[test]
fn public_fixtures_round_trip_bytes_and_metadata() {
    let licenses = licenses_text();
    for fixture in FIXTURES {
        assert!(
            licenses.contains(fixture.url) && licenses.contains(fixture.sha256),
            "tests/fixtures/LICENSES.md is missing {} or its SHA-256",
            fixture.filename
        );
        let source = fetch_pinned(fixture);
        let before = sha256_hex(&source).expect("hash source");
        assert_eq!(before, fixture.sha256);

        let library = tempfile::tempdir().expect("library");
        let asset = discover(&source).expect("discover");
        let report = copy_verify(&asset, &LocalStore::new(library.path())).expect("ingest");
        assert_eq!(
            sha256_hex(&source).expect("source unchanged"),
            fixture.sha256,
            "ingest changed the source"
        );

        let read_back = tempfile::tempdir().expect("read back");
        for file in &report.files {
            LocalStore::new(library.path())
                .get_verified(&file.filename, read_back.path())
                .expect("read back");
        }

        let exported = tempfile::tempdir().expect("export");
        let root = SourceRoot::open(library.path()).expect("open library");
        let entries = export_tree(&root, &DirectoryTree::new(exported.path())).expect("export");
        assert_eq!(entries.len(), 1, "{}", fixture.filename);
        assert_eq!(entries[0].sha256, fixture.sha256);
        assert_eq!(entries[0].byte_len, fs::metadata(&source).unwrap().len());

        for restored in [
            read_back.path().join(fixture.filename),
            exported.path().join(fixture.filename),
        ] {
            assert!(restored.is_file(), "missing {}", restored.display());
            assert_eq!(
                restored.file_name().and_then(|name| name.to_str()),
                Some(fixture.filename)
            );
            assert_eq!(sha256_hex(&restored).expect("hash copy"), fixture.sha256);
            assert_exif_eq(&source, &restored, fixture.groups);
        }
    }
}

fn licenses_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/LICENSES.md");
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn fetch_pinned(fixture: &Fixture) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("albumen-public-{}", &fixture.sha256[..16]));
    fs::create_dir_all(&dir).expect("cache dir");
    let path = dir.join(fixture.filename);
    if path.is_file() && sha256_hex(&path).ok().as_deref() == Some(fixture.sha256) {
        return path;
    }
    if path.exists() {
        fs::remove_file(&path).expect("drop bad cache");
    }
    let mut last_error = String::from("curl was not run");
    for attempt in 1..=5 {
        let output = Command::new("curl")
            .args(["-fsSL", "--retry", "3", "--retry-delay", "2", "-o"])
            .arg(&path)
            .arg(fixture.url)
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let actual = sha256_hex(&path).expect("hash download");
                assert_eq!(
                    actual, fixture.sha256,
                    "SHA-256 mismatch for {} from {}",
                    fixture.filename, fixture.url
                );
                return path;
            }
            Ok(output) => {
                last_error = format!(
                    "curl exit {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(err) => last_error = err.to_string(),
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        std::thread::sleep(Duration::from_secs(attempt));
    }
    panic!(
        "download of {} failed after 5 attempts: {last_error}",
        fixture.url
    );
}

fn assert_exif_eq(source: &Path, dest: &Path, groups: &[&str]) {
    let left = exif_dump(source);
    let right = exif_dump(dest);
    require_groups(&left, groups, source);
    if left != right {
        panic!(
            "ExifTool -a -G1 -b mismatch\n  source: {}\n  dest:   {}\n  changed: {:?}",
            source.display(),
            dest.display(),
            differing_keys(&left, &right)
        );
    }
}

fn require_groups(dump: &Value, groups: &[&str], path: &Path) {
    let map = dump.as_object().expect("exif object");
    for group in groups {
        let found = map.keys().any(|key| key.starts_with(group));
        assert!(
            found,
            "{} has no {group} tags; MakerNotes, XMP, and GPS must be present when the fixture carries them",
            path.display()
        );
    }
}

fn exif_dump(path: &Path) -> Value {
    let output = exiftool_command()
        .args(["-j", "-G1", "-a", "-b", "-s", "-api", "LargeFileSupport=1"])
        .arg(path)
        .output()
        .expect("exiftool is required (scripts/install-exiftool.sh)");
    assert!(
        output.status.success(),
        "exiftool -a -G1 -b failed for {}: {}",
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
    let mut keys: std::collections::BTreeSet<_> = left.keys().cloned().collect();
    keys.extend(right.keys().cloned());
    keys.into_iter()
        .filter(|key| left.get(key) != right.get(key))
        .collect()
}
