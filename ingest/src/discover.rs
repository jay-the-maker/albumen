// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::IngestError;
use crate::source::SourceRoot;

/// Why a file belongs to an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The file the caller pointed at.
    Primary,
    /// XMP, Apple AAE, or Takeout JSON sidecar.
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

/// Names of sidecars and pairs that sit beside `primary`.
///
/// This stats the known sibling names. It does not read the rest of the
/// directory. The returned names are file names, not relative paths.
pub(crate) fn companion_names(primary: &Path) -> Result<(Vec<String>, Vec<String>), IngestError> {
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
    let root = SourceRoot::open(parent)?;
    let mut sidecars = Vec::new();
    for name in sidecar_candidates(&file_name, stem) {
        if let Some(actual) = on_disk_name(&root, parent, &name) {
            if actual != file_name {
                sidecars.push(actual);
            }
        }
    }
    sidecars.sort();
    sidecars.dedup();
    let mut pairs = Vec::new();
    if let Some(class) = class_of(&extension_lower(primary)) {
        for ext in pair_extensions(class) {
            for name in [
                format!("{stem}.{ext}"),
                format!("{stem}.{}", ext.to_ascii_uppercase()),
            ] {
                let Some(actual) = on_disk_name(&root, parent, &name) else {
                    continue;
                };
                if actual != file_name && !sidecars.iter().any(|existing| existing == &actual) {
                    pairs.push(actual);
                }
            }
        }
    }
    pairs.sort();
    pairs.dedup();
    Ok((sidecars, pairs))
}

/// The directory entry's own name when `parent/name` is a regular file.
///
/// On a case-insensitive volume, probing `Photo.XMP` finds `Photo.xmp`.
/// The returned name is the one stored on disk, so the two probes collapse.
fn on_disk_name(root: &SourceRoot, parent: &Path, name: &str) -> Option<String> {
    let path = parent.join(name);
    let meta = root.probe(&path).ok().flatten()?;
    if !meta.is_file() {
        return None;
    }
    fs::canonicalize(&path)
        .ok()
        .and_then(|canon| canon.file_name()?.to_str().map(str::to_string))
        .or_else(|| Some(name.to_string()))
}

fn pair_extensions(class: Class) -> Vec<&'static str> {
    const STILLS: &[&str] = &[
        "jpg", "jpeg", "heic", "heif", "hif", "png", "tif", "tiff", "avif", "webp",
    ];
    const RAWS: &[&str] = &[
        "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw",
        "raw", "3fr", "fff", "mef", "mos", "erf", "kdc", "dcr", "mrw", "x3f",
    ];
    const VIDEOS: &[&str] = &["mov", "mp4", "m4v"];
    match class {
        Class::Still => {
            let mut out = Vec::with_capacity(RAWS.len() + VIDEOS.len());
            out.extend_from_slice(RAWS);
            out.extend_from_slice(VIDEOS);
            out
        }
        Class::Raw | Class::Video => STILLS.to_vec(),
    }
}

