// SPDX-License-Identifier: Apache-2.0

//! Live Photos in `albumen gui`: pair a still with its movie by the shared
//! ContentIdentifier, keep a disposable preview cache under ALBUMEN_HOME,
//! and serve the movie bytes as they are on disk (no transcode), with HTTP
//! Range support so the player can seek.
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

/// A library movie by its relative path, opened read-only, with its length.
/// `..`, absolute paths and anything resolving outside the library are
/// refused.
pub fn open_video(library: &Path, rel: &str) -> Option<(std::fs::File, u64)> {
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
    let f = std::fs::File::open(full).ok()?;
    let len = f.metadata().ok()?.len();
    Some((f, len))
}

/// The MIME type for a served movie. video/quicktime is the registered type
/// for .mov; scripts/gui-live-behaviour.mjs shows headless Chrome plays it.
pub const VIDEO_MIME: &str = "video/quicktime";

/// What a `Range` header asks for, against a file of `len` bytes.
#[derive(Debug, PartialEq)]
pub enum Range {
    /// No usable range: send everything (200).
    Full,
    /// Inclusive byte range (206).
    Part(u64, u64),
    /// Outside the file (416).
    Unsatisfiable,
}

/// Parse a single `bytes=` range. Multiple ranges and other units fall back
/// to the whole file, which RFC 9110 allows.
pub fn parse_range(header: Option<&str>, len: u64) -> Range {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return Range::Full;
    };
    if spec.contains(',') {
        return Range::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return Range::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    let (start, end) = match (a.is_empty(), b.is_empty()) {
        (false, _) => {
            let Ok(start) = a.parse::<u64>() else {
                return Range::Full;
            };
            let end = if b.is_empty() {
                len.saturating_sub(1)
            } else {
                match b.parse::<u64>() {
                    Ok(e) if e >= start => e.min(len.saturating_sub(1)),
                    _ => return Range::Full,
                }
            };
            (start, end)
        }
        (true, false) => {
            let Ok(n) = b.parse::<u64>() else {
                return Range::Full;
            };
            if n == 0 {
                return Range::Unsatisfiable;
            }
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        (true, true) => return Range::Full,
    };
    if len == 0 || start >= len {
        return Range::Unsatisfiable;
    }
    Range::Part(start, end)
}

/// Write an HTTP response for a movie: 200 with the whole file, 206 with
/// one range, or 416. The body is streamed from the file in small chunks,
/// so memory stays flat however large the movie is. Never writes the file.
pub fn write_video<W: std::io::Write>(
    out: &mut W,
    file: &mut std::fs::File,
    len: u64,
    range: Option<&str>,
) -> std::io::Result<()> {
    let common = format!(
        "Content-Type: {VIDEO_MIME}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n"
    );
    let (status, extra, start, count) = match parse_range(range, len) {
        Range::Full => ("200 OK", String::new(), 0, len),
        Range::Part(a, b) => (
            "206 Partial Content",
            format!("Content-Range: bytes {a}-{b}/{len}\r\n"),
            a,
            b - a + 1,
        ),
        Range::Unsatisfiable => {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\n{common}Content-Range: bytes */{len}\r\nContent-Length: 0\r\n\r\n"
            );
            out.write_all(head.as_bytes())?;
            return out.flush();
        }
    };
    let head = format!("HTTP/1.1 {status}\r\n{common}{extra}Content-Length: {count}\r\n\r\n");
    out.write_all(head.as_bytes())?;
    file.seek(SeekFrom::Start(start))?;
    let mut body = std::io::BufReader::with_capacity(64 * 1024, file.take(count));
    std::io::copy(&mut body, out)?;
    out.flush()
}

/// Sample-entry codes from a movie's `stsd` boxes (e.g. `avc1`, `mp4a`,
/// `hvc1`), read from the first and last MiB.
pub fn sample_entries(path: &Path) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut windows = Vec::new();
    let mut head = Vec::new();
    if (&mut f).take(WINDOW).read_to_end(&mut head).is_ok() {
        windows.push(head);
    }
    if len > WINDOW
        && f.seek(SeekFrom::Start(len.saturating_sub(WINDOW).max(WINDOW)))
            .is_ok()
    {
        let mut tail = Vec::new();
        if f.read_to_end(&mut tail).is_ok() {
            windows.push(tail);
        }
    }
    let mut out = Vec::new();
    for w in &windows {
        let mut from = 0;
        while let Some(at) = find(&w[from..], b"stsd").map(|i| i + from) {
            // 'stsd' version/flags(4) entry_count(4) entry_size(4) entry_type(4)
            if let Some(code) = w.get(at + 16..at + 20) {
                if code.iter().all(|c| c.is_ascii_alphanumeric()) {
                    out.push(String::from_utf8_lossy(code).into_owned());
                }
            }
            from = at + 4;
        }
    }
    out
}

