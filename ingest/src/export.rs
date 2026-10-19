// SPDX-License-Identifier: Apache-2.0

//! Copy a whole library to another store and check the manifest.
//!
//! Export calls the same verified copy used by ingest. It does not modify
//! the source tree. A file already at the destination with the same SHA-256
//! is left alone. A different file at that relative path is an error.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::checksum::{sha256_file, sha256_hex, to_hex};
use crate::discover::{check_filename, discover, file_name_str};
use crate::error::IngestError;
use crate::local::copy_verify_file;
use crate::mtime::mtime_of;
use crate::store::VerifiedObject;

/// One line of an export manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub relative_path: String,
    pub sha256: String,
    pub byte_len: u64,
}

/// Result of placing one library file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub object: VerifiedObject,
    pub copied: bool,
}

/// Where an export writes. Implementations must not overwrite bytes.
pub trait ExportDest {
    fn place(&self, source: &Path, relative: &str) -> Result<Placed, IngestError>;
    /// Hash the bytes at `relative`. `Ok(None)` means the path is absent.
    fn read_hash(&self, relative: &str) -> Result<Option<(String, u64)>, IngestError>;
    fn list(&self) -> Result<Vec<String>, IngestError>;
    /// Remove a destination this export just created. Not for sources.
    fn remove_placed(&self, relative: &str) -> Result<(), IngestError>;

    /// Local directory this destination writes into, when it has one.
    fn local_root(&self) -> Option<&Path> {
        None
    }
}

/// A directory that mirrors the library's relative paths.
#[derive(Debug, Clone)]
pub struct DirectoryTree {
    root: PathBuf,
}

impl DirectoryTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn full(&self, relative: &str) -> Result<PathBuf, IngestError> {
        check_relative(relative)?;
        Ok(self.root.join(relative_path(relative)))
    }
}

impl ExportDest for DirectoryTree {
    fn place(&self, source: &Path, relative: &str) -> Result<Placed, IngestError> {
        let dest = self.full(relative)?;
        require_same_filename(source, relative)?;
        if dest.is_file() {
            let (hash, len) = hash_file(&dest)?;
            let (source_hash, source_len) = hash_through_source(source)?;
            if hash == source_hash && len == source_len {
                return Ok(Placed {
                    object: VerifiedObject {
                        filename: relative.to_string(),
                        sha256: hash,
                        byte_len: len,
                        mtime: mtime_of(&dest)?,
                        sha256_verify_ms: 0,
                    },
                    copied: false,
                });
            }
            return Err(IngestError::AlreadyExists {
                filename: relative.to_string(),
                destination: dest.display().to_string(),
            });
        }
        if dest.exists() {
            return Err(IngestError::AlreadyExists {
                filename: relative.to_string(),
                destination: dest.display().to_string(),
            });
        }
        let parent = dest.parent().unwrap_or(&self.root);
        let object = copy_verify_file(source, parent)?;
        Ok(Placed {
            object: VerifiedObject {
                filename: relative.to_string(),
                ..object
            },
            copied: true,
        })
    }

    fn read_hash(&self, relative: &str) -> Result<Option<(String, u64)>, IngestError> {
        let dest = self.full(relative)?;
        if !dest.exists() {
            return Ok(None);
        }
        if !dest.is_file() {
            return Err(IngestError::NotAFile(dest));
        }
        Ok(Some(hash_file(&dest)?))
    }