/// Discover sidecars and known pairs that sit beside `primary`.
///
/// Sidecars are `{stem}.xmp`, `{filename}.xmp`, the same patterns for Apple
/// `.aae` edit sidecars, and the Takeout JSON names for that file, in either
/// case. A Takeout JSON is `{filename}.json` or
/// `{filename}.supplemental-metadata.json`, shortened to Takeout's 51-unit
/// name when the full name would be longer. Pairs are same-stem files in the
/// same directory: a still with a RAW, or a still with a video (Live Photo).
/// Android motion photos are a single file; copying that file bit-exactly
/// keeps the embedded video.
pub fn discover(primary: &Path) -> Result<Asset, IngestError> {
    let parent = match primary.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let root = SourceRoot::open(parent)?;
    let primary_meta = root.metadata_nofollow(primary)?;
    if !primary_meta.is_file() {
        return Err(IngestError::NotAFile(primary.to_path_buf()));
    }
    let file_name = file_name_str(primary)?;
    let stem = primary
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| IngestError::NonUtf8Filename {
            path: primary.to_path_buf(),
        })?;
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
            &root,
            parent,
            &name,
            Role::Sidecar,
            &mut files,
            &mut seen_names,
            &mut seen_ids,
        )?;
    }
    files[1..].sort_by(|left, right| left.path.file_name().cmp(&right.path.file_name()));

    let mut pairs = Vec::new();
    for entry in root.read_dir(parent)? {
        let entry = entry.map_err(|err| IngestError::io(parent, err))?;
        let path = entry.path();
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
        let meta = match root.metadata_nofollow(&path) {
            Ok(meta) => meta,
            Err(IngestError::Symlink(_)) => continue,
            Err(err) => return Err(err),
        };
        if !meta.is_file() {
            continue;
        }
        if is_pair_extension(&primary_ext, &extension_lower(&path)) {
            pairs.push((name.to_string(), path));
        }
    }
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, _) in pairs {
        consider_companion(
            &root,
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
    root: &SourceRoot,
    parent: &Path,
    name: &str,
    role: Role,
    files: &mut Vec<AssetFile>,
    seen_names: &mut BTreeSet<String>,
    seen_ids: &mut BTreeSet<(u64, u64)>,
) -> Result<(), IngestError> {
    let Some(actual) = on_disk_name(root, parent, name) else {
        return Ok(());
    };
    check_filename(&actual)?;
    let path = parent.join(&actual);
    let meta = root.metadata_nofollow(&path)?;
    if !meta.is_file() {
        return Ok(());
    }
    if let Some(id) = file_identity(&path) {
        if !seen_ids.insert(id) {
            return Ok(());
        }
    }
    if !seen_names.insert(actual) {
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
    names.extend(takeout_json_sidecar_names(file_name));
    names.sort();
    names.dedup();
    names
}

/// UTF-16 code units kept before `.json`. Takeout's longest JSON name is 51
/// units, and the five units of `.json` are never cut. A `(N)` duplicate
/// marker is appended after this cut, so a numbered name can pass 51.
const TAKEOUT_JSON_STEM_UNITS: usize = 46;
const TAKEOUT_SUPPLEMENTAL_SUFFIX: &str = ".supplemental-metadata";

/// Supplemental and legacy JSON names Takeout would write beside `media`.
///
/// The supplemental form is `{name}.supplemental-metadata`, cut to 46 UTF-16
/// code units, then `.json`. A `(N)` before the media extension is removed
/// from that stem and written after the cut. The legacy form is the same cut
/// of `{name}` alone. `-edited` stays in the media name, so an edited file
/// pairs with its own JSON.
pub(crate) fn takeout_json_sidecar_names(media: &str) -> [String; 2] {
    let (core, duplicate) = split_takeout_duplicate_marker(media);
    [
        takeout_json_name(&core, TAKEOUT_SUPPLEMENTAL_SUFFIX, &duplicate),
        takeout_json_name(&core, "", &duplicate),
    ]
}

fn takeout_json_name(core: &str, supplemental: &str, duplicate: &str) -> String {
    let mut middle = String::with_capacity(core.len() + supplemental.len());
    middle.push_str(core);
    middle.push_str(supplemental);
    let clipped = clip_utf16(&middle, TAKEOUT_JSON_STEM_UNITS);
    let mut name = String::with_capacity(clipped.len() + duplicate.len() + 5);
    name.push_str(&clipped);
    name.push_str(duplicate);
    name.push_str(".json");
    name
}

fn clip_utf16(value: &str, units: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let width = ch.len_utf16();
        if used + width > units {
            break;
        }
        out.push(ch);
        used += width;
    }
    out
}

/// `(N)` immediately before the last extension, as in `IMG_1234(1).jpg`.
/// Returns the name with that marker moved out, and the marker `(N)`.
fn split_takeout_duplicate_marker(media: &str) -> (String, String) {
    let (stem, extension) = match media.rfind('.') {
        Some(dot) if dot > 0 && !media[dot + 1..].is_empty() => (&media[..dot], &media[dot..]),
        _ => (media, ""),
    };
    if let Some(open) = trailing_paren_number(stem) {
        let duplicate = &stem[open..];
        let core_stem = &stem[..open];
        if !core_stem.is_empty() {
            return (format!("{core_stem}{extension}"), duplicate.to_string());
        }
    }
    (media.to_string(), String::new())
}

fn trailing_paren_number(stem: &str) -> Option<usize> {
    if !stem.ends_with(')') {
        return None;
    }
    let open = stem.rfind('(')?;
    let inner = &stem[open + 1..stem.len() - 1];
    if inner.is_empty() || !inner.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(open)
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
        let meta = fs::symlink_metadata(path).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
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
    fn companion_names_stat_known_siblings_only() {
        let tmp = Tmp::new();
        let dir = tmp.path();
        touch(dir, "IMG_0005.HEIC");
        touch(dir, "IMG_0005.MOV");
        touch(dir, "IMG_0005.xmp");
        touch(dir, "unrelated.jpg");
        let (sidecars, pairs) = companion_names(&dir.join("IMG_0005.HEIC")).unwrap();
        assert_eq!(sidecars, vec!["IMG_0005.xmp".to_string()]);
        assert_eq!(pairs, vec!["IMG_0005.MOV".to_string()]);
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

    #[test]
    fn discovers_takeout_json_beside_the_original() {
        let tmp = Tmp::new();
        let dir = tmp.path();
        touch(dir, "IMG_0001.jpg");
        touch(dir, "IMG_0001.jpg.json");
        touch(dir, "IMG_0001.jpg.supplemental-metadata.json");
        touch(dir, "IMG_0001-edited.jpg");
        touch(dir, "IMG_0001-edited.jpg.supplemental-metadata.json");
        let long = format!("{}.jpg", "n".repeat(43));
        assert_eq!(long.chars().count(), 47);
        let truncated = takeout_json_sidecar_names(&long)[0].clone();
        assert_ne!(truncated, format!("{long}.json"));
        assert!(truncated.ends_with(".json"));
        touch(dir, &long);
        touch(dir, &truncated);

        let plain = discover(&dir.join("IMG_0001.jpg")).unwrap();
        let plain_sidecars: Vec<_> = plain
            .files()
            .iter()
            .filter(|file| file.role == Role::Sidecar)
            .map(|file| file.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            plain_sidecars,
            vec![
                "IMG_0001.jpg.json".to_string(),
                "IMG_0001.jpg.supplemental-metadata.json".to_string(),
            ]
        );

        let edited = discover(&dir.join("IMG_0001-edited.jpg")).unwrap();
        let edited_names: Vec<_> = edited
            .files()
            .iter()
            .skip(1)
            .map(|file| file.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            edited_names,
            vec!["IMG_0001-edited.jpg.supplemental-metadata.json".to_string()]
        );

        let clipped = discover(&dir.join(&long)).unwrap();
        assert_eq!(clipped.files().len(), 2);
        assert_eq!(clipped.files()[1].role, Role::Sidecar);
        assert_eq!(
            clipped.files()[1]
                .path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            truncated
        );
    }
}
