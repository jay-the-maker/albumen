// SPDX-License-Identifier: Apache-2.0

//! Metadata read from the files in front of us.
//!
//! The cache remembers a file only while its stat key and SHA-256 still
//! match, and a search that misses re-reads that file with ExifTool. Sidecar
//! metadata is joined in memory. Search persists the cache as a checksummed
//! search tree beside the index (see [`crate::search_tree`]); nothing else
//! reads it.

use albumen_ingest::{exiftool_command, sha256_hex};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Capture time as written in the file, without converting time zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WallTime {
    pub(crate) year: i32,
    pub(crate) month: u8,
    pub(crate) day: u8,
    pub(crate) hour: u8,
    pub(crate) minute: u8,
    pub(crate) second: u8,
    pub(crate) nanos: u32,
}

impl WallTime {
    pub fn to_text(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Inclusive GPS bounds. When `west` is greater than `east`, the box crosses
/// the antimeridian.
#[derive(Debug, Clone, Copy)]
pub struct GeoBox {
    pub south: f64,
    pub north: f64,
    pub west: f64,
    pub east: f64,
}

impl GeoBox {
    pub fn contains(self, latitude: f64, longitude: f64) -> bool {
        if !latitude.is_finite() || !longitude.is_finite() {
            return false;
        }
        if latitude < self.south || latitude > self.north {
            return false;
        }
        if self.west <= self.east {
            longitude >= self.west && longitude <= self.east
        } else {
            longitude >= self.west || longitude <= self.east
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub captured: Option<WallTime>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub keywords: Vec<String>,
    pub captions: Vec<String>,
}

impl Record {
    fn empty() -> Self {
        Self {
            captured: None,
            camera: None,
            lens: None,
            latitude: None,
            longitude: None,
            keywords: Vec::new(),
            captions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub captured_after: Option<WallTime>,
    pub captured_before: Option<WallTime>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub keyword: Option<String>,
    pub caption: Option<String>,
    pub gps: Option<GeoBox>,
}

impl Filters {
    pub fn matches(&self, record: &Record) -> bool {
        if let Some(after) = self.captured_after {
            match record.captured {
                Some(captured) if captured >= after => {}
                _ => return false,
            }
        }
        if let Some(before) = self.captured_before {
            match record.captured {
                Some(captured) if captured <= before => {}
                _ => return false,
            }
        }
        if let Some(camera) = &self.camera {
            if !contains_ignore_case(record.camera.as_deref(), camera) {
                return false;
            }
        }
        if let Some(lens) = &self.lens {
            if !contains_ignore_case(record.lens.as_deref(), lens) {
                return false;
            }
        }
        if let Some(keyword) = &self.keyword {
            if !record
                .keywords
                .iter()
                .any(|item| contains_ignore_case(Some(item), keyword))
            {
                return false;
            }
        }
        if let Some(caption) = &self.caption {
            if !record
                .captions
                .iter()
                .any(|item| contains_ignore_case(Some(item), caption))
            {
                return false;
            }
        }
        if let Some(gps) = self.gps {
            match (record.latitude, record.longitude) {
                (Some(latitude), Some(longitude)) if gps.contains(latitude, longitude) => {}
                _ => return false,
            }
        }
        true
    }
}

/// The five stat fields the warm search trusts instead of a re-hash.
///
/// Search only. Export, verify-manifest, backup and status never read this:
/// they SHA-256 every file every time (see `file_hash_count` tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatKey {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
}

impl StatKey {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            StatKey {
                device: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
                ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
            }
        }
        #[cfg(not(unix))]
        {
            // No portable device/inode or ctime here: use mtime for both times
            // and the creation time as a stand-in identity.
            let ns = |t: std::io::Result<std::time::SystemTime>| -> i128 {
                t.ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i128)
                    .unwrap_or(-1)
            };
            StatKey {
                device: 0,
                inode: ns(meta.created()) as u64,
                size: meta.len(),
                mtime_ns: ns(meta.modified()),
                ctime_ns: ns(meta.modified()),
            }
        }
    }

    /// A hash taken within `RACY_NANOS` of the file's last change cannot rule
    /// out a same-second write after it, so it is not trusted next time.
    pub(crate) fn settled_at(&self, hashed_ns: i128, racy_ns: i128) -> bool {
        self.mtime_ns >= 0 && hashed_ns - self.mtime_ns.max(self.ctime_ns) >= racy_ns
    }
}

pub(crate) fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// One directory's `.xmp`/`.aae` names, by lowercase name. Re-listed when
/// the directory's mtime moves or the listing was taken inside the racy window.
#[derive(Debug)]
struct DirSidecars {
    mtime_ns: i128,
    settled: bool,
    names: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) sha: String,
    pub(crate) stat: StatKey,
    /// True when the hash was taken at least `RACY_NANOS` after the change.
    pub(crate) settled: bool,
    pub(crate) record: Record,
    /// When this entry's stat key was last compared with the disk (ns).
    pub(crate) checked_ns: i128,
}

/// A cached file whose stat was not compared with the disk this search
/// because the recheck budget ran out.
#[derive(Debug, Clone)]
pub struct Unchecked {
    pub path: String,
    pub checked_ns: i128,
}

/// Memory of ExifTool reads, keyed by SHA-256 and the file's stat.
///
/// Warm search: when all five [`StatKey`] fields are unchanged since a
/// settled hash, the file is not read again. Any change, or a hash taken too
/// close to the last write, means a fresh SHA-256. Dropping it drops the only copy.
#[derive(Debug)]
pub struct Cache {
    pub(crate) entries: HashMap<String, Entry>,
    dirs: HashMap<String, DirSidecars>,
    racy_ns: i128,
    hashed: u64,
    /// Entries changed since the search tree was last written.
    pub(crate) dirty: bool,
    /// The search tree file this cache was loaded from, once tried.
    pub(crate) tree: Option<std::path::PathBuf>,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            entries: HashMap::new(),
            dirs: HashMap::new(),
            racy_ns: i128::from(albumen_ingest::index::RACY_NANOS),
            hashed: 0,
            dirty: false,
            tree: None,
        }
    }
}

