// SPDX-License-Identifier: Apache-2.0

//! The only module that creates, writes, renames, or removes files.
//!
//! A source archive is never passed here as something to modify. Callers
//! refuse a destination that resolves inside a [`crate::source::SourceRoot`]
//! before they ask this module to create a directory or a file.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
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
}

thread_local! {
    static LAST_DIR_SYNC: Cell<u8> = const { Cell::new(0) };
}

fn note_dir_sync(kind: DirSyncKind) {
    LAST_DIR_SYNC.set(kind as u8);
}

static DIR_FLUSH_OK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIR_WRITE_THROUGH_OK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIR_SYNC_FAILED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn count(counter: &std::sync::atomic::AtomicU64) {
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Directory durability counts for this process:
/// `(flushed, write_through, failed)`.
///
/// `flushed` is a directory flush that succeeded (`fsync` on Unix,
/// `FlushFileBuffers` on a write handle on Windows). `write_through` is a
/// rename that `MoveFileExW` committed with `MOVEFILE_WRITE_THROUGH`.
/// `failed` is a directory sync that returned an error. The bench prints these
/// and fails when `failed` is not zero.
pub fn dir_sync_counts() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        DIR_FLUSH_OK.load(Relaxed),
        DIR_WRITE_THROUGH_OK.load(Relaxed),
        DIR_SYNC_FAILED.load(Relaxed),
    )
}

#[cfg(test)]
pub(crate) fn last_dir_sync() -> DirSyncKind {
    match LAST_DIR_SYNC.get() {
        1 => DirSyncKind::Unix,
        2 => DirSyncKind::Flush,
        3 => DirSyncKind::WriteThrough,
        other => panic!("directory sync was not recorded ({other})"),
    }
}

fn opened(path: &Path) -> PathBuf {
    crate::platform::io_path(path)
}

pub fn create_dir_all(path: &Path) -> Result<(), IngestError> {
    let opened = opened(path);
    match fs::create_dir_all(&opened) {
        Ok(()) => Ok(()),
        // Windows can deny CreateDirectory on a directory that already exists,
        // including the process temp root. The directory is there.
        Err(_) if opened.is_dir() || path.is_dir() => Ok(()),
        Err(err) => Err(IngestError::io(path, err)),
    }
}

pub fn create_file(path: &Path) -> Result<File, IngestError> {
    let opened = opened(path);
    File::create(&opened).map_err(|err| IngestError::io(path, err))
}

pub fn open_new(path: &Path) -> Result<File, IngestError> {
    let opened = opened(path);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))
}

pub fn open_append(path: &Path) -> Result<File, IngestError> {
    let opened = opened(path);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))
}

/// Append one line to a log beside a library. The line is fsynced.
///
/// An existing log is extended. It is not replaced and it is not removed.
pub fn append_line(path: &Path, line: &str) -> Result<(), IngestError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_all(parent)?;
        }
    }
    let mut file = open_append(path)?;
    let mut bytes = line.as_bytes().to_vec();
    if !line.ends_with('\n') {
        bytes.push(b'\n');
    }
    std::io::Write::write_all(&mut file, &bytes).map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    drop(file);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

pub fn open_read_write(path: &Path) -> Result<File, IngestError> {
    let opened = opened(path);
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))
}

pub fn remove_file(path: &Path) -> Result<(), IngestError> {
    let opened = opened(path);
    fs::remove_file(&opened).map_err(|err| IngestError::io(path, err))
}

pub fn remove_file_best_effort(path: &Path) {
    let _ = fs::remove_file(opened(path));
}

#[cfg(windows)]
pub fn rename(from: &Path, to: &Path) -> Result<(), IngestError> {
    rename_durable(from, to)
}

#[cfg(not(windows))]
pub fn rename(from: &Path, to: &Path) -> Result<(), IngestError> {
    let from_opened = opened(from);
    let to_opened = opened(to);
    fs::rename(&from_opened, &to_opened).map_err(|err| IngestError::io(to, err))
}

/// `file:stream` for one named NTFS stream. The file portion uses `\\?\` when needed.
pub fn stream_path(file: &Path, stream: &str) -> PathBuf {
    let mut name = opened(file).into_os_string();
    name.push(":");
    name.push(stream);
    PathBuf::from(name)
}

