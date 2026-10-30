// SPDX-License-Identifier: Apache-2.0

//! M5 AC2-AC4 HEIC extension: a content-keyed thumbnail index, kept outside
//! the library, with no decoder.
//!
//! Layout (S-H1): `$ALBUMEN_HOME/previews/<library-key>/thumbs/v1/<aa>/<content-key>.<ext>`.
//! The `previews/<library-key>` folder comes from [`super::live::cache_dir_in`],
//! so a cache path inside the library is refused the same way.
//!
//! Keying (S-H2): the content key is SHA-256 over the recipe tag
//! [`RECIPE`] and the SHA-256 of the original bytes. Byte-identical files at
//! different paths share one key, and a renamed file keeps its key.
//!
//! Invalidation (S-H3): the index records path, size, mtime, SHA-256 and key.
//! A file whose size and mtime still match is not read again. A size or mtime
//! change re-hashes it; a changed hash gives a new key, and the old key is no
//! longer referenced, so it is never looked up. Removing the cache folder only
//! costs a rebuild. The index is written to a temp file and renamed over.
//!
//! Hash reuse: an M3 scan manifest line with the same path, size and mtime
//! (and no drift) supplies the SHA-256, so a warm run hashes nothing.
//!
//! Nothing here decodes HEIC/HEVC or writes a thumbnail (S-H4). The library is
//! only stat'ed and read.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The thumbnail recipe. Changing it changes every key.
pub const RECIPE: &str = "thumb-v1";
/// The index, one JSON object per line, sorted by path.
pub const INDEX_FILE: &str = "index-v1.jsonl";
/// Extension a future thumbnail would use. None is written today.
pub const THUMB_EXT: &str = "jpg";

/// Photos the browser cannot show that get an index entry.
pub fn is_heic(rel: &str) -> bool {
    rel.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .is_some_and(|e| e == "heic" || e == "heif")
}

/// `<cache>/thumbs/v1`.
pub fn thumbs_root(cache: &Path) -> PathBuf {
    cache.join("thumbs").join("v1")
}

/// Thumbnail root for `library` under `home`, or None when it would be inside
/// the library.
#[cfg(test)]
pub fn thumbs_dir_in(home: &Path, library: &Path) -> Option<PathBuf> {
    super::live::cache_dir_in(home, library).map(|c| thumbs_root(&c))
}

/// Thumbnail root for `library` under `$ALBUMEN_HOME` (or the platform data
/// folder), or None when it would be inside the library.
pub fn thumbs_dir(library: &Path) -> Option<PathBuf> {
    super::live::cache_dir(library).map(|c| thumbs_root(&c))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The content key for an original whose SHA-256 is `sha256_hex`.
pub fn content_key(sha256_hex: &str) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(RECIPE.as_bytes());
    h.update(b"\n");
    h.update(sha256_hex.as_bytes());
    hex(&h.finalize())
}

/// `<root>/<aa>/<key>.<ext>`.
pub fn thumb_path(root: &Path, key: &str) -> PathBuf {
    root.join(&key[..2]).join(format!("{key}.{THUMB_EXT}"))
}

/// One indexed original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub mtime_ns: String,
    pub sha256: String,
    pub key: String,
}

impl Entry {
    fn to_line(&self) -> String {
        serde_json::json!({
            "path": self.path, "bytes": self.bytes, "mtime_ns": self.mtime_ns,
            "sha256": self.sha256, "key": self.key,
        })
        .to_string()
    }

    fn from_line(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let sha256 = v["sha256"].as_str()?.to_string();
        Some(Entry {
            path: v["path"].as_str()?.to_string(),
            bytes: v["bytes"].as_u64()?,
            mtime_ns: v["mtime_ns"].as_str()?.to_string(),
            // The key is always derived again, so a hand-edited key is never used.
            key: content_key(&sha256),
            sha256,
        })
    }
}

/// What one [`refresh`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refresh {
    /// Originals read and hashed in this call.
    pub hashed: u64,
    /// Originals whose hash came from the previous index.
    pub from_index: u64,
    /// Originals whose hash came from the M3 scan manifest.
    pub from_scan: u64,
    /// Whether the index file was written.
    pub written: bool,
    /// Current entries, sorted by path.
    pub entries: Vec<Entry>,
}

impl Refresh {
    /// Distinct content keys referenced now.
    #[cfg(test)]
    pub fn keys(&self) -> HashSet<&str> {
        self.entries.iter().map(|e| e.key.as_str()).collect()
    }
}

fn stamp(path: &Path) -> Option<(u64, String)> {
    let m = std::fs::metadata(path).ok()?;
    let ns = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((m.len(), ns.to_string()))
}