/// Whether a browser can play this Live Photo movie: H.264 (`avc1`/`avc3`)
/// only. HEVC (`hvc1`/`hev1`) or an unknown codec gets the can't-preview
/// copy instead of a broken player.
pub fn playable(path: &Path) -> (bool, Option<&'static str>) {
    let codes = sample_entries(path);
    let has = |c: &[&str]| codes.iter().any(|x| c.contains(&x.as_str()));
    if has(&["hvc1", "hev1"]) {
        (false, Some("HEVC"))
    } else if has(&["avc1", "avc3"]) {
        (true, Some("H.264"))
    } else {
        (false, None)
    }
}

/// Test helper: the whole movie as bytes.
#[cfg(test)]
pub fn read_video(library: &Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    let (mut f, _) = open_video(library, rel)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    Some((bytes, VIDEO_MIME))
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
    #[test]
    fn range_header_parsing() {
        assert_eq!(parse_range(None, 100), Range::Full);
        assert_eq!(parse_range(Some("bytes=0-9"), 100), Range::Part(0, 9));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Range::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Range::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Range::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=-500"), 100), Range::Part(0, 99));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Range::Full);
        assert_eq!(parse_range(Some("bytes=9-3"), 100), Range::Full);
        assert_eq!(parse_range(Some("items=0-1"), 100), Range::Full);
        assert_eq!(parse_range(Some("bytes=x-1"), 100), Range::Full);
    }

    fn response(rel_lib: &Path, range: Option<&str>) -> (String, Vec<u8>) {
        let (mut f, len) = open_video(rel_lib, "IMG_0005.mov").unwrap();
        let mut out = Vec::new();
        write_video(&mut out, &mut f, len, range).unwrap();
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8_lossy(&out[..at]).into_owned(),
            out[at + 4..].to_vec(),
        )
    }

    #[test]
    fn video_responses_stream_whole_ranges_and_416() {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo");
        let want = std::fs::read(fx.join("IMG_0005.mov")).unwrap();
        let (head, body) = response(&fx, None);
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(head.contains("Accept-Ranges: bytes\r\n"));
        assert!(head.contains("Content-Type: video/quicktime\r\n"));
        assert!(head.ends_with(&format!("Content-Length: {}", want.len())));
        assert_eq!(body, want);
        let (head, body) = response(&fx, Some("bytes=100-199"));
        assert!(
            head.starts_with("HTTP/1.1 206 Partial Content\r\n"),
            "{head}"
        );
        assert!(head.contains(&format!("Content-Range: bytes 100-199/{}\r\n", want.len())));
        assert!(head.ends_with("Content-Length: 100"));
        assert_eq!(body, want[100..200]);
        let (head, body) = response(&fx, Some("bytes=-16"));
        assert_eq!(body, want[want.len() - 16..]);
        assert!(head.contains("206"));
        let (head, body) = response(&fx, Some("bytes=999999-"));
        assert!(head.starts_with("HTTP/1.1 416 "), "{head}");
        assert!(head.contains(&format!("Content-Range: bytes */{}\r\n", want.len())));
        assert!(body.is_empty());
    }

    /// Streaming: a large sparse movie is served through a fixed buffer.
    /// The response is counted, not kept, so the test itself stays small.
    #[test]
    fn large_video_streams_without_loading_it() {
        struct Count(u64);
        impl std::io::Write for Count {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0 += b.len() as u64;
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let size: u64 = 256 << 20;
        let f = albumen_ingest::dest::create_file(&dir.path().join("big.mov")).unwrap();
        f.set_len(size).unwrap();
        drop(f);
        let (mut f, len) = open_video(dir.path(), "big.mov").unwrap();
        assert_eq!(len, size);
        let mut out = Count(0);
        write_video(&mut out, &mut f, len, None).unwrap();
        assert!(out.0 > size && out.0 < size + 512);
        let mut out = Count(0);
        write_video(&mut out, &mut f, len, Some("bytes=134217728-")).unwrap();
        assert!(out.0 > size / 2 && out.0 < size / 2 + 512);
    }

    #[test]
    fn codec_is_read_from_the_sample_entry() {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo");
        let codes = sample_entries(&fx.join("IMG_0005.mov"));
        assert!(codes.contains(&"avc1".to_string()), "{codes:?}");
        assert_eq!(playable(&fx.join("IMG_0005.mov")), (true, Some("H.264")));
        // Synthetic HEVC stub: the same file with the video sample entry
        // code changed to hvc1 (and hev1). Not decodable, only labelled.
        let dir = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(fx.join("IMG_0005.mov")).unwrap();
        for code in [b"hvc1", b"hev1"] {
            let mut stub = bytes.clone();
            let at = stub.windows(4).position(|w| w == b"avc1").unwrap();
            stub[at..at + 4].copy_from_slice(code);
            let p = dir
                .path()
                .join(format!("{}.mov", String::from_utf8_lossy(code)));
            std::fs::write(&p, &stub).unwrap();
            assert_eq!(playable(&p), (false, Some("HEVC")));
        }
        let junk = dir.path().join("junk.mov");
        std::fs::write(&junk, b"not a movie").unwrap();
        assert_eq!(playable(&junk), (false, None));
    }
}
