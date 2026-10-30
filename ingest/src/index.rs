// SPDX-License-Identifier: Apache-2.0

//! A path index of one library, kept outside the library.
//!
//! Search reads this index instead of walking every directory on every query.
//! The index holds the sorted listing of each directory and that directory's
//! modification time. [`LibraryIndex::refresh`] stats each indexed directory
//! and lists again only the ones whose time changed, so a file added,
//! renamed, or removed since the last run is picked up before the next
//! answer. A listing taken within [`RACY_NANOS`] of the directory's own time is
//! listed again on the next refresh, so a change inside one clock tick is not
//! missed.
//!
//! The library is opened as a [`SourceRoot`] and only listed and stat'ed. The
//! saved index lives in the platform data directory (or `--index-dir`), and a
//! directory inside the library is refused. Writes go through [`crate::dest`].
//!
//! More than one process may keep the same index (the GUI and the search
//! tool both open `<sha256(root)>.idx`). A save never blocks a reader and
//! takes no lock:
//!
//! - The file carries a generation. A save reads the generation on disk; if
//!   another writer moved it on, the two indexes are merged directory by
//!   directory (the later listing of each directory wins) before writing, and
//!   the generation is checked again just before the rename, so an older
//!   snapshot never replaces a newer one.
//! - The new file is written beside the old one, `sync_all`ed, renamed over
//!   it, and the directory is synced, so a kill at any point leaves the old
//!   index or the new one.
//! - The file ends with its length and SHA-256, so a short or damaged file is
//!   refused (and rebuilt) rather than read as a smaller library.
//! - A rename refused because another program holds the file is retried with
//!   backoff; a save that still fails is kept pending and retried by the next
//!   refresh. Leftover temp files from a killed writer are removed by the next
//!   save.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::dest;
use crate::error::IngestError;
use crate::source::SourceRoot;

/// A listing this close to its directory's own modification time is listed
/// again on the next refresh.
pub const RACY_NANOS: u64 = 2_000_000_000;

/// The format written: a generation line after the root, and a footer.
const FORMAT: &str = "albumen-index/2";

/// The first format: no generation, no footer. Still read.
const FORMAT_V1: &str = "albumen-index/1";

/// Test hook: `ALBUMEN_INDEX_CRASH=before-rename` aborts after half of the
/// temp file is written and synced; `after-rename` aborts right after the
/// rename, before the directory sync. Either is what a kill there leaves.
pub const CRASH_ENV: &str = "ALBUMEN_INDEX_CRASH";

/// Times a refused rename is tried again, waiting 5 ms, then twice as long
/// each time (about 2.5 s in all).
const RENAME_RETRIES: u32 = 9;

/// Times a writer tries for the index lock, waiting 5 ms, then twice as
/// long each time, capped at 640 ms (about 2.5 s in all). A writer that
/// cannot get it leaves its save pending for the next refresh.
const LOCK_TRIES: u32 = 9;

/// The two slots a saved index alternates between, after `<key>.idx`.
const SLOTS: [&str; 2] = ["a", "b"];

static PARTIAL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Environment override for the index directory.
pub const INDEX_DIR_ENV: &str = "ALBUMEN_INDEX_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirRecord {
    mtime_ns: u64,
    listed_ns: u64,
    /// Entries in walk order. `true` marks a directory.
    entries: Vec<(String, bool)>,
}

/// What the last [`LibraryIndex::open`] or [`LibraryIndex::refresh`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshStats {
    /// Directories stat'ed.
    pub dirs_checked: usize,
    /// Directories listed again because their time changed or was racy.
    pub dirs_listed: usize,
    /// Whether the file list changed.
    pub changed: bool,
}