/// Hashes from an M3 scan manifest for the wanted paths, by path, as
/// (bytes, mtime_ns, sha256). Lines that recorded drift are skipped. Read
/// line by line; only wanted paths are kept.
fn scan_hashes(scan: &Path, wanted: &HashSet<&str>) -> HashMap<String, (u64, String, String)> {
    let mut out = HashMap::new();
    let Ok(f) = std::fs::File::open(scan) else {
        return out;
    };
    for line in BufReader::new(f).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(path) = v["path"].as_str() else {
            continue;
        };
        if !wanted.contains(path) || !v["drift"].is_null() {
            continue;
        }
        if let (Some(b), Some(m), Some(s)) = (
            v["bytes"].as_u64(),
            v["mtime_ns"].as_str(),
            v["sha256"].as_str(),
        ) {
            out.insert(path.to_string(), (b, m.to_string(), s.to_string()));
        }
    }
    out
}

fn read_index(root: &Path) -> HashMap<String, Entry> {
    let mut out = HashMap::new();
    if let Ok(f) = std::fs::File::open(root.join(INDEX_FILE)) {
        for line in BufReader::new(f).lines() {
            let Ok(line) = line else { break };
            if let Some(e) = Entry::from_line(&line) {
                out.insert(e.path.clone(), e);
            }
        }
    }
    out
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `entries` to `<root>/INDEX_FILE` through a temp file and a rename.
pub fn write_index(root: &Path, entries: &[Entry]) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let tmp = root.join(format!(
        "{INDEX_FILE}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        for e in entries {
            writeln!(w, "{}", e.to_line())?;
        }
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    std::fs::rename(&tmp, root.join(INDEX_FILE))
}

/// Bring the index at `root` up to date for `rels` (library-relative paths).
///
/// The library is only stat'ed and, for new or changed files with no
/// reusable hash, read. `scan` is an M3 scan manifest whose hashes are reused
/// when path, size and mtime match.
pub fn refresh(library: &Path, rels: &[&str], root: &Path, scan: Option<&Path>) -> Refresh {
    let old = read_index(root);
    let wanted: HashSet<&str> = rels
        .iter()
        .copied()
        .filter(|r| !old.contains_key(*r))
        .collect();
    let mut from_scan_map = match scan {
        Some(s) if !wanted.is_empty() => scan_hashes(s, &wanted),
        _ => HashMap::new(),
    };
    let mut out = Refresh::default();
    let mut sorted: Vec<&str> = rels.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for rel in sorted {
        let path = library.join(rel);
        let Some((bytes, mtime_ns)) = stamp(&path) else {
            continue;
        };
        let sha256 = match old.get(rel) {
            Some(e) if e.bytes == bytes && e.mtime_ns == mtime_ns => {
                out.from_index += 1;
                e.sha256.clone()
            }
            _ => match from_scan_map.remove(rel) {
                Some((b, m, s)) if b == bytes && m == mtime_ns => {
                    out.from_scan += 1;
                    s
                }
                _ => match albumen_ingest::sha256_hex(&path) {
                    Ok(s) => {
                        out.hashed += 1;
                        s
                    }
                    Err(_) => continue,
                },
            },
        };
        out.entries.push(Entry {
            path: rel.to_string(),
            bytes,
            mtime_ns,
            key: content_key(&sha256),
            sha256,
        });
    }
    let unchanged = old.len() == out.entries.len()
        && out.entries.iter().all(|e| old.get(&e.path) == Some(e))
        && root.join(INDEX_FILE).is_file();
    if !unchanged {
        out.written = write_index(root, &out.entries).is_ok();
    }
    out
}

/// One page of the index, read line by line: entries `offset..offset+limit`
/// and the number of lines read. Only `limit` entries are held.
pub fn page(root: &Path, offset: usize, limit: usize) -> (Vec<Entry>, usize) {
    let mut out = Vec::with_capacity(limit);
    let mut read = 0;
    let Ok(f) = std::fs::File::open(root.join(INDEX_FILE)) else {
        return (out, 0);
    };
    for line in BufReader::new(f).lines() {
        if out.len() == limit {
            break;
        }
        let Ok(line) = line else { break };
        read += 1;
        if read > offset {
            if let Some(e) = Entry::from_line(&line) {
                out.push(e);
            }
        }
    }
    (out, read)
}

/// "cached" when a thumbnail file exists for the entry's current key, else
/// "none". An old key is never consulted.
pub fn thumb_state(root: &Path, entry: &Entry) -> &'static str {
    if thumb_path(root, &entry.key).is_file() {
        "cached"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn put(lib: &Path, rel: &str, bytes: &[u8]) {
        let p = lib.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        let home = dir.path().join("home");
        (dir, lib, home)
    }

    /// S-H1: layout under ALBUMEN_HOME, and a home inside the library is refused.
    #[test]
    fn sh1_layout_under_albumen_home_and_refused_inside_library() {
        let (_d, lib, home) = setup();
        put(&lib, "a.heic", b"synthetic heic A");
        let root = thumbs_dir_in(&home, &lib).unwrap();
        let canon_home = home.clone();
        assert!(root.starts_with(canon_home.join("previews")));
        assert!(root.ends_with(Path::new("thumbs").join("v1")));
        let r = refresh(&lib, &["a.heic"], &root, None);
        let key = &r.entries[0].key;
        let p = thumb_path(&root, key);
        assert_eq!(p.parent().unwrap().file_name().unwrap(), &key[..2]);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            format!("{key}.jpg")
        );
        assert!(root.join(INDEX_FILE).is_file());
        // Through the ALBUMEN_HOME variable, as the GUI resolves it.
        let saved = std::env::var_os("ALBUMEN_HOME");
        std::env::set_var("ALBUMEN_HOME", &home);
        let via_env = thumbs_dir(&lib).unwrap();
        assert_eq!(via_env, root);
        // ALBUMEN_HOME inside the library: refused.
        std::env::set_var("ALBUMEN_HOME", lib.join(".albumen"));
        assert!(thumbs_dir(&lib).is_none());
        match saved {
            Some(v) => std::env::set_var("ALBUMEN_HOME", v),
            None => std::env::remove_var("ALBUMEN_HOME"),
        }
        assert!(thumbs_dir_in(&lib.join(".albumen"), &lib).is_none());
        assert!(thumbs_dir_in(&lib, &lib).is_none());
    }

    /// S-H2: identical bytes at two paths share one key; a rename keeps it.
    #[test]
    fn sh2_identical_bytes_share_a_key_and_rename_keeps_it() {
        let (d, lib, home) = setup();
        put(&lib, "a/IMG_1.heic", b"same synthetic bytes");
        put(&lib, "b/IMG_2.HEIC", b"same synthetic bytes");
        put(&lib, "c/IMG_3.heif", b"other synthetic bytes");
        let root = thumbs_dir_in(&home, &lib).unwrap();
        let r = refresh(
            &lib,
            &["a/IMG_1.heic", "b/IMG_2.HEIC", "c/IMG_3.heif"],
            &root,
            None,
        );
        assert_eq!(r.entries.len(), 3);
        assert_eq!(r.keys().len(), 2, "two identical files, one key");
        assert_eq!(r.entries[0].key, r.entries[1].key);
        let key = r.entries[2].key.clone();
        // Rename (test fixture only, inside the temp library).
        std::fs::rename(lib.join("c/IMG_3.heif"), lib.join("c/renamed.heif")).unwrap();
        let r2 = refresh(
            &lib,
            &["a/IMG_1.heic", "b/IMG_2.HEIC", "c/renamed.heif"],
            &root,
            None,
        );
        let e = r2
            .entries
            .iter()
            .find(|e| e.path == "c/renamed.heif")
            .unwrap();
        assert_eq!(e.key, key, "renamed file keeps its key");
        assert!(!r2.entries.iter().any(|e| e.path == "c/IMG_3.heif"));
        drop(d);
    }

    /// S-H3: mtime-only touch keeps the key; a byte change gives a new key and
    /// the old one is unreferenced; removing the cache rebuilds without error.
    #[test]
    fn sh3_invalidation_touch_change_and_rebuild() {
        let (d, lib, home) = setup();
        put(&lib, "x.heic", b"version one");
        let root = thumbs_dir_in(&home, &lib).unwrap();
        let first = refresh(&lib, &["x.heic"], &root, None);
        assert_eq!(first.hashed, 1);
        let old_key = first.entries[0].key.clone();
        // Pretend a thumbnail exists for the old key (cache side only).
        let tp = thumb_path(&root, &old_key);
        std::fs::create_dir_all(tp.parent().unwrap()).unwrap();
        std::fs::write(&tp, b"not a real thumbnail").unwrap();
        assert_eq!(thumb_state(&root, &first.entries[0]), "cached");

        // mtime-only touch: re-hashed, same hash, same key.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(lib.join("x.heic"))
            .unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        let touched = refresh(&lib, &["x.heic"], &root, None);
        assert_eq!(touched.hashed, 1, "mtime change re-hashes");
        assert_eq!(touched.entries[0].key, old_key, "same bytes, entry kept");
        assert_ne!(touched.entries[0].mtime_ns, first.entries[0].mtime_ns);

        // Byte change: new key; the old key is no longer referenced or served.
        put(&lib, "x.heic", b"version two, longer");
        let changed = refresh(&lib, &["x.heic"], &root, None);
        assert_eq!(changed.hashed, 1);
        let new_key = changed.entries[0].key.clone();
        assert_ne!(new_key, old_key);
        assert!(!changed.keys().contains(old_key.as_str()));
        assert_eq!(thumb_state(&root, &changed.entries[0]), "none");
        let (pg, _) = page(&root, 0, 10);
        assert!(pg.iter().all(|e| e.key != old_key));

        // Cache removed (moved away, never deleted): rebuild, no error.
        let cache = root.parent().unwrap().parent().unwrap().to_path_buf();
        std::fs::rename(&cache, d.path().join("moved-away")).unwrap();
        let rebuilt = refresh(&lib, &["x.heic"], &root, None);
        assert!(rebuilt.written);
        assert_eq!(rebuilt.hashed, 1);
        assert_eq!(rebuilt.entries[0].key, new_key);
        assert!(root.join(INDEX_FILE).is_file());
        // No temp file is left behind by the rename.
        let leftovers = std::fs::read_dir(&root)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    /// Hash reuse: hashes come from the M3 scan manifest, and a warm run
    /// hashes nothing.
    #[test]
    fn warm_refresh_reuses_scan_and_index_hashes_with_zero_rehash() {
        let (d, lib, home) = setup();
        put(&lib, "p/1.heic", b"one");
        put(&lib, "p/2.heic", b"two");
        let root = thumbs_dir_in(&home, &lib).unwrap();
        // A scan manifest line in the M3 shape for each file.
        let manifest = d.path().join("state").join("scan-1.jsonl");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut m = std::fs::File::create(&manifest).unwrap();
        for rel in ["p/1.heic", "p/2.heic"] {
            let (b, t) = stamp(&lib.join(rel)).unwrap();
            let sha = albumen_ingest::sha256_hex(&lib.join(rel)).unwrap();
            writeln!(
                m,
                "{}",
                serde_json::json!({"path": rel, "bytes": b, "mtime_ns": t, "sha256": sha,
                    "sidecars": [], "pairs": [], "metadata": {}})
            )
            .unwrap();
        }
        drop(m);
        let rels = ["p/1.heic", "p/2.heic"];
        let cold = refresh(&lib, &rels, &root, Some(&manifest));
        assert_eq!((cold.hashed, cold.from_scan), (0, 2), "scan hashes reused");
        let warm = refresh(&lib, &rels, &root, Some(&manifest));
        assert_eq!((warm.hashed, warm.from_index, warm.written), (0, 2, false));
        // A stale scan line (different size) is not trusted.
        put(&lib, "p/2.heic", b"two, changed");
        std::fs::rename(&root, d.path().join("old-root")).unwrap();
        let after = refresh(&lib, &rels, &root, Some(&manifest));
        assert_eq!((after.hashed, after.from_scan), (1, 1));
    }

    /// S-H6: 10,000 synthetic entries, no cap; a page reads only what it
    /// returns, and the first page is inside the 300 ms first-thumbnail budget.
    #[test]
    fn sh6_ten_thousand_entries_paginate_flat() {
        let (_d, lib, home) = setup();
        let n = 10_000;
        let rels: Vec<String> = (0..n)
            .map(|i| format!("s/{:03}/IMG_{i:05}.heic", i / 100))
            .collect();
        for (i, rel) in rels.iter().enumerate() {
            put(&lib, rel, format!("synthetic {i}").as_bytes());
        }
        let refs: Vec<&str> = rels.iter().map(String::as_str).collect();
        let root = thumbs_dir_in(&home, &lib).unwrap();
        let cold = refresh(&lib, &refs, &root, None);
        assert_eq!(cold.entries.len(), n, "no cap");
        assert_eq!(cold.hashed as usize, n);
        let t = std::time::Instant::now();
        let warm = refresh(&lib, &refs, &root, None);
        let warm_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(warm.hashed, 0, "warm refresh re-hashes nothing");
        let t = std::time::Instant::now();
        let (first, read) = page(&root, 0, 100);
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            (first.len(), read),
            (100, 100),
            "first page reads 100 lines"
        );
        let (last, read_last) = page(&root, n - 100, 100);
        assert_eq!(last.len(), 100);
        assert_eq!(read_last, n, "held entries stay at the page size");
        assert_eq!(last.last().unwrap().path, rels[n - 1]);
        let (past, _) = page(&root, n, 100);
        assert!(past.is_empty());
        println!("sh6 entries={n} warm_refresh_ms={warm_ms:.1} first_page_ms={first_ms:.3}");
        assert!(
            first_ms <= 300.0,
            "first page {first_ms} ms > 300 ms budget"
        );
    }

    #[test]
    fn key_is_recipe_tagged_sha256() {
        let k = content_key(&"0".repeat(64));
        assert_eq!(k.len(), 64);
        assert_ne!(k, "0".repeat(64));
        assert!(is_heic("a/B.HEIC") && is_heic("c.heif") && !is_heic("d.jpg"));
    }
}
