// SPDX-License-Identifier: Apache-2.0

//! Local real-device corpus. Nothing in this test is a checked-in photo.
//!
//! `ALBUMEN_REAL_CORPUS` points at a directory outside the repository. The
//! directory holds the media files and `sanitized.sha256` in sha256sum form.
//! When the variable is unset the test returns without reading a corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use albumen_ingest::{
    copy_verify, discover, sha256_hex, BlobStore, LocalStore, Role, S3Config, S3Store,
};
use serde_json::{Map, Value};

const MANIFEST_NAME: &str = "sanitized.sha256";
const SKIP_MESSAGE: &str = "skipping real-device corpus: ALBUMEN_REAL_CORPUS is unset. \
    Point it at a local directory with the media files and sanitized.sha256. \
    Those files are not in the repository, and CI does not set the variable.";

/// Tags that describe the filesystem location rather than the original.
/// FileModifyDate, FileName, and FileSize are not in this list: they must survive.
///
/// `FileCreateDate` is the Windows name for the creation time ExifTool reads
/// from the same stat field Unix reports as `FileInodeChangeDate`.
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

#[test]
fn real_device_corpus_round_trip() {
    let Ok(root) = env::var("ALBUMEN_REAL_CORPUS") else {
        println!("{SKIP_MESSAGE}");
        return;
    };
    if root.trim().is_empty() {
        panic!("ALBUMEN_REAL_CORPUS is set but empty");
    }
    let root = PathBuf::from(root);
    assert!(
        root.is_dir(),
        "ALBUMEN_REAL_CORPUS is not a directory: {}",
        root.display()
    );
    if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
        eprintln!(
            "skipping S3: ALBUMEN_S3_UNAVAILABLE=1. Docker is not used. Local filesystem only."
        );
        return;
    }
    let mut config = S3Config::from_env().unwrap_or_else(|err| {
        panic!(
            "real-device corpus S3 round-trip needs MinIO ({err}). \
             Run scripts/with-minio.sh cargo test --locked -p albumen-ingest --test real_corpus"
        )
    });
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    config.prefix = format!("real-corpus-{}-{nanos}/", std::process::id());
    let store = S3Store::connect(config).expect("connect to S3-compatible endpoint");
    check_corpus(&root, &store);
}

fn check_corpus(root: &Path, store: &S3Store) {
    let manifest = fs::read_to_string(root.join(MANIFEST_NAME))
        .unwrap_or_else(|err| panic!("read {MANIFEST_NAME}: {err}"));
    let listed = parse_sha256sum(&manifest).unwrap_or_else(|err| panic!("{err}"));
    let on_disk = files_under(root);
    let listed_keys: BTreeSet<_> = listed.iter().map(|entry| entry.relative.clone()).collect();
    assert_eq!(
        on_disk, listed_keys,
        "{MANIFEST_NAME} must list every file in the corpus directory except itself"
    );

    let mut identifiers = Vec::new();
    for entry in &listed {
        let path = root.join(&entry.relative);
        assert!(
            path.is_file(),
            "manifest path is not a file: {}",
            entry.relative
        );
        assert!(
            !fs::symlink_metadata(&path)
                .expect("metadata")
                .file_type()
                .is_symlink(),
            "refusing a symlink: {}",
            entry.relative
        );
        let actual =
            sha256_hex(&path).unwrap_or_else(|err| panic!("hash {}: {err}", entry.relative));
        assert_eq!(
            actual, entry.sha256,
            "checksum mismatch for {}",
            entry.relative
        );

        let asset =
            discover(&path).unwrap_or_else(|err| panic!("discover {}: {err}", entry.relative));
        assert_sidecars(&path, &asset);
        identifiers.push((path.clone(), content_identifier(&path)));
        round_trip_local(&asset);
        round_trip_s3(&asset, store);
    }
    assert_live_photos(&identifiers);
}

fn round_trip_local(asset: &albumen_ingest::Asset) {
    let once = tempfile::tempdir().expect("local dest");
    let report = copy_verify(asset, &LocalStore::new(once.path())).expect("local copy_verify");
    let restored = tempfile::tempdir().expect("local restore");
    let store = LocalStore::new(once.path());
    for file in &report.files {
        store
            .get_verified(&file.filename, restored.path())
            .unwrap_or_else(|err| panic!("local read-back of {}: {err}", file.filename));
    }
    assert_preserved(asset, restored.path(), &report);
}