/// The sorted path list of one library.
#[derive(Debug)]
pub struct LibraryIndex {
    root: SourceRoot,
    file: Option<PathBuf>,
    dirs: BTreeMap<String, DirRecord>,
    paths: Vec<String>,
    lower: Vec<String>,
    /// True when the index came from a saved file rather than a first build.
    pub loaded: bool,
    /// Milliseconds the open took, including the first refresh.
    pub open_ms: f64,
    /// A save that failed. The index still answers from memory, and the next
    /// refresh saves again.
    pub save_error: Option<String>,
    /// The generation of the saved file this index last read or wrote.
    generation: Option<u64>,
    /// A save is owed (an earlier one failed).
    dirty: bool,
    /// Saves that found another writer's newer file and merged it.
    pub merges: u64,
    /// Renames tried again after the file was held by another program.
    pub rename_retries: u64,
}

/// A saved index read from disk.
struct Saved {
    generation: u64,
    dirs: BTreeMap<String, DirRecord>,
}

/// The default index directory: `ALBUMEN_INDEX_DIR`, else `index` under the
/// platform data directory.
pub fn default_index_dir() -> Option<PathBuf> {
    if let Ok(value) = std::env::var(INDEX_DIR_ENV) {
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    crate::platform::platform_dirs()
        .ok()
        .map(|(_, data)| data.join("index"))
}

impl LibraryIndex {
    /// Load the saved index for `library` from `index_dir`, or build it, then
    /// bring it current. `None` keeps the index in memory only.
    pub fn open(library: &Path, index_dir: Option<&Path>) -> Result<Self, IngestError> {
        Self::open_with_progress(library, index_dir, &mut |_| {})
    }

    /// [`LibraryIndex::open`], calling `on_file` with each file a directory
    /// listing finds, as it is found, so a caller can show progress while a
    /// first build walks the tree. Directories that are still fresh in a
    /// saved index are not listed, so they report nothing.
    pub fn open_with_progress(
        library: &Path,
        index_dir: Option<&Path>,
        on_file: &mut dyn FnMut(&str),
    ) -> Result<Self, IngestError> {
        let began = Instant::now();
        let root = SourceRoot::open(library)?;
        let file = match index_dir {
            Some(dir) => {
                root.refuse_inside(dir)?;
                let key = {
                    use sha2::Digest;
                    let digest =
                        sha2::Sha256::digest(root.canonical().to_string_lossy().as_bytes());
                    digest
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                };
                Some(dir.join(format!("{key}.idx")))
            }
            None => None,
        };
        let mut index = Self {
            root,
            file,
            dirs: BTreeMap::new(),
            paths: Vec::new(),
            lower: Vec::new(),
            loaded: false,
            open_ms: 0.0,
            save_error: None,
            generation: None,
            dirty: false,
            merges: 0,
            rename_retries: 0,
        };
        if let Some(saved) = index.read_saved() {
            index.dirs = saved.dirs;
            index.generation = Some(saved.generation);
            index.loaded = true;
        }
        index.refresh_with(on_file)?;
        index.open_ms = began.elapsed().as_secs_f64() * 1000.0;
        Ok(index)
    }

    fn read_saved(&self) -> Option<Saved> {
        let file = self.file.as_ref()?;
        read_newest(file, &self.root.canonical().to_string_lossy()).map(|(_, saved)| saved)
    }

    /// Stat every indexed directory and list again the ones that changed.
    pub fn refresh(&mut self) -> Result<RefreshStats, IngestError> {
        self.refresh_with(&mut |_| {})
    }

    fn refresh_with(&mut self, on_file: &mut dyn FnMut(&str)) -> Result<RefreshStats, IngestError> {
        let mut stats = RefreshStats::default();
        let now = now_ns();
        let changed = self.sync_dir(String::new(), now, &mut stats, on_file)?;
        let before = self.dirs.len();
        self.prune();
        let pruned = self.dirs.len() != before;
        stats.changed = changed || pruned;
        if stats.changed || (self.paths.is_empty() && !self.dirs.is_empty()) {
            self.flatten();
        }
        if stats.dirs_listed > 0 || pruned || self.dirty {
            match self.save() {
                Ok(()) => {
                    self.save_error = None;
                    self.dirty = false;
                }
                Err(err) => {
                    self.save_error = Some(err.to_string());
                    self.dirty = true;
                }
            }
        }
        Ok(stats)
    }

    fn sync_dir(
        &mut self,
        relative: String,
        now: u64,
        stats: &mut RefreshStats,
        on_file: &mut dyn FnMut(&str),
    ) -> Result<bool, IngestError> {
        let path = self.absolute(&relative);
        stats.dirs_checked += 1;
        let meta = self.root.metadata_nofollow(&path)?;
        let mtime_ns = meta.modified().map(to_ns).unwrap_or(0);
        let mut changed = false;
        let fresh = self.dirs.get(&relative).is_some_and(|record| {
            record.mtime_ns == mtime_ns
                && mtime_ns != 0
                && record.listed_ns.saturating_sub(mtime_ns) >= RACY_NANOS
        });
        if !fresh {
            stats.dirs_listed += 1;
            let entries = list_dir(&self.root, &path)?;
            for (name, is_dir) in &entries {
                if !is_dir {
                    on_file(&join(&relative, name));
                }
            }
            let record = DirRecord {
                mtime_ns,
                listed_ns: now,
                entries,
            };
            let differs = self
                .dirs
                .get(&relative)
                .is_none_or(|old| old.entries != record.entries);
            changed |= differs;
            self.dirs.insert(relative.clone(), record);
        }
        let children: Vec<String> = self.dirs[&relative]
            .entries
            .iter()
            .filter(|(_, is_dir)| *is_dir)
            .map(|(name, _)| join(&relative, name))
            .collect();
        for child in children {
            changed |= self.sync_dir(child, now, stats, on_file)?;
        }
        Ok(changed)
    }

    fn absolute(&self, relative: &str) -> PathBuf {
        let mut path = self.root.canonical().to_path_buf();
        if !relative.is_empty() {
            for part in relative.split('/') {
                path.push(part);
            }
        }
        path
    }

    /// Drop records for directories no longer reachable from the root.
    fn prune(&mut self) {
        let mut reachable = std::collections::BTreeSet::new();
        let mut stack = vec![String::new()];
        while let Some(dir) = stack.pop() {
            if let Some(record) = self.dirs.get(&dir) {
                for (name, is_dir) in &record.entries {
                    if *is_dir {
                        stack.push(join(&dir, name));
                    }
                }
            }
            reachable.insert(dir);
        }
        self.dirs.retain(|dir, _| reachable.contains(dir));
    }

    /// Take each directory's later listing from another writer's index.
    fn merge(&mut self, other: BTreeMap<String, DirRecord>) {
        for (dir, theirs) in other {
            match self.dirs.get(&dir) {
                Some(ours) if ours.listed_ns >= theirs.listed_ns => {}
                _ => {
                    self.dirs.insert(dir, theirs);
                }
            }
        }
        self.prune();
        self.flatten();
    }

    fn flatten(&mut self) {
        let paths = flatten_dirs(&self.dirs);
        self.lower = paths.iter().map(|path| path.to_lowercase()).collect();
        self.paths = paths;
    }

    /// Save under the index lock into the slot that does not hold the
    /// newest file, so readers (who never lock) always find the newest
    /// complete file under its own name while this one is replaced.
    fn save(&mut self) -> Result<(), IngestError> {
        let Some(file) = self.file.clone() else {
            return Ok(());
        };
        if let Some(parent) = file.parent() {
            dest::create_dir_all(parent)?;
        }
        let root = self.root.canonical().to_string_lossy().into_owned();
        let crash = std::env::var(CRASH_ENV).ok();
        let _lock = lock_index(&file)?;
        // Under the lock no other writer is between its read and its rename,
        // so what is read here is what this save replaces.
        let newest = read_newest(&file, &root);
        let mut disk_generation = None;
        let mut newest_slot = None;
        if let Some((slot, saved)) = newest {
            disk_generation = Some(saved.generation);
            newest_slot = slot;
            if Some(saved.generation) != self.generation {
                self.merges += 1;
                self.merge(saved.dirs);
            }
        }
        let generation = disk_generation.max(self.generation).unwrap_or(0) + 1;
        let target = match newest_slot {
            Some(0) => slot_path(&file, 1),
            _ => slot_path(&file, 0),
        };
        let bytes = render_saved(&root, generation, &self.dirs);
        let partial = file.with_extension(format!(
            "idx.partial-{}-{}",
            std::process::id(),
            PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // Only the lock holder writes temp files, so any left are a crashed
        // writer's.
        remove_stale_partials(&file, &partial);
        let mut handle = dest::create_file(&partial)?;
        let half = crash.as_deref() == Some("before-rename");
        let body = if half {
            &bytes[..bytes.len() / 2]
        } else {
            &bytes[..]
        };
        handle
            .write_all(body)
            .and_then(|()| handle.sync_all())
            .map_err(|err| IngestError::io(&partial, err))?;
        drop(handle);
        if half {
            std::process::abort();
        }
        if let Err(err) = self.rename_with_retry(&partial, &target) {
            dest::remove_file_best_effort(&partial);
            return Err(err);
        }
        if crash.as_deref() == Some("after-rename") {
            std::process::abort();
        }
        #[cfg(not(windows))]
        if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
            dest::sync_dir(parent)?;
        }
        self.generation = Some(generation);
        Ok(())
    }

    /// [`dest::rename`], tried again with backoff while another program holds
    /// the file (Windows refuses a replace then).
    fn rename_with_retry(&mut self, from: &Path, to: &Path) -> Result<(), IngestError> {
        let mut wait = std::time::Duration::from_millis(5);
        let mut tries = 0;
        loop {
            match dest::rename(from, to) {
                Ok(()) => return Ok(()),
                Err(err) if tries < RENAME_RETRIES && !is_not_found(&err) => {
                    tries += 1;
                    self.rename_retries += 1;
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(std::time::Duration::from_millis(640));
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// The generation of the saved file this index last read or wrote.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Where the index is saved, when it is saved.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Every file, in the order [`SourceRoot::walk`] visits them.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Every file, in walk order, without keeping the rest of the index.
    pub fn into_paths(self) -> Vec<String> {
        self.paths
    }

    /// Visit files that sort after `after`, in walk order, with each path's
    /// lowercase form. Return `false` to stop.
    pub fn visit_after(&self, after: Option<&str>, visit: &mut dyn FnMut(&str, &str) -> bool) {
        let start = match after {
            Some(after) => self.paths.partition_point(|path| path.as_str() <= after),
            None => 0,
        };
        for (path, lower) in self.paths[start..].iter().zip(&self.lower[start..]) {
            if !visit(path, lower) {
                return;
            }
        }
    }
}

fn is_not_found(err: &IngestError) -> bool {
    matches!(err, IngestError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
}

/// `<key>.idx.a` or `<key>.idx.b`.
fn slot_path(file: &Path, slot: usize) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".");
    name.push(SLOTS[slot]);
    PathBuf::from(name)
}

/// Take the writers' lock, `<key>.idx.lock`. The OS drops it when the
/// writer exits or is killed, so a crash never leaves it held.
fn lock_index(file: &Path) -> Result<std::fs::File, IngestError> {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    let path = PathBuf::from(name);
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| IngestError::io(&path, err))?;
    let mut wait = std::time::Duration::from_millis(5);
    for _ in 0..LOCK_TRIES {
        match handle.try_lock() {
            Ok(()) => return Ok(handle),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(std::time::Duration::from_millis(640));
            }
            Err(std::fs::TryLockError::Error(err)) => return Err(IngestError::io(&path, err)),
        }
    }
    handle.try_lock().map(|()| handle).map_err(|_| {
        IngestError::io(
            &path,
            std::io::Error::other("another writer kept the index lock; save left pending"),
        )
    })
}

/// The newest complete saved index for `file`: the slot with the highest
/// generation whose footer checks, else the other slot, else a file saved
/// by an earlier version under `file` itself. Returns which slot it was
/// (`None` for the earlier file). Readers never lock: a slot is only ever
/// replaced by a rename, and never the newest one, so a reader that finds
/// one slot mid-replace still has the other.
fn read_newest(file: &Path, root: &str) -> Option<(Option<usize>, Saved)> {
    let mut slots: Vec<(u64, usize)> = (0..SLOTS.len())
        .filter_map(|slot| read_generation(&slot_path(file, slot)).map(|g| (g, slot)))
        .collect();
    slots.sort_unstable_by(|a, b| b.cmp(a));
    for (_, slot) in &slots {
        if let Some(saved) = read_whole(&slot_path(file, *slot), root) {
            return Some((Some(*slot), saved));
        }
    }
    if slots.is_empty() {
        // Heads can be missed mid-replace; try both whole before falling back.
        for slot in 0..SLOTS.len() {
            if let Some(saved) = read_whole(&slot_path(file, slot), root) {
                return Some((Some(slot), saved));
            }
        }
    }
    read_whole(file, root).map(|saved| (None, saved))
}

fn read_whole(path: &Path, root: &str) -> Option<Saved> {
    let bytes = std::fs::read(path).ok()?;
    parse_saved(std::str::from_utf8(&bytes).ok()?, root)
}

/// The concrete file [`read_saved_file`] and [`check_saved_file`] read for
/// an index `file`: its newest complete slot, else the earlier-version file.
pub fn newest_slot(file: &Path) -> Option<PathBuf> {
    newest_checked(file).map(|(path, _)| path)
}

/// [`read_newest`] for a caller that does not know the root: each file's own
/// root line is used.
fn newest_checked(file: &Path) -> Option<(PathBuf, Saved)> {
    let own_root = |path: &Path| -> Option<Saved> {
        let bytes = std::fs::read(path).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let root = text.split('\n').nth(1)?.strip_prefix("root\t")?;
        parse_saved(text, root)
    };
    let mut slots: Vec<(u64, usize)> = (0..SLOTS.len())
        .filter_map(|slot| read_generation(&slot_path(file, slot)).map(|g| (g, slot)))
        .collect();
    slots.sort_unstable_by(|a, b| b.cmp(a));
    let mut order: Vec<usize> = slots.iter().map(|(_, slot)| *slot).collect();
    if order.is_empty() {
        order = (0..SLOTS.len()).collect();
    }
    for slot in order {
        let path = slot_path(file, slot);
        if let Some(saved) = own_root(&path) {
            return Some((path, saved));
        }
    }
    own_root(file).map(|saved| (file.to_path_buf(), saved))
}

/// The two slot files of an index `file`, for tests and tools.
pub fn slot_files(file: &Path) -> [PathBuf; 2] {
    [slot_path(file, 0), slot_path(file, 1)]
}

/// Remove `<file>.partial-*` temp files other than `keep`. One a live writer
/// is still writing is refused on Windows (it is open) and, on Unix, makes
/// that writer's rename fail and start over; neither loses the saved index.
fn remove_stale_partials(file: &Path, keep: &Path) {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let prefix = format!("{name}.partial-");
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(&prefix));
        if stale && path != keep && entry.file_type().is_ok_and(|t| t.is_file()) {
            dest::remove_file_best_effort(&path);
        }
    }
}

/// The generation in the first lines of a saved file, without reading the
/// rest. A first-format file is generation 0.
fn read_generation(file: &Path) -> Option<u64> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(std::fs::File::open(file).ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    match line.trim_end_matches('\n') {
        FORMAT_V1 => return Some(0),
        FORMAT => {}
        _ => return None,
    }
    line.clear();
    reader.read_line(&mut line).ok()?;
    line.clear();
    reader.read_line(&mut line).ok()?;
    line.trim_end_matches('\n')
        .strip_prefix("gen\t")?
        .parse()
        .ok()
}

/// Every file of a directory map, in walk order.
fn flatten_dirs(dirs: &BTreeMap<String, DirRecord>) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack: Vec<(String, usize)> = vec![(String::new(), 0)];
    while let Some((dir, position)) = stack.pop() {
        let Some(record) = dirs.get(&dir) else {
            continue;
        };
        let mut next = position;
        while next < record.entries.len() {
            let (name, is_dir) = &record.entries[next];
            next += 1;
            let relative = join(&dir, name);
            if *is_dir {
                stack.push((dir.clone(), next));
                stack.push((relative, 0));
                break;
            }
            paths.push(relative);
        }
    }
    paths
}

