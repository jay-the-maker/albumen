// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::checksum::{copy_hashed, sha256_file, to_hex};
use crate::dest;
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::mtime::{mtime_of, set_mtime, sync_file, to_unix_nanos_string};
use crate::source::{parent_dir, SourceRoot};
use crate::store::{BlobStore, VerifiedObject};

/// Filesystem destination. The directory is created if it is missing.
/// Existing files are never overwritten.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl BlobStore for LocalStore {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        let actual = file_name_str(source)?;
        if actual != filename {
            return Err(IngestError::FilenameMismatch {
                source_name: actual,
                requested: filename.to_string(),
            });
        }
        copy_verify_file(source, &self.root)
    }

    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        copy_verify_file(&self.root.join(filename), dest_dir)
    }

    fn delete(&self, filename: &str) -> Result<(), IngestError> {
        check_filename(filename)?;
        let path = self.root.join(filename);
        match dest::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(IngestError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

/// Copy one file into `dest_dir`, keeping its filename and mtime.
///
/// The destination file is hashed after the write. On mismatch the
/// destination is removed and the error names both SHA-256 digests.
pub fn copy_verify_file(source: &Path, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
    if !source.is_file() {
        return Err(IngestError::NotAFile(source.to_path_buf()));
    }
    let filename = file_name_str(source)?;
    check_filename(&filename)?;
    let root = SourceRoot::open(parent_dir(source))?;
    root.refuse_destination(dest_dir)?;
    let dest = dest_dir.join(&filename);
    root.refuse_destination(&dest)?;
    dest::create_dir_all(dest_dir)?;
    if dest.exists() {
        return Err(IngestError::AlreadyExists {
            filename: filename.clone(),
            destination: dest.display().to_string(),
        });
    }

    let mtime = mtime_of(source)?;
    let mut guard = RemoveOnDrop {
        path: dest.clone(),
        armed: true,
    };
    let input = root.open_read(source)?;
    let output = dest::open_new(&dest)?;
    let written_hash = copy_hashed(input, &output, source, &dest)?;
    sync_file(&output, &dest)?;
    drop(output);

    let record = finalize_verified(&dest, &filename, &written_hash, mtime)?;
    guard.armed = false;
    Ok(record)
}

/// Stream `reader` into a new file at `dest` and verify it the same way as
/// [`copy_verify_file`].
///
/// The destination is created with `create_new`. An existing path is left
/// untouched. On a hash or mtime mismatch, only that new file is removed.
pub(crate) fn copy_verify_reader<R: Read>(
    reader: &mut R,
    source_label: &Path,
    dest: &Path,
    mtime: std::time::SystemTime,
) -> Result<VerifiedObject, IngestError> {
    let filename = file_name_str(dest)?;
    check_filename(&filename)?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }
    if fs::symlink_metadata(dest).is_ok() {
        return Err(IngestError::AlreadyExists {
            filename: filename.clone(),
            destination: dest.display().to_string(),
        });
    }

    let mut guard = RemoveOnDrop {
        path: dest.to_path_buf(),
        armed: true,
    };
    let output = dest::open_new(dest)?;
    let written_hash = copy_hashed(reader, &output, source_label, dest)?;
    sync_file(&output, dest)?;
    drop(output);

    let record = finalize_verified(dest, &filename, &written_hash, mtime)?;
    guard.armed = false;
    Ok(record)
}

pub(crate) fn finalize_verified(
    path: &Path,
    filename: &str,
    expected: &[u8; 32],
    mtime: std::time::SystemTime,
) -> Result<VerifiedObject, IngestError> {
    let actual = sha256_file(path)?;
    if actual != *expected {
        let removed = dest::remove_file(path).is_ok();
        return Err(IngestError::checksum_mismatch(
            filename,
            to_hex(expected),
            to_hex(&actual),
            path.display().to_string(),
            removed,
        ));
    }

    let expected_mtime = crate::mtime::representable_mtime(mtime);
    set_mtime(path, expected_mtime)?;
    let stored_mtime = mtime_of(path)?;
    if stored_mtime != expected_mtime {
        let removed = dest::remove_file(path).is_ok();
        return Err(IngestError::MtimeNotPreserved {
            filename: filename.to_string(),
            expected_unix_nanos: to_unix_nanos_string(expected_mtime),
            actual_unix_nanos: to_unix_nanos_string(stored_mtime),
            destination: path.display().to_string(),
            removed,
        });
    }

    let byte_len = fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len();
    Ok(VerifiedObject {
        filename: filename.to_string(),
        sha256: to_hex(expected),
        byte_len,
        mtime: stored_mtime,
    })
}

struct RemoveOnDrop {
    path: PathBuf,
    armed: bool,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if self.armed {
            dest::remove_file_best_effort(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_file, Tmp};
    use std::time::{Duration, UNIX_EPOCH};

    fn fixed_mtime() -> std::time::SystemTime {
        // 100 ns aligned so NTFS can store this exact tick.
        UNIX_EPOCH + Duration::new(1_339_746_000, 123_456_700)
    }

    #[test]
    fn copy_keeps_the_finest_mtime_the_filesystem_stores() {
        let tmp = Tmp::new();
        let requested = UNIX_EPOCH + Duration::new(1_339_746_000, 123_456_789);
        let src_dir = tmp.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        let source = write_file(&src_dir, "tick.jpg", b"tick-bytes", requested);
        let stored = mtime_of(&source).unwrap();
        assert_eq!(stored, crate::mtime::representable_mtime(requested));
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();
        copy_verify_file(&source, &dest_dir).unwrap();
        assert_eq!(mtime_of(&dest_dir.join("tick.jpg")).unwrap(), stored);
        assert_eq!(fs::read(dest_dir.join("tick.jpg")).unwrap(), b"tick-bytes");
    }

    #[test]
    fn copy_preserves_bytes_name_and_mtime() {
        let tmp = Tmp::new();
        let src_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src_dir).unwrap();
        let bytes: Vec<u8> = (0..1_500_000).map(|i| (i % 251) as u8).collect();
        let source = write_file(&src_dir, "IMG 0001.jpg", &bytes, fixed_mtime());
        let stored = copy_verify_file(&source, &dest_dir).unwrap();
        let dest = dest_dir.join("IMG 0001.jpg");
        assert_eq!(fs::read(&dest).unwrap(), bytes);
        assert_eq!(stored.filename, "IMG 0001.jpg");
        assert_eq!(stored.sha256, crate::sha256_hex(&source).unwrap());
        assert_eq!(mtime_of(&dest).unwrap(), fixed_mtime());
        assert_eq!(stored.byte_len, bytes.len() as u64);
    }

    #[test]
    fn mismatch_removes_destination_and_names_both_hashes() {
        let tmp = Tmp::new();
        let path = write_file(tmp.path(), "bad.jpg", b"tampered-bytes", fixed_mtime());
        let expected = [7u8; 32];
        let err = finalize_verified(&path, "bad.jpg", &expected, fixed_mtime()).unwrap_err();
        match err {
            IngestError::ChecksumMismatch {
                filename,
                expected_sha256,
                actual_sha256,
                removed,
                message,
                ..
            } => {
                assert_eq!(filename, "bad.jpg");
                assert!(removed);
                assert_ne!(expected_sha256, actual_sha256);
                assert!(message.contains("checksum mismatch"));
                assert!(message.contains(&expected_sha256));
                assert!(message.contains(&actual_sha256));
                assert!(message.contains("removed destination"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_overwrite() {
        let tmp = Tmp::new();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.jpg", b"one", fixed_mtime());
        let dest = tmp.path().join("out");
        copy_verify_file(&source, &dest).unwrap();
        let err = copy_verify_file(&source, &dest).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_eq!(fs::read(dest.join("a.jpg")).unwrap(), b"one");
    }

    #[test]
    fn copy_refuses_a_destination_inside_the_source() {
        let tmp = Tmp::new();
        let src = tmp.path().join("library");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.txt", b"alpha", fixed_mtime());
        let nested = src.join("nested");
        let err = copy_verify_file(&source, &nested).unwrap_err();
        assert!(
            matches!(err, IngestError::DestinationInsideSource { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("sources are read-only"), "{err}");
        assert!(!nested.exists());
        assert_eq!(fs::read(&source).unwrap(), b"alpha");
    }

    #[test]
    fn copy_refuses_a_hardlink_of_a_source_file() {
        let tmp = Tmp::new();
        let src = tmp.path().join("library");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.txt", b"alpha", fixed_mtime());
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let link = outside.join("a.txt");
        fs::hard_link(&source, &link).unwrap();
        let err = copy_verify_file(&source, &outside).unwrap_err();
        assert!(
            matches!(err, IngestError::DestinationInsideSource { .. }),
            "{err}"
        );
        assert_eq!(fs::read(&source).unwrap(), b"alpha");
        assert_eq!(fs::read(&link).unwrap(), b"alpha");
    }
}