    fn list(&self) -> Result<Vec<String>, IngestError> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        walk_sorted(&self.root, None, &mut |relative| {
            out.push(relative.to_string());
            WalkControl::Continue
        })?;
        Ok(out)
    }

    fn remove_placed(&self, relative: &str) -> Result<(), IngestError> {
        let dest = self.full(relative)?;
        match crate::dest::remove_file(&dest) {
            Ok(()) => Ok(()),
            Err(IngestError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn local_root(&self) -> Option<&Path> {
        Some(&self.root)
    }
}

/// Copy every file under `source`, keeping sidecars and pairs together.
///
/// The source tree is not modified. Relative paths are preserved.
pub fn export_tree(
    source: &crate::source::SourceRoot,
    dest: &dyn ExportDest,
) -> Result<Vec<ManifestEntry>, IngestError> {
    let root = source.canonical();
    if !root.is_dir() {
        return Err(IngestError::NotAFile(root.to_path_buf()));
    }
    if let Some(dest_root) = dest.local_root() {
        source.refuse_destination(dest_root)?;
    }
    let mut files = Vec::new();
    source.walk(None, &mut |relative| {
        files.push(relative.to_string());
        true
    })?;
    let assets = plan_assets(root, &files)?;
    let mut entries = Vec::new();
    for asset in assets {
        let mut copied_now = Vec::new();
        for file in asset.files() {
            let relative = to_relative(root, &file.path)?;
            match dest.place(&file.path, &relative) {
                Ok(placed) => {
                    if placed.copied {
                        copied_now.push(relative.clone());
                    }
                    entries.push(ManifestEntry {
                        relative_path: relative,
                        sha256: placed.object.sha256,
                        byte_len: placed.object.byte_len,
                    });
                }
                Err(err) => {
                    for relative in copied_now.iter().rev() {
                        if let Err(rollback) = dest.remove_placed(relative) {
                            return Err(IngestError::store(
                                "rollback",
                                relative,
                                format!("{err}; rollback: {rollback}"),
                            ));
                        }
                    }
                    return Err(err);
                }
            }
        }
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(entries)
}

/// Hash every listed file and require the destination to contain exactly that set.
///
/// `ignore` names destination paths that are not library files, such as the
/// manifest when it was written inside the export directory.
pub fn verify_tree(
    entries: &[ManifestEntry],
    dest: &dyn ExportDest,
    ignore: &[String],
) -> Result<(), IngestError> {
    let mut expected = BTreeSet::new();
    for entry in entries {
        check_relative(&entry.relative_path)?;
        if !expected.insert(entry.relative_path.clone()) {
            return Err(IngestError::DuplicateFilename(entry.relative_path.clone()));
        }
        let Some((actual_sha, actual_len)) = dest.read_hash(&entry.relative_path)? else {
            return Err(IngestError::ManifestMissing(entry.relative_path.clone()));
        };
        if actual_sha != entry.sha256 || actual_len != entry.byte_len {
            return Err(IngestError::ManifestMismatch {
                relative_path: entry.relative_path.clone(),
                expected_sha256: entry.sha256.clone(),
                actual_sha256: actual_sha,
                expected_len: entry.byte_len,
                actual_len,
            });
        }
    }
    for relative in dest.list()? {
        if ignore.iter().any(|name| name == &relative) {
            continue;
        }
        if !expected.contains(&relative) {
            return Err(IngestError::ManifestExtra(relative));
        }
    }
    Ok(())
}

pub fn format_manifest(entries: &[ManifestEntry]) -> String {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut out = String::from("# albumen-manifest-v1\n");
    for entry in ordered {
        out.push_str(&entry.sha256);
        out.push(' ');
        out.push_str(&entry.byte_len.to_string());
        out.push(' ');
        out.push_str(&entry.relative_path);
        out.push('\n');
    }
    out
}

pub fn parse_manifest(text: &str) -> Result<Vec<ManifestEntry>, IngestError> {
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let sha = parts.next().unwrap_or("");
        let len = parts.next().unwrap_or("");
        let relative = parts.next().unwrap_or("");
        let byte_len = len.parse::<u64>().ok();
        let sha_ok = sha.len() == 64 && sha.chars().all(|ch| ch.is_ascii_hexdigit());
        if !sha_ok || byte_len.is_none() || check_relative(relative).is_err() {
            return Err(IngestError::ManifestParse {
                line_number,
                line: line.to_string(),
            });
        }
        entries.push(ManifestEntry {
            relative_path: relative.to_string(),
            sha256: sha.to_ascii_lowercase(),
            byte_len: byte_len.expect("checked"),
        });
    }
    Ok(entries)
}

/// Write the manifest by renaming a temporary file in the same directory.
///
/// The previous manifest is never unlinked first. The temporary file is
/// fsynced, renamed over the old name, and the directory is fsynced. This is
/// the generated checksum list, not an original. Callers must not point it
/// at a library file.
pub fn write_manifest(path: &Path, entries: &[ManifestEntry]) -> Result<(), IngestError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    crate::dest::create_dir_all(parent)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".albumen-manifest-{}-{nanos}.tmp",
        std::process::id()
    ));
    let text = format_manifest(entries);
    let write_result = (|| {
        let mut file = crate::dest::create_file(&tmp)?;
        file.write_all(text.as_bytes())
            .map_err(|err| IngestError::io(&tmp, err))?;
        file.sync_all().map_err(|err| IngestError::io(&tmp, err))?;
        crate::dest::rename(&tmp, path)?;
        crate::dest::sync_dir(parent)?;
        Ok(())
    })();
    if write_result.is_err() {
        crate::dest::remove_file_best_effort(&tmp);
    }
    write_result
}

