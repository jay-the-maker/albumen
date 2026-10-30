// SPDX-License-Identifier: Apache-2.0

//! Pain point #6: a Live Photo survives a move between ecosystems.
//!
//! A copy of the committed CC0 pair `fixtures/live-photo/IMG_0005.{heic,mov}`
//! gets synthetic EXIF GPS (0.0001, 0.0001) and OffsetTimeOriginal on the
//! HEIC, then goes in through `import-icloud`, out through `export`, and back
//! in through `import-takeout` from a Takeout-shaped tree. After every leg:
//! both members are byte-identical (SHA-256) to the tagged copy, the HEIC is
//! still the same HEIC (never transcoded), the pair is one asset, the ExifTool
//! dump of each member is unchanged (GPS and offset included), each leg writes
//! only the pair, and every source folder keeps its file list, sizes, mtimes
//! and permissions. This proves preservation only, not playback.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use albumen_ingest::{
    discover, exiftool_command, export_tree, import_icloud, import_takeout, sha256_hex,
    DirectoryTree, Role, SourceRoot,
};
use serde_json::Value;

const CONTENT_ID: &str = "B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5F";
const HEIC: &str = "IMG_0005.heic";
const MOV: &str = "IMG_0005.mov";
const HEIC_SHA: &str = "2535dad4532a6559037109d8541eaf0c1414b812c19e3d418fe977a484b93cc7";
const MOV_SHA: &str = "8e4f6a8900f9b4a597533f09f35f68287156654b963bfdd55fbb15afa5af922e";

/// Synthetic location and zone written into the HEIC EXIF (AC4).
const GPS_LAT: &str = "0 deg 0' 0.36\" N";
const GPS_LON: &str = "0 deg 0' 0.36\" E";
const OFFSET: &str = "+00:00";

/// Tags that describe where a copy sits on disk, not what it contains.
const VOLATILE_TAGS: &[&str] = &[
    "SourceFile",
    "System:Directory",
    "System:FileModifyDate",
    "System:FileAccessDate",
    "System:FileInodeChangeDate",
    "System:FileCreateDate",
    "System:FilePermissions",
    "System:ZoneIdentifier",
];

