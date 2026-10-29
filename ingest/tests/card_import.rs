// SPDX-License-Identifier: Apache-2.0

//! Issue #28: a mounted card is imported through copy_verify and left
//! unchanged. Synthetic fixtures only.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use albumen_ingest::{import_card, sha256_hex, BlobStore, IngestError, LocalStore, VerifiedObject};

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
