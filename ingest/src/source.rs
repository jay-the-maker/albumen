// SPDX-License-Identifier: Apache-2.0

//! A user's archive opened so this process cannot change it.
//!
//! `SourceRoot` canonicalizes the directory the caller named. Every file it
//! opens uses `O_RDONLY` and, on Linux, `O_NOATIME` when the kernel allows
//! that flag. The type has no create, write, rename, unlink, utimens, chmod,
//! or xattr-set operation. A destination that resolves inside the root, or
//! that is the same inode as something in the root, is refused. There is no
//! flag that turns the refusal off.
//!
//! The contract is [DESIGN_PHILOSOPHY.md](../../DESIGN_PHILOSOPHY.md):
//! originals stay bit-exact, and the default path never writes a source.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;

use crate::error::IngestError;

/// The directory a command is reading.
///
/// Read operations are [`Self::open_read`], [`Self::metadata_nofollow`],
/// [`Self::read_dir`], and [`Self::walk`]. This type has no method that
/// creates, writes, renames, or removes. The contract is
/// [DESIGN_PHILOSOPHY.md](../../DESIGN_PHILOSOPHY.md).
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

    /// Visit each regular file under this root in relative-path order.
    ///
    /// `after` skips a path that sorts at or before that relative path, so a
    /// resumed page does not read files it already returned. One directory is
    /// listed at a time. Return `false` from `visit` to stop before unread
    /// branches. The walk calls [`Self::read_dir`] and
    /// [`Self::metadata_nofollow`] only.
    pub fn walk(
        &self,
        after: Option<&str>,
        visit: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), IngestError> {
        walk_files(self, &self.canonical, "", after, visit)?;
        Ok(())
    }

    /// Refuse `dest` when it is this root, is inside this root, or is a
    /// hard link to a file or directory in this root.
    pub fn refuse_destination(&self, dest: &Path) -> Result<(), IngestError> {
        self.refuse_destinations(&[dest])
    }

    /// [`Self::refuse_destination`] for each path, in order, with one
    /// lookup cache for the whole call. A path that two checks both resolve
    /// (the root, the destination's parents) is canonicalized and stat-ed
    /// once instead of once per comparison. Each check gives the same
    /// answer it gives alone.
    pub fn refuse_destinations(&self, dests: &[&Path]) -> Result<(), IngestError> {
        let lookups = Lookups::default();
        for dest in dests {
            if destination_aliases(self, dest, &lookups)? {
                return Err(IngestError::DestinationInsideSource {
                    destination: dest.display().to_string(),
                    source_tree: self.canonical.display().to_string(),
                });
            }
        }
        Ok(())
    }

    /// Refuse `dest` when it resolves to this root or inside it. Unlike
    /// [`Self::refuse_destination`] this does not look for hard links, so it
    /// fits a directory this process creates for its own files.
    pub fn refuse_inside(&self, dest: &Path) -> Result<(), IngestError> {
        let resolved = resolve_dest(dest)?;
        if path_is_inside(&self.canonical, &resolved, &Lookups::default()) {
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

fn walk_files(
    root: &SourceRoot,
    dir: &Path,
    prefix: &str,
    after: Option<&str>,
    visit: &mut dyn FnMut(&str) -> bool,
) -> Result<bool, IngestError> {
    let mut entries = Vec::new();
    let read = root.read_dir(dir)?;
    for entry in read {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        let path = entry.path();
        let meta = root.metadata_nofollow(&path)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(IngestError::NonUtf8Filename { path });
        };
        crate::discover::check_filename(name)?;
        if meta.is_dir() {
            entries.push((name.to_string(), true));
        } else if meta.is_file() {
            entries.push((name.to_string(), false));
        }
    }
    entries.sort_by(|left, right| cmp_walk(&left.0, left.1, &right.0, right.1));
    for (name, is_dir) in entries {
        let child = dir.join(&name);
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if is_dir {
            if let Some(after) = after {
                if directory_is_at_or_before(&relative, after) {
                    continue;
                }
            }
            if !walk_files(root, &child, &relative, after, visit)? {
                return Ok(false);
            }
        } else {
            if let Some(after) = after {
                if relative.as_str() <= after {
                    continue;
                }
            }
            if !visit(&relative) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn directory_is_at_or_before(relative: &str, after: &str) -> bool {
    let mut prefix = String::with_capacity(relative.len() + 1);
    prefix.push_str(relative);
    prefix.push('/');
    !after.starts_with(&prefix) && prefix.as_str() < after
}

pub(crate) fn cmp_walk(
    left_name: &str,
    left_dir: bool,
    right_name: &str,
    right_dir: bool,
) -> std::cmp::Ordering {
    let mut left = left_name.bytes().chain(left_dir.then_some(b'/'));
    let mut right = right_name.bytes().chain(right_dir.then_some(b'/'));
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(left_byte), Some(right_byte)) => match left_byte.cmp(&right_byte) {
                std::cmp::Ordering::Equal => {}
                order => return order,
            },
        }
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

/// Canonical paths and metadata looked up during one destination check.
///
/// The Windows comparisons below resolve the root and every parent of the
/// destination against each other, which asked the file system for the same
/// path dozens of times per imported file. The cache lives for one
/// [`SourceRoot::refuse_destinations`] call only, so nothing carries over
/// between files.
#[derive(Default)]
struct Lookups {
    canonical: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<PathBuf>>>,
    #[cfg(windows)]
    metadata: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<fs::Metadata>>>,
}

impl Lookups {
    /// `path.canonicalize()`, remembered. An error is remembered as `None`;
    /// callers that need the error call `canonicalize` themselves.
    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        if let Some(found) = self.canonical.borrow().get(path) {
            return found.clone();
        }
        let found = path.canonicalize().ok();
        self.canonical
            .borrow_mut()
            .insert(path.to_path_buf(), found.clone());
        found
    }

    #[cfg(windows)]
    fn metadata(&self, path: &Path) -> Option<fs::Metadata> {
        if let Some(found) = self.metadata.borrow().get(path) {
            return found.clone();
        }
        let found = fs::metadata(path).ok();
        self.metadata
            .borrow_mut()
            .insert(path.to_path_buf(), found.clone());
        found
    }
}

fn destination_aliases(
    root: &SourceRoot,
    dest: &Path,
    lookups: &Lookups,
) -> Result<bool, IngestError> {
    let resolved = resolve_dest(dest)?;
    if path_is_inside(&root.canonical, &resolved, lookups) {
        return Ok(true);
    }
    hardlink_aliases(root, dest, lookups)
}

/// `dest` is `root` or a path under it.
///
/// Windows canonical paths disagree about the `\\?\` prefix, letter case,
/// and 8.3 short names (`RUNNER~1` and `runneradmin` are one directory).
/// A string prefix is not enough. The same directory also matches on
/// creation time, size, and attributes.
fn path_is_inside(root: &Path, dest: &Path, lookups: &Lookups) -> bool {
    if normalized_starts_with(dest, root) {
        return true;
    }
    #[cfg(windows)]
    {
        dest.ancestors()
            .any(|ancestor| same_directory(ancestor, root, lookups))
    }
    #[cfg(not(windows))]
    {
        let _ = lookups;
        false
    }
}

fn root_is_under(root: &Path, dir: &Path, lookups: &Lookups) -> bool {
    if normalized_starts_with(root, dir) {
        return true;
    }
    #[cfg(windows)]
    {
        root.ancestors()
            .any(|ancestor| same_directory(ancestor, dir, lookups))
    }
    #[cfg(not(windows))]
    {
        let _ = lookups;
        false
    }
}

fn normalized_starts_with(path: &Path, prefix: &Path) -> bool {
    let path = normalize_path(path);
    let prefix = normalize_path(prefix);
    if path == prefix {
        return true;
    }
    let separator = std::path::MAIN_SEPARATOR;
    let mut prefix = prefix;
    if !prefix.ends_with(separator) {
        prefix.push(separator);
    }
    path.starts_with(&prefix)
}

fn normalize_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let stripped = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    };
    #[cfg(windows)]
    {
        stripped.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        stripped
    }
}

