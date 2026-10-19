// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::SystemTime;

use crate::discover::{file_name_str, Asset, Role};
use crate::error::IngestError;

/// A file that was written and hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedObject {
    pub filename: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mtime: SystemTime,
}

/// A verified file plus its role in the asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub filename: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mtime: SystemTime,
    pub role: Role,
}

/// Result of copying an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub files: Vec<StoredFile>,
}

/// Destination that can store opaque original bytes.
///
/// Implementations must not transform bytes, must keep the filename, must
/// persist mtime (as a filesystem timestamp or as object metadata), must
/// re-read and hash after the write, and must refuse to overwrite.
pub trait BlobStore {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError>;
    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError>;
    fn delete(&self, filename: &str) -> Result<(), IngestError>;
}

/// Copy every file in `asset` into `store`.
///
/// Filenames and mtimes are preserved. Each destination is hashed and
/// compared to the bytes that were read. If any file fails, files already
/// written by this call are deleted and the error is returned. Sources are
/// not modified.
pub fn copy_verify<S: BlobStore + ?Sized>(
    asset: &Asset,
    store: &S,
) -> Result<IngestReport, IngestError> {
    let mut stored = Vec::with_capacity(asset.files().len());
    for file in asset.files() {
        let filename = file_name_str(&file.path)?;
        match store.put_verified(&file.path, &filename) {
            Ok(object) => stored.push(StoredFile {
                filename: object.filename,
                sha256: object.sha256,
                byte_len: object.byte_len,
                mtime: object.mtime,
                role: file.role,
            }),
            Err(err) => {
                let mut rollback_notes = Vec::new();
                for done in stored.iter().rev() {
                    if let Err(rollback) = store.delete(&done.filename) {
                        rollback_notes.push(format!("{}: {rollback}", done.filename));
                    }
                }
                if rollback_notes.is_empty() {
                    return Err(err);
                }
                return Err(IngestError::store(
                    "rollback",
                    "asset",
                    format!("{err}; rollback failures: {}", rollback_notes.join("; ")),
                ));
            }
        }
    }
    Ok(IngestReport { files: stored })
}

/// Explicit per-run opt-in for moving sources out of the library path.
///
/// There is no default. `copy_verify` never constructs this. The caller
/// chooses the trash directory for this call only.
#[derive(Debug, Clone)]
pub struct Quarantine {
    trash_dir: std::path::PathBuf,
}

impl Quarantine {
    pub fn new(trash_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            trash_dir: trash_dir.into(),
        }
    }

    pub fn trash_dir(&self) -> &Path {
        &self.trash_dir
    }
}

