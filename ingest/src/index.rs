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

const FORMAT: &str = "albumen-index/1";

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
    /// A save that failed. The index still answers from memory.
    pub save_error: Option<String>,
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
        };
        if let Some(saved) = index.read_saved() {
            index.dirs = saved;
            index.loaded = true;
        }
        index.refresh()?;
        index.open_ms = began.elapsed().as_secs_f64() * 1000.0;
        Ok(index)
    }

    fn read_saved(&self) -> Option<BTreeMap<String, DirRecord>> {
        let file = self.file.as_ref()?;
        let text = std::fs::read_to_string(file).ok()?;
        parse_saved(&text, &self.root.canonical().to_string_lossy())
    }

    /// Stat every indexed directory and list again the ones that changed.
    pub fn refresh(&mut self) -> Result<RefreshStats, IngestError> {
        let mut stats = RefreshStats::default();
        let now = now_ns();
        let changed = self.sync_dir(String::new(), now, &mut stats)?;
        // Drop records for directories that are no longer reachable.
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
        let before = self.dirs.len();
        self.dirs.retain(|dir, _| reachable.contains(dir));
        let pruned = self.dirs.len() != before;
        stats.changed = changed || pruned;
        if stats.changed || (self.paths.is_empty() && !self.dirs.is_empty()) {
            self.flatten();
        }
        if stats.dirs_listed > 0 || pruned {
            if let Err(err) = self.save() {
                self.save_error = Some(err.to_string());
            }
        }
        Ok(stats)
    }

    fn sync_dir(
        &mut self,
        relative: String,
        now: u64,
        stats: &mut RefreshStats,
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
            changed |= self.sync_dir(child, now, stats)?;
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

    fn flatten(&mut self) {
        let mut paths = Vec::new();
        let mut stack: Vec<(String, usize)> = vec![(String::new(), 0)];
        while let Some((dir, position)) = stack.pop() {
            let Some(record) = self.dirs.get(&dir) else {
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
        self.lower = paths.iter().map(|path| path.to_lowercase()).collect();
        self.paths = paths;
    }

    fn save(&self) -> Result<(), IngestError> {
        let Some(file) = self.file.as_ref() else {
            return Ok(());
        };
        let bytes = render_saved(&self.root.canonical().to_string_lossy(), &self.dirs);
        if let Some(parent) = file.parent() {
            dest::create_dir_all(parent)?;
        }
        let partial = file.with_extension(format!("idx.partial-{}", std::process::id()));
        let mut handle = dest::create_file(&partial)?;
        handle
            .write_all(&bytes)
            .map_err(|err| IngestError::io(&partial, err))?;
        drop(handle);
        dest::rename(&partial, file)
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
/// albumen-index/1
/// root<TAB><canonical library path>
/// D<TAB><mtime ns><TAB><listed ns><TAB><relative dir>
/// f<TAB><file name>
/// d<TAB><directory name>
/// ```
fn render_saved(root: &str, dirs: &BTreeMap<String, DirRecord>) -> Vec<u8> {
    let mut out = String::with_capacity(dirs.values().map(|d| d.entries.len() * 24).sum());
    out.push_str(FORMAT);
    out.push('\n');
    out.push_str("root\t");
    out.push_str(root);
    out.push('\n');
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
    out.into_bytes()
}

fn parse_saved(text: &str, root: &str) -> Option<BTreeMap<String, DirRecord>> {
    let mut lines = text.split('\n');
    if lines.next()? != FORMAT {
        return None;
    }
    if lines.next()?.strip_prefix("root\t")? != root {
        return None;
    }
    let mut dirs = BTreeMap::new();
    let mut current: Option<(String, DirRecord)> = None;
    for line in lines {
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
    Some(dirs)
}