/// Read a saved index file as it is on disk, without refreshing it: its
/// generation and every file, in walk order. A short or damaged file is an
/// error. For tests and tools that watch the file.
///
/// `file` is an index's [`LibraryIndex::file`] (its newest complete slot is
/// read, the way an opening index reads it) or one concrete slot file.
pub fn read_saved_file(file: &Path) -> Result<(u64, Vec<String>), String> {
    if is_index_name(file) {
        let (_, saved) = newest_checked(file).ok_or("no complete saved index")?;
        return Ok((saved.generation, flatten_dirs(&saved.dirs)));
    }
    let bytes = std::fs::read(file).map_err(|err| err.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|err| err.to_string())?;
    let root = text
        .split('\n')
        .nth(1)
        .and_then(|line| line.strip_prefix("root\t"))
        .ok_or("no root line")?;
    let saved = parse_saved(text, root).ok_or("damaged or short index file")?;
    Ok((saved.generation, flatten_dirs(&saved.dirs)))
}

/// Check a saved index file: format, footer length and checksum. Returns its
/// generation.
/// `file` is resolved as for [`read_saved_file`].
///
/// A current-format file is verified without building its directory tree:
/// the footer's length and SHA-256 cover every body byte, so a body that
/// matches them is the body the writer wrote. A first-format file has no
/// checksum and is still fully parsed.
pub fn check_saved_file(file: &Path) -> Result<u64, String> {
    if is_index_name(file) {
        let mut slots: Vec<(u64, usize)> = (0..SLOTS.len())
            .filter_map(|slot| read_generation(&slot_path(file, slot)).map(|g| (g, slot)))
            .collect();
        slots.sort_unstable_by(|a, b| b.cmp(a));
        let mut order: Vec<PathBuf> = slots
            .iter()
            .map(|(_, slot)| slot_path(file, *slot))
            .collect();
        if order.is_empty() {
            order = (0..SLOTS.len()).map(|slot| slot_path(file, slot)).collect();
        }
        order.push(file.to_path_buf());
        return order
            .iter()
            .find_map(|path| check_one(path).ok())
            .ok_or_else(|| "no complete saved index".to_string());
    }
    check_one(file)
}

