// SPDX-License-Identifier: Apache-2.0

//! The only module that creates, writes, renames, or removes files.
//!
//! A source archive is never passed here as something to modify. Callers
//! refuse a destination that resolves inside a [`crate::source::SourceRoot`]
//! before they ask this module to create a directory or a file.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::IngestError;

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

pub fn set_mtime(path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    let opened = opened(path);
    let file = OpenOptions::new()
        .write(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))?;
    file.set_times(std::fs::FileTimes::new().set_modified(mtime))
        .map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    Ok(())
}