impl Cache {
    /// Files this cache has SHA-256'd since it was made.
    pub fn hashed(&self) -> u64 {
        self.hashed
    }

    /// [`existing_sidecars`], but each directory is listed once and reused
    /// while its mtime is unchanged, instead of one listing per file.
    pub fn sidecars(&mut self, root: &Path, relative: &str) -> Vec<String> {
        if is_sidecar(relative) {
            return Vec::new();
        }
        let (parent, _) = split_relative(relative);
        self.refresh_dir(root, parent);
        self.listed_sidecars(root, relative)
    }

    /// Re-list `parent` when its mtime moved or its listing was taken inside
    /// the racy window. Call once per directory per batch, then
    /// [`Cache::listed_sidecars`] for each file in it.
    pub fn refresh_dir(&mut self, root: &Path, parent: &str) -> bool {
        let dir = if parent.is_empty() {
            root.to_path_buf()
        } else {
            root.join(PathBuf::from_iter(parent.split('/')))
        };
        let Ok(meta) = std::fs::metadata(&dir) else {
            self.dirs.remove(parent);
            return false;
        };
        let mtime_ns = StatKey::of(&meta).mtime_ns;
        let fresh = match self.dirs.get(parent) {
            Some(cached) => !(cached.settled && cached.mtime_ns == mtime_ns),
            None => true,
        };
        if fresh {
            let listed_ns = now_ns();
            let mut names: HashMap<String, Vec<String>> = HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let Ok(actual) = entry.file_name().into_string() else {
                        continue;
                    };
                    let lower = actual.to_ascii_lowercase();
                    if lower.ends_with(".xmp") || lower.ends_with(".aae") {
                        names.entry(lower).or_default().push(actual);
                    }
                }
            }
            for list in names.values_mut() {
                list.sort();
            }
            self.dirs.insert(
                parent.to_string(),
                DirSidecars {
                    mtime_ns,
                    settled: mtime_ns >= 0 && listed_ns - mtime_ns >= self.racy_ns,
                    names,
                },
            );
        }
        self.dirs
            .get(parent)
            .is_some_and(|dir| !dir.names.is_empty())
    }

    /// Sidecars of `relative` from the listing [`Cache::refresh_dir`] took.
    pub fn listed_sidecars(&self, root: &Path, relative: &str) -> Vec<String> {
        if is_sidecar(relative) {
            return Vec::new();
        }
        let (parent, name) = split_relative(relative);
        let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
        let Some(listing) = self.dirs.get(parent) else {
            return Vec::new();
        };
        if listing.names.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        for candidate in [
            format!("{stem}.xmp"),
            format!("{name}.xmp"),
            format!("{stem}.aae"),
            format!("{name}.aae"),
        ] {
            let Some(actuals) = listing.names.get(&candidate.to_ascii_lowercase()) else {
                continue;
            };
            // Exact case first, then other spellings, as existing_sidecars does.
            let ordered = actuals
                .iter()
                .filter(|actual| **actual == candidate)
                .chain(actuals.iter().filter(|actual| **actual != candidate));
            for actual in ordered {
                let relative_sidecar = join_relative(parent, actual);
                if found.contains(&relative_sidecar) {
                    continue;
                }
                if root
                    .join(PathBuf::from_iter(relative_sidecar.split('/')))
                    .is_file()
                {
                    found.push(relative_sidecar);
                }
            }
        }
        found
    }

    #[cfg(test)]
    fn with_racy_ns(racy_ns: i128) -> Self {
        Cache {
            racy_ns,
            ..Cache::default()
        }
    }

    pub fn load(
        &mut self,
        root: &Path,
        relative_paths: &[String],
    ) -> Result<HashMap<String, Record>, String> {
        let mut digests = self.refresh(root, relative_paths)?;
        for relative in relative_paths {
            if !digests.contains_key(relative) {
                if let Some(entry) = self.entries.get(relative) {
                    digests.insert(relative.clone(), entry.record.clone());
                }
            }
        }
        Ok(digests)
    }

    /// Bring every path's entry up to date, as [`Cache::load`] does, without
    /// copying records. Records the cache did not keep (over the item limit)
    /// are returned; read the rest with [`Cache::record`].
    pub fn refresh(
        &mut self,
        root: &Path,
        relative_paths: &[String],
    ) -> Result<HashMap<String, Record>, String> {
        let stats = stat_all(root, relative_paths, None);
        let mut unkept = HashMap::new();
        self.refresh_stated(
            root,
            relative_paths,
            stats,
            &mut unkept,
            &mut Vec::new(),
            &|_, _| None,
            &mut Vec::new(),
        )?;
        Ok(unkept)
    }

    /// [`Cache::refresh`] with each path's stat already taken by
    /// [`stat_all`]. A `None` stat (budget ran out) keeps a cached entry as
    /// it is and reports it in `unchecked`; a path with no entry is stat'd
    /// here. Returns each path's length.
    ///
    /// `judge(i, record)` runs on each entry that needed no rehash, while
    /// it is in hand, and its answer goes in `verdicts[i]` (`None` for the
    /// rest), so a caller filtering records need not look them up again.
    #[allow(clippy::too_many_arguments)]
    pub fn refresh_stated<S: AsRef<str>>(
        &mut self,
        root: &Path,
        relative_paths: &[S],
        stats: Vec<Option<Result<StatKey, String>>>,
        unkept: &mut HashMap<String, Record>,
        unchecked: &mut Vec<Unchecked>,
        judge: &dyn Fn(usize, &Record) -> Option<bool>,
        verdicts: &mut Vec<Option<bool>>,
    ) -> Result<Vec<u64>, String> {
        let mut lengths = Vec::with_capacity(relative_paths.len());
        verdicts.clear();
        verdicts.reserve(relative_paths.len());
        let mut missing = Vec::new();
        let checked_ns = now_ns();
        for (i, (relative, stat)) in relative_paths.iter().zip(stats).enumerate() {
            let relative = relative.as_ref();
            let stat = match stat {
                Some(stat) => stat?,
                None => {
                    if let Some(entry) = self.entries.get(relative) {
                        lengths.push(entry.stat.size);
                        verdicts.push(judge(i, &entry.record));
                        unchecked.push(Unchecked {
                            path: relative.to_string(),
                            checked_ns: entry.checked_ns,
                        });
                        continue;
                    }
                    stat_one(root, relative)?
                }
            };
            lengths.push(stat.size);
            if let Some(entry) = self.entries.get_mut(relative) {
                if entry.settled && entry.stat == stat {
                    entry.checked_ns = checked_ns;
                    verdicts.push(judge(i, &entry.record));
                    continue;
                }
            }
            verdicts.push(None);
            let full = root.join(PathBuf::from_iter(relative.split('/')));
            let hashed_ns = now_ns();
            self.hashed += 1;
            let sha = sha256_hex(&full).map_err(|err| err.to_string())?;
            let settled = stat.settled_at(hashed_ns, self.racy_ns);
            if let Some(entry) = self.entries.get_mut(relative) {
                if entry.sha == sha && entry.stat.size == stat.size {
                    entry.stat = stat;
                    entry.settled = settled;
                    entry.checked_ns = hashed_ns;
                    self.dirty = true;
                    continue;
                }
            }
            missing.push((relative.to_string(), sha, stat, settled, hashed_ns));
        }
        if !missing.is_empty() {
            let fresh = read_exiftool(
                root,
                &missing
                    .iter()
                    .map(|(path, ..)| path.clone())
                    .collect::<Vec<_>>(),
            )?;
            for (relative, sha, stat, settled, hashed_ns) in missing {
                let record = fresh.get(&relative).cloned().unwrap_or_else(Record::empty);
                if !self.remember(
                    relative.clone(),
                    Entry {
                        sha,
                        stat,
                        settled,
                        record: record.clone(),
                        checked_ns: hashed_ns,
                    },
                ) {
                    unkept.insert(relative, record);
                }
            }
        }
        Ok(lengths)
    }

    /// The record for `relative`: from `unkept` (what [`Cache::refresh`]
    /// returned) or from the cache.
    pub fn record<'a>(
        &'a self,
        unkept: &'a HashMap<String, Record>,
        relative: &str,
    ) -> Option<&'a Record> {
        unkept
            .get(relative)
            .or_else(|| self.entries.get(relative).map(|entry| &entry.record))
    }

    /// Use the search tree at `file` for this cache: load it the first time
    /// (whole or not at all), and remember it so [`Cache::save_tree`] can
    /// rewrite it. Returns what happened, for diagnostics.
    pub fn open_tree(&mut self, file: PathBuf, root_id: &str) -> String {
        if self.tree.as_ref() == Some(&file) {
            return "in memory".to_string();
        }
        self.tree = Some(file.clone());
        match crate::search_tree::load(&file, root_id) {
            Ok(entries) if self.entries.is_empty() && cache_item_limit().is_none() => {
                // Nothing to merge and no limit: take the loaded map as is
                // rather than re-inserting it, which held two full maps at once.
                self.entries = entries;
                "loaded".to_string()
            }
            Ok(entries) => {
                let limit = cache_item_limit().unwrap_or(usize::MAX);
                for (path, entry) in entries {
                    if self.entries.len() >= limit {
                        break;
                    }
                    self.entries.entry(path).or_insert(entry);
                }
                "loaded".to_string()
            }
            Err(why) => {
                self.dirty = true;
                format!("discarded: {why}")
            }
        }
    }

    /// Write the search tree if anything changed, keeping only paths
    /// `exists` still knows.
    pub fn save_tree(
        &mut self,
        root_id: &str,
        exists: impl Fn(&str) -> bool,
    ) -> std::io::Result<()> {
        let Some(file) = self.tree.clone() else {
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }
        self.entries.retain(|path, _| exists(path));
        crate::search_tree::save(&file, root_id, self.entries.iter())?;
        self.dirty = false;
        Ok(())
    }

    /// Keep `entry`. False when the item limit left it out.
    fn remember(&mut self, relative: String, entry: Entry) -> bool {
        let replacing = self.entries.contains_key(&relative);
        if let Some(limit) = cache_item_limit() {
            if !replacing && self.entries.len() >= limit {
                return false;
            }
        }
        self.entries.insert(relative, entry);
        self.dirty = true;
        true
    }
}

