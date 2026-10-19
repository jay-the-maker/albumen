// SPDX-License-Identifier: Apache-2.0

//! A synthetic CC0 Live Photo generated in this test, then copied.
//!
//! `scripts/synthetic-live-photo.sh` encodes the pixels and asks ExifTool to
//! write one shared UUID. The movie tag is `Keys:ContentIdentifier`. The still
//! tag is whatever that script was able to store. Today that is
//! `XMP-dc:Identifier`, and `tests/fixtures/LICENSES.md` says why.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use albumen_ingest::{
    copy_verify, discover, exiftool_command, export_tree, import_icloud, import_takeout,
    sha256_hex, BlobStore, DirectoryTree, LocalStore, Role, SourceRoot,
};
use serde_json::{Map, Value};

const CONTENT_ID: &str = "A1B2C3D4-E5F6-4071-8A9B-C0D1E2F30405";
const HEIC_TAG: &str = "XMP-dc:Identifier";
const MOV_TAG: &str = "Keys:ContentIdentifier";
const HEIC_NAME: &str = "IMG_LIVE.heic";
const MOV_NAME: &str = "IMG_LIVE.mov";

const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FileCreateDate",
    "System:FilePermissions",
    "System:ZoneIdentifier",
];

#[test]
fn synthetic_live_photo_survives_import_and_export() {
    let licenses = fs::read_to_string(repo_path("tests/fixtures/LICENSES.md")).unwrap();
    assert!(licenses.contains(CONTENT_ID));
    assert!(licenses.contains(HEIC_TAG));
    assert!(licenses.contains("does not create an Apple maker-note block"));

    let generated = tempfile::tempdir().unwrap();
    let tag = generate(generated.path());
    assert_eq!(tag, HEIC_TAG);
    let heic = generated.path().join(HEIC_NAME);
    let mov = generated.path().join(MOV_NAME);
    stamp(&heic);
    stamp(&mov);
    let heic_sha = sha256_hex(&heic).unwrap();
    let mov_sha = sha256_hex(&mov).unwrap();
    assert_identifier(&heic, HEIC_TAG, CONTENT_ID);
    assert_identifier(&mov, MOV_TAG, CONTENT_ID);
    assert!(
        !exif_dump(&heic)
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key == "ContentIdentifier" || key.ends_with(":ContentIdentifier")),
        "HEIC gained a ContentIdentifier tag; update LICENSES.md with the tag ExifTool stored"
    );
    assert_discovered_pair(&heic);

    let staged = tempfile::tempdir().unwrap();
    let takeout = staged.path().join("takeout");
    let album = takeout.join("Google Photos").join("Album");
    fs::create_dir_all(&album).unwrap();
    copy_stamped(&heic, &album.join(HEIC_NAME));
    copy_stamped(&mov, &album.join(MOV_NAME));
    let library = staged.path().join("library");
    let report = staged.path().join("takeout.jsonl");
    let outcome = import_takeout(&takeout, &library, &report).unwrap();
    assert_eq!(outcome.count_in, 2);
    assert_eq!(outcome.count_out, 2);
    assert_eq!(outcome.mismatched, 0);
    assert_report_stem(&report, "motion_photo_stem");
    assert_same(&heic, &library.join("Google Photos/Album").join(HEIC_NAME));
    assert_same(&mov, &library.join("Google Photos/Album").join(MOV_NAME));
    assert_discovered_pair(&library.join("Google Photos/Album").join(HEIC_NAME));
    let exported = staged.path().join("exported");
    export_tree(
        &SourceRoot::open(&library).unwrap(),
        &DirectoryTree::new(&exported),
    )
    .unwrap();
    assert_same(&heic, &exported.join("Google Photos/Album").join(HEIC_NAME));
    assert_same(&mov, &exported.join("Google Photos/Album").join(MOV_NAME));
    assert_eq!(sha256_hex(&album.join(HEIC_NAME)).unwrap(), heic_sha);
    assert_eq!(sha256_hex(&album.join(MOV_NAME)).unwrap(), mov_sha);

    let icloud_src = staged.path().join("icloud");
    let originals = icloud_src.join("originals").join("0");
    fs::create_dir_all(&originals).unwrap();
    copy_stamped(&heic, &originals.join(HEIC_NAME));
    copy_stamped(&mov, &originals.join(MOV_NAME));
    let icloud_lib = staged.path().join("icloud-library");
    let icloud_report = staged.path().join("icloud.jsonl");
    let icloud = import_icloud(&icloud_src, &icloud_lib, &icloud_report).unwrap();
    assert_eq!(icloud.count_in, 2);
    assert_eq!(icloud.count_out, 2);
    assert_eq!(icloud.mismatched, 0);
    assert_report_stem(&icloud_report, "live_photo_stem");
    assert_same(&heic, &icloud_lib.join("originals/0").join(HEIC_NAME));
    assert_same(&mov, &icloud_lib.join("originals/0").join(MOV_NAME));
    assert_discovered_pair(&icloud_lib.join("originals/0").join(HEIC_NAME));
    let icloud_export = staged.path().join("icloud-export");
    export_tree(
        &SourceRoot::open(&icloud_lib).unwrap(),
        &DirectoryTree::new(&icloud_export),
    )
    .unwrap();
    assert_same(&heic, &icloud_export.join("originals/0").join(HEIC_NAME));
    assert_same(&mov, &icloud_export.join("originals/0").join(MOV_NAME));
    assert_eq!(sha256_hex(&originals.join(HEIC_NAME)).unwrap(), heic_sha);
    assert_eq!(sha256_hex(&originals.join(MOV_NAME)).unwrap(), mov_sha);

    let ingested = tempfile::tempdir().unwrap();
    let asset = discover(&heic).unwrap();
    let stored = copy_verify(&asset, &LocalStore::new(ingested.path())).unwrap();
    assert_eq!(stored.files.len(), 2);
    let read_back = tempfile::tempdir().unwrap();
    for file in &stored.files {
        LocalStore::new(ingested.path())
            .get_verified(&file.filename, read_back.path())
            .unwrap();
    }
    assert_same(&heic, &read_back.path().join(HEIC_NAME));
    assert_same(&mov, &read_back.path().join(MOV_NAME));
    assert_eq!(sha256_hex(&heic).unwrap(), heic_sha);
    assert_eq!(sha256_hex(&mov).unwrap(), mov_sha);
}