fn round_trip_s3(asset: &albumen_ingest::Asset, store: &S3Store) {
    let report = copy_verify(asset, store).expect("S3 copy_verify");
    let restored = tempfile::tempdir().expect("S3 restore");
    for file in &report.files {
        store
            .get_verified(&file.filename, restored.path())
            .unwrap_or_else(|err| panic!("S3 read-back of {}: {err}", file.filename));
    }
    assert_preserved(asset, restored.path(), &report);
    for file in &report.files {
        store
            .delete(&file.filename)
            .unwrap_or_else(|err| panic!("S3 delete of {}: {err}", file.filename));
    }
}

fn assert_preserved(
    asset: &albumen_ingest::Asset,
    restored: &Path,
    report: &albumen_ingest::IngestReport,
) {
    assert_eq!(report.files.len(), asset.files().len());
    for file in asset.files() {
        let name = file
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8 filename");
        let dest = restored.join(name);
        assert!(dest.is_file(), "missing restored {name}");
        assert_eq!(file.path.file_name(), dest.file_name());
        let source_hash = sha256_hex(&file.path).expect("hash source");
        assert_eq!(sha256_hex(&dest).expect("hash dest"), source_hash);
        let source_mtime = fs::metadata(&file.path).unwrap().modified().unwrap();
        let dest_mtime = fs::metadata(&dest).unwrap().modified().unwrap();
        assert_eq!(source_mtime, dest_mtime, "mtime mismatch for {name}");
        let stored = report
            .files
            .iter()
            .find(|stored| stored.filename == name)
            .unwrap_or_else(|| panic!("report missing {name}"));
        assert_eq!(stored.sha256, source_hash);
        assert_eq!(stored.mtime, source_mtime);
        assert_exif_eq(&file.path, &dest);
    }
}

fn assert_sidecars(path: &Path, asset: &albumen_ingest::Asset) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("name");
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("stem");
    let parent = path.parent().unwrap_or(Path::new("."));
    for candidate in sidecar_candidates(file_name, stem) {
        if candidate == file_name {
            continue;
        }
        let sidecar = parent.join(&candidate);
        if !sidecar.is_file() {
            continue;
        }
        let found = asset.files().iter().find(|file| {
            file.path.file_name().and_then(|name| name.to_str()) == Some(candidate.as_str())
        });
        match found {
            Some(file) if file.role == Role::Sidecar => {}
            Some(file) => panic!(
                "{candidate} was discovered as {:?}, not a sidecar",
                file.role
            ),
            None => panic!("sidecar {candidate} was not discovered beside {file_name}"),
        }
    }
}

fn assert_live_photos(entries: &[(PathBuf, Option<String>)]) {
    for members in live_photo_groups(entries) {
        for member in &members {
            let asset = discover(member).unwrap_or_else(|err| panic!("discover live photo: {err}"));
            let found: BTreeSet<_> = asset.files().iter().map(|file| file.path.clone()).collect();
            for other in &members {
                if other == member {
                    continue;
                }
                assert!(
                    found.contains(other),
                    "files that share a ContentIdentifier were not discovered as one pair"
                );
                let role = asset
                    .files()
                    .iter()
                    .find(|file| &file.path == other)
                    .map(|file| file.role);
                assert_eq!(role, Some(Role::Pair));
            }
        }
    }
}

fn live_photo_groups(entries: &[(PathBuf, Option<String>)]) -> Vec<Vec<PathBuf>> {
    let mut grouped: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    for (path, identifier) in entries {
        let Some(identifier) = identifier.as_ref() else {
            continue;
        };
        if identifier.is_empty() {
            continue;
        }
        let parent = path.parent().unwrap_or(Path::new("")).to_path_buf();
        grouped
            .entry((parent, identifier.clone()))
            .or_default()
            .push(path.clone());
    }
    grouped
        .into_values()
        .filter(|members| members.len() >= 2)
        .collect()
}

