// SPDX-License-Identifier: Apache-2.0

//! Pinned public corpora. Nothing in this test is a checked-in photo.
//!
//! Each manifest under `corpus/` pins real-camera files by size, SHA-1 and
//! SHA-256:
//! - `commons.manifest.json`: 12 Wikimedia Commons files (`ALBUMEN_COMMONS_CORPUS`).
//! - `media.manifest.json`: CC0 HEIC, MOV and MP4 from the Internet Archive
//!   (`ALBUMEN_MEDIA_CORPUS`).
//!
//! A CI job per corpus downloads the files with `scripts/fetch-commons-corpus.py`,
//! sets the directory variable and `<VARIABLE>_REQUIRED=1`. Elsewhere the
//! round-trip tests skip with a message. These tests never open a network connection.

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, FileTimes, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use albumen_ingest::{copy_verify, discover, exiftool_command, sha256_hex, BlobStore, LocalStore};
use serde_json::{Map, Value};

struct Corpus {
    /// Prefix of the per-file log line and the `--corpus` name of the fetcher.
    name: &'static str,
    manifest: &'static str,
    attribution: &'static str,
    dir_var: &'static str,
    required_var: &'static str,
    files: usize,
    total_bytes: u64,
    licenses: &'static [&'static str],
}

const COMMONS: Corpus = Corpus {
    name: "commons",
    manifest: "commons.manifest.json",
    attribution: "ATTRIBUTION.md",
    dir_var: "ALBUMEN_COMMONS_CORPUS",
    required_var: "ALBUMEN_COMMONS_CORPUS_REQUIRED",
    files: 12,
    total_bytes: 37_161_280,
    licenses: &[
        "CC0",
        "Public domain",
        "CC BY 2.5",
        "CC BY-SA 3.0",
        "CC BY-SA 4.0",
    ],
};

const MEDIA: Corpus = Corpus {
    name: "media",
    manifest: "media.manifest.json",
    attribution: "MEDIA_ATTRIBUTION.md",
    dir_var: "ALBUMEN_MEDIA_CORPUS",
    required_var: "ALBUMEN_MEDIA_CORPUS_REQUIRED",
    files: 6,
    total_bytes: 63_117_112,
    licenses: &["CC0"],
};

/// Same list as `roundtrip.rs`: tags that describe where a file sits, not the original.
const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FileCreateDate",
    "System:FilePermissions",
    "System:ZoneIdentifier",
];

/// Family-0 groups that ExifTool derives from the filesystem or computes.
const NON_EMBEDDED_GROUPS: &[&str] = &["ExifTool", "File", "System", "Composite"];

struct Entry {
    local_name: String,
    sha256: String,
    size: u64,
    license: String,
    author: String,
    page: String,
    model_tag: String,
    model: Option<String>,
    /// `None` keeps the original Commons rule: GPS means a `GPS:GPSLatitude` key.
    gps_tag: Option<Option<String>>,
    gps: bool,
    tags: BTreeMap<String, u64>,
}

#[test]
fn commons_manifest_is_well_formed_and_attributed() {
    check_manifest(&COMMONS);
}

#[test]
fn media_manifest_is_well_formed_and_attributed() {
    check_manifest(&MEDIA);
}

#[test]
fn commons_corpus_round_trip() {
    run_corpus(&COMMONS);
}

#[test]
fn media_corpus_round_trip() {
    run_corpus(&MEDIA);
}

fn check_manifest(corpus: &Corpus) {
    let entries = load_manifest(corpus);
    assert_eq!(
        entries.len(),
        corpus.files,
        "{} corpus size changed",
        corpus.name
    );
    let total: u64 = entries.iter().map(|e| e.size).sum();
    assert_eq!(
        total, corpus.total_bytes,
        "{} corpus byte total changed",
        corpus.name
    );
    let attribution = fs::read_to_string(corpus_dir().join(corpus.attribution))
        .unwrap_or_else(|err| panic!("corpus/{}: {err}", corpus.attribution));
    for entry in &entries {
        assert_eq!(entry.sha256.len(), 64, "{}", entry.local_name);
        assert!(
            attribution.contains(&entry.page) && attribution.contains(&entry.author),
            "{} is missing {} by {}",
            corpus.attribution,
            entry.page,
            entry.author
        );
        assert!(
            corpus.licenses.contains(&entry.license.as_str()),
            "unexpected license {} for {}",
            entry.license,
            entry.local_name
        );
    }
}

