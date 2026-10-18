// SPDX-License-Identifier: Apache-2.0

//! A received upload is a temporary file until `copy_verify_file` accepts it.
//! The library is never overwritten, and a failed upload deletes only the spool.

use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use albumen_ingest::{copy_verify_file, VerifiedObject};

#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "library path is not a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn has(&self, filename: &str) -> bool {
        self.root.join(filename).is_file()
    }

    pub fn files(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn commit(&self, spool: &Spool) -> Result<VerifiedObject, String> {
        copy_verify_file(&spool.path, &self.root).map_err(|err| err.to_string())
    }
}

pub struct Spool {
    _dir: tempfile::TempDir,
    path: PathBuf,
    file: File,
    len: u64,
}

impl Spool {
    pub fn create(filename: &str) -> Result<Self, String> {
        let filename = stored_name(filename)?;
        let dir = tempfile::TempDir::new().map_err(|err| err.to_string())?;
        let path = dir.path().join(&filename);
        let file = File::create(&path).map_err(|err| err.to_string())?;
        Ok(Self {
            _dir: dir,
            path,
            file,
            len: 0,
        })
    }

    pub fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), String> {
        if offset > self.len {
            return Err("upload has a gap".to_string());
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|err| err.to_string())?;
        self.file.write_all(data).map_err(|err| err.to_string())?;
        let end = offset + data.len() as u64;
        if end > self.len {
            self.len = end;
        }
        Ok(())
    }

    pub fn set_mtime(&self, seconds: u64) -> Result<(), String> {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        self.file.set_modified(when).map_err(|err| err.to_string())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.file.flush().map_err(|err| err.to_string())?;
        self.file.sync_all().map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Keep the filename the camera sent. Directories in the upload path are not stored.
pub fn stored_name(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("missing filename".to_string());
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err("refusing a path that climbs out of the library".to_string());
    }
    let name = path
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty() && *part != ".")
        .ok_or_else(|| "missing filename".to_string())?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return Err(format!("refusing filename {name}"));
    }
    Ok(name.to_string())
}