fn check_one(file: &Path) -> Result<u64, String> {
    let bytes = std::fs::read(file).map_err(|err| err.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|err| err.to_string())?;
    let mut lines = text.split('\n');
    let _format = lines.next();
    let root = lines
        .next()
        .and_then(|line| line.strip_prefix("root\t"))
        .ok_or("no root line")?;
    let damaged = || "damaged or short index file".to_string();
    if !text.starts_with(&format!("{FORMAT}\n")) {
        return parse_saved(text, root)
            .map(|saved| saved.generation)
            .ok_or_else(damaged);
    }
    verify_footer(text).ok_or_else(damaged)
}

/// The generation of a current-format file whose footer length and checksum
/// match its body; the directory lines are not parsed.
fn verify_footer(text: &str) -> Option<u64> {
    let trimmed = text.strip_suffix('\n')?;
    let start = trimmed.rfind('\n')? + 1;
    let mut footer = trimmed[start..].strip_prefix("end\t")?.splitn(2, '\t');
    let len: usize = footer.next()?.parse().ok()?;
    let sum = footer.next()?;
    if len != start || sha256_hex(&text.as_bytes()[..start]) != sum {
        return None;
    }
    let mut lines = text[..start].splitn(4, '\n');
    lines.next()?;
    lines.next()?.strip_prefix("root\t")?;
    lines.next()?.strip_prefix("gen\t")?.parse().ok()
}

