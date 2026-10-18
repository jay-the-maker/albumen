// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::IngestError;

/// Why a file belongs to an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The file the caller pointed at.
    Primary,
    /// XMP or Apple AAE sidecar.
    Sidecar,
    /// RAW+JPEG or Live Photo companion with the same stem.
    Pair,
}

/// One file in an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFile {
    pub path: PathBuf,
    pub role: Role,
}

/// An original plus the companions that must travel with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    files: Vec<AssetFile>,
}

impl Asset {
    /// Build an asset from an explicit file list.
    ///
    /// The first file must be the primary. Filenames must be safe single
    /// components and unique inside the asset.
    pub fn from_files(files: Vec<AssetFile>) -> Result<Self, IngestError> {
        if files.is_empty() {
            return Err(IngestError::EmptyAsset);
        }
        if files[0].role != Role::Primary {
            return Err(IngestError::MissingPrimary);
        }
        let mut seen = BTreeSet::new();
        for file in &files {
            let name = file_name_str(&file.path)?;
            check_filename(&name)?;
            if !seen.insert(name.clone()) {
                return Err(IngestError::DuplicateFilename(name));
            }
        }
        Ok(Self { files })
    }

    pub fn files(&self) -> &[AssetFile] {
        &self.files
    }

    pub fn primary(&self) -> &AssetFile {
        &self.files[0]
    }
}

/// Discover sidecars and known pairs that sit beside `primary`.
///
/// Sidecars are `{stem}.xmp`, `{filename}.xmp`, and the same patterns for
/// Apple `.aae` edit sidecars, in either case. Pairs are same-stem files in
/// the same directory: a still with a RAW, or a still with a video (Live
/// Photo). Android motion photos are a single file; copying that file
/// bit-exactly keeps the embedded video.
pub fn discover(primary: &Path) -> Result<Asset, IngestError> {
    if !primary.is_file() {
        return Err(IngestError::NotAFile(primary.to_path_buf()));
    }
    let file_name = file_name_str(primary)?;
    let stem = primary
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| IngestError::NonUtf8Filename {
            path: primary.to_path_buf(),
        })?;
    let parent = match primary.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let primary_ext = extension_lower(primary);

    let mut files = vec![AssetFile {
        path: primary.to_path_buf(),
        role: Role::Primary,
    }];
    let mut seen_names = BTreeSet::from([file_name.clone()]);
    let mut seen_ids = BTreeSet::new();
    if let Some(id) = file_identity(primary) {
        seen_ids.insert(id);
    }

    for name in sidecar_candidates(&file_name, stem) {
        consider_companion(
            parent,
            &name,
            Role::Sidecar,
            &mut files,
            &mut seen_names,
            &mut seen_ids,
        )?;
    }

    let mut pairs = Vec::new();
    for entry in fs::read_dir(parent).map_err(|err| IngestError::io(parent, err))? {
        let entry = entry.map_err(|err| IngestError::io(parent, err))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let raw_name = entry.file_name();
        let Some(name) = raw_name.to_str() else {
            if stem_matches_os(&raw_name, stem) {
                return Err(IngestError::NonUtf8Filename { path });
            }
            continue;
        };
        if seen_names.contains(name) {
            continue;
        }
        if !stem_matches_os(&raw_name, stem) {
            continue;
        }
        if is_pair_extension(&primary_ext, &extension_lower(&path)) {
            pairs.push((name.to_string(), path));
        }
    }
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, _) in pairs {
        consider_companion(
            parent,
            &name,
            Role::Pair,
            &mut files,
            &mut seen_names,
            &mut seen_ids,
        )?;
    }

    Asset::from_files(files)
}

fn consider_companion(
    parent: &Path,
    name: &str,
    role: Role,
    files: &mut Vec<AssetFile>,
    seen_names: &mut BTreeSet<String>,
    seen_ids: &mut BTreeSet<(u64, u64)>,
) -> Result<(), IngestError> {
    check_filename(name)?;
    let path = parent.join(name);
    if !path.is_file() {
        return Ok(());
    }
    if let Some(id) = file_identity(&path) {
        if !seen_ids.insert(id) {
            return Ok(());
        }
    }
    if !seen_names.insert(name.to_string()) {
        return Ok(());
    }
    files.push(AssetFile { path, role });
    Ok(())
}

pub(crate) fn file_name_str(path: &Path) -> Result<String, IngestError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| IngestError::NonUtf8Filename {
            path: path.to_path_buf(),
        })
}