fn stat_one(root: &Path, relative: &str) -> Result<StatKey, String> {
    // Index paths always use '/', which is also the separator here.
    #[cfg(unix)]
    let full = root.join(relative);
    #[cfg(not(unix))]
    let full = root.join(PathBuf::from_iter(relative.split('/')));
    std::fs::metadata(&full)
        .map(|meta| StatKey::of(&meta))
        .map_err(|err| format!("{relative}: {err}"))
}

/// Stat every path on a small thread pool, in order. A path not reached
/// before `deadline` is `None`.
pub fn stat_all<S: AsRef<str> + Sync>(
    root: &Path,
    relative_paths: &[S],
    deadline: Option<std::time::Instant>,
) -> Vec<Option<Result<StatKey, String>>> {
    let mut out: Vec<Option<Result<StatKey, String>>> = Vec::new();
    out.resize_with(relative_paths.len(), || None);
    let run = |paths: &[S], slots: &mut [Option<Result<StatKey, String>>]| {
        for (relative, slot) in paths.iter().zip(slots.iter_mut()) {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return;
            }
            *slot = Some(stat_one(root, relative.as_ref()));
        }
    };
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 8);
    if cfg!(windows) && std::env::var_os("ALBUMEN_STAT_PER_FILE").is_none() {
        return stat_all_by_dir(root, relative_paths, deadline, threads);
    }
    if threads == 1 || relative_paths.len() < 512 {
        run(relative_paths, &mut out);
        return out;
    }
    let chunk = relative_paths.len().div_ceil(threads);
    std::thread::scope(|scope| {
        for (paths, slots) in relative_paths.chunks(chunk).zip(out.chunks_mut(chunk)) {
            scope.spawn(move || run(paths, slots));
        }
    });
    out
}