fn is_index_name(file: &Path) -> bool {
    file.extension().is_some_and(|e| e == "idx")
}

/// List one directory the way [`SourceRoot::walk`] does: symlinks and unsafe
/// names are errors, other non-regular files are skipped.
fn list_dir(root: &SourceRoot, dir: &Path) -> Result<Vec<(String, bool)>, IngestError> {
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
    entries.sort_by(|left, right| crate::source::cmp_walk(&left.0, left.1, &right.0, right.1));
    Ok(entries)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn to_ns(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn now_ns() -> u64 {
    to_ns(SystemTime::now())
}

/// The saved form is plain text, one line per record. Filenames cannot hold
/// a control character (see `check_filename`), so a tab and a newline are
/// safe separators.
///
/// ```text
/// albumen-index/2
/// root<TAB><canonical library path>
/// gen<TAB><generation>
/// D<TAB><mtime ns><TAB><listed ns><TAB><relative dir>
/// f<TAB><file name>
/// d<TAB><directory name>
/// end<TAB><bytes before this line><TAB><SHA-256 of those bytes, hex>
/// ```
///
/// `albumen-index/1` files (no `gen`, no `end`) are still read.
fn render_saved(root: &str, generation: u64, dirs: &BTreeMap<String, DirRecord>) -> Vec<u8> {
    let mut out = String::with_capacity(dirs.values().map(|d| d.entries.len() * 24).sum());
    out.push_str(FORMAT);
    out.push('\n');
    out.push_str("root\t");
    out.push_str(root);
    out.push('\n');
    out.push_str(&format!("gen\t{generation}\n"));
    for (relative, record) in dirs {
        out.push_str(&format!(
            "D\t{}\t{}\t{}\n",
            record.mtime_ns, record.listed_ns, relative
        ));
        for (name, is_dir) in &record.entries {
            out.push_str(if *is_dir { "d\t" } else { "f\t" });
            out.push_str(name);
            out.push('\n');
        }
    }
    let footer = format!("end\t{}\t{}\n", out.len(), sha256_hex(out.as_bytes()));
    out.push_str(&footer);
    out.into_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn parse_saved(text: &str, root: &str) -> Option<Saved> {
    let (body, generation) = if text.starts_with(&format!("{FORMAT}\n")) {
        // The footer is the last line; the body is every byte before it.
        let trimmed = text.strip_suffix('\n')?;
        let start = trimmed.rfind('\n')? + 1;
        let mut footer = trimmed[start..].strip_prefix("end\t")?.splitn(2, '\t');
        let len: usize = footer.next()?.parse().ok()?;
        let sum = footer.next()?;
        if len != start || sha256_hex(&text.as_bytes()[..start]) != sum {
            return None;
        }
        let body = &text[..start];
        let mut lines = body.splitn(4, '\n');
        lines.next()?;
        if lines.next()?.strip_prefix("root\t")? != root {
            return None;
        }
        let generation = lines.next()?.strip_prefix("gen\t")?.parse().ok()?;
        (lines.next().unwrap_or(""), generation)
    } else {
        let mut lines = text.splitn(3, '\n');
        if lines.next()? != FORMAT_V1 {
            return None;
        }
        if lines.next()?.strip_prefix("root\t")? != root {
            return None;
        }
        (lines.next().unwrap_or(""), 0)
    };
    let mut dirs = BTreeMap::new();
    let mut current: Option<(String, DirRecord)> = None;
    for line in body.split('\n') {
        if line.is_empty() {
            continue;
        }
        let (kind, rest) = line.split_once('\t')?;
        match kind {
            "D" => {
                if let Some((relative, record)) = current.take() {
                    dirs.insert(relative, record);
                }
                let mut parts = rest.splitn(3, '\t');
                let mtime_ns = parts.next()?.parse().ok()?;
                let listed_ns = parts.next()?.parse().ok()?;
                let relative = parts.next()?.to_string();
                current = Some((
                    relative,
                    DirRecord {
                        mtime_ns,
                        listed_ns,
                        entries: Vec::new(),
                    },
                ));
            }
            "f" | "d" => current
                .as_mut()?
                .1
                .entries
                .push((rest.to_string(), kind == "d")),
            _ => return None,
        }
    }
    if let Some((relative, record)) = current.take() {
        dirs.insert(relative, record);
    }
    Some(Saved { generation, dirs })
}