/// Quarantine cannot target a source.
///
/// A user's archive may be their only copy. This function used to rename
/// sources into a trash directory after a verified copy. It no longer copies
/// and it no longer renames. There is no flag that turns the rename back on.
pub fn copy_verify_quarantine<S: BlobStore + ?Sized>(
    asset: &Asset,
    _store: &S,
    _quarantine: &Quarantine,
) -> Result<IngestReport, IngestError> {
    let path = asset
        .files()
        .first()
        .map(|file| file.path.display().to_string())
        .unwrap_or_else(|| "source".to_string());
    Err(IngestError::SourceReadOnly { path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{Asset, AssetFile, Role};
    use crate::local::LocalStore;
    use crate::test_support::{write_file, Tmp};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    struct BoomStore {
        puts: AtomicUsize,
        deleted: AtomicUsize,
    }

    impl BlobStore for BoomStore {
        fn put_verified(
            &self,
            _source: &Path,
            filename: &str,
        ) -> Result<VerifiedObject, IngestError> {
            let n = self.puts.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                return Err(IngestError::store("put", filename, "boom"));
            }
            Ok(VerifiedObject {
                filename: filename.to_string(),
                sha256: "abc".to_string(),
                byte_len: 1,
                mtime: UNIX_EPOCH,
            })
        }

        fn get_verified(
            &self,
            filename: &str,
            _dest_dir: &Path,
        ) -> Result<VerifiedObject, IngestError> {
            Err(IngestError::store("get", filename, "unused"))
        }

        fn delete(&self, _filename: &str) -> Result<(), IngestError> {
            self.deleted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn failed_copy_rolls_back_earlier_files() {
        let store = BoomStore {
            puts: AtomicUsize::new(0),
            deleted: AtomicUsize::new(0),
        };
        let asset = Asset::from_files(vec![
            AssetFile {
                path: Path::new("a.jpg").to_path_buf(),
                role: Role::Primary,
            },
            AssetFile {
                path: Path::new("a.xmp").to_path_buf(),
                role: Role::Sidecar,
            },
        ])
        .unwrap();
        let err = copy_verify(&asset, &store).unwrap_err();
        assert!(matches!(err, IngestError::Store { .. }));
        assert_eq!(store.deleted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn default_copy_does_not_remove_or_alter_a_source() {
        let tmp = Tmp::new();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let source = write_file(&src, "solo.jpg", b"original-bytes", mtime);
        let before = source_snap(&source);
        let asset = Asset::from_files(vec![AssetFile {
            path: source.clone(),
            role: Role::Primary,
        }])
        .unwrap();
        let dest = tmp.path().join("library");
        let report = copy_verify(&asset, &LocalStore::new(&dest)).unwrap();
        assert_eq!(
            std::fs::read(dest.join("solo.jpg")).unwrap(),
            b"original-bytes"
        );
        assert_eq!(report.files[0].mtime, mtime);
        assert_same_source(&source, &before);

        let err = copy_verify(&asset, &LocalStore::new(&dest)).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_same_source(&source, &before);
        assert_eq!(
            std::fs::read(dest.join("solo.jpg")).unwrap(),
            b"original-bytes"
        );
    }

    #[test]
    fn failed_asset_copy_leaves_every_source_untouched() {
        let tmp = Tmp::new();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_100);
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let primary = write_file(&src, "pair.jpg", b"still", mtime);
        let sidecar = write_file(&src, "pair.xmp", b"<xmp/>", mtime);
        let primary_before = source_snap(&primary);
        let sidecar_before = source_snap(&sidecar);
        let asset = Asset::from_files(vec![
            AssetFile {
                path: primary.clone(),
                role: Role::Primary,
            },
            AssetFile {
                path: sidecar.clone(),
                role: Role::Sidecar,
            },
        ])
        .unwrap();
        let dest = tmp.path().join("library");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("pair.xmp"), b"different").unwrap();
        let err = copy_verify(&asset, &LocalStore::new(&dest)).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_same_source(&primary, &primary_before);
        assert_same_source(&sidecar, &sidecar_before);
        assert!(!dest.join("pair.jpg").exists());
        assert_eq!(std::fs::read(dest.join("pair.xmp")).unwrap(), b"different");
    }

    #[test]
    fn quarantine_cannot_target_a_source() {
        let tmp = Tmp::new();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_200);
        let source = write_file(tmp.path(), "solo.jpg", b"original", mtime);
        #[cfg(unix)]
        let before = source_snap(&source);
        let asset = Asset::from_files(vec![AssetFile {
            path: source.clone(),
            role: Role::Primary,
        }])
        .unwrap();
        let dest = tmp.path().join("library");
        let trash = tmp.path().join("trash");
        let err = copy_verify_quarantine(&asset, &LocalStore::new(&dest), &Quarantine::new(&trash))
            .unwrap_err();
        assert!(matches!(err, IngestError::SourceReadOnly { .. }), "{err}");
        assert_same_source(&source, &before);
        assert!(!dest.join("solo.jpg").exists());
        assert!(!trash.exists());
    }

    #[test]
    fn quarantine_refuses_even_when_trash_is_empty() {
        let tmp = Tmp::new();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_300);
        let source = write_file(tmp.path(), "solo.jpg", b"original", mtime);
        let before = source_snap(&source);
        let trash = tmp.path().join("trash");
        std::fs::create_dir(&trash).unwrap();
        let asset = Asset::from_files(vec![AssetFile {
            path: source.clone(),
            role: Role::Primary,
        }])
        .unwrap();
        let err = copy_verify_quarantine(
            &asset,
            &LocalStore::new(tmp.path().join("library")),
            &Quarantine::new(&trash),
        )
        .unwrap_err();
        assert!(matches!(err, IngestError::SourceReadOnly { .. }), "{err}");
        assert_same_source(&source, &before);
        assert!(trash.read_dir().unwrap().next().is_none());
    }

    struct SourceSnap {
        bytes: Vec<u8>,
        mtime: std::time::SystemTime,
        #[cfg(unix)]
        dev: u64,
        #[cfg(unix)]
        ino: u64,
    }

    fn source_snap(path: &Path) -> SourceSnap {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).unwrap();
        SourceSnap {
            bytes: std::fs::read(path).unwrap(),
            mtime: meta.modified().unwrap(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
        }
    }

    fn assert_same_source(path: &Path, before: &SourceSnap) {
        assert!(path.is_file());
        let after = source_snap(path);
        assert_eq!(after.bytes, before.bytes);
        assert_eq!(after.mtime, before.mtime);
        #[cfg(unix)]
        {
            assert_eq!(after.dev, before.dev);
            assert_eq!(after.ino, before.ino);
        }
    }
}