/// Stat by listing each directory once instead of opening every file.
///
/// On Windows `std::fs::metadata` opens a handle per file (CreateFileW), and
/// every open passes the antivirus filter: 380k opens cost ~2.4 s on 8
/// threads. `DirEntry::metadata` there comes from the directory listing
/// (FindFirstFile/FindNextFile) with no open, ~0.4 s for the same tree.
/// NTFS can lag a directory entry's size and time while a writer still has
/// the file open; the entry is current once the writer closes it. Set
/// `ALBUMEN_STAT_PER_FILE=1` to stat file by file. A path the listing does
/// not show falls back to [`stat_one`], so its error text is unchanged.
pub(crate) fn stat_all_by_dir<S: AsRef<str> + Sync>(
    root: &Path,
    relative_paths: &[S],
    deadline: Option<std::time::Instant>,
    threads: usize,
) -> Vec<Option<Result<StatKey, String>>> {
    let mut by_dir: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, relative) in relative_paths.iter().enumerate() {
        let relative = relative.as_ref();
        let dir = relative.rfind('/').map_or("", |at| &relative[..at]);
        by_dir.entry(dir).or_default().push(i);
    }
    let groups: Vec<(&str, Vec<usize>)> = by_dir.into_iter().collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::with_capacity(relative_paths.len()));
    let work = || {
        let mut local = Vec::new();
        loop {
            let g = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some((dir, members)) = groups.get(g) else {
                break;
            };
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                break;
            }
            let full_dir = if dir.is_empty() {
                root.to_path_buf()
            } else {
                root.join(PathBuf::from_iter(dir.split('/')))
            };
            let mut listed: std::collections::HashMap<std::ffi::OsString, StatKey> =
                std::collections::HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&full_dir) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        if !meta.is_dir() && !meta.file_type().is_symlink() {
                            listed.insert(entry.file_name(), StatKey::of(&meta));
                        }
                    }
                }
            }
            for &i in members {
                let relative = relative_paths[i].as_ref();
                let name = relative.rsplit('/').next().unwrap_or(relative);
                let stat = match listed.get(std::ffi::OsStr::new(name)) {
                    Some(key) => Ok(*key),
                    None => stat_one(root, relative),
                };
                local.push((i, stat));
            }
        }
        results.lock().unwrap().extend(local);
    };
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1).min(groups.len().max(1)) {
            scope.spawn(work);
        }
    });
    let mut out: Vec<Option<Result<StatKey, String>>> = Vec::new();
    out.resize_with(relative_paths.len(), || None);
    for (i, stat) in results.into_inner().unwrap() {
        out[i] = Some(stat);
    }
    out
}

/// How many files one ExifTool process reads.
///
/// `ALBUMEN_EXIFTOOL_BATCH` overrides the soft default of 64. It is a process
/// batch, not a limit on the library or on a page.
pub fn exiftool_batch() -> usize {
    positive_env("ALBUMEN_EXIFTOOL_BATCH").unwrap_or(64)
}