#[test]
fn live_photo_round_trips_icloud_export_takeout_unchanged() {
    let fixture = repo_path("fixtures/live-photo");
    assert_eq!(sha256_hex(&fixture.join(HEIC)).unwrap(), HEIC_SHA);
    assert_eq!(sha256_hex(&fixture.join(MOV)).unwrap(), MOV_SHA);
    let fixture_snap = snapshot(&fixture);

    let work = tempfile::tempdir().unwrap();
    let root = work.path();

    // AC4: a copy of the committed still gets synthetic EXIF GPS and a zone
    // offset, so the round trip can prove location and zone survive. The
    // committed fixture itself stays as it is.
    let tagged = root.join("tagged");
    fs::create_dir_all(&tagged).unwrap();
    let heic = tagged.join(HEIC);
    let mov = tagged.join(MOV);
    fs::copy(fixture.join(HEIC), &heic).unwrap();
    fs::copy(fixture.join(MOV), &mov).unwrap();
    let status = exiftool_command()
        .args([
            "-overwrite_original",
            "-GPSLatitude=0.0001",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=0.0001",
            "-GPSLongitudeRef=E",
            "-OffsetTimeOriginal=+00:00",
        ])
        .arg(&heic)
        .status()
        .expect("exiftool");
    assert!(status.success(), "exiftool could not tag the HEIC copy");
    let heic_sha = sha256_hex(&heic).unwrap();
    assert_ne!(
        heic_sha, HEIC_SHA,
        "the tagged copy must differ from the fixture"
    );
    let heic_exif = exif_dump(&heic);
    let mov_exif = exif_dump(&mov);
    assert_eq!(heic_exif["Apple:ContentIdentifier"], CONTENT_ID);
    assert_eq!(mov_exif["Keys:ContentIdentifier"], CONTENT_ID);
    assert_location(&heic_exif, "tagged copy");
    assert_eq!(heic_exif["File:FileType"], "HEIC");
    let heic_size = fs::metadata(&heic).unwrap().len();
    let check = |dir: &Path, leg: &str| {
        let h = dir.join(HEIC);
        let m = dir.join(MOV);
        assert_eq!(
            sha256_hex(&h).unwrap(),
            heic_sha,
            "{leg}: HEIC bytes changed"
        );
        assert_eq!(sha256_hex(&m).unwrap(), MOV_SHA, "{leg}: MOV bytes changed");
        assert_eq!(
            fs::metadata(&h).unwrap().len(),
            heic_size,
            "{leg}: HEIC size"
        );
        let (hd, md) = (exif_dump(&h), exif_dump(&m));
        assert_eq!(hd["File:FileType"], "HEIC", "{leg}: HEIC transcoded");
        assert_location(&hd, leg);
        assert_eq!(hd, heic_exif, "{leg}: HEIC ExifTool dump changed");
        assert_eq!(md, mov_exif, "{leg}: MOV ExifTool dump changed");
        assert_one_asset(&h, leg);
    };

    // Leg 1: in through import-icloud.
    let icloud_src = root.join("icloud");
    let originals = icloud_src.join("originals").join("0");
    fs::create_dir_all(&originals).unwrap();
    fs::copy(&heic, originals.join(HEIC)).unwrap();
    fs::copy(&mov, originals.join(MOV)).unwrap();
    let icloud_snap = snapshot(&icloud_src);
    let icloud_lib = root.join("icloud-library");
    let icloud_report = root.join("icloud.jsonl");
    let outcome = import_icloud(&icloud_src, &icloud_lib, &icloud_report).unwrap();
    assert_eq!(
        (outcome.count_in, outcome.count_out, outcome.mismatched),
        (2, 2, 0)
    );
    for line in report_lines(&icloud_report) {
        if line.get("summary").is_some() {
            assert_eq!(line["pair_basis"]["content_id"], 2, "{line}");
            assert_eq!(line["pair_basis"]["content_id_mismatch"], 0, "{line}");
        } else {
            assert_eq!(line["pair_basis"], "content-id", "{line}");
            assert_eq!(line["content_identifier"], CONTENT_ID, "{line}");
        }
    }
    check(&icloud_lib.join("originals/0"), "import-icloud");
    assert_only_pair(&icloud_lib, "originals/0", "import-icloud");
    assert_unchanged(&fixture, &fixture_snap, "import-icloud");
    assert_unchanged(&icloud_src, &icloud_snap, "import-icloud");
    let icloud_lib_snap = snapshot(&icloud_lib);

    // Leg 2: out through export.
    let exported = root.join("exported");
    export_tree(
        &SourceRoot::open(&icloud_lib).unwrap(),
        &DirectoryTree::new(&exported),
    )
    .unwrap();
    let exported_pair = exported.join("originals/0");
    check(&exported_pair, "export");
    assert_only_pair(&exported, "originals/0", "export");
    assert_unchanged(&fixture, &fixture_snap, "export");
    assert_unchanged(&icloud_src, &icloud_snap, "export");
    assert_unchanged(&icloud_lib, &icloud_lib_snap, "export");

    // Leg 3: back in through import-takeout from a Takeout-shaped tree.
    let takeout = root.join("takeout");
    let album = takeout.join("Google Photos").join("Photos from 2020");
    fs::create_dir_all(&album).unwrap();
    fs::copy(exported_pair.join(HEIC), album.join(HEIC)).unwrap();
    fs::copy(exported_pair.join(MOV), album.join(MOV)).unwrap();
    let takeout_snap = snapshot(&takeout);
    let exported_snap = snapshot(&exported);
    let takeout_lib = root.join("takeout-library");
    let takeout_report = root.join("takeout.jsonl");
    let outcome = import_takeout(&takeout, &takeout_lib, &takeout_report).unwrap();
    assert_eq!(
        (outcome.count_in, outcome.count_out, outcome.mismatched),
        (2, 2, 0)
    );
    check(
        &takeout_lib.join("Google Photos").join("Photos from 2020"),
        "import-takeout",
    );
    assert_only_pair(
        &takeout_lib,
        "Google Photos/Photos from 2020",
        "import-takeout",
    );
    assert_unchanged(&fixture, &fixture_snap, "import-takeout");
    assert_unchanged(&icloud_src, &icloud_snap, "import-takeout");
    assert_unchanged(&icloud_lib, &icloud_lib_snap, "import-takeout");
    assert_unchanged(&exported, &exported_snap, "import-takeout");
    assert_unchanged(&takeout, &takeout_snap, "import-takeout");

    // Sources stay untouched.
    assert_eq!(sha256_hex(&fixture.join(HEIC)).unwrap(), HEIC_SHA);
    assert_eq!(sha256_hex(&fixture.join(MOV)).unwrap(), MOV_SHA);
    check(&tagged, "tagged copy after run");
    check(&originals, "iCloud source after run");
    check(&album, "Takeout source after run");
}