fn content_identifier(path: &Path) -> Option<String> {
    let Value::Object(map) = exif_dump(path) else {
        return None;
    };
    for (key, value) in map {
        if key == "ContentIdentifier" || key.ends_with(":ContentIdentifier") {
            if let Some(text) = value.as_str() {
                let text = text.trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

struct ListedFile {
    sha256: String,
    relative: String,
}

fn parse_sha256sum(text: &str) -> Result<Vec<ListedFile>, String> {
    let mut listed = Vec::new();
    let mut seen = BTreeSet::new();
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let (sha256, relative) = split_sha256sum_line(line).ok_or_else(|| {
            format!(
                "line {} of {MANIFEST_NAME} is not sha256sum format",
                number + 1
            )
        })?;
        if !seen.insert(relative.to_string()) {
            return Err(format!("duplicate manifest path {relative}"));
        }
        listed.push(ListedFile {
            sha256: sha256.to_ascii_lowercase(),
            relative: relative.to_string(),
        });
    }
    if listed.is_empty() {
        return Err(format!("{MANIFEST_NAME} lists no files"));
    }
    Ok(listed)
}

fn split_sha256sum_line(line: &str) -> Option<(&str, &str)> {
    let (sha256, rest) = line.split_at_checked(64)?;
    if !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let relative = rest
        .strip_prefix("  ")
        .or_else(|| rest.strip_prefix(" *"))?;
    if relative.is_empty() || relative.contains('\\') || relative.contains('\0') {
        return None;
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        return None;
    }
    if path.components().any(|component| {
        matches!(component, Component::ParentDir | Component::CurDir)
            || component.as_os_str().is_empty()
    }) {
        return None;
    }
    if relative.split('/').any(|part| part.is_empty()) {
        return None;
    }
    if relative == MANIFEST_NAME || relative.ends_with(&format!("/{MANIFEST_NAME}")) {
        return None;
    }
    Some((sha256, relative))
}

fn files_under(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk(root, root, &mut found);
    found
}

fn walk(root: &Path, dir: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display())) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).expect("metadata");
        if meta.file_type().is_symlink() {
            panic!("refusing a symlink under the real corpus");
        }
        if meta.is_dir() {
            walk(root, &path, found);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("path under corpus")
            .components()
            .map(|component| component.as_os_str().to_str().expect("utf-8 path"))
            .collect::<Vec<_>>()
            .join("/");
        if relative == MANIFEST_NAME {
            continue;
        }
        found.insert(relative);
    }
}

fn sidecar_candidates(file_name: &str, stem: &str) -> Vec<String> {
    let mut names = vec![
        format!("{stem}.xmp"),
        format!("{stem}.XMP"),
        format!("{file_name}.xmp"),
        format!("{file_name}.XMP"),
        format!("{stem}.aae"),
        format!("{stem}.AAE"),
        format!("{file_name}.aae"),
        format!("{file_name}.AAE"),
    ];
    names.sort();
    names.dedup();
    names
}

fn assert_exif_eq(source: &Path, dest: &Path) {
    let left = exif_dump(source);
    let right = exif_dump(dest);
    if left != right {
        panic!(
            "ExifTool metadata mismatch; changed tags: {:?}",
            differing_keys(&left, &right)
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
        .expect("exiftool is required; run scripts/install-exiftool.sh");
    assert!(
        output.status.success(),
        "exiftool failed: {}",
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

mod parsing {
    use super::{live_photo_groups, parse_sha256sum, sidecar_candidates, MANIFEST_NAME};
    use std::path::PathBuf;

    fn digest(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    #[test]
    fn sha256sum_lines_keep_relative_paths_only() {
        let hex = digest(1);
        let text = format!("{hex}  nested/still.jpg\n{hex} *other.mov\n");
        let listed = parse_sha256sum(&text).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].relative, "nested/still.jpg");
        assert_eq!(listed[1].relative, "other.mov");
        assert_eq!(listed[0].sha256, hex);
    }

    #[test]
    fn sha256sum_rejects_escape_and_the_manifest_itself() {
        let hex = digest(2);
        assert!(parse_sha256sum(&format!("{hex}  ../outside.jpg")).is_err());
        assert!(parse_sha256sum(&format!("{hex}  /tmp/outside.jpg")).is_err());
        assert!(parse_sha256sum(&format!("{hex}  {MANIFEST_NAME}")).is_err());
        assert!(parse_sha256sum("abcd  still.jpg").is_err());
        assert!(parse_sha256sum("").is_err());
    }

    #[test]
    fn live_photos_group_by_directory_and_content_identifier() {
        let here = PathBuf::from("/corpus");
        let groups = live_photo_groups(&[
            (here.join("a.heic"), Some("same".into())),
            (here.join("a.mov"), Some("same".into())),
            (here.join("b.jpg"), Some("other".into())),
            (
                PathBuf::from("/elsewhere").join("c.mov"),
                Some("same".into()),
            ),
            (here.join("d.jpg"), None),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn sidecar_names_are_the_generic_xmp_and_aae_spellings() {
        let names = sidecar_candidates("still.jpg", "still");
        assert!(names.contains(&"still.xmp".to_string()));
        assert!(names.contains(&"still.XMP".to_string()));
        assert!(names.contains(&"still.jpg.xmp".to_string()));
        assert!(names.contains(&"still.aae".to_string()));
        assert!(names.contains(&"still.AAE".to_string()));
        assert!(names.contains(&"still.jpg.aae".to_string()));
    }
}
