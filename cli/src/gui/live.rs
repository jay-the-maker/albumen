// SPDX-License-Identifier: Apache-2.0

//! Live Photos in `albumen gui`: pair a still with its movie by the shared
//! ContentIdentifier, keep a disposable preview cache under ALBUMEN_HOME,
//! and serve the movie bytes as they are on disk (no transcode).
//!
//! Everything here reads the library. The only writes go to the cache
//! folder, which is never inside the library.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Stills that can be the photo half of a Live Photo.
pub const STILL_EXT: &[&str] = &["heic", "heif", "jpg", "jpeg"];
/// Movies that can be the video half. Only H.264 plays today (AC5); an HEVC
/// movie is served the same way and the browser decides.
pub const VIDEO_EXT: &[&str] = &["mov"];

/// Bytes read from each end of a file when looking for the identifier.
const WINDOW: u64 = 1 << 20;
/// The movie key ExifTool calls Keys:ContentIdentifier.
const MOV_KEY: &[u8] = b"com.apple.quicktime.content.identifier";
/// The Apple maker-note signature that holds Apple:ContentIdentifier.
const APPLE_NOTE: &[u8] = b"Apple iOS";

fn ext(rel: &str) -> Option<String> {
    rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

pub fn is_still(rel: &str) -> bool {
    ext(rel).is_some_and(|e| STILL_EXT.contains(&e.as_str()))
}

pub fn is_video(rel: &str) -> bool {
    ext(rel).is_some_and(|e| VIDEO_EXT.contains(&e.as_str()))
}

/// The first UUID-shaped ASCII string (8-4-4-4-12 hex) in `bytes`.
fn first_uuid(bytes: &[u8]) -> Option<String> {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let len = 36;
    if bytes.len() < len {
        return None;
    }
    'outer: for start in 0..=bytes.len() - len {
        let mut i = start;
        for (n, run) in SHAPE.iter().enumerate() {
            for _ in 0..*run {
                if !bytes[i].is_ascii_hexdigit() {
                    continue 'outer;
                }
                i += 1;
            }
            if n < 4 {
                if bytes[i] != b'-' {
                    continue 'outer;
                }
                i += 1;
            }
        }
        return Some(String::from_utf8_lossy(&bytes[start..start + len]).to_ascii_uppercase());
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The Live Photo ContentIdentifier of a still (Apple maker note) or a movie
/// (QuickTime keys), read from the first and last MiB of the file. None when
/// the file has no identifier. The file is opened read-only.
pub fn content_identifier(path: &Path) -> Option<String> {
    let rel = path.to_string_lossy();
    let marker = if is_video(&rel) { MOV_KEY } else { APPLE_NOTE };
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut windows = Vec::new();
    let mut head = Vec::new();
    (&mut f).take(WINDOW).read_to_end(&mut head).ok()?;
    windows.push(head);
    if len > WINDOW {
        let from = len.saturating_sub(WINDOW).max(WINDOW);
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut tail = Vec::new();
        f.read_to_end(&mut tail).ok()?;
        windows.push(tail);
    }
    windows
        .iter()
        .find_map(|w| find(w, marker).and_then(|at| first_uuid(&w[at..])))
}

/// Where the GUI keeps disposable previews for `library`: under
/// `$ALBUMEN_HOME/previews` (or the platform data folder), keyed by the
/// library path. None when that folder would be inside the library.
pub fn cache_dir(library: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("ALBUMEN_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| albumen_ingest::platform_dirs().ok().map(|(_, data)| data))
        .unwrap_or_else(|| std::env::temp_dir().join("albumen"));
    cache_dir_in(&home, library)
}

/// `cache_dir` with an explicit home folder.
pub fn cache_dir_in(home: &Path, library: &Path) -> Option<PathBuf> {
    let canonical = library.canonicalize().ok()?;
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "library".to_string());
    let key = {
        use sha2::Digest as _;
        let d = sha2::Sha256::digest(canonical.as_os_str().to_string_lossy().as_bytes());
        d[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let dir = home.join("previews").join(format!("{name}-{key}"));
    // Resolve as much of the cache path as exists, then refuse it if it is
    // the library or inside it.
    let mut probe = dir.clone();
    let mut tail = Vec::new();
    let resolved = loop {
        if let Ok(c) = probe.canonicalize() {
            break tail
                .iter()
                .rev()
                .fold(c, |p: PathBuf, t: &std::ffi::OsString| p.join(t));
        }
        tail.push(probe.file_name()?.to_os_string());
        probe = probe.parent()?.to_path_buf();
    };
    (!resolved.starts_with(&canonical)).then_some(dir)
}

/// One cache entry: the identifier read from a file of this size and mtime.
#[derive(Clone, PartialEq, Debug)]
struct Entry {
    len: u64,
    mtime_ns: u128,
    id: Option<String>,
}

fn stamp(path: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(path).ok()?;
    let ns = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((m.len(), ns))
}

const CACHE_FILE: &str = "live-ids-v1.json";

/// Identifiers for `rels`, from the cache when the size and mtime still
/// match, otherwise read again from the original. The cache is rewritten
/// when anything changed; losing it only costs a re-read.
pub fn identifiers(
    library: &Path,
    rels: &[&str],
    cache: Option<&Path>,
) -> BTreeMap<String, Option<String>> {
    let mut old: BTreeMap<String, Entry> = BTreeMap::new();
    if let Some(v) = cache
        .and_then(|c| std::fs::read(c.join(CACHE_FILE)).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    {
        if let Some(map) = v["files"].as_object() {
            for (rel, e) in map {
                let (Some(len), Some(mtime)) = (e["len"].as_u64(), e["mtime_ns"].as_str()) else {
                    continue;
                };
                let Ok(mtime_ns) = mtime.parse() else {
                    continue;
                };
                old.insert(
                    rel.clone(),
                    Entry {
                        len,
                        mtime_ns,
                        id: e["id"].as_str().map(str::to_string),
                    },
                );
            }
        }
    }
    let mut new = BTreeMap::new();
    let mut out = BTreeMap::new();
    for rel in rels {
        let path = library.join(rel);
        let Some((len, mtime_ns)) = stamp(&path) else {
            continue;
        };
        let entry = match old.get(*rel) {
            Some(e) if e.len == len && e.mtime_ns == mtime_ns => e.clone(),
            _ => Entry {
                len,
                mtime_ns,
                id: content_identifier(&path),
            },
        };
        out.insert(rel.to_string(), entry.id.clone());
        new.insert(rel.to_string(), entry);
    }
    if let Some(dir) = cache {
        let changed = new.len() != old.len() || new.iter().any(|(k, e)| old.get(k) != Some(e));
        if changed {
            let files: serde_json::Map<String, serde_json::Value> = new
                .iter()
                .map(|(k, e)| {
                    (
                        k.clone(),
                        serde_json::json!({
                            "len": e.len, "mtime_ns": e.mtime_ns.to_string(), "id": e.id,
                        }),
                    )
                })
                .collect();
            let body = serde_json::json!({ "files": files }).to_string();
            // Write beside, then rename over: a reader never sees half a file.
            if std::fs::create_dir_all(dir).is_ok() {
                let tmp = dir.join(format!("{CACHE_FILE}.{}.tmp", std::process::id()));
                if std::fs::write(&tmp, body).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join(CACHE_FILE));
                }
            }
        }
    }
    out
}

/// A library movie by its relative path, as its bytes on disk. `..`,
/// absolute paths and anything resolving outside the library are refused.
/// The MIME type is video/mp4: QuickTime H.264 plays under it in browsers,
/// and the bytes are not changed.
pub fn read_video(library: &Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || !is_video(rel)
    {
        return None;
    }
    let root = library.canonicalize().ok()?;
    let full = root.join(rel_path).canonicalize().ok()?;
    if !full.starts_with(&root) {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(full)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    Some((bytes, "video/mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_shape_is_strict() {
        assert_eq!(
            first_uuid(b"xx b4e3c1a0-7f2d-4c8a-9e11-0a1b2c3d4e5f yy").as_deref(),
            Some("B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5F")
        );
        assert_eq!(first_uuid(b"B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5"), None);
        assert_eq!(first_uuid(b"B4E3C1A0_7F2D-4C8A-9E11-0A1B2C3D4E5F"), None);
    }

    #[test]
    fn fixture_halves_share_one_identifier() {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo");
        let still = content_identifier(&fx.join("IMG_0005.heic"));
        let movie = content_identifier(&fx.join("IMG_0005.mov"));
        assert_eq!(
            still.as_deref(),
            Some("B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5F")
        );
        assert_eq!(still, movie);
        let plain = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/heic/IMG_0002.heic");
        assert_eq!(content_identifier(&plain), None);
    }

    #[test]
    fn cache_is_refused_inside_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        assert!(cache_dir_in(&lib, &lib).is_none());
        assert!(cache_dir_in(&lib.join("sub"), &lib).is_none());
        let home = dir.path().join("home");
        let c = cache_dir_in(&home, &lib).unwrap();
        assert!(c.starts_with(&home));
    }

    #[test]
    fn video_refuses_traversal_and_non_movies() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_video(dir.path(), "../x.mov").is_none());
        assert!(read_video(dir.path(), "a.jpg").is_none());
        assert!(read_video(dir.path(), "").is_none());
    }
}
