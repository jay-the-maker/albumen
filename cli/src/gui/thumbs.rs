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
    /// Entries in the index now.
    pub entries: u64,
    /// Paths whose current key has a thumbnail file (none today).
    pub cached: HashSet<String>,
    /// Most index or scan entries held in memory at once. The previous
    /// index is streamed one entry at a time; only scan hashes for files
    /// missing from it are held.
    pub peak_held: usize,
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

/// The previous index, streamed in path order: one entry held at a time.
struct OldIndex {
    lines: Option<std::io::Lines<BufReader<std::fs::File>>>,
    cur: Option<Entry>,
    /// An entry was passed over because no wanted path matched it.
    skipped: bool,
    exists: bool,
    /// `cur` was returned by `find`.
    matched: bool,
}

impl OldIndex {
    fn open(root: &Path) -> Self {
        let f = std::fs::File::open(root.join(INDEX_FILE)).ok();
        let exists = f.is_some();
        let mut s = OldIndex {
            lines: f.map(|f| BufReader::new(f).lines()),
            cur: None,
            skipped: false,
            exists,
            matched: false,
        };
        s.advance();
        s
    }

    fn advance(&mut self) {
        self.cur = None;
        self.matched = false;
        if let Some(lines) = self.lines.as_mut() {
            for line in lines.by_ref() {
                let Ok(line) = line else { break };
                if let Some(e) = Entry::from_line(&line) {
                    self.cur = Some(e);
                    return;
                }
            }
        }
    }

    /// The entry for `rel`, if the index has one. `rel` must be asked for
    /// in sorted order.
    fn find(&mut self, rel: &str) -> Option<&Entry> {
        while self.cur.as_ref().is_some_and(|e| e.path.as_str() < rel) {
            self.skipped |= !self.matched;
            self.advance();
        }
        self.matched = self.cur.as_ref().is_some_and(|e| e.path == rel);
        self.cur.as_ref().filter(|e| e.path == rel)
    }