pub(crate) fn check_filename(name: &str) -> Result<(), IngestError> {
    let unsafe_name = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control);
    if unsafe_name {
        return Err(IngestError::UnsafeFilename {
            filename: name.to_string(),
        });
    }
    Ok(())
}

fn sidecar_candidates(file_name: &str, stem: &str) -> Vec<String> {
    let mut names = vec![
        format!("{stem}.xmp"),
        format!("{stem}.XMP"),
        format!("{file_name}.xmp"),
        format!("{file_name}.XMP"),
        format!("{stem}.aae"),
        format!("{stem}.AAE"),
        format!("{file_name}.aae"),
        format!("{file_name}.AAE"),
    ];
    names.sort();
    names.dedup();
    names
}

fn extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn stem_matches_os(file_name: &std::ffi::OsStr, stem: &str) -> bool {
    Path::new(file_name)
        .file_stem()
        .and_then(|other| other.to_str())
        == Some(stem)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Still,
    Raw,
    Video,
}

fn class_of(ext: &str) -> Option<Class> {
    const STILLS: &[&str] = &[
        "jpg", "jpeg", "heic", "heif", "hif", "png", "tif", "tiff", "avif", "webp",
    ];
    const RAWS: &[&str] = &[
        "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw",
        "raw", "3fr", "fff", "mef", "mos", "erf", "kdc", "dcr", "mrw", "x3f",
    ];
    const VIDEOS: &[&str] = &["mov", "mp4", "m4v"];
    if STILLS.contains(&ext) {
        Some(Class::Still)
    } else if RAWS.contains(&ext) {
        Some(Class::Raw)
    } else if VIDEOS.contains(&ext) {
        Some(Class::Video)
    } else {
        None
    }
}

fn is_pair_extension(primary: &str, other: &str) -> bool {
    matches!(
        (class_of(primary), class_of(other)),
        (Some(Class::Still), Some(Class::Raw | Class::Video))
            | (Some(Class::Raw), Some(Class::Still))
            | (Some(Class::Video), Some(Class::Still))
    )
}

fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_file, Tmp};
    use std::time::{Duration, UNIX_EPOCH};

    fn touch(dir: &Path, name: &str) {
        let mtime = UNIX_EPOCH + Duration::from_secs(1_400_000_000);
        write_file(dir, name, b"bytes", mtime);
    }

    #[test]
    fn discovers_xmp_raw_pair_and_live_photo() {
        let tmp = Tmp::new();
        let dir = tmp.path();
        touch(dir, "IMG_0003.dng");
        touch(dir, "IMG_0003.jpg");
        touch(dir, "IMG_0003.xmp");
        touch(dir, "IMG_0003.dng.xmp");
        touch(dir, "other.jpg");

        let asset = discover(&dir.join("IMG_0003.dng")).unwrap();
        let names: Vec<_> = asset
            .files()
            .iter()
            .map(|file| {
                (
                    file.path.file_name().unwrap().to_str().unwrap().to_string(),
                    file.role,
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("IMG_0003.dng".to_string(), Role::Primary),
                ("IMG_0003.dng.xmp".to_string(), Role::Sidecar),
                ("IMG_0003.xmp".to_string(), Role::Sidecar),
                ("IMG_0003.jpg".to_string(), Role::Pair),
            ]
        );
    }

    #[test]
    fn discovers_live_photo_from_the_still() {
        let tmp = Tmp::new();
        let dir = tmp.path();
        touch(dir, "IMG_0005.HEIC");
        touch(dir, "IMG_0005.MOV");
        let asset = discover(&dir.join("IMG_0005.HEIC")).unwrap();
        assert_eq!(asset.files().len(), 2);
        assert_eq!(asset.files()[1].role, Role::Pair);
        assert_eq!(asset.files()[1].path.file_name().unwrap(), "IMG_0005.MOV");
    }

    #[test]
    fn discovers_apple_edit_sidecar() {
        let tmp = Tmp::new();
        let dir = tmp.path();
        touch(dir, "IMG_0008.jpg");
        touch(dir, "IMG_0008.aae");
        let asset = discover(&dir.join("IMG_0008.jpg")).unwrap();
        assert_eq!(asset.files().len(), 2);
        assert_eq!(asset.files()[1].role, Role::Sidecar);
        assert_eq!(asset.files()[1].path.file_name().unwrap(), "IMG_0008.aae");
    }

    #[test]
    fn rejects_path_components_in_names() {
        let err = check_filename("../secret.jpg").unwrap_err();
        assert!(matches!(err, IngestError::UnsafeFilename { .. }));
    }
}