/// Soft cap on records kept in this process.
///
/// Unset means 16384. `ALBUMEN_METADATA_CACHE_ITEMS=0` keeps every record the
/// process has read. A full library larger than the cache is still searchable;
/// a miss reads the file again.
pub fn cache_item_limit() -> Option<usize> {
    match std::env::var("ALBUMEN_METADATA_CACHE_ITEMS") {
        Ok(text) if text == "0" => None,
        Ok(text) => match text.parse::<usize>() {
            Ok(value) if value > 0 => Some(value),
            _ => Some(16_384),
        },
        Err(_) => Some(16_384),
    }
}

fn positive_env(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|value| *value >= 1)
}

/// Sidecar paths that sit beside `relative` and exist on disk.
pub fn existing_sidecars(root: &Path, relative: &str) -> Vec<String> {
    if is_sidecar(relative) {
        return Vec::new();
    }
    let (parent, name) = split_relative(relative);
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let candidates = [
        format!("{stem}.xmp"),
        format!("{name}.xmp"),
        format!("{stem}.aae"),
        format!("{name}.aae"),
    ];
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for candidate in &candidates {
        let relative_sidecar = join_relative(parent, candidate);
        if root
            .join(PathBuf::from_iter(relative_sidecar.split('/')))
            .is_file()
        {
            if !found.contains(&relative_sidecar) {
                found.push(relative_sidecar);
            }
        } else {
            missing.push(candidate.clone());
        }
    }
    if missing.is_empty() {
        return found;
    }
    let dir = if parent.is_empty() {
        root.to_path_buf()
    } else {
        root.join(PathBuf::from_iter(parent.split('/')))
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let raw = entry.file_name();
        let Some(name) = raw.to_str() else {
            continue;
        };
        if !missing
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        if !entry.path().is_file() {
            continue;
        }
        let relative_sidecar = join_relative(parent, name);
        if !found.contains(&relative_sidecar) {
            found.push(relative_sidecar);
        }
    }
    found
}

fn join_relative(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// [`with_sidecars`] for sidecar paths already found (by [`Cache::sidecars`]),
/// without searching `all` for case variants.
pub fn with_found_sidecars<'a>(
    own: &Record,
    sidecars: &[String],
    lookup: impl Fn(&str) -> Option<&'a Record>,
) -> Record {
    let mut merged = own.clone();
    for path in sidecars {
        if let Some(sidecar) = lookup(path) {
            merge_sidecar(&mut merged, sidecar);
        }
    }
    merged
}

fn merge_sidecar(merged: &mut Record, sidecar: &Record) {
    if merged.captured.is_none() {
        merged.captured = sidecar.captured;
    }
    if merged.camera.is_none() {
        merged.camera.clone_from(&sidecar.camera);
    }
    if merged.lens.is_none() {
        merged.lens.clone_from(&sidecar.lens);
    }
    if merged.latitude.is_none() {
        merged.latitude = sidecar.latitude;
    }
    if merged.longitude.is_none() {
        merged.longitude = sidecar.longitude;
    }
    push_unique(&mut merged.keywords, &sidecar.keywords);
    push_unique(&mut merged.captions, &sidecar.captions);
}

pub fn with_sidecars(relative: &str, own: &Record, all: &HashMap<String, Record>) -> Record {
    if is_sidecar(relative) {
        return own.clone();
    }
    let (parent, name) = split_relative(relative);
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let mut merged = own.clone();
    for candidate in [
        format!("{stem}.xmp"),
        format!("{name}.xmp"),
        format!("{stem}.aae"),
        format!("{name}.aae"),
    ] {
        if let Some(sidecar) = find_sibling(all, parent, &candidate) {
            merge_sidecar(&mut merged, sidecar);
        }
    }
    merged
}

pub fn parse_user_time(text: &str, end_of_day: bool) -> Result<WallTime, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("a capture time must not be empty".to_string());
    }
    let (wall, date_only) = parse_wall(text)?;
    if date_only && end_of_day {
        Ok(WallTime {
            hour: 23,
            minute: 59,
            second: 59,
            nanos: 999_999_999,
            ..wall
        })
    } else {
        Ok(wall)
    }
}

pub fn parse_geo_box(south: f64, north: f64, west: f64, east: f64) -> Result<GeoBox, String> {
    if ![south, north, west, east]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err("gps bounds must be finite numbers".to_string());
    }
    if !(-90.0..=90.0).contains(&south) || !(-90.0..=90.0).contains(&north) || south > north {
        return Err(
            "gps south/north must be within -90..90 and south must not exceed north".to_string(),
        );
    }
    if !(-180.0..=180.0).contains(&west) || !(-180.0..=180.0).contains(&east) {
        return Err("gps west/east must be within -180..180".to_string());
    }
    Ok(GeoBox {
        south,
        north,
        west,
        east,
    })
}

