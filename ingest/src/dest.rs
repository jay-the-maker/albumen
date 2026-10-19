// SPDX-License-Identifier: Apache-2.0

//! The only module that creates, writes, renames, or removes files.
//!
//! A source archive is never passed here as something to modify. Callers
//! refuse a destination that resolves inside a [`crate::source::SourceRoot`]
//! before they ask this module to create a directory or a file.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::time::SystemTime;

use crate::error::IngestError;

/// Which durability call last ran on this thread. Tests read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DirSyncKind {
    Unix = 1,
    /// `FlushFileBuffers` on a directory opened with `FILE_FLAG_BACKUP_SEMANTICS`.
    Flush = 2,
    /// `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`.
    WriteThrough = 3,
    /// Both Windows calls failed. A warning was logged. The rename still landed.
    Warned = 4,
}

thread_local! {
    static LAST_DIR_SYNC: Cell<u8> = const { Cell::new(0) };
}

fn note_dir_sync(kind: DirSyncKind) {
    LAST_DIR_SYNC.set(kind as u8);
}

#[cfg(test)]
pub(crate) fn last_dir_sync() -> DirSyncKind {
    match LAST_DIR_SYNC.get() {
        1 => DirSyncKind::Unix,
        2 => DirSyncKind::Flush,
        3 => DirSyncKind::WriteThrough,
        4 => DirSyncKind::Warned,
        other => panic!("directory sync was not recorded ({other})"),
    }
}

pub fn create_dir_all(path: &Path) -> Result<(), IngestError> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        // Windows can deny CreateDirectory on a directory that already exists,
        // including the process temp root. The directory is there.
        Err(_) if path.is_dir() => Ok(()),
        Err(err) => Err(IngestError::io(path, err)),
    }
}

pub fn create_file(path: &Path) -> Result<File, IngestError> {
    File::create(path).map_err(|err| IngestError::io(path, err))
}

pub fn open_new(path: &Path) -> Result<File, IngestError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| IngestError::io(path, err))
}

pub fn open_append(path: &Path) -> Result<File, IngestError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| IngestError::io(path, err))
}

pub fn open_read_write(path: &Path) -> Result<File, IngestError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|err| IngestError::io(path, err))
}

pub fn remove_file(path: &Path) -> Result<(), IngestError> {
    fs::remove_file(path).map_err(|err| IngestError::io(path, err))
}

pub fn remove_file_best_effort(path: &Path) {
    let _ = fs::remove_file(path);
}

pub fn rename(from: &Path, to: &Path) -> Result<(), IngestError> {
    #[cfg(windows)]
    {
        return rename_durable(from, to);
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to).map_err(|err| IngestError::io(to, err))
    }
}

pub fn set_len(file: &File, path: &Path, len: u64) -> Result<(), IngestError> {
    file.set_len(len).map_err(|err| IngestError::io(path, err))
}

/// Make a directory entry durable after a create or a rename into `path`.
///
/// Unix opens the directory and `fsync`s it. A failure is returned.
///
/// Windows will not open a directory with ordinary `File::open` (access
/// denied). Two calls are tried, in order:
///
/// 1. Open the directory with `FILE_FLAG_BACKUP_SEMANTICS` and call
///    `FlushFileBuffers` (`File::sync_all`).
/// 2. `MoveFileExW(path, path, MOVEFILE_WRITE_THROUGH)`, which asks the
///    filesystem to flush that metadata. This does not replace the directory.
///
/// If both fail, a warning is logged and the function returns success. The
/// file rename or create has already landed; durability is the part that
/// could not be confirmed. Callers that rename use
/// [`rename`], which tries `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH` first and only then this flush.
pub fn sync_dir(path: &Path) -> Result<(), IngestError> {
    #[cfg(windows)]
    {
        return sync_dir_windows(path);
    }
    #[cfg(not(windows))]
    {
        let dir = File::open(path).map_err(|err| IngestError::io(path, err))?;
        dir.sync_all().map_err(|err| IngestError::io(path, err))?;
        note_dir_sync(DirSyncKind::Unix);
        Ok(())
    }
}