fn stamp(path: &Path) {
    let when = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn copy_stamped(source: &Path, dest: &Path) {
    fs::copy(source, dest).unwrap();
    stamp(dest);
}

fn generate(dir: &Path) -> String {
    let script = repo_path("scripts/synthetic-live-photo.sh");
    let output = Command::new("bash")
        .arg(&script)
        .arg(dir)
        .output()
        .unwrap_or_else(|err| panic!("run {}: {err}", script.display()));
    assert!(
        output.status.success(),
        "synthetic live photo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next_back()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../")
        .join(relative)
}

fn assert_report_stem(report: &Path, field: &str) {
    let text = fs::read_to_string(report).unwrap();
    for name in [HEIC_NAME, MOV_NAME] {
        let line = text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| {
                line["member"]
                    .as_str()
                    .is_some_and(|member| member.ends_with(name))
            })
            .unwrap_or_else(|| panic!("report missing {name}: {text}"));
        assert_eq!(line[field], "IMG_LIVE", "{line}");
    }
}

fn assert_discovered_pair(heic: &Path) {
    let asset = discover(heic).unwrap();
    let names: Vec<_> = asset
        .files()
        .iter()
        .map(|file| {
            (
                file.path.file_name().unwrap().to_str().unwrap().to_string(),
                file.role,
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (HEIC_NAME.to_string(), Role::Primary),
            (MOV_NAME.to_string(), Role::Pair),
        ]
    );
}

fn assert_same(source: &Path, dest: &Path) {
    assert!(dest.is_file(), "missing {}", dest.display());
    assert_eq!(sha256_hex(source).unwrap(), sha256_hex(dest).unwrap());
    assert_eq!(
        source.file_name(),
        dest.file_name(),
        "filename changed for {}",
        dest.display()
    );
    assert_exif_eq(source, dest);
    assert_identifier(dest, tag_for(dest), CONTENT_ID);
}

fn tag_for(path: &Path) -> &'static str {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mov"))
    {
        MOV_TAG
    } else {
        HEIC_TAG
    }
}

fn assert_identifier(path: &Path, key: &str, expected: &str) {
    let dump = exif_dump(path);
    let value = dump
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("{} has no {key}", path.display()));
    assert_eq!(value, expected, "{}", path.display());
}

fn assert_exif_eq(source: &Path, dest: &Path) {
    let left = exif_dump(source);
    let right = exif_dump(dest);
    if left != right {
        panic!(
            "ExifTool -a -G1 -b mismatch\n  source: {}\n  dest:   {}\n  changed: {:?}",
            source.display(),
            dest.display(),
            differing_keys(&left, &right)
        );
    }
}

fn exif_dump(path: &Path) -> Value {
    let output = exiftool_command()
        .args(["-j", "-G1", "-a", "-b", "-s", "-api", "LargeFileSupport=1"])
        .arg(path)
        .output()
        .expect("exiftool");
    assert!(
        output.status.success(),
        "exiftool failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut items: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let mut value = items.pop().unwrap();
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