fn run_corpus(corpus: &Corpus) {
    let required = env::var(corpus.required_var).ok().as_deref() == Some("1");
    let root = match env::var(corpus.dir_var) {
        Ok(root) if !root.trim().is_empty() => PathBuf::from(root),
        _ if required => panic!("{}=1 but {} is unset", corpus.required_var, corpus.dir_var),
        _ => {
            println!(
                "skipping {} corpus: {} is unset. Run scripts/fetch-commons-corpus.py \
                 --corpus {} fetch DIR and point the variable at DIR. The CI job for \
                 this corpus does this and requires it.",
                corpus.name, corpus.dir_var, corpus.name
            );
            return;
        }
    };
    assert!(
        root.is_dir(),
        "{} is not a directory: {}",
        corpus.dir_var,
        root.display()
    );

    let mut failures = Vec::new();
    for (index, entry) in load_manifest(corpus).iter().enumerate() {
        match round_trip_one(&root, entry, index) {
            Ok(line) => println!("{}-roundtrip {line}", corpus.name),
            Err(err) => {
                println!(
                    "{}-roundtrip name={} result=FAIL {err}",
                    corpus.name, entry.local_name
                );
                failures.push(format!("{}: {err}", entry.local_name));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} corpus round-trip failed:\n{}",
        corpus.name,
        failures.join("\n")
    );
}

fn round_trip_one(root: &Path, entry: &Entry, index: usize) -> Result<String, String> {
    let original = root.join(&entry.local_name);
    let before = sha256_hex(&original).map_err(|e| format!("hash original: {e}"))?;
    if before != entry.sha256 {
        return Err(format!(
            "original sha256 {before} != manifest {}",
            entry.sha256
        ));
    }
    let original_tags = exif_dump(&original);
    let original_groups = group_counts(&original);

    // Stage a copy so the downloaded corpus is only ever read.
    let stage = tempfile::tempdir().expect("stage");
    let staged = stage.path().join(&entry.local_name);
    fs::copy(&original, &staged).map_err(|e| format!("stage copy: {e}"))?;
    // The fetcher leaves the downloads read-only; fs::copy carries that mode over.
    let mut perms = fs::metadata(&staged)
        .map_err(|e| e.to_string())?
        .permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&staged, perms).map_err(|e| format!("stage chmod: {e}"))?;
    let mtime = UNIX_EPOCH + Duration::new(1_500_000_000 + index as u64 * 3_600, 987_654_300);
    OpenOptions::new()
        .write(true)
        .open(&staged)
        .and_then(|file| file.set_times(FileTimes::new().set_modified(mtime)))
        .map_err(|e| format!("set mtime: {e}"))?;

    let asset = discover(&staged).map_err(|e| format!("discover: {e}"))?;
    if asset.files().len() != 1 {
        return Err(format!(
            "discover found {} files, expected 1",
            asset.files().len()
        ));
    }
    let library = tempfile::tempdir().expect("library");
    let store = LocalStore::new(library.path());
    let report = copy_verify(&asset, &store).map_err(|e| format!("copy_verify: {e}"))?;
    let restored = tempfile::tempdir().expect("restored");
    let back = store
        .get_verified(&entry.local_name, restored.path())
        .map_err(|e| format!("read back: {e}"))?;
    let restored_path = restored.path().join(&entry.local_name);

    let restored_sha = sha256_hex(&restored_path).map_err(|e| format!("hash restored: {e}"))?;
    let stored = &report.files[0];
    if restored_sha != entry.sha256 || stored.sha256 != entry.sha256 || back.sha256 != entry.sha256
    {
        return Err(format!(
            "bytes changed: stored={} restored={restored_sha}",
            stored.sha256
        ));
    }
    let dest_mtime = fs::metadata(&restored_path)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?;
    if dest_mtime != mtime || stored.mtime != mtime {
        return Err("mtime not preserved".to_string());
    }

    // Every embedded tag must match between the original and the copy.
    let staged_tags = exif_dump(&staged);
    let restored_tags = exif_dump(&restored_path);
    let changed = differing_keys(&staged_tags, &restored_tags);
    if !changed.is_empty() {
        return Err(format!("ExifTool tags changed: {changed:?}"));
    }
    let embedded_changed: Vec<_> = differing_keys(&original_tags, &restored_tags)
        .into_iter()
        .filter(|key| {
            !key.starts_with("System:")
                && !key.starts_with("Composite:")
                && !key.starts_with("ExifTool:")
        })
        .collect();
    if !embedded_changed.is_empty() {
        return Err(format!(
            "embedded tags differ from the download: {embedded_changed:?}"
        ));
    }
    let restored_groups = group_counts(&restored_path);
    if restored_groups != original_groups {
        return Err(format!(
            "group counts {restored_groups:?} != original {original_groups:?}"
        ));
    }
    if restored_groups != entry.tags {
        return Err(format!(
            "group counts {restored_groups:?} != manifest {:?}",
            entry.tags
        ));
    }
    let model = restored_tags
        .get(&entry.model_tag)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    if model != entry.model {
        return Err(format!(
            "camera model {model:?} != manifest {:?}",
            entry.model
        ));
    }
    let gps = restored_tags
        .as_object()
        .is_some_and(|map| match &entry.gps_tag {
            None => map.keys().any(|key| key.starts_with("GPS:GPSLatitude")),
            Some(Some(tag)) => map.contains_key(tag),
            // No GPS recorded: no embedded latitude or coordinates tag in any group.
            Some(None) => map
                .keys()
                .any(|key| key.ends_with(":GPSLatitude") || key.ends_with(":GPSCoordinates")),
        });
    if gps != entry.gps {
        return Err(format!("GPS present={gps}, manifest says {}", entry.gps));
    }

    let after = sha256_hex(&original).map_err(|e| format!("re-hash original: {e}"))?;
    if after != before {
        return Err("downloaded original changed during the test".to_string());
    }
    let count = |g: &str| restored_groups.get(g).copied().unwrap_or(0);
    Ok(format!(
        "name={} result=ok bytes={} sha256_bit_exact=yes mtime=yes tags_equal={} exif={} xmp={} iptc={} makernotes={} quicktime={} gps={} model={}",
        entry.local_name,
        entry.size,
        restored_tags.as_object().map_or(0, Map::len),
        count("EXIF"),
        count("XMP"),
        count("IPTC"),
        count("MakerNotes"),
        count("QuickTime"),
        if gps { "yes" } else { "no" },
        model.as_deref().unwrap_or("-").replace(' ', "_"),
    ))
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
        .expect("exiftool is required (scripts/install-exiftool.sh)");
    assert!(
        output.status.success(),
        "exiftool failed for {}",
        path.display()
    );
    let mut items: Vec<Value> = serde_json::from_slice(&output.stdout).expect("exiftool json");
    let mut value = items.pop().expect("one exif object");
    if let Some(map) = value.as_object_mut() {
        for key in VOLATILE_TAGS {
            map.remove(*key);
        }
        map.remove("System:FileName");
        // Composite: tags are calculated by ExifTool from other tags. They are
        // derived, not stored in the file, so they say nothing about whether the
        // copy kept the original's metadata. An ambiguous Composite:LensID can
        // also list the same candidates in a different order on two reads of
        // identical bytes (seen on commons-05 in CI).
        //
        // This is not a loosening. The bytes are still compared by SHA-256, every
        // embedded tag (EXIF, XMP, IPTC, MakerNotes, ICC_Profile, QuickTime, ...)
        // is still compared one by one, and the family-0 group counts are still
        // compared with the original and the manifest in `group_counts`.
        map.retain(|key, _| !key.starts_with("Composite:"));
    }
    value
}

