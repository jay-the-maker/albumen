// SPDX-License-Identifier: Apache-2.0

//! Issue #28: a mounted card is imported through copy_verify and left
//! unchanged. Synthetic fixtures only.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use albumen_ingest::{
    import_card, import_card_local, sha256_hex, BlobStore, IngestError, LocalStore, Renamed,
    VerifiedObject, CARD_RECORD_SUFFIX,
};

const FIXTURES: &[&str] = &[
    "raw-jpeg/IMG_0003.dng",
    "raw-jpeg/IMG_0003.jpg",
    "xmp/IMG_0004.jpg",
    "xmp/IMG_0004.xmp",
    "aae/IMG_0008.jpg",
    "aae/IMG_0008.aae",
];

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    relative: String,
    sha256: String,
    mtime: SystemTime,
    len: u64,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

/// Lay out a fake card: DCIM/100TEST/<fixture files>.
fn make_card(root: &Path) -> PathBuf {
    let card = root.join("card");
    let dcim = card.join("DCIM").join("100TEST");
    fs::create_dir_all(&dcim).unwrap();
    for fixture in FIXTURES {
        let src = fixtures().join(fixture);
        let name = src.file_name().unwrap();
        fs::copy(&src, dcim.join(name)).unwrap();
    }
    card
}

fn snapshot(dir: &Path) -> Vec<Snap> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push(Snap {
                    relative: path
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    sha256: sha256_hex(&path).unwrap(),
                    mtime: meta.modified().unwrap(),
                    len: meta.len(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    out
}

#[test]
fn card_import_copies_everything_and_leaves_card_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let before = snapshot(&card);
    assert_eq!(before.len(), FIXTURES.len());

    let report = import_card(&card, &LocalStore::new(&library)).unwrap();

    assert_eq!(
        snapshot(&card),
        before,
        "card must be byte- and mtime-identical"
    );
    assert_eq!(report.assets, 3, "dng+jpg, jpg+xmp, jpg+aae");
    assert_eq!(report.files.len(), FIXTURES.len());
    for snap in &before {
        let name = Path::new(&snap.relative).file_name().unwrap();
        let copy = library.join(name);
        let meta = fs::metadata(&copy).unwrap();
        assert_eq!(sha256_hex(&copy).unwrap(), snap.sha256, "{name:?} hash");
        assert_eq!(meta.modified().unwrap(), snap.mtime, "{name:?} mtime");
        let stored = report
            .files
            .iter()
            .find(|f| Path::new(&f.filename) == Path::new(name))
            .unwrap();
        assert_eq!(stored.sha256, snap.sha256);
    }
}

/// Real local store whose second put reports a checksum mismatch after the
/// bytes landed, as a failing destination would.
struct CorruptSecondPut {
    inner: LocalStore,
    root: PathBuf,
    puts: AtomicUsize,
}

impl BlobStore for CorruptSecondPut {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        let object = self.inner.put_verified(source, filename)?;
        if self.puts.fetch_add(1, Ordering::SeqCst) == 1 {
            fs::remove_file(self.root.join(filename)).unwrap();
            return Err(IngestError::ChecksumMismatch {
                filename: filename.to_string(),
                expected_sha256: object.sha256,
                actual_sha256: "0".repeat(64),
                destination: self.root.join(filename).display().to_string(),
                removed: true,
                message: "forced by test".to_string(),
            });
        }
        Ok(object)
    }

    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
        self.inner.get_verified(filename, dest_dir)
    }

    fn delete(&self, filename: &str) -> Result<(), IngestError> {
        self.inner.delete(filename)
    }
}

#[test]
fn forced_checksum_failure_deletes_nothing_from_card() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let before = snapshot(&card);
    let store = CorruptSecondPut {
        inner: LocalStore::new(&library),
        root: library.clone(),
        puts: AtomicUsize::new(0),
    };

    let err = import_card(&card, &store).unwrap_err();

    assert!(matches!(err, IngestError::ChecksumMismatch { .. }), "{err}");
    assert_eq!(
        snapshot(&card),
        before,
        "card must be untouched after failure"
    );
    // The first asset (two files) was rolled back by copy_verify.
    let left: Vec<_> = fs::read_dir(&library).unwrap().collect();
    assert!(left.is_empty(), "no partial asset may remain: {left:?}");
}

#[test]
fn reimporting_the_same_card_dedupes_by_sha256() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let first = import_card_local(&card, &library).unwrap();
    assert_eq!((first.assets, first.deduped), (3, 0));
    let lib_before = snapshot(&library);
    let card_before = snapshot(&card);

    let second = import_card_local(&card, &library).unwrap();

    assert_eq!((second.assets, second.deduped), (0, 3));
    assert!(second.files.is_empty());
    assert_eq!(snapshot(&library), lib_before);
    assert_eq!(snapshot(&card), card_before);
}

