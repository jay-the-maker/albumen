// SPDX-License-Identifier: Apache-2.0

//! IPTC Core written onto copies of the committed CC0 JPEG and HEIC.
//!
//! `scripts/write-iptc-core.sh` must reproduce the pinned SHA-256. Ingest and
//! export then keep those bytes, including the IPTC group and XMP-iptcCore.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use albumen_ingest::{
    copy_verify, discover, exiftool_command, export_tree, sha256_hex, BlobStore, DirectoryTree,
    LocalStore, SourceRoot,
};
use serde_json::{Map, Value};

const JPEG_SHA: &str = "2f9307265d29e613457a8b434467cb8c57cfebacb5b15c8d082038cfdafabded";
const HEIC_SHA: &str = "c724675f291a91aa073d19ef830d58ff33cb9b8f3fe2511fcbec91d849f0e21b";

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
fn iptc_core_is_deterministic_and_survives_ingest_and_export() {
    let licenses = fs::read_to_string(repo_path("tests/fixtures/LICENSES.md")).unwrap();
    assert!(licenses.contains(JPEG_SHA));
    assert!(licenses.contains(HEIC_SHA));
    assert!(licenses.contains("CC0 1.0 Universal"));
    assert!(licenses.contains("XMP-iptcCore"));

    let generated = tempfile::tempdir().unwrap();
    let script = repo_path("scripts/write-iptc-core.sh");
    let output = Command::new("bash")
        .arg(&script)
        .arg(generated.path())
        .output()
        .unwrap_or_else(|err| panic!("run {}: {err}", script.display()));
    assert!(
        output.status.success(),
        "write-iptc-core failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for (name, sha) in [("IMG_0010.jpg", JPEG_SHA), ("IMG_0011.heic", HEIC_SHA)] {
        let fresh = generated.path().join(name);
        let committed = repo_path("fixtures/iptc").join(name);
        assert_eq!(
            sha256_hex(&fresh).unwrap(),
            sha,
            "{name} is not deterministic"
        );
        assert_eq!(sha256_hex(&committed).unwrap(), sha, "committed {name}");
        assert_eq!(fs::read(&fresh).unwrap(), fs::read(&committed).unwrap());
        round_trip(&fresh, name);
    }
}

fn round_trip(source: &Path, filename: &str) {
    let before = sha256_hex(source).unwrap();
    let dump = exif_dump(source);
    require_iptc_core(&dump, source);

    let library = tempfile::tempdir().unwrap();
    let asset = discover(source).unwrap();
    copy_verify(&asset, &LocalStore::new(library.path())).unwrap();
    assert_eq!(
        sha256_hex(source).unwrap(),
        before,
        "ingest changed the source"
    );

    let read_back = tempfile::tempdir().unwrap();
    LocalStore::new(library.path())
        .get_verified(filename, read_back.path())
        .unwrap();

    let exported = tempfile::tempdir().unwrap();
    export_tree(
        &SourceRoot::open(library.path()).unwrap(),
        &DirectoryTree::new(exported.path()),
    )
    .unwrap();

    for restored in [
        read_back.path().join(filename),
        exported.path().join(filename),
    ] {
        assert_eq!(sha256_hex(&restored).unwrap(), before);
        assert_exif_eq(source, &restored);
        require_iptc_core(&exif_dump(&restored), &restored);
    }
}

fn require_iptc_core(dump: &Value, path: &Path) {
    let map = dump.as_object().expect("exif object");
    for group in ["IPTC:", "XMP-iptcCore:"] {
        assert!(
            map.keys().any(|key| key.starts_with(group)),
            "{} has no {group} tags",
            path.display()
        );
    }
    assert_eq!(map["IPTC:By-line"], "Albumen Project");
    assert_eq!(
        map["IPTC:Caption-Abstract"],
        "Synthetic CC0 fixture. Not a photograph."
    );
    assert_eq!(
        map["IPTC:Keywords"],
        Value::Array(vec![Value::from("albumen"), Value::from("cc0")])
    );
    assert_eq!(map["IPTC:City"], "Fixture City");
    assert_eq!(map["IPTC:Country-PrimaryLocationName"], "Fixture Country");
    assert_eq!(map["IPTC:CopyrightNotice"], "CC0 1.0 Universal");
    assert_eq!(map["XMP-iptcCore:CountryCode"], "XC");
    assert_eq!(map["XMP-iptcCore:Location"], "Synthetic location");
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../")
        .join(relative)
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
