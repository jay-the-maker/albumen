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

/// Copy-verify the asset, then delete the sources.
///
/// Sources are removed only after every destination has verified. A failed
/// copy leaves the sources in place and rolls the destination back.
pub fn move_verified<S: BlobStore + ?Sized>(
    asset: &Asset,
    store: &S,
) -> Result<IngestReport, IngestError> {
    let report = copy_verify(asset, store)?;
    for file in asset.files() {
        std::fs::remove_file(&file.path).map_err(|err| IngestError::io(&file.path, err))?;
    }
    Ok(report)
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
    fn move_deletes_source_only_after_verify() {
        let tmp = Tmp::new();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        let source = write_file(tmp.path(), "solo.jpg", b"original", mtime);
        let asset = Asset::from_files(vec![AssetFile {
            path: source.clone(),
            role: Role::Primary,
        }])
        .unwrap();
        let dest = tmp.path().join("library");
        let report = move_verified(&asset, &LocalStore::new(&dest)).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(dest.join("solo.jpg")).unwrap(), b"original");
        assert_eq!(
            report.files[0].sha256,
            crate::sha256_hex(&dest.join("solo.jpg")).unwrap()
        );
        assert_eq!(report.files[0].mtime, mtime);
    }
}