fn read_record(library: &Path, primary: &str) -> serde_json::Value {
    let path = library.join(format!("{primary}{CARD_RECORD_SUFFIX}"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn a_taken_camera_name_renames_the_whole_asset_and_records_it() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    fs::write(library.join("IMG_0004.jpg"), b"someone else's photo").unwrap();
    let taken_before = snapshot(&library);
    let card_before = snapshot(&card);
    let jpg = card.join("DCIM/100TEST/IMG_0004.jpg");
    let xmp = card.join("DCIM/100TEST/IMG_0004.xmp");
    let sha = sha256_hex(&jpg).unwrap();
    let new_jpg = format!("IMG_0004~{}.jpg", &sha[..8]);
    let new_xmp = format!("IMG_0004~{}.xmp", &sha[..8]);

    let report = import_card_local(&card, &library).unwrap();

    assert_eq!(snapshot(&card), card_before, "card untouched");
    assert_eq!((report.assets, report.deduped), (3, 0));
    let now = snapshot(&library);
    assert_eq!(
        now.iter().find(|s| s.relative == "IMG_0004.jpg"),
        taken_before.first(),
        "the existing library file keeps its bytes and mtime"
    );
    assert!(
        !library.join("IMG_0004.xmp").exists(),
        "sidecar renamed with its original"
    );
    for (camera, renamed) in [(&jpg, &new_jpg), (&xmp, &new_xmp)] {
        let copy = library.join(renamed);
        assert_eq!(sha256_hex(&copy).unwrap(), sha256_hex(camera).unwrap());
        assert_eq!(
            fs::metadata(&copy).unwrap().modified().unwrap(),
            fs::metadata(camera).unwrap().modified().unwrap()
        );
    }
    assert_eq!(
        report.renamed,
        vec![
            Renamed {
                camera_filename: "IMG_0004.jpg".into(),
                library_filename: new_jpg.clone()
            },
            Renamed {
                camera_filename: "IMG_0004.xmp".into(),
                library_filename: new_xmp.clone()
            },
        ]
    );
    let record = read_record(&library, &new_jpg);
    let files = record["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["camera_filename"], "IMG_0004.jpg");
    assert_eq!(files[0]["library_filename"], new_jpg.as_str());
    assert_eq!(files[0]["sha256"], sha.as_str());
    assert!(
        !library.join(".albumen-card-staging").exists(),
        "staging removed"
    );

    // A second import of the same card dedupes the renamed asset too.
    let lib_after = snapshot(&library);
    let again = import_card_local(&card, &library).unwrap();
    assert_eq!((again.assets, again.deduped), (0, 3));
    assert!(again.renamed.is_empty());
    assert_eq!(snapshot(&library), lib_after);
}

#[test]
fn a_raw_jpeg_pair_is_renamed_together_when_one_name_is_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    // Only the JPEG half of the pair collides.
    fs::write(library.join("IMG_0003.jpg"), b"other jpeg").unwrap();
    let dng_sha = sha256_hex(&card.join("DCIM/100TEST/IMG_0003.dng")).unwrap();

    let report = import_card_local(&card, &library).unwrap();

    let suffix = &dng_sha[..8];
    assert!(library.join(format!("IMG_0003~{suffix}.dng")).exists());
    assert!(library.join(format!("IMG_0003~{suffix}.jpg")).exists());
    assert!(
        !library.join("IMG_0003.dng").exists(),
        "pair keeps one stem"
    );
    assert_eq!(
        fs::read(library.join("IMG_0003.jpg")).unwrap(),
        b"other jpeg"
    );
    assert_eq!(report.renamed.len(), 2);
}

#[test]
fn a_renamed_target_with_other_bytes_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let sha = sha256_hex(&card.join("DCIM/100TEST/IMG_0004.jpg")).unwrap();
    let squatter = format!("IMG_0004~{}.jpg", &sha[..8]);
    fs::write(library.join("IMG_0004.jpg"), b"taken").unwrap();
    fs::write(library.join(&squatter), b"also taken").unwrap();
    let card_before = snapshot(&card);

    let err = import_card_local(&card, &library).unwrap_err();

    assert!(err.to_string().contains("not overwritten"), "{err}");
    assert_eq!(snapshot(&card), card_before);
    assert_eq!(fs::read(library.join(&squatter)).unwrap(), b"also taken");
    assert_eq!(fs::read(library.join("IMG_0004.jpg")).unwrap(), b"taken");
}

#[test]
fn a_library_inside_the_card_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = card.join("library");
    fs::create_dir_all(&library).unwrap();
    let before = snapshot(&card);
    assert!(import_card_local(&card, &library).is_err());
    assert_eq!(snapshot(&card), before);
}