fn read_exiftool(
    root: &Path,
    relative_paths: &[String],
) -> Result<HashMap<String, Record>, String> {
    albumen_ingest::ledger::record_analysis(Some(root), "exiftool", "on-device")
        .map_err(|err| err.to_string())?;
    let mut out = HashMap::new();
    for chunk in relative_paths.chunks(exiftool_batch()) {
        let output = exiftool_command()
            .current_dir(root)
            .arg("-json")
            .arg("-n")
            .arg("-G1")
            .arg("-s")
            .arg("-api")
            .arg("largefilesupport=1")
            .args(chunk)
            .output()
            .map_err(|err| format!("exiftool is required to search metadata: {err}"))?;
        if !output.status.success() {
            return Err(format!(
                "exiftool failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let items: Vec<Map<String, Value>> = serde_json::from_slice(&output.stdout)
            .map_err(|err| format!("exiftool json: {err}"))?;
        if items.len() != chunk.len() {
            return Err(format!(
                "exiftool returned {} records for {} files",
                items.len(),
                chunk.len()
            ));
        }
        for (relative, item) in chunk.iter().zip(items) {
            let source = item
                .get("SourceFile")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim_start_matches("./");
            if source != relative {
                return Err(format!("exiftool reported {source} for {relative}"));
            }
            out.insert(relative.clone(), record_from_exif(&item));
        }
    }
    Ok(out)
}

fn record_from_exif(item: &Map<String, Value>) -> Record {
    let mut record = Record::empty();
    record.captured = capture_time(item);
    record.camera = join_tags(
        item,
        &["Make", "Model", "UniqueCameraModel", "CameraModelName"],
    );
    record.lens = join_tags(item, &["LensModel", "LensMake", "Lens", "LensType"]);
    record.latitude = coordinate(item, "Latitude");
    record.longitude = coordinate(item, "Longitude");
    record.keywords = string_tags(item, &["Keywords", "Subject", "HierarchicalSubject"]);
    record.captions = string_tags(
        item,
        &[
            "Caption-Abstract",
            "Description",
            "ImageDescription",
            "Title",
        ],
    );
    record
}

fn capture_time(item: &Map<String, Value>) -> Option<WallTime> {
    const TAGS: &[&str] = &[
        "DateTimeOriginal",
        "CreateDate",
        "DateCreated",
        "MediaCreateDate",
        "TrackCreateDate",
        "CreationDate",
        "DateTimeDigitized",
    ];
    for tag in TAGS {
        for value in tag_values(item, tag) {
            if let Some(text) = value.as_str() {
                if let Ok((wall, _)) = parse_wall(text) {
                    return Some(wall);
                }
            }
        }
    }
    None
}

fn coordinate(item: &Map<String, Value>, axis: &str) -> Option<f64> {
    let suffix = format!("GPS{axis}");
    let composite = format!("Composite:{suffix}");
    if let Some(value) = item.get(&composite).and_then(as_f64) {
        return Some(value);
    }
    if let Some(position) = item.get("Composite:GPSPosition").and_then(Value::as_str) {
        let mut parts = position.split_whitespace();
        let latitude: f64 = parts.next()?.parse().ok()?;
        let longitude: f64 = parts.next()?.parse().ok()?;
        return Some(if axis == "Latitude" {
            latitude
        } else {
            longitude
        });
    }
    for (key, value) in item {
        if key == "SourceFile" || key.ends_with("Ref") || !key_matches(key, &suffix) {
            continue;
        }
        let Some(number) = as_f64(value) else {
            continue;
        };
        let reference = ref_value(item, axis);
        return Some(match reference {
            Some('N') | Some('E') => number.abs(),
            Some('S') | Some('W') => -number.abs(),
            _ => number,
        });
    }
    None
}

fn ref_value(item: &Map<String, Value>, axis: &str) -> Option<char> {
    let suffix = format!("GPS{axis}Ref");
    for (key, value) in item {
        if key_matches(key, &suffix) {
            let text = value.as_str()?.trim();
            return text.chars().next().map(|ch| ch.to_ascii_uppercase());
        }
    }
    None
}

fn join_tags(item: &Map<String, Value>, suffixes: &[&str]) -> Option<String> {
    let mut parts = Vec::new();
    for suffix in suffixes {
        for value in string_values(&tag_values(item, suffix)) {
            if parts
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
            {
                continue;
            }
            parts.push(value);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn string_tags(item: &Map<String, Value>, suffixes: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for suffix in suffixes {
        push_unique(&mut out, &string_values(&tag_values(item, suffix)));
    }
    out
}

fn tag_values<'a>(item: &'a Map<String, Value>, suffix: &str) -> Vec<&'a Value> {
    item.iter()
        .filter(|(key, _)| key_matches(key, suffix))
        .map(|(_, value)| value)
        .collect()
}

fn key_matches(key: &str, suffix: &str) -> bool {
    key == suffix || key.rsplit_once(':').is_some_and(|(_, tail)| tail == suffix)
}

fn string_values(values: &[&Value]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        collect_strings(value, &mut out);
    }
    out
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                out.push(text.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn parse_wall(text: &str) -> Result<(WallTime, bool), String> {
    let mut text = text.trim();
    if let Some(stripped) = text.strip_suffix('Z').or_else(|| text.strip_suffix('z')) {
        text = stripped.trim();
    }
    if let Some(index) = timezone_index(text) {
        text = text[..index].trim();
    }
    let (date, time) = if let Some((date, time)) = text.split_once(['T', ' ']) {
        (date, Some(time.trim()))
    } else {
        (text, None)
    };
    let mut date_parts = date.split(['-', ':']);
    let year = take_number(&mut date_parts, "year")?;
    let month = take_number(&mut date_parts, "month")?;
    let day = take_number(&mut date_parts, "day")?;
    if date_parts.next().is_some() {
        return Err(format!("bad capture date {text}"));
    }
    let (hour, minute, second, nanos) = match time {
        None => (0, 0, 0, 0),
        Some(time) => {
            let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
            let mut clock_parts = clock.split(':');
            let hour = take_number(&mut clock_parts, "hour")?;
            let minute = take_number(&mut clock_parts, "minute")?;
            let second = take_number(&mut clock_parts, "second")?;
            if clock_parts.next().is_some() {
                return Err(format!("bad capture time {text}"));
            }
            (hour, minute, second, fraction_nanos(fraction)?)
        }
    };
    let wall = WallTime {
        year: i32::try_from(year).map_err(|_| format!("bad year in {text}"))?,
        month: u8::try_from(month).map_err(|_| format!("bad month in {text}"))?,
        day: u8::try_from(day).map_err(|_| format!("bad day in {text}"))?,
        hour: u8::try_from(hour).map_err(|_| format!("bad hour in {text}"))?,
        minute: u8::try_from(minute).map_err(|_| format!("bad minute in {text}"))?,
        second: u8::try_from(second).map_err(|_| format!("bad second in {text}"))?,
        nanos,
    };
    validate_wall(wall, text)?;
    Ok((wall, time.is_none()))
}

fn timezone_index(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 11.min(bytes.len());
    while index < bytes.len() {
        if bytes[index] == b'+' || bytes[index] == b'-' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn take_number<'a>(parts: &mut impl Iterator<Item = &'a str>, label: &str) -> Result<u32, String> {
    let text = parts
        .next()
        .ok_or_else(|| format!("capture time is missing {label}"))?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("bad {label} in capture time"));
    }
    text.parse()
        .map_err(|_| format!("bad {label} in capture time"))
}

fn fraction_nanos(text: &str) -> Result<u32, String> {
    if text.is_empty() {
        return Ok(0);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) || text.len() > 9 {
        return Err(format!("bad fractional seconds {text}"));
    }
    let mut digits = text.to_string();
    digits.extend(std::iter::repeat_n('0', 9 - text.len()));
    digits
        .parse()
        .map_err(|_| format!("bad fractional seconds {text}"))
}

fn validate_wall(wall: WallTime, text: &str) -> Result<(), String> {
    let bad = || format!("bad capture time {text}");
    if !(1..=12).contains(&wall.month) || wall.hour > 23 || wall.minute > 59 || wall.second > 60 {
        return Err(bad());
    }
    let max_day = match wall.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(wall.year) {
                29
            } else {
                28
            }
        }
        _ => return Err(bad()),
    };
    if wall.day == 0 || wall.day > max_day {
        return Err(bad());
    }
    Ok(())
}

fn is_leap(year: i32) -> bool {
    let year = year as u32;
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn contains_ignore_case(haystack: Option<&str>, needle: &str) -> bool {
    haystack
        .map(|text| text.to_lowercase().contains(&needle.to_lowercase()))
        .unwrap_or(false)
}

fn is_sidecar(relative: &str) -> bool {
    let name = split_relative(relative).1;
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    ext.eq_ignore_ascii_case("xmp") || ext.eq_ignore_ascii_case("aae")
}

fn split_relative(relative: &str) -> (&str, &str) {
    match relative.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", relative),
    }
}

fn find_sibling<'a>(
    all: &'a HashMap<String, Record>,
    parent: &str,
    name: &str,
) -> Option<&'a Record> {
    let exact = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    };
    if let Some(record) = all.get(&exact) {
        return Some(record);
    }
    all.iter().find_map(|(path, record)| {
        let (other_parent, other_name) = split_relative(path);
        (other_parent == parent && other_name.eq_ignore_ascii_case(name)).then_some(record)
    })
}

