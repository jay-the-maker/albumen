// SPDX-License-Identifier: Apache-2.0

//! A user's archive opened so this process cannot change it.
//!
//! `SourceRoot` canonicalizes the directory the caller named. Every file it
//! opens uses `O_RDONLY` and, on Linux, `O_NOATIME` when the kernel allows
//! that flag. The type has no create, write, rename, unlink, utimens, chmod,
//! or xattr-set operation. A destination that resolves inside the root, or
//! that is the same inode as something in the root, is refused. There is no
//! flag that turns the refusal off.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;

use crate::error::IngestError;

/// The directory a command is reading. Writes do not exist on this type.
#[derive(Debug, Clone)]
pub struct SourceRoot {
    canonical: PathBuf,
}

impl SourceRoot {
    /// Open `path` as a read-only root. `path` must already exist.
    pub fn open(path: &Path) -> Result<Self, IngestError> {
        if !path.exists() {
            return Err(IngestError::io(
                path,
                std::io::Error::new(ErrorKind::NotFound, "source does not exist"),
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|err| IngestError::io(path, err))?;
        Ok(Self { canonical })
    }

    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// Open one regular file inside this root. `O_RDONLY` only.
    pub fn open_read(&self, path: &Path) -> Result<File, IngestError> {
        let meta = self.metadata_nofollow(path)?;
        if !meta.is_file() {
            return Err(IngestError::NotAFile(path.to_path_buf()));
        }
        open_rdonly(path)
    }

    /// List a directory inside this root. Symlinks are not followed.
    pub fn read_dir(&self, path: &Path) -> Result<fs::ReadDir, IngestError> {
        self.ensure_inside(path)?;
        fs::read_dir(crate::platform::io_path(path)).map_err(|err| IngestError::io(path, err))
    }

    /// Stat `path` without following a symlink. A symlink is an error.
    pub fn metadata_nofollow(&self, path: &Path) -> Result<fs::Metadata, IngestError> {
        let meta = fs::symlink_metadata(crate::platform::io_path(path))
            .map_err(|err| IngestError::io(path, err))?;
        if meta.file_type().is_symlink() {
            return Err(IngestError::Symlink(path.to_path_buf()));
        }
        self.ensure_inside(path)?;
        Ok(meta)
    }

    /// Stat a probed sibling. Missing paths and symlinks are `None`.
    pub fn probe(&self, path: &Path) -> Result<Option<fs::Metadata>, IngestError> {
        match fs::symlink_metadata(crate::platform::io_path(path)) {
            Ok(meta) if meta.file_type().is_symlink() => Ok(None),
            Ok(meta) => {
                self.ensure_inside(path)?;
                Ok(Some(meta))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(IngestError::io(path, err)),
        }
    }

    /// Refuse `dest` when it is this root, is inside this root, or is a
    /// hard link to a file or directory in this root.
    pub fn refuse_destination(&self, dest: &Path) -> Result<(), IngestError> {
        if destination_aliases(self, dest)? {
            return Err(IngestError::DestinationInsideSource {
                destination: dest.display().to_string(),
                source_tree: self.canonical.display().to_string(),
            });
        }
        Ok(())
    }

    fn ensure_inside(&self, path: &Path) -> Result<(), IngestError> {
        let resolved = path
            .canonicalize()
            .map_err(|err| IngestError::io(path, err))?;
        if resolved == self.canonical || resolved.starts_with(&self.canonical) {
            return Ok(());
        }
        Err(IngestError::store(
            "source",
            path.display().to_string(),
            format!(
                "path resolves to {} which is outside {}",
                resolved.display(),
                self.canonical.display()
            ),
        ))
    }
}

/// Parent directory of a file, or `.` when the path has none.
pub fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn open_rdonly(path: &Path) -> Result<File, IngestError> {
    let opened = crate::platform::io_path(path);
    #[cfg(target_os = "linux")]
    {
        const O_NOATIME: i32 = 0o1000000;
        match OpenOptions::new()
            .read(true)
            .custom_flags(O_NOATIME)
            .open(&opened)
        {
            Ok(file) => return Ok(file),
            Err(err) if err.raw_os_error() == Some(1) => {}
            Err(err) => return Err(IngestError::io(path, err)),
        }
    }
    OpenOptions::new()
        .read(true)
        .open(&opened)
        .map_err(|err| IngestError::io(path, err))
}

fn destination_aliases(root: &SourceRoot, dest: &Path) -> Result<bool, IngestError> {
    let resolved = resolve_dest(dest)?;
    if resolved == root.canonical || resolved.starts_with(&root.canonical) {
        return Ok(true);
    }
    hardlink_aliases(root, dest)
}

fn resolve_dest(dest: &Path) -> Result<PathBuf, IngestError> {
    let dest = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| IngestError::io(dest, err))?
            .join(dest)
    };
    let mut missing = Vec::new();
    let mut cursor = dest;
    loop {
        if cursor.exists() {
            let mut resolved = cursor
                .canonicalize()
                .map_err(|err| IngestError::io(&cursor, err))?;
            for part in missing.iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
        let Some(name) = cursor.file_name() else {
            return Err(IngestError::store(
                "destination",
                cursor.display().to_string(),
                "could not resolve the destination path",
            ));
        };
        missing.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return Err(IngestError::store(
                "destination",
                cursor.display().to_string(),
                "could not resolve the destination path",
            ));
        };
        if parent.as_os_str().is_empty() {
            return Err(IngestError::store(
                "destination",
                cursor.display().to_string(),
                "could not resolve the destination path",
            ));
        }
        cursor = parent.to_path_buf();
    }
}

fn hardlink_aliases(root: &SourceRoot, dest: &Path) -> Result<bool, IngestError> {
    let interesting = dest_ids(root, dest)?;
    if interesting.is_empty() {
        return Ok(false);
    }
    let mut stack = vec![root.canonical.clone()];
    while let Some(dir) = stack.pop() {
        let meta = fs::symlink_metadata(&dir).map_err(|err| IngestError::io(&dir, err))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if id_of(&meta).is_some_and(|id| interesting.contains(&id)) {
            return Ok(true);
        }
        if !meta.is_dir() {
            continue;
        }
        let read = fs::read_dir(&dir).map_err(|err| IngestError::io(&dir, err))?;
        for entry in read {
            let entry = entry.map_err(|err| IngestError::io(&dir, err))?;
            let path = entry.path();
            let child = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
            if child.file_type().is_symlink() {
                continue;
            }
            if id_of(&child).is_some_and(|id| interesting.contains(&id)) {
                return Ok(true);
            }
            if child.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(false)
}

fn dest_ids(root: &SourceRoot, dest: &Path) -> Result<Vec<(u64, u64)>, IngestError> {
    let mut ids = Vec::new();
    let mut cursor = dest.to_path_buf();
    if !cursor.exists() {
        while !cursor.exists() {
            if !cursor.pop() {
                return Ok(ids);
            }
        }
    }
    loop {
        if !cursor.exists() {
            break;
        }
        let canon = cursor
            .canonicalize()
            .map_err(|err| IngestError::io(&cursor, err))?;
        if root.canonical.starts_with(&canon) {
            break;
        }
        let meta = fs::symlink_metadata(&cursor).map_err(|err| IngestError::io(&cursor, err))?;
        if !meta.file_type().is_symlink() {
            if let Some(id) = id_of(&meta) {
                ids.push(id);
            }
        }
        if !cursor.pop() {
            break;
        }
    }
    Ok(ids)
}

#[cfg(unix)]
fn id_of(meta: &fs::Metadata) -> Option<(u64, u64)> {
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn id_of(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}