/// One entry per file or folder: relative path, size, mtime, read-only bit.
type Snapshot = Vec<(String, u64, SystemTime, bool)>;

fn snapshot(root: &Path) -> Snapshot {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Snapshot) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::metadata(&path).unwrap();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let len = if meta.is_dir() { 0 } else { meta.len() };
        out.push((
            rel,
            len,
            meta.modified().unwrap(),
            meta.permissions().readonly(),
        ));
        if meta.is_dir() {
            walk(root, &path, out);
        }
    }
}

/// R16: a source folder's file list, sizes, mtimes and permissions are unchanged.
fn assert_unchanged(dir: &Path, before: &Snapshot, leg: &str) {
    assert_eq!(
        &snapshot(dir),
        before,
        "{leg}: source {} changed",
        dir.display()
    );
}

/// AC5: the leg wrote the pair and no other file.
fn assert_only_pair(lib: &Path, pair_dir: &str, leg: &str) {
    let files: Vec<String> = snapshot(lib)
        .into_iter()
        .filter(|(rel, ..)| !lib.join(rel).is_dir())
        .map(|(rel, ..)| rel)
        .collect();
    assert_eq!(
        files,
        vec![format!("{pair_dir}/{HEIC}"), format!("{pair_dir}/{MOV}")],
        "{leg}: extra or missing files"
    );
}

fn assert_location(dump: &Value, leg: &str) {
    assert_eq!(
        dump["Composite:GPSLatitude"], GPS_LAT,
        "{leg}: GPS latitude"
    );
    assert_eq!(
        dump["Composite:GPSLongitude"], GPS_LON,
        "{leg}: GPS longitude"
    );
    assert_eq!(
        dump["ExifIFD:OffsetTimeOriginal"], OFFSET,
        "{leg}: OffsetTimeOriginal"
    );
}

fn assert_one_asset(heic: &Path, leg: &str) {
    let asset = discover(heic).unwrap();
    let files: Vec<_> = asset
        .files()
        .iter()
        .map(|f| {
            (
                f.path.file_name().unwrap().to_str().unwrap().to_string(),
                f.role,
            )
        })
        .collect();
    assert_eq!(
        files,
        vec![
            (HEIC.to_string(), Role::Primary),
            (MOV.to_string(), Role::Pair)
        ],
        "{leg}: pair is not one asset"
    );
}

fn report_lines(report: &Path) -> Vec<Value> {
    fs::read_to_string(report)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn exif_dump(path: &Path) -> Value {
    let output = exiftool_command()
        .args(["-j", "-G1", "-a", "-b", "-s", "-api", "LargeFileSupport=1"])
        .arg(path)
        .output()
        .expect("exiftool");
    assert!(
        output.status.success(),
        "exiftool failed for {}",
        path.display()
    );
    let mut items: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let mut value = items.pop().unwrap();
    let map = value.as_object_mut().unwrap();
    for key in VOLATILE_TAGS {
        map.remove(*key);
    }
    map.remove("System:FileName");
    value
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}