#[cfg(windows)]
fn same_directory(left: &Path, right: &Path, lookups: &Lookups) -> bool {
    if let (Some(left_canon), Some(right_canon)) =
        (lookups.canonical(left), lookups.canonical(right))
    {
        if normalize_path(&left_canon) == normalize_path(&right_canon) {
            return true;
        }
    }
    // 8.3 and long names can canonicalize to different strings. The leaf
    // name of one directory does not change between those forms, and two
    // different directories created in one tick must not compare equal.
    if left.file_name() != right.file_name() {
        return false;
    }
    let (Some(left_meta), Some(right_meta)) = (lookups.metadata(left), lookups.metadata(right))
    else {
        return false;
    };
    if !left_meta.is_dir() || !right_meta.is_dir() {
        return false;
    }
    use std::os::windows::fs::MetadataExt;
    left_meta.creation_time() == right_meta.creation_time()
        && left_meta.last_write_time() == right_meta.last_write_time()
        && left_meta.file_size() == right_meta.file_size()
        && left_meta.file_attributes() == right_meta.file_attributes()
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

fn hardlink_aliases(
    root: &SourceRoot,
    dest: &Path,
    lookups: &Lookups,
) -> Result<bool, IngestError> {
    let (interesting, only_dirs) = dest_ids(root, dest, lookups)?;
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
            // When every id is a directory's, only a directory entry can
            // match: an inode has one type. The entry type comes from the
            // directory listing, so files are skipped without a stat.
            if only_dirs {
                let kind = entry
                    .file_type()
                    .map_err(|err| IngestError::io(entry.path(), err))?;
                if !kind.is_dir() {
                    continue;
                }
            }
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

/// Identities of `dest` and its existing ancestors, and whether all of them
/// are directories.
fn dest_ids(
    root: &SourceRoot,
    dest: &Path,
    lookups: &Lookups,
) -> Result<(Vec<(u64, u64)>, bool), IngestError> {
    let mut ids = Vec::new();
    let mut only_dirs = true;
    let mut cursor = dest.to_path_buf();
    if !cursor.exists() {
        while !cursor.exists() {
            if !cursor.pop() {
                return Ok((ids, only_dirs));
            }
        }
    }
    loop {
        if !cursor.exists() {
            break;
        }
        let canon = match lookups.canonical(&cursor) {
            Some(canon) => canon,
            None => cursor
                .canonicalize()
                .map_err(|err| IngestError::io(&cursor, err))?,
        };
        if root_is_under(&root.canonical, &canon, lookups) {
            break;
        }
        let meta = fs::symlink_metadata(&cursor).map_err(|err| IngestError::io(&cursor, err))?;
        if !meta.file_type().is_symlink() {
            if let Some(id) = id_of(&meta) {
                ids.push(id);
                only_dirs &= meta.is_dir();
            }
        }
        if !cursor.pop() {
            break;
        }
    }
    Ok((ids, only_dirs))
}

#[cfg(unix)]
fn id_of(meta: &fs::Metadata) -> Option<(u64, u64)> {
    Some((meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn id_of(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    // `file_index` is still unstable. A hard link of a file shares that
    // file's creation FILETIME and size. A directory's creation time is not
    // an identity: NTFS stamps a burst of creates with one tick, so a sibling
    // library would otherwise match the source directory.
    if !meta.is_file() {
        return None;
    }
    let created = meta.creation_time();
    if created == 0 {
        return None;
    }
    Some((meta.file_size(), created))
}

#[cfg(not(any(unix, windows)))]
fn id_of(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum::{sha256_file, sha256_reader};
    use crate::mtime::mtime_of;

    #[test]
    fn reading_fixtures_through_source_root_leaves_sha256_and_mtime() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/jpeg-gps");
        let source = SourceRoot::open(&root).unwrap();
        let mut relatives = Vec::new();
        source
            .walk(None, &mut |relative| {
                relatives.push(relative.to_string());
                true
            })
            .unwrap();
        assert!(!relatives.is_empty(), "fixture directory had no files");
        for relative in relatives {
            let path = root.join(&relative);
            let before_hash = sha256_file(&path).unwrap();
            let before_mtime = mtime_of(&path).unwrap();
            let file = source.open_read(&path).unwrap();
            let read_hash = sha256_reader(file, &path).unwrap();
            assert_eq!(read_hash, before_hash, "{relative}");
            assert_eq!(sha256_file(&path).unwrap(), before_hash, "{relative}");
            assert_eq!(mtime_of(&path).unwrap(), before_mtime, "{relative}");
        }
    }
}