/// ExifTool family-0 group counts, the same command used to write the manifest.
fn group_counts(path: &Path) -> BTreeMap<String, u64> {
    let output = exiftool_command()
        .args(["-j", "-G0", "-a", "-u", "-s"])
        .arg(path)
        .output()
        .expect("exiftool is required (scripts/install-exiftool.sh)");
    assert!(
        output.status.success(),
        "exiftool failed for {}",
        path.display()
    );
    let mut items: Vec<Value> = serde_json::from_slice(&output.stdout).expect("exiftool json");
    let value = items.pop().expect("one exif object");
    let mut counts = BTreeMap::new();
    for key in value.as_object().expect("object").keys() {
        if let Some((group, _)) = key.split_once(':') {
            if !NON_EMBEDDED_GROUPS.contains(&group) {
                *counts.entry(group.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
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

fn load_manifest(corpus: &Corpus) -> Vec<Entry> {
    let path = corpus_dir().join(corpus.manifest);
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).expect("manifest"))
        .expect("parse manifest");
    assert_eq!(value["schema"], 1);
    value["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| Entry {
            local_name: f["local_name"].as_str().unwrap().to_string(),
            sha256: f["sha256"].as_str().unwrap().to_string(),
            size: f["size"].as_u64().unwrap(),
            license: f["license"].as_str().unwrap().to_string(),
            author: f["author"].as_str().unwrap().to_string(),
            page: f["page"].as_str().unwrap().to_string(),
            model_tag: f["expect"]["model_tag"]
                .as_str()
                .unwrap_or("IFD0:Model")
                .to_string(),
            gps_tag: f["expect"]
                .get("gps_tag")
                .map(|tag| tag.as_str().map(str::to_string)),
            model: f["expect"]["exif_model"]
                .as_str()
                .map(|s| s.trim().to_string()),
            gps: f["expect"]["gps"].as_bool().unwrap(),
            tags: f["expect"]["tags"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_u64().unwrap()))
                .collect(),
        })
        .collect()
}

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus")
}