    /// Whether entries remain after the last wanted path.
    fn has_rest(&mut self) -> bool {
        if self.matched {
            self.advance();
        }
        self.cur.is_some()
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Bring the index at `root` up to date for `rels` (library-relative paths).
///
/// The library is only stat'ed and, for new or changed files with no
/// reusable hash, read. `scan` is an M3 scan manifest whose hashes are reused
/// when path, size and mtime match. The previous index is merged as a stream
/// against the sorted paths, so memory does not grow with the index: a warm
/// run makes one stat pass and writes nothing; otherwise the new index is
/// streamed to a temp file and renamed over.
pub fn refresh(library: &Path, rels: &[&str], root: &Path, scan: Option<&Path>) -> Refresh {
    let mut sorted: Vec<&str> = rels.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Refresh {
        peak_held: 1,
        ..Refresh::default()
    };
    // Pass 1: is everything unchanged? Stat only, nothing read or written.
    let mut old = OldIndex::open(root);
    let mut unchanged = old.exists;
    for rel in &sorted {
        if !unchanged {
            break;
        }
        let st = stamp(&library.join(rel));
        match (old.find(rel), st) {
            (Some(e), Some((b, m))) if e.bytes == b && e.mtime_ns == m => {
                out.from_index += 1;
                if thumb_state(root, e) == "cached" {
                    out.cached.insert(e.path.clone());
                }
            }
            (None, None) => {}
            _ => unchanged = false,
        }
    }
    if unchanged && !old.skipped && !old.has_rest() {
        out.entries = out.from_index;
        return out;
    }
    // Pass 2: merge and stream the new index to a temp file.
    out = Refresh {
        peak_held: 1,
        ..Refresh::default()
    };
    let mut old = OldIndex::open(root);
    let mut scan_map: Option<HashMap<String, (u64, String, String)>> = None;
    let tmp = root.join(format!(
        "{INDEX_FILE}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut w = match std::fs::create_dir_all(root).and_then(|_| std::fs::File::create(&tmp)) {
        Ok(f) => Some(std::io::BufWriter::new(f)),
        Err(_) => None,
    };
    let mut ok = w.is_some();
    for rel in &sorted {
        let path = library.join(rel);
        let Some((bytes, mtime_ns)) = stamp(&path) else {
            continue;
        };
        let sha256 = match old.find(rel) {
            Some(e) if e.bytes == bytes && e.mtime_ns == mtime_ns => {
                out.from_index += 1;
                e.sha256.clone()
            }
            _ => {
                let map = scan_map.get_or_insert_with(|| match scan {
                    Some(s) => scan_hashes(s, &sorted.iter().copied().collect()),
                    None => HashMap::new(),
                });
                out.peak_held = out.peak_held.max(1 + map.len());
                match map.remove(*rel) {
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
                }
            }
        };
        let e = Entry {
            path: rel.to_string(),
            bytes,
            mtime_ns,
            key: content_key(&sha256),
            sha256,
        };
        out.entries += 1;
        if thumb_state(root, &e) == "cached" {
            out.cached.insert(e.path.clone());
        }
        if let Some(w) = w.as_mut() {
            ok &= writeln!(w, "{}", e.to_line()).is_ok();
        }
    }
    if let Some(mut w) = w {
        ok &= w.flush().is_ok() && w.get_ref().sync_all().is_ok();
        drop(w);
        out.written = ok && std::fs::rename(&tmp, root.join(INDEX_FILE)).is_ok();
    }
    out
}

/// One page of the index, read line by line: entries `offset..offset+limit`
/// and the number of lines read. Only `limit` entries are held.
pub fn page(root: &Path, offset: usize, limit: usize) -> (Vec<Entry>, usize) {
    let mut out = Vec::with_capacity(limit.min(1024));
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

    /// Every entry in the index at `root` (tests only).
    fn all(root: &Path) -> Vec<Entry> {
        page(root, 0, usize::MAX).0
    }

    fn keys(entries: &[Entry]) -> HashSet<&str> {
        entries.iter().map(|e| e.key.as_str()).collect()
    }

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
        refresh(&lib, &["a.heic"], &root, None);
        let entries = all(&root);
        let key = &entries[0].key;
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
        let entries = all(&root);
        assert_eq!((r.entries, entries.len()), (3, 3));
        assert_eq!(keys(&entries).len(), 2, "two identical files, one key");
        assert_eq!(entries[0].key, entries[1].key);
        let key = entries[2].key.clone();
        // Rename (test fixture only, inside the temp library).
        std::fs::rename(lib.join("c/IMG_3.heif"), lib.join("c/renamed.heif")).unwrap();
        refresh(
            &lib,
            &["a/IMG_1.heic", "b/IMG_2.HEIC", "c/renamed.heif"],
            &root,
            None,
        );
        let r2 = all(&root);
        let e = r2.iter().find(|e| e.path == "c/renamed.heif").unwrap();
        assert_eq!(e.key, key, "renamed file keeps its key");
        assert!(!r2.iter().any(|e| e.path == "c/IMG_3.heif"));
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
        let first_e = all(&root).remove(0);
        let old_key = first_e.key.clone();
        // Pretend a thumbnail exists for the old key (cache side only).
        let tp = thumb_path(&root, &old_key);
        std::fs::create_dir_all(tp.parent().unwrap()).unwrap();
        std::fs::write(&tp, b"not a real thumbnail").unwrap();
        assert_eq!(thumb_state(&root, &first_e), "cached");
        // A warm refresh reports it cached.
        let again = refresh(&lib, &["x.heic"], &root, None);
        assert!(again.cached.contains("x.heic") && !again.written);

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
        let touched_e = all(&root).remove(0);
        assert_eq!(touched_e.key, old_key, "same bytes, entry kept");
        assert_ne!(touched_e.mtime_ns, first_e.mtime_ns);

        // Byte change: new key; the old key is no longer referenced or served.
        put(&lib, "x.heic", b"version two, longer");
        let changed = refresh(&lib, &["x.heic"], &root, None);
        assert_eq!(changed.hashed, 1);
        assert!(changed.cached.is_empty());
        let changed_e = all(&root);
        let new_key = changed_e[0].key.clone();
        assert_ne!(new_key, old_key);
        assert!(!keys(&changed_e).contains(old_key.as_str()));
        assert_eq!(thumb_state(&root, &changed_e[0]), "none");
        let (pg, _) = page(&root, 0, 10);
        assert!(pg.iter().all(|e| e.key != old_key));

        // Cache removed (moved away, never deleted): rebuild, no error.
        let cache = root.parent().unwrap().parent().unwrap().to_path_buf();
        std::fs::rename(&cache, d.path().join("moved-away")).unwrap();
        let rebuilt = refresh(&lib, &["x.heic"], &root, None);
        assert!(rebuilt.written);
        assert_eq!(rebuilt.hashed, 1);
        assert_eq!(all(&root)[0].key, new_key);
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

    /// One S-H6 run at `n` entries: (peak held cold, peak held warm).
    fn sh6_run(n: usize) -> (usize, usize) {
        let (_d, lib, home) = setup();
        let rels: Vec<String> = (0..n)
            .map(|i| format!("s/{:04}/IMG_{i:06}.heic", i / 100))
            .collect();
        for (i, rel) in rels.iter().enumerate() {
            put(&lib, rel, format!("synthetic {i}").as_bytes());
        }
        let refs: Vec<&str> = rels.iter().map(String::as_str).collect();
        let root = thumbs_dir_in(&home, &lib).unwrap();
        let cold = refresh(&lib, &refs, &root, None);
        assert_eq!(cold.entries as usize, n, "no cap");
        assert_eq!(cold.hashed as usize, n);
        let t = std::time::Instant::now();
        let warm = refresh(&lib, &refs, &root, None);
        let warm_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(warm.hashed, 0, "warm refresh re-hashes nothing");
        assert_eq!((warm.from_index as usize, warm.written), (n, false));
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
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        println!(
            "sh6 profile={profile} entries={n} warm_refresh_ms={warm_ms:.1} first_page_ms={first_ms:.3} peak_held_cold={} peak_held_warm={}",
            cold.peak_held, warm.peak_held
        );
        assert!(
            first_ms <= 300.0,
            "first page {first_ms} ms > 300 ms budget"
        );
        (cold.peak_held, warm.peak_held)
    }

    /// S-H6: 10,000 and 100,000 synthetic entries, no cap. A page reads only
    /// what it returns, the first page is inside the 300 ms first-thumbnail
    /// budget, and the entries held in memory by a refresh stay flat.
    #[test]
    fn sh6_ten_thousand_entries_paginate_flat() {
        let small = sh6_run(10_000);
        let large = sh6_run(100_000);
        assert_eq!(small, large, "entries held stay flat from 10k to 100k");
        assert_eq!(large, (1, 1));
    }

    /// One size per test, so each can run alone in its own process and the
    /// bench can read that process's peak memory (sh6 per size).
    #[test]
    fn sh6_size_10k() {
        assert_eq!(sh6_run(10_000), (1, 1));
    }

    #[test]
    fn sh6_size_100k() {
        assert_eq!(sh6_run(100_000), (1, 1));
    }

    #[test]
    fn key_is_recipe_tagged_sha256() {
        let k = content_key(&"0".repeat(64));
        assert_eq!(k.len(), 64);
        assert_ne!(k, "0".repeat(64));
        assert!(is_heic("a/B.HEIC") && is_heic("c.heif") && !is_heic("d.jpg"));
    }
}