pub fn open_stream(file: &Path, stream: &str) -> Result<File, IngestError> {
    let path = stream_path(file, stream);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| IngestError::io(path, err))
}

pub fn remove_stream(file: &Path, stream: &str) {
    let _ = fs::remove_file(stream_path(file, stream));
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
#[cfg(windows)]
pub fn sync_dir(path: &Path) -> Result<(), IngestError> {
    sync_dir_windows(path)
}

#[cfg(not(windows))]
pub fn sync_dir(path: &Path) -> Result<(), IngestError> {
    let synced = File::open(opened(path)).and_then(|dir| dir.sync_all());
    if let Err(err) = synced {
        count(&DIR_SYNC_FAILED);
        return Err(IngestError::io(path, err));
    }
    count(&DIR_FLUSH_OK);
    note_dir_sync(DirSyncKind::Unix);
    Ok(())
}

#[cfg(windows)]
fn sync_dir_windows(path: &Path) -> Result<(), IngestError> {
    // `FlushFileBuffers` needs a handle with GENERIC_WRITE, and a directory
    // opens only with FILE_FLAG_BACKUP_SEMANTICS. A failure is an error: the
    // directory entry is not known to be durable.
    match flush_dir(&opened(path)) {
        Ok(()) => {
            count(&DIR_FLUSH_OK);
            note_dir_sync(DirSyncKind::Flush);
            Ok(())
        }
        Err(err) => {
            count(&DIR_SYNC_FAILED);
            Err(IngestError::io(path, err))
        }
    }
}

#[cfg(windows)]
fn rename_durable(from: &Path, to: &Path) -> Result<(), IngestError> {
    // `MOVEFILE_WRITE_THROUGH` does not return until the move is flushed to
    // disk. If that call fails, the rename uses `fs::rename`, and the parent
    // directory is then flushed through a write handle. A failed flush is an
    // error.
    let from_opened = opened(from);
    let to_opened = opened(to);
    let flags = albumen_win::MOVEFILE_REPLACE_EXISTING | albumen_win::MOVEFILE_WRITE_THROUGH;
    if albumen_win::move_file_ex(&from_opened, &to_opened, flags).is_ok() {
        count(&DIR_WRITE_THROUGH_OK);
        note_dir_sync(DirSyncKind::WriteThrough);
        return Ok(());
    }
    fs::rename(&from_opened, &to_opened).map_err(|err| IngestError::io(to, err))?;
    let parent = to
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_dir_windows(parent)
}

#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

#[cfg(windows)]
fn flush_dir(path: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // `sync_all` is `FlushFileBuffers` on Windows.
    file.sync_all()
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

/// Set the modification time on a file this process already has open.
///
/// This does not fsync. The caller fsyncs that same handle so the bytes and
/// the time become durable together, before the file is reported stored.
pub fn set_mtime_on(file: &File, path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    file.set_times(std::fs::FileTimes::new().set_modified(mtime))
        .map_err(|err| IngestError::io(path, err))
}

/// Set the modification time and fsync that file once.
///
/// The `sync_all` commits the time and any bytes still dirty for this inode.
pub fn set_mtime(path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    let opened = opened(path);
    let file = OpenOptions::new()
        .write(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))?;
    set_mtime_on(&file, path, mtime)?;
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
            assert_eq!(
                kind,
                DirSyncKind::Flush,
                "windows directory sync did not flush through a write handle"
            );
        }
        #[cfg(not(windows))]
        assert_eq!(last_dir_sync(), DirSyncKind::Unix);
    }

    #[test]
    fn a_directory_flush_never_fails_and_is_counted() {
        let tmp = Tmp::new();
        let dir = tmp.path().join("library");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("photo.jpg"), b"bytes").unwrap();
        let (flushed_before, _, _) = super::dir_sync_counts();
        for _ in 0..3 {
            sync_dir(&dir).expect("directory flush returned an error");
        }
        let (flushed, _, _) = super::dir_sync_counts();
        assert!(flushed >= flushed_before + 3, "flushes were not counted");
    }

    #[test]
    fn a_missing_directory_is_an_error_and_counted_as_failed() {
        let tmp = Tmp::new();
        let (_, _, failed_before) = super::dir_sync_counts();
        assert!(sync_dir(&tmp.path().join("missing")).is_err());
        let (_, _, failed) = super::dir_sync_counts();
        assert!(failed > failed_before);
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
