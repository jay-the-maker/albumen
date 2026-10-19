// SPDX-License-Identifier: Apache-2.0

//! The only module that creates, writes, renames, or removes files.
//!
//! A source archive is never passed here as something to modify. Callers
//! refuse a destination that resolves inside a [`crate::source::SourceRoot`]
//! before they ask this module to create a directory or a file.

use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::time::SystemTime;

use crate::error::IngestError;

pub fn create_dir_all(path: &Path) -> Result<(), IngestError> {
    fs::create_dir_all(path).map_err(|err| IngestError::io(path, err))
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
    fs::rename(from, to).map_err(|err| IngestError::io(to, err))
}

pub fn set_len(file: &File, path: &Path, len: u64) -> Result<(), IngestError> {
    file.set_len(len).map_err(|err| IngestError::io(path, err))
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