fn push_unique(dest: &mut Vec<String>, extra: &[String]) {
    let mut seen: HashSet<String> = dest.iter().map(|item| item.to_lowercase()).collect();
    for item in extra {
        if seen.insert(item.to_lowercase()) {
            dest.push(item.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_by_dir_listing_matches_per_file_stat() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        let mut paths = Vec::new();
        for (i, rel) in ["top.jpg", "a/one.jpg", "a/b/two.jpg", "a/b/three.dng"]
            .iter()
            .enumerate()
        {
            let full = root.path().join(PathBuf::from_iter(rel.split('/')));
            std::fs::write(&full, vec![b'x'; 10 + i]).unwrap();
            paths.push(rel.to_string());
        }
        paths.push("a/b/missing.jpg".to_string());
        paths.push("nodir/missing.jpg".to_string());
        let per_file: Vec<_> = paths.iter().map(|p| stat_one(root.path(), p)).collect();
        for threads in [1, 3, 8] {
            let by_dir = stat_all_by_dir(root.path(), &paths, None, threads);
            assert_eq!(by_dir.len(), paths.len());
            for (want, got) in per_file.iter().zip(by_dir) {
                assert_eq!(Some(want.clone()), got);
            }
        }
        let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
        assert!(stat_all_by_dir(root.path(), &paths, Some(past), 2)
            .iter()
            .all(Option::is_none));
    }

    #[test]
    fn date_bounds_and_antimeridian() {
        let day = parse_user_time("2016-03-21", false).unwrap();
        let end = parse_user_time("2016-03-21", true).unwrap();
        let captured = parse_wall("2016:03:21 09:15:00").unwrap().0;
        assert!(captured >= day && captured <= end);
        assert!(parse_user_time("2016-02-31", false).is_err());

        let box_ = parse_geo_box(10.0, 20.0, 170.0, -170.0).unwrap();
        assert!(box_.contains(15.0, 175.0));
        assert!(box_.contains(15.0, -175.0));
        assert!(!box_.contains(15.0, 0.0));
        assert!(parse_geo_box(20.0, 10.0, 0.0, 1.0).is_err());
    }

    #[test]
    fn fixture_files_and_sidecar_join_round_trip_without_a_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let before = list_files(&root);
        let mut cache = Cache::default();
        let paths = [
            "jpeg-gps/IMG_0001.jpg".to_string(),
            "xmp/IMG_0004.jpg".to_string(),
            "xmp/IMG_0004.xmp".to_string(),
            "raw-jpeg/IMG_0003.jpg".to_string(),
        ];
        let first = cache.load(&root, &paths).unwrap();
        let again = cache.load(&root, &paths).unwrap();
        assert_eq!(first, again);
        assert_eq!(list_files(&root), before);

        let jpeg = first.get("jpeg-gps/IMG_0001.jpg").unwrap();
        assert_eq!(
            jpeg.captured,
            Some(parse_wall("2012:06:15 10:30:00").unwrap().0)
        );
        assert_eq!(jpeg.latitude, Some(37.8199));
        assert_eq!(jpeg.longitude, Some(-122.4783));
        assert!(jpeg.keywords.iter().any(|item| item == "cc0"));
        assert!(jpeg
            .captions
            .iter()
            .any(|item| item.contains("Not a photograph")));

        let vegas = first.get("raw-jpeg/IMG_0003.jpg").unwrap();
        assert_eq!(vegas.longitude, Some(-115.1398));

        let joined = with_sidecars(
            "xmp/IMG_0004.jpg",
            first.get("xmp/IMG_0004.jpg").unwrap(),
            &first,
        );
        assert!(joined.latitude.unwrap() > 48.0);
        assert!(joined.longitude.unwrap() > 2.0);
        assert!(joined
            .captions
            .iter()
            .any(|item| item.contains("sidecar fixture")));
        assert!(joined.camera.as_deref().unwrap().contains("Fixture Camera"));
    }

    #[test]
    fn a_changed_file_is_read_again() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.xmp");
        std::fs::write(&path, xmp("alpha caption", "alpha-keyword", "Lens A")).unwrap();
        let mut cache = Cache::default();
        let relative = vec!["note.xmp".to_string()];
        let first = cache.load(root.path(), &relative).unwrap();
        assert!(first["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("alpha caption")));
        std::fs::write(&path, xmp("beta caption", "beta-keyword", "Lens B")).unwrap();
        let second = cache.load(root.path(), &relative).unwrap();
        assert!(second["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("beta caption")));
        assert!(!second["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("alpha caption")));
        assert_eq!(second["note.xmp"].lens.as_deref(), Some("Lens B"));
        let names = list_files(root.path());
        assert_eq!(names, vec!["note.xmp".to_string()]);
    }

    #[test]
    fn warm_search_skips_only_unchanged_settled_files() {
        let root = tempfile::tempdir().unwrap();
        let mut relative = Vec::new();
        for n in 0..5 {
            let name = format!("n{n}.xmp");
            std::fs::write(
                root.path().join(&name),
                xmp(&format!("cap {n}"), "kw", "Lens A"),
            )
            .unwrap();
            relative.push(name);
        }
        let mut cache = Cache::with_racy_ns(0);
        cache.load(root.path(), &relative).unwrap();
        assert_eq!(cache.hashed(), 5);
        cache.load(root.path(), &relative).unwrap();
        assert_eq!(cache.hashed(), 5, "unchanged stat must not re-hash");

        // Same size, one byte flipped, mtime put back: ctime still moves.
        let path = root.path().join("n2.xmp");
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.windows(5).position(|w| w == b"cap 2").unwrap() + 4;
        bytes[at] = b'9';
        std::fs::write(&path, &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let again = cache.load(root.path(), &relative).unwrap();
        assert_eq!(cache.hashed(), 6, "exactly the changed file is re-hashed");
        assert!(again["n2.xmp"].captions.iter().any(|c| c.contains("cap 9")));
    }

    #[test]
    fn a_hash_taken_inside_the_racy_window_is_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.xmp"), xmp("c", "k", "L")).unwrap();
        let relative = vec!["a.xmp".to_string()];
        let mut cache = Cache::default();
        cache.load(root.path(), &relative).unwrap();
        cache.load(root.path(), &relative).unwrap();
        assert_eq!(cache.hashed(), 2, "a just-written file is hashed again");
    }

    fn list_files(root: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    names.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        names.sort();
        names
    }

    fn xmp(caption: &str, keyword: &str, lens: &str) -> String {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:aux="http://ns.adobe.com/exif/1.0/aux/">
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">{caption}</rdf:li></rdf:Alt></dc:description>
   <dc:subject><rdf:Bag><rdf:li>{keyword}</rdf:li></rdf:Bag></dc:subject>
   <aux:Lens>{lens}</aux:Lens>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#
        )
    }
}