fn hash_through_source(path: &Path) -> Result<(String, u64), IngestError> {
    let root = crate::source::SourceRoot::open(crate::source::parent_dir(path))?;
    let file = root.open_read(path)?;
    let len = file
        .metadata()
        .map_err(|err| IngestError::io(path, err))?
        .len();
    let hash = crate::checksum::sha256_reader(file, path)?;
    Ok((to_hex(&hash), len))
}

fn hash_file(path: &Path) -> Result<(String, u64), IngestError> {
    let hash = sha256_file(path)?;
    let len = fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len();
    Ok((to_hex(&hash), len))
}

fn require_same_filename(source: &Path, relative: &str) -> Result<(), IngestError> {
    let source_name = file_name_str(source)?;
    let last = relative.rsplit('/').next().unwrap_or(relative);
    if source_name != last {
        return Err(IngestError::FilenameMismatch {
            source_name,
            requested: relative.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn check_relative(relative: &str) -> Result<(), IngestError> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.ends_with('/')
        || relative.contains('\\')
        || relative.contains('\0')
        || relative.contains('\n')
        || relative.contains('\r')
    {
        return Err(IngestError::UnsafeFilename {
            filename: relative.to_string(),
        });
    }
    for component in relative.split('/') {
        check_filename(component)?;
        if component == "." || component == ".." {
            return Err(IngestError::UnsafeFilename {
                filename: relative.to_string(),
            });
        }
    }
    Ok(())
}

fn relative_path(relative: &str) -> PathBuf {
    relative.split('/').collect()
}

fn to_relative(root: &Path, path: &Path) -> Result<String, IngestError> {
    let rel = path.strip_prefix(root).map_err(|_| {
        IngestError::store(
            "export",
            path.display().to_string(),
            "path is outside the library",
        )
    })?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| IngestError::NonUtf8Filename {
                    path: path.to_path_buf(),
                })?;
                check_filename(part)?;
                parts.push(part.to_string());
            }
            _ => {
                return Err(IngestError::UnsafeFilename {
                    filename: path.display().to_string(),
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(IngestError::UnsafeFilename {
            filename: path.display().to_string(),
        });
    }
    Ok(parts.join("/"))
}

/// Ask the walker to keep going, or to stop before unread branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkControl {
    Continue,
    Stop,
}

/// Visit files under `root` in relative-path order.
///
/// `after` skips paths that sort at or before that relative path, so a page
/// can resume. One directory is listed so the order stays stable. The walk
/// does not keep a vector of the whole tree, and it returns when `visit`
/// asks it to stop.
pub fn walk_sorted(
    root: &Path,
    after: Option<&str>,
    visit: &mut dyn FnMut(&str) -> WalkControl,
) -> Result<(), IngestError> {
    if !root.exists() {
        return Ok(());
    }
    let source = crate::source::SourceRoot::open(root)?;
    source.walk(after, &mut |relative| {
        visit(relative) == WalkControl::Continue
    })
}

/// How many characters of a filename are kept in a local scratch path.
///
/// `ALBUMEN_SCRATCH_NAME_CHARS` overrides 80. The stored object name is not
/// shortened.
pub(crate) fn scratch_name_chars() -> usize {
    match std::env::var("ALBUMEN_SCRATCH_NAME_CHARS") {
        Ok(text) => match text.parse::<usize>() {
            Ok(value) if value >= 1 => value,
            _ => 80,
        },
        Err(_) => 80,
    }
}

fn plan_assets(
    root: &Path,
    relative_files: &[String],
) -> Result<Vec<crate::discover::Asset>, IngestError> {
    let mut remaining: BTreeSet<PathBuf> = relative_files
        .iter()
        .map(|relative| root.join(relative_path(relative)))
        .collect();
    let mut order: Vec<PathBuf> = remaining.iter().cloned().collect();
    order.sort_by(|left, right| {
        let rank = |path: &Path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let lower = name.to_ascii_lowercase();
            let sidecar = lower.ends_with(".xmp") || lower.ends_with(".aae");
            (u8::from(sidecar), name.to_string())
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    let mut assets = Vec::new();
    for path in order {
        if !remaining.contains(&path) {
            continue;
        }
        let source_root = crate::source::SourceRoot::open(root)?;
        let _meta = source_root.metadata_nofollow(&path)?;
        let asset = discover(&path)?;
        for file in asset.files() {
            let _file_meta = source_root.metadata_nofollow(&file.path)?;
            if !file.path.starts_with(root) {
                return Err(IngestError::store(
                    "export",
                    file.path.display().to_string(),
                    "companion is outside the library",
                ));
            }
            remaining.remove(&file.path);
        }
        assets.push(asset);
    }
    Ok(assets)
}

/// Hash a file the CLI and S3 export both use when comparing an existing object.
pub(crate) fn source_fingerprint(path: &Path) -> Result<(String, u64), IngestError> {
    let hash = sha256_hex(path)?;
    let len = fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len();
    Ok((hash, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceRoot;
    use crate::test_support::{write_file, Tmp};
    use std::time::Duration;

    fn as_root(path: &Path) -> SourceRoot {
        SourceRoot::open(path).unwrap()
    }

    fn mtime() -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_600_000_000)
    }

    #[test]
    fn manifest_in_the_process_temp_directory_is_written() {
        let path = std::env::temp_dir().join(format!(
            "albumen-manifest-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let entries = [ManifestEntry {
            relative_path: "aae/IMG_0008.jpg".to_string(),
            sha256: "ab".repeat(32),
            byte_len: 4,
        }];
        write_manifest(&path, &entries).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("aae/IMG_0008.jpg"), "{text}");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn walk_is_lexicographic_and_can_stop() {
        let tmp = Tmp::new();
        let root = tmp.path();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a.txt"), b"1").unwrap();
        fs::write(root.join("a").join("z.txt"), b"2").unwrap();
        fs::write(root.join("ab.txt"), b"3").unwrap();
        fs::write(root.join("b.txt"), b"4").unwrap();
        let mut got = Vec::new();
        walk_sorted(root, None, &mut |path| {
            got.push(path.to_string());
            WalkControl::Continue
        })
        .unwrap();
        assert_eq!(
            got,
            vec![
                "a.txt".to_string(),
                "a/z.txt".to_string(),
                "ab.txt".to_string(),
                "b.txt".to_string()
            ]
        );
        assert_eq!(DirectoryTree::new(root).list().unwrap(), got);
        let mut page = Vec::new();
        walk_sorted(root, Some("a.txt"), &mut |path| {
            page.push(path.to_string());
            if page.len() == 2 {
                WalkControl::Stop
            } else {
                WalkControl::Continue
            }
        })
        .unwrap();
        assert_eq!(page, vec!["a/z.txt".to_string(), "ab.txt".to_string()]);
    }

    #[test]
    fn export_keeps_pairs_and_does_not_touch_sources() {
        let tmp = Tmp::new();
        let library = tmp.path().join("library");
        let pair = library.join("raw-jpeg");
        fs::create_dir_all(&pair).unwrap();
        let dng = write_file(&pair, "IMG_0003.dng", b"raw-bytes", mtime());
        let jpg = write_file(&pair, "IMG_0003.jpg", b"jpeg-bytes", mtime());
        write_file(&pair, "IMG_0003.xmp", b"<xmp/>", mtime());
        let before_dng = fs::read(&dng).unwrap();
        let before_jpg = fs::read(&jpg).unwrap();
        let before_mtime = fs::metadata(&dng).unwrap().modified().unwrap();

        let dest_root = tmp.path().join("export");
        let dest = DirectoryTree::new(&dest_root);
        let entries = export_tree(&as_root(&library), &dest).unwrap();
        let paths: Vec<_> = entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "raw-jpeg/IMG_0003.dng",
                "raw-jpeg/IMG_0003.jpg",
                "raw-jpeg/IMG_0003.xmp",
            ]
        );
        assert_eq!(fs::read(&dng).unwrap(), before_dng);
        assert_eq!(fs::read(&jpg).unwrap(), before_jpg);
        assert_eq!(
            fs::metadata(&dng).unwrap().modified().unwrap(),
            before_mtime
        );

        let manifest = format_manifest(&entries);
        let parsed = parse_manifest(&manifest).unwrap();
        verify_tree(&parsed, &dest, &[]).unwrap();

        let kept = fs::metadata(dest_root.join("raw-jpeg/IMG_0003.jpg"))
            .unwrap()
            .modified()
            .unwrap();
        let again = export_tree(&as_root(&library), &dest).unwrap();
        assert_eq!(again, entries);
        assert_eq!(
            fs::metadata(dest_root.join("raw-jpeg/IMG_0003.jpg"))
                .unwrap()
                .modified()
                .unwrap(),
            kept
        );

        let mut tampered = fs::read(dest_root.join("raw-jpeg/IMG_0003.jpg")).unwrap();
        tampered[0] ^= 0xff;
        fs::write(dest_root.join("raw-jpeg/IMG_0003.jpg"), &tampered).unwrap();
        let err = export_tree(&as_root(&library), &dest).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_eq!(
            fs::read(dest_root.join("raw-jpeg/IMG_0003.jpg")).unwrap(),
            tampered
        );
        assert_eq!(fs::read(&jpg).unwrap(), before_jpg);
        let verify_err = verify_tree(&parsed, &dest, &[]).unwrap_err();
        assert!(matches!(verify_err, IngestError::ManifestMismatch { .. }));
    }

    #[test]
    fn resume_copies_only_the_missing_file() {
        let tmp = Tmp::new();
        let library = tmp.path().join("library");
        fs::create_dir_all(library.join("video")).unwrap();
        write_file(&library.join("video"), "IMG_0006.mp4", b"video", mtime());
        write_file(&library, "note.txt", b"note", mtime());
        let dest_root = tmp.path().join("export");
        let dest = DirectoryTree::new(&dest_root);
        export_tree(&as_root(&library), &dest).unwrap();
        let kept = fs::metadata(dest_root.join("note.txt"))
            .unwrap()
            .modified()
            .unwrap();
        fs::remove_file(dest_root.join("video/IMG_0006.mp4")).unwrap();
        export_tree(&as_root(&library), &dest).unwrap();
        assert_eq!(
            fs::read(dest_root.join("video/IMG_0006.mp4")).unwrap(),
            b"video"
        );
        assert_eq!(
            fs::metadata(dest_root.join("note.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            kept
        );
    }

    #[test]
    fn s3_export_skips_identical_objects_and_verifies() {
        use crate::{S3Config, S3Store};
        if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
            eprintln!(
                "skipping S3: ALBUMEN_S3_UNAVAILABLE=1. Docker is not used. Local filesystem only."
            );
            return;
        }
        let mut config = S3Config::from_env().unwrap_or_else(|err| {
            panic!(
                "S3 export requires MinIO ({err}). Run scripts/with-minio.sh cargo test --locked"
            )
        });
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        config.prefix = format!("export-{}-{nanos}/", std::process::id());
        let store = S3Store::connect(config).expect("connect");
        let tmp = Tmp::new();
        let library = tmp.path().join("library");
        fs::create_dir_all(library.join("pair")).unwrap();
        let source = library.join("pair").join("IMG_0003.jpg");
        write_file(
            &library.join("pair"),
            "IMG_0003.jpg",
            b"jpeg-bytes",
            mtime(),
        );
        write_file(&library.join("pair"), "IMG_0003.aae", b"aae-bytes", mtime());
        let before = fs::read(&source).unwrap();

        let entries = export_tree(&as_root(&library), &store).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["pair/IMG_0003.aae", "pair/IMG_0003.jpg"]
        );
        let etag = store.object_etag("pair/IMG_0003.jpg").unwrap();
        let again = export_tree(&as_root(&library), &store).unwrap();
        assert_eq!(again, entries);
        assert_eq!(store.object_etag("pair/IMG_0003.jpg").unwrap(), etag);
        verify_tree(&entries, &store, &[]).unwrap();
        assert_eq!(fs::read(&source).unwrap(), before);

        fs::write(&source, b"replacement-bytes").unwrap();
        let err = export_tree(&as_root(&library), &store).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_eq!(store.object_etag("pair/IMG_0003.jpg").unwrap(), etag);
        let (hash, len) = store.read_hash("pair/IMG_0003.jpg").unwrap().unwrap();
        assert_eq!(len, b"jpeg-bytes".len() as u64);
        assert_ne!(hash, crate::sha256_hex(&source).unwrap());
    }
}
