// SPDX-License-Identifier: Apache-2.0

//! A writable destination must not be the library mount.
//!
//! `--enable-export` and `--enable-upload` require `--destination`. The
//! destination may be another directory on the same disk. It may not be the
//! library, a path inside the library, or a second mount of that same
//! directory. When the library path is itself a mount, a destination on that
//! mount is refused too.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Mount {
    pub point: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

pub fn require_separate_destination(library: &Path, destination: &Path) -> Result<(), String> {
    let library = canonical_dir(library)?;
    let destination = canonical_dir(destination)?;
    if paths_overlap(&library, &destination) {
        return Err(format!(
            "refusing to start: destination {} is the library {}",
            destination.display(),
            library.display()
        ));
    }
    if same_mount(&library, &destination, &read_mounts()) {
        return Err(format!(
            "refusing to start: destination {} is the same mount as the library {}",
            destination.display(),
            library.display()
        ));
    }
    Ok(())
}

/// `path` is the destination directory, or a file or directory that will be created under it.
pub fn is_within(root: &Path, path: &Path) -> Result<bool, String> {
    let root = canonical_dir(root)?;
    let candidate = canonical_existing(path)?;
    Ok(candidate == root || candidate.starts_with(&root))
}

fn canonical_dir(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|err| format!("{}: {err}", path.display()))
}

fn canonical_existing(path: &Path) -> Result<PathBuf, String> {
    if path
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(format!(
            "refusing a path that contains ..: {}",
            path.display()
        ));
    }
    let mut suffix = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        if cursor.exists() {
            let mut full =
                fs::canonicalize(&cursor).map_err(|err| format!("{}: {err}", cursor.display()))?;
            for part in suffix.iter().rev() {
                full.push(part);
            }
            return Ok(full);
        }
        let Some(name) = cursor.file_name().map(|name| name.to_os_string()) else {
            return Err(format!("cannot place {}", path.display()));
        };
        suffix.push(name);
        let Some(parent) = cursor.parent() else {
            return Err(format!("cannot place {}", path.display()));
        };
        if parent.as_os_str().is_empty() {
            return Err(format!("cannot place {}", path.display()));
        }
        cursor = parent.to_path_buf();
    }
}

fn paths_overlap(library: &Path, destination: &Path) -> bool {
    destination == library || destination.starts_with(library) || library.starts_with(destination)
}

fn same_mount(library: &Path, destination: &Path, mounts: &[Mount]) -> bool {
    let Some(library_mount) = longest_mount(library, mounts) else {
        return false;
    };
    let Some(dest_mount) = longest_mount(destination, mounts) else {
        return false;
    };
    if library_mount.point == dest_mount.point {
        return library == library_mount.point.as_path();
    }
    library == library_mount.point.as_path()
        && destination == dest_mount.point.as_path()
        && library_mount.dev == dest_mount.dev
        && library_mount.ino == dest_mount.ino
        && library_mount.ino != 0
}

fn longest_mount<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.point))
        .max_by_key(|mount| mount.point.as_os_str().len())
}

fn read_mounts() -> Vec<Mount> {
    #[cfg(target_os = "linux")]
    let text = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let text = String::new();
    parse_mountinfo(&text)
}

fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some(point) = mount_point_field(line) else {
            continue;
        };
        let point = PathBuf::from(unescape_mount(&point));
        let (dev, ino) = identity(&point);
        mounts.push(Mount { point, dev, ino });
    }
    mounts
}

fn mount_point_field(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    parts.next()?;
    parts.next()?;
    parts.next()?;
    parts.next().map(str::to_string)
}

fn unescape_mount(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &text[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                out.push(char::from(value));
                index += 4;
                continue;
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

fn identity(path: &Path) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = fs::metadata(path) {
            return (meta.dev(), meta.ino());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(point: &str, dev: u64, ino: u64) -> Mount {
        Mount {
            point: PathBuf::from(point),
            dev,
            ino,
        }
    }

    #[test]
    fn a_library_mount_refuses_a_child_and_allows_another_mount() {
        let mounts = vec![
            mount("/library", 1, 10),
            mount("/destination", 1, 20),
            mount("/", 1, 2),
        ];
        assert!(same_mount(
            Path::new("/library"),
            Path::new("/library/out"),
            &mounts
        ));
        assert!(!same_mount(
            Path::new("/library"),
            Path::new("/destination"),
            &mounts
        ));
    }

    #[test]
    fn the_same_directory_mounted_twice_is_one_mount() {
        let mounts = vec![mount("/library", 7, 10), mount("/destination", 7, 10)];
        assert!(same_mount(
            Path::new("/library"),
            Path::new("/destination"),
            &mounts
        ));
    }

    #[test]
    fn sibling_directories_on_one_disk_are_not_the_library_mount() {
        let mounts = vec![mount("/", 1, 2)];
        assert!(!same_mount(
            Path::new("/home/library"),
            Path::new("/home/destination"),
            &mounts
        ));
    }

    #[test]
    fn mountinfo_unescape_reads_a_space() {
        let text = "22 1 8:1 / /library\\040photos rw - ext4 /dev/sda1 rw\n";
        let mounts = parse_mountinfo(text);
        assert_eq!(mounts[0].point, PathBuf::from("/library photos"));
    }

    #[test]
    fn a_destination_inside_the_library_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let inside = library.join("out");
        fs::create_dir_all(&inside).unwrap();
        let err = require_separate_destination(&library, &inside).unwrap_err();
        assert!(err.contains("refusing to start"), "{err}");
        let dest = root.path().join("destination");
        fs::create_dir(&dest).unwrap();
        require_separate_destination(&library, &dest).unwrap();
    }
}