#[cfg(windows)]
fn sync_dir_windows(path: &Path) -> Result<(), IngestError> {
    match flush_dir(path) {
        Ok(()) => {
            note_dir_sync(DirSyncKind::Flush);
            Ok(())
        }
        Err(flush_err) => match move_file_ex(path, path, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                note_dir_sync(DirSyncKind::WriteThrough);
                Ok(())
            }
            Err(move_err) => {
                warn_dir(path, &flush_err, &move_err);
                Ok(())
            }
        },
    }
}

#[cfg(windows)]
fn rename_durable(from: &Path, to: &Path) -> Result<(), IngestError> {
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    match move_file_ex(from, to, flags) {
        Ok(()) => {
            note_dir_sync(DirSyncKind::WriteThrough);
            Ok(())
        }
        Err(move_err) => {
            fs::rename(from, to).map_err(|err| IngestError::io(to, err))?;
            let parent = to
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            match flush_dir(parent) {
                Ok(()) => {
                    note_dir_sync(DirSyncKind::Flush);
                    Ok(())
                }
                Err(flush_err) => {
                    warn_dir(parent, &flush_err, &move_err);
                    Ok(())
                }
            }
        }
    }
}

#[cfg(windows)]
fn warn_dir(path: &Path, flush_err: &std::io::Error, move_err: &std::io::Error) {
    eprintln!(
        "albumen: warning: directory {} was not flushed: FlushFileBuffers failed ({flush_err}); MoveFileExW write-through failed ({move_err})",
        path.display()
    );
    note_dir_sync(DirSyncKind::Warned);
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

#[cfg(windows)]
fn flush_dir(path: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // `sync_all` is `FlushFileBuffers` on Windows.
    file.sync_all()
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn move_file_ex(from: &Path, to: &Path, flags: u32) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    let from_wide: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to_wide: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both buffers are valid, null-terminated UTF-16, and live for the call.
    let moved = unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

/// Write a new file and, on Unix, restrict it to the owner.
///
/// Used for a family signing key that lives outside the library. An existing
/// path is left untouched.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_all(parent)?;
        }
    }
    let mut file = open_new(path)?;
    std::io::Write::write_all(&mut file, bytes).map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|err| IngestError::io(path, err))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions).map_err(|err| IngestError::io(path, err))?;
    }
    Ok(())
}

pub fn set_mtime(path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|err| IngestError::io(path, err))?;
    file.set_times(std::fs::FileTimes::new().set_modified(mtime))
        .map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{last_dir_sync, sync_dir, DirSyncKind};
    use crate::test_support::Tmp;
    use std::fs;

    #[test]
    fn directory_sync_runs_on_this_platform() {
        let tmp = Tmp::new();
        let dir = tmp.path().join("library");
        fs::create_dir(&dir).unwrap();
        sync_dir(&dir).unwrap();
        #[cfg(windows)]
        {
            let kind = last_dir_sync();
            assert!(
                kind == DirSyncKind::Flush || kind == DirSyncKind::WriteThrough,
                "windows directory sync did not flush or write-through: {kind:?}"
            );
        }
        #[cfg(not(windows))]
        assert_eq!(last_dir_sync(), DirSyncKind::Unix);
    }

    #[cfg(windows)]
    #[test]
    fn windows_rename_uses_write_through_or_flush() {
        use super::rename;
        let tmp = Tmp::new();
        let from = tmp.path().join("from.txt");
        let to = tmp.path().join("to.txt");
        fs::write(&from, b"durable").unwrap();
        rename(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"durable");
        let kind = last_dir_sync();
        assert!(
            kind == DirSyncKind::WriteThrough || kind == DirSyncKind::Flush,
            "windows rename did not use MoveFileExW write-through or FlushFileBuffers: {kind:?}"
        );
    }
}
