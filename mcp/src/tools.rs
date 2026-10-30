// SPDX-License-Identifier: Apache-2.0

//! Library tools. Search, browse, and ledger only read. Export and upload call
//! `copy_verify` and refuse to overwrite. Nothing here deletes a source or
//! renames one into quarantine.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use albumen_ingest::index::LibraryIndex;
use albumen_ingest::{
    copy_verify, discover, export_tree, walk_sorted, write_manifest, DirectoryTree, ExportDest,
    IngestError, LocalStore, S3Config, S3Store, SourceRoot, WalkControl,
};
use serde_json::{json, Value};

use crate::metadata::{self, Filters, GeoBox};
use crate::mount::{self, is_within};
use crate::page::{Hit, Page};

pub struct Library {
    root: PathBuf,
    destination: Option<PathBuf>,
    pub enable_export: bool,
    pub enable_upload: bool,
    pub apply: bool,
    metadata: Mutex<metadata::Cache>,
    index_dir: Option<PathBuf>,
    index: Mutex<Option<LibraryIndex>>,
    thumbnail_dir: Option<PathBuf>,
    recheck_budget: std::time::Duration,
}

impl Library {
    pub fn new(
        root: PathBuf,
        enable_export: bool,
        enable_upload: bool,
        apply: bool,
        destination: Option<PathBuf>,
    ) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("library is not a directory: {}", root.display()));
        }
        let destination = if enable_export || enable_upload {
            let destination = destination.ok_or_else(|| {
                "--enable-export and --enable-upload require --destination, a directory on a different mount from --library".to_string()
            })?;
            if !destination.is_dir() {
                return Err(format!(
                    "destination is not a directory: {}",
                    destination.display()
                ));
            }
            mount::require_separate_destination(&root, &destination)?;
            Some(destination)
        } else {
            destination
        };
        albumen_ingest::ledger::anchor(&root, "export");
        Ok(Self {
            root,
            destination,
            enable_export,
            enable_upload,
            apply,
            metadata: Mutex::new(metadata::Cache::default()),
            index_dir: albumen_ingest::index::default_index_dir(),
            index: Mutex::new(None),
            thumbnail_dir: crate::thumbnail::default_thumbnail_dir(),
            recheck_budget: std::time::Duration::from_millis(
                std::env::var("ALBUMEN_SEARCH_RECHECK_MS")
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .unwrap_or(10_000),
            ),
        })
    }

    /// Save the search index in `dir` instead of the platform data directory.
    /// `None` keeps it in memory only. A directory inside the library is
    /// refused when the index is opened.
    /// How long one search may spend comparing known files with the disk
    /// before it answers from the search tree with a "checked against disk
    /// at T; N folders not rechecked" label. `ALBUMEN_SEARCH_RECHECK_MS`
    /// sets it for the binary; the default is 10 s.
    pub fn with_recheck_budget(mut self, budget: std::time::Duration) -> Self {
        self.recheck_budget = budget;
        self
    }

    pub fn with_index_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.index_dir = dir;
        self
    }

    /// Cache thumbnails in `dir` instead of the platform data directory.
    /// `None` keeps no cache. A directory inside the library is refused.
    pub fn with_thumbnail_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.thumbnail_dir = dir;
        self
    }

    /// Open the search index now, so the first query does not wait for it.
    /// Returns the open time in milliseconds and whether it was loaded.
    pub fn warm_index(&self) -> Result<(f64, bool), String> {
        let mut slot = self.index.lock().unwrap_or_else(|err| err.into_inner());
        let index = self.current_index(&mut slot)?;
        Ok((index.open_ms, index.loaded))
    }

    fn current_index<'a>(
        &self,
        slot: &'a mut Option<LibraryIndex>,
    ) -> Result<&'a LibraryIndex, String> {
        match slot {
            Some(index) => {
                index.refresh().map_err(show)?;
            }
            None => {
                let index = match LibraryIndex::open(&self.root, self.index_dir.as_deref()) {
                    Ok(index) => index,
                    Err(IngestError::DestinationInsideSource { .. }) => {
                        return Err(format!(
                        "the index directory is inside the library; choose --index-dir outside {}",
                        self.root.display()
                    ))
                    }
                    Err(err) => return Err(show(err)),
                };
                if let Some(err) = index.save_error.as_ref() {
                    eprintln!("albumen-mcp: search index kept in memory only: {err}");
                }
                *slot = Some(index);
            }
        }
        Ok(slot.as_ref().expect("index set above"))
    }

    /// The `freshness` object for a tool error, when the error is search's
    /// state D (the library root could not be read).
    pub fn error_freshness(&self, name: &str, err: &str) -> Option<Value> {
        (name == "search" && err == unreachable_text(&self.root))
            .then(|| unreachable_freshness(&self.root))
    }

    pub fn tool_defs(&self) -> Vec<Value> {
        let mut tools = vec![search_def(), browse_def(), ledger_def(), thumbnail_def()];
        if self.enable_export {
            tools.push(export_def());
        }
        if self.enable_upload {
            tools.push(upload_def());
        }
        tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        tools
    }

    pub fn call(&self, name: &str, arguments: &Value) -> Result<Value, String> {
        match name {
            "search" => self.search(arguments),
            "browse" => self.browse(arguments),
            "ledger" => self.ledger(arguments),
            "thumbnail" => {
                let path = required_string(arguments, "path")?;
                let size = match arguments.get("size") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_u64()
                            .ok_or_else(|| "size must be a positive integer".to_string())?,
                    ),
                };
                crate::thumbnail::thumbnail(&self.root, self.thumbnail_dir.as_deref(), path, size)
            }
            "export" => {
                if !self.enable_export {
                    return Err(
                        "export is disabled; start albumen-mcp with --enable-export".to_string()
                    );
                }
                self.export(arguments)
            }
            "upload" => {
                if !self.enable_upload {
                    return Err(
                        "upload is disabled; start albumen-mcp with --enable-upload".to_string()
                    );
                }
                self.upload(arguments)
            }
            "delete" | "remove" | "unlink" | "move" | "move_verified" => {
                Err("albumen has no delete tool".to_string())
            }
            "quarantine" | "copy_verify_quarantine" => {
                Err("copy_verify_quarantine is not a default path".to_string())
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    fn search(&self, arguments: &Value) -> Result<Value, String> {
        reject_unknown_search_keys(arguments)?;
        let query = optional_string(arguments, "query")?;
        if query.is_some_and(|query| query.is_empty()) {
            return Err("query must not be empty".to_string());
        }
        let filters = metadata_filters(arguments)?;
        if query.is_none() && filters.is_none() {
            return Err("search needs a query or a metadata filter".to_string());
        }
        let query = query.map(str::to_lowercase);
        let limit = optional_limit(arguments)?.unwrap_or(default_page("ALBUMEN_SEARCH_PAGE", 1000));
        let after = optional_cursor(arguments)?;
        let began = std::time::Instant::now();
        let deadline = began.checked_add(self.recheck_budget);
        let mut page = Page::default();
        // Borrowed from the index: no per-file copy until a hit.
        let mut pending: Vec<&str> = Vec::new();
        // One stat pool run per flush; ExifTool batches inside it.
        let flush_at = metadata::exiftool_batch().max(65_536);
        // State D: a library root that cannot be listed (an unmounted drive,
        // a removed folder) gives a tool error, never an empty answer.
        if fs::read_dir(&self.root).is_err() {
            return Err(unreachable_text(&self.root));
        }
        let mut cache = self.metadata.lock().unwrap_or_else(|err| err.into_inner());
        let hashed_before = cache.hashed();
        let mut unchecked: Vec<metadata::Unchecked> = Vec::new();
        let mut failed: Option<String> = None;
        let mut index_slot = self.index.lock().unwrap_or_else(|err| err.into_inner());
        let index = self.current_index(&mut index_slot)?;
        let root_id = crate::search_tree::root_id(&self.root);
        let mut tree_status = "none".to_string();
        if filters.is_some() {
            if let Some(file) = index.file() {
                tree_status = cache.open_tree(crate::search_tree::path_for(file), &root_id);
            }
        }
        let flush = |cache: &mut metadata::Cache,
                     pending: &mut Vec<&str>,
                     page: &mut Page,
                     unchecked: &mut Vec<metadata::Unchecked>|
         -> Result<(), String> {
            flush_metadata(
                &self.root,
                cache,
                filters.as_ref().expect("metadata search"),
                limit,
                pending,
                page,
                deadline,
                unchecked,
            )
        };
        let paths = index.paths();
        let mut next = after
            .as_deref()
            .map(|after| paths.partition_point(|path| path.as_str() <= after))
            .unwrap_or(0);
        index.visit_after(after.as_deref(), &mut |relative, lower| {
            // visit_after walks paths[start..] in order; keep the borrow.
            let slot = next;
            next += 1;
            if query.as_ref().is_some_and(|query| !lower.contains(query)) {
                return true;
            }
            if filters.is_none() {
                let bytes = match file_len(&self.root, relative) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        failed = Some(err);
                        return false;
                    }
                };
                let stop = page.consider(
                    limit,
                    Hit {
                        path: relative.to_string(),
                        bytes,
                        record: None,
                    },
                );
                return !stop;
            }
            let relative: &str = match paths.get(slot) {
                Some(path) if std::ptr::eq(path.as_str(), relative) => path,
                _ => {
                    failed = Some("index walk out of step".to_string());
                    return false;
                }
            };
            pending.push(relative);
            if pending.len() >= flush_at {
                if let Err(err) = flush(&mut cache, &mut pending, &mut page, &mut unchecked) {
                    failed = Some(err);
                    return false;
                }
                if page.truncated {
                    return false;
                }
            }
            true
        });
        if let Some(err) = failed {
            return Err(err);
        }
        if !pending.is_empty() && !page.truncated && filters.is_some() {
            flush(&mut cache, &mut pending, &mut page, &mut unchecked)?;
        }
        if filters.is_some() {
            let known: std::collections::HashSet<&str> = if cache.dirty {
                index.paths().iter().map(String::as_str).collect()
            } else {
                Default::default()
            };
            if let Err(err) = cache.save_tree(&root_id, |path| known.contains(path)) {
                eprintln!("albumen-mcp: search tree kept in memory only: {err}");
            }
        }
        drop(index_slot);
        let mut value = page_value(&page);
        // Every answer carries freshness; nothing is returned unlabeled.
        // A page cut by `limit`, or reached with a cursor, is one page of
        // an uncounted total: the label says so.
        let paginated = page.truncated || after.is_some();
        value["freshness"] = freshness(page.hits.len(), paginated, &unchecked, &tree_status);
        if std::env::var("ALBUMEN_SEARCH_STATS").is_ok_and(|v| v == "1") {
            // Diagnostics for library-scale.py and tests.
            value["stats"] = json!({
                "hashed": cache.hashed() - hashed_before,
                "unchecked_files": unchecked.len(),
                "tree": tree_status,
                "ms": began.elapsed().as_secs_f64() * 1000.0,
            });
        }
        Ok(value)
    }

    fn browse(&self, arguments: &Value) -> Result<Value, String> {
        let prefix = optional_string(arguments, "prefix")?
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        let limit = optional_limit(arguments)?.unwrap_or(default_page("ALBUMEN_BROWSE_PAGE", 200));
        let after = optional_cursor(arguments)?;
        let mut page = Page::default();
        let mut failed: Option<String> = None;
        walk_sorted(&self.root, after.as_deref(), &mut |relative| {
            if !under_prefix(relative, &prefix) {
                return WalkControl::Continue;
            }
            let bytes = match file_len(&self.root, relative) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failed = Some(err);
                    return WalkControl::Stop;
                }
            };
            let stop = page.consider(
                limit,
                Hit {
                    path: relative.to_string(),
                    bytes,
                    record: None,
                },
            );
            if stop {
                WalkControl::Stop
            } else {
                WalkControl::Continue
            }
        })
        .map_err(show)?;
        if let Some(err) = failed {
            return Err(err);
        }
        Ok(page_value(&page))
    }

    fn export(&self, arguments: &Value) -> Result<Value, String> {
        let to = required_string(arguments, "to")?;
        let manifest = match optional_string(arguments, "manifest")? {
            Some(path) => PathBuf::from(path),
            None if to.starts_with("s3://") => {
                return Err("pass manifest when exporting to S3".to_string());
            }
            None => Path::new(to).join("ALBUMEN-MANIFEST.txt"),
        };
        let source = SourceRoot::open(&self.root).map_err(show)?;
        if !to.starts_with("s3://") {
            source.refuse_destination(Path::new(to)).map_err(show)?;
            self.require_on_destination(Path::new(to))?;
            self.require_on_destination(&manifest)?;
            albumen_ingest::family::refuse_shared_write_root(Path::new(to)).map_err(show)?;
        }
        source.refuse_destination(&manifest).map_err(show)?;
        if !self.apply {
            return export_plan(&source, to, &manifest);
        }
        albumen_ingest::ledger::anchor(&self.root, "export");
        let dest = open_target(to)?;
        let entries = export_tree(&source, dest.as_ref()).map_err(show)?;
        if !to.starts_with("s3://") {
            if let Ok(relative) = manifest.strip_prefix(to) {
                let relative = relative_text(relative);
                if entries.iter().any(|entry| entry.relative_path == relative) {
                    return Err(format!(
                        "refusing to overwrite exported file {relative} with the manifest"
                    ));
                }
            }
        }
        write_manifest(&manifest, &entries).map_err(show)?;
        Ok(json!({
            "files": entries.len(),
            "manifest": manifest.display().to_string(),
            "destination": to,
        }))
    }

    fn ledger(&self, arguments: &Value) -> Result<Value, String> {
        if arguments.as_object().is_some_and(|map| !map.is_empty()) {
            return Err("ledger takes no arguments".to_string());
        }
        let ledger = albumen_ingest::ledger::build(&self.root, None).map_err(show)?;
        serde_json::to_value(ledger).map_err(|err| err.to_string())
    }

    fn upload(&self, arguments: &Value) -> Result<Value, String> {
        albumen_ingest::family::refuse_shared_write_root(&self.root).map_err(show)?;
        albumen_ingest::family::refuse_shared_write_root(self.write_root()?).map_err(show)?;
        let path = PathBuf::from(required_string(arguments, "path")?);
        let asset = discover(&path).map_err(show)?;
        if !self.apply {
            let files = asset
                .files()
                .iter()
                .map(|file| {
                    file.path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>();
            return Ok(json!({
                "dry_run": true,
                "into": self.write_root()?.display().to_string(),
                "files": files,
                "network": "none",
            }));
        }
        let report = copy_verify(&asset, &LocalStore::new(self.write_root()?)).map_err(show)?;
        let files = report
            .files
            .iter()
            .map(|file| {
                json!({
                    "filename": file.filename,
                    "sha256": file.sha256,
                    "bytes": file.byte_len,
                    "role": match file.role {
                        albumen_ingest::Role::Primary => "primary",
                        albumen_ingest::Role::Sidecar => "sidecar",
                        albumen_ingest::Role::Pair => "pair",
                    },
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"files": files}))
    }

    fn write_root(&self) -> Result<&Path, String> {
        self.destination.as_deref().ok_or_else(|| {
            "--enable-export and --enable-upload require --destination, a directory on a different mount from --library".to_string()
        })
    }

    fn require_on_destination(&self, path: &Path) -> Result<(), String> {
        let destination = self.write_root()?;
        if is_within(destination, path)? {
            return Ok(());
        }
        Err(format!(
            "refusing to write {} because it is not on the --destination mount {}",
            path.display(),
            destination.display()
        ))
    }
}

fn export_plan(root: &SourceRoot, to: &str, manifest: &Path) -> Result<Value, String> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    walk_sorted(root.canonical(), None, &mut |relative| {
        let full = root.canonical().join(relative);
        bytes += root
            .metadata_nofollow(&full)
            .map(|meta| meta.len())
            .unwrap_or(0);
        files += 1;
        WalkControl::Continue
    })
    .map_err(show)?;
    let network = if to.starts_with("s3://") {
        "on --apply, connect to ALBUMEN_S3_ENDPOINT and write objects"
    } else {
        "none"
    };
    Ok(json!({
        "dry_run": true,
        "destination": to,
        "manifest": manifest.display().to_string(),
        "files": files,
        "bytes": bytes,
        "network": network,
    }))
}

fn show(err: IngestError) -> String {
    err.to_string()
}

fn file_len(root: &Path, relative: &str) -> Result<u64, String> {
    let full = root.join(PathBuf::from_iter(relative.split('/')));
    fs::metadata(&full)
        .map(|meta| meta.len())
        .map_err(|err| format!("{relative}: {err}"))
}

fn under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Test hook: `ALBUMEN_SEARCH_CLOCK_NS` (nanoseconds since the epoch) pins
/// the time T shown in every freshness label and in `checked_at`. It does not
/// change the clock used for hashing or the racy window.
const CLOCK_ENV: &str = "ALBUMEN_SEARCH_CLOCK_NS";

fn pinned_clock_ns() -> Option<i128> {
    std::env::var(CLOCK_ENV).ok()?.parse().ok()
}

/// "3 matches", "1 match", "0 matches"; on one page of a paginated answer
/// "3 matches on this page". The total is not counted: search stops at
/// `limit`, so counting it would mean reading every later page.
fn matches_text(matches: usize, paginated: bool) -> String {
    let count = if matches == 1 {
        "1 match".to_string()
    } else {
        format!("{matches} matches")
    };
    if paginated {
        format!("{count} on this page")
    } else {
        count
    }
}

/// The `freshness` object every search answer carries (freshness label copy
/// spec v1, states A to C). `label` is line 1 and `next` the next: line, word
/// for word; T in `label` is local time with a numeric offset, and
/// `checked_at` is the same instant in RFC 3339 UTC.
///
/// - partial (B): some files were not compared with the disk; T is the
///   oldest check among them.
/// - rebuilt (C): the saved search tree was missing ("no saved index yet")
///   or damaged, another version or for another root ("the saved index was
///   rebuilt"), so every folder was read; T is now. Partial wins over it.
/// - fresh (A): every folder was compared with the disk; T is now. This
///   includes a memory-only index: every file is stat'ed unless the budget
///   runs out, and then the answer is partial.
///
/// `tree` is the search tree status ("discarded: missing", "loaded", ...).
pub fn freshness(
    matches: usize,
    paginated: bool,
    unchecked: &[metadata::Unchecked],
    tree: &str,
) -> Value {
    let pinned = pinned_clock_ns();
    let m = matches_text(matches, paginated);
    if let Some(oldest) = unchecked.iter().map(|item| item.checked_ns).min() {
        let at = pinned.unwrap_or(oldest);
        let folders: std::collections::BTreeSet<&str> = unchecked
            .iter()
            .map(|item| item.path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(""))
            .collect();
        let n = folders.len();
        let (count, which) = if n == 1 {
            ("1 folder".to_string(), "that folder")
        } else {
            (format!("{n} folders"), "those folders")
        };
        return json!({
            "state": "partial",
            "checked_at": utc_text(at),
            "folders_not_rechecked": n,
            "label": format!(
                "search: {m}, checked against disk at {}; {count} not rechecked",
                local_text(at)
            ),
            "next": format!("next: run the same search again to recheck {which}"),
        });
    }
    let at = pinned.unwrap_or_else(metadata::now_ns);
    let (state, label) = if tree.starts_with("discarded") {
        let why = if tree == "discarded: missing" {
            "no saved index yet"
        } else {
            "the saved index was rebuilt"
        };
        (
            "rebuilt",
            format!(
                "search: {m}, read every folder at {} ({why})",
                local_text(at)
            ),
        )
    } else {
        (
            "fresh",
            format!("search: {m}, checked against disk at {}", local_text(at)),
        )
    };
    json!({
        "state": state,
        "checked_at": utc_text(at),
        "folders_not_rechecked": 0,
        "label": label,
        "next": null,
    })
}

/// State D: the library root cannot be read. Search answers with a tool
/// error whose text is line 1 and the next: line.
pub fn unreachable_lines(root: &Path) -> (String, String) {
    (
        format!(
            "search: could not read {}; no results shown",
            root.display()
        ),
        "next: connect the drive or check the path, then search again".to_string(),
    )
}

pub fn unreachable_text(root: &Path) -> String {
    let (label, next) = unreachable_lines(root);
    format!("{label}\n{next}")
}

pub fn unreachable_freshness(root: &Path) -> Value {
    let (label, next) = unreachable_lines(root);
    json!({
        "state": "unreachable",
        "checked_at": null,
        "folders_not_rechecked": 0,
        "label": label,
        "next": next,
    })
}

/// "2026-10-29 17:42 UTC-7": local time from `TZ` with the numeric offset
/// localtime gives for that instant, "UTC+5:30" for part-hour zones and
/// plain "UTC" at zero; the same format as `albumen_ingest::clock::local_stamp`.
/// Zone abbreviations such as PDT are never printed. On Windows see the
/// `cfg(windows)` version below. Never a bare time without a zone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn local_text(ns: i128) -> String {
    extern "C" {
        fn tzset();
    }
    let secs = ns.div_euclid(1_000_000_000) as libc::time_t;
    // SAFETY: tzset only re-reads TZ; localtime_r writes into `tm`, which we
    // own, and returns null on failure.
    let tm = unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return utc_minutes(ns);
        }
        tm
    };
    // Real offsets are within +-14 h; anything else means a bad conversion.
    // tm_gmtoff is i64 on 64-bit Linux and macOS but narrower on some targets.
    #[allow(clippy::useless_conversion)]
    let offset_min = i64::from(tm.tm_gmtoff).div_euclid(60);
    if offset_min.abs() > 14 * 60 {
        return utc_minutes(ns);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        offset_text(offset_min)
    )
}

/// Windows: "2026-10-29 17:42 UTC-7": local time with the numeric offset
/// the OS applies at that instant (its current zone and that year's DST
/// rules), "UTC+5:30" when the offset is not whole hours. Zone names and
/// abbreviations such as PDT are never printed. If the OS cannot convert,
/// "2026-10-30 00:42 UTC".
#[cfg(windows)]
fn local_text(ns: i128) -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{
        GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx,
        DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
    };
    let secs = ns.div_euclid(1_000_000_000) as i64;
    let full = utc_text(ns);
    let num = |a: usize, b: usize| full[a..b].parse::<u16>().unwrap_or(0);
    let utc = SYSTEMTIME {
        wYear: num(0, 4),
        wMonth: num(5, 7),
        wDayOfWeek: 0,
        wDay: num(8, 10),
        wHour: num(11, 13),
        wMinute: num(14, 16),
        wSecond: num(17, 19),
        wMilliseconds: 0,
    };
    // SAFETY: both structs are plain data we own; the calls only write into
    // them and report failure through their return values.
    let local = unsafe {
        let mut zone: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut zone) == TIME_ZONE_ID_INVALID {
            return utc_minutes(ns);
        }
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTimeEx(&zone, &utc, &mut local) == 0 {
            return utc_minutes(ns);
        }
        local
    };
    let local_secs = days_from_civil(
        i64::from(local.wYear),
        i64::from(local.wMonth),
        i64::from(local.wDay),
    ) * 86_400
        + i64::from(local.wHour) * 3600
        + i64::from(local.wMinute) * 60
        + i64::from(local.wSecond);
    // Real offsets are within +-14 h; anything else means a bad conversion.
    let offset_min = (local_secs - secs).div_euclid(60);
    if offset_min.abs() > 14 * 60 {
        return utc_minutes(ns);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {}",
        local.wYear,
        local.wMonth,
        local.wDay,
        local.wHour,
        local.wMinute,
        offset_text(offset_min)
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn local_text(ns: i128) -> String {
    utc_minutes(ns)
}

/// "UTC", "UTC-7", "UTC+5:30", "UTC-9:30" from minutes east of UTC.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    allow(dead_code)
)]
fn offset_text(minutes: i64) -> String {
    if minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
#[cfg_attr(not(windows), allow(dead_code))]
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-10-30 00:42 UTC".
fn utc_minutes(ns: i128) -> String {
    let full = utc_text(ns);
    format!("{} {} UTC", &full[..10], &full[11..16])
}

fn utc_text(ns: i128) -> String {
    let secs = ns.div_euclid(1_000_000_000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[allow(clippy::too_many_arguments)]
fn flush_metadata(
    root: &Path,
    cache: &mut metadata::Cache,
    filters: &Filters,
    limit: u64,
    pending: &mut Vec<&str>,
    page: &mut Page,
    deadline: Option<std::time::Instant>,
    unchecked: &mut Vec<metadata::Unchecked>,
) -> Result<(), String> {
    let stats = metadata::stat_all(root, pending, deadline);
    let mut extra: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut found = Vec::with_capacity(pending.len());
    let mut last_parent: Option<&str> = None;
    let mut dir_has_sidecars = false;
    for path in pending.iter() {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        if last_parent != Some(parent) {
            dir_has_sidecars = cache.refresh_dir(root, parent);
            last_parent = Some(parent);
        }
        if !dir_has_sidecars {
            found.push(Vec::new());
            continue;
        }
        let sidecars = cache.listed_sidecars(root, path);
        for sidecar in &sidecars {
            extra.insert(sidecar.clone());
        }
        found.push(sidecars);
    }
    let mut unkept = std::collections::HashMap::new();
    let mut verdicts = Vec::new();
    let lengths = cache.refresh_stated(
        root,
        pending,
        stats,
        &mut unkept,
        unchecked,
        &|i, record| found[i].is_empty().then(|| filters.matches(record)),
        &mut verdicts,
    )?;
    if !extra.is_empty() {
        let primary: std::collections::HashSet<&str> = pending.iter().copied().collect();
        let mut extra: Vec<String> = extra
            .into_iter()
            .filter(|path| !primary.contains(path.as_str()))
            .collect();
        extra.sort();
        unkept.extend(cache.refresh(root, &extra)?);
    }
    let cache = &*cache;
    for (((path, bytes), sidecars), verdict) in
        pending.drain(..).zip(lengths).zip(found).zip(verdicts)
    {
        if verdict == Some(false) {
            continue;
        }
        let Some(own) = cache.record(&unkept, path) else {
            continue;
        };
        let matched = if verdict == Some(true) {
            Some(own.clone())
        } else if sidecars.is_empty() {
            filters.matches(own).then(|| own.clone())
        } else {
            let merged =
                metadata::with_found_sidecars(own, &sidecars, |path| cache.record(&unkept, path));
            filters.matches(&merged).then_some(merged)
        };
        let Some(merged) = matched else {
            continue;
        };
        if page.consider(
            limit,
            Hit {
                path: path.to_string(),
                bytes,
                record: Some(merged),
            },
        ) {
            return Ok(());
        }
    }
    Ok(())
}

fn page_value(page: &Page) -> Value {
    let files = page
        .hits
        .iter()
        .map(|hit| {
            let mut file = json!({"path": hit.path, "bytes": hit.bytes});
            if let Some(record) = &hit.record {
                attach_metadata(&mut file, record);
            }
            file
        })
        .collect::<Vec<_>>();
    let mut value = json!({"files": files, "truncated": page.truncated});
    if let Some(cursor) = page.next_cursor() {
        value["next_cursor"] = Value::String(cursor.to_string());
    }
    value
}

fn default_page(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn open_target(spec: &str) -> Result<Box<dyn ExportDest>, String> {
    if let Some(rest) = spec.strip_prefix("s3://") {
        let rest = rest.trim_end_matches('/');
        let (bucket, prefix) = match rest.split_once('/') {
            Some((bucket, prefix)) => (bucket.to_string(), prefix.to_string()),
            None => (rest.to_string(), String::new()),
        };
        if bucket.is_empty()
            || (!prefix.is_empty()
                && prefix
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == ".."))
        {
            return Err(format!("bad s3 target {spec}"));
        }
        let mut config = S3Config::from_env().map_err(show)?;
        config.bucket = bucket;
        config.prefix = prefix;
        let store = S3Store::connect(config).map_err(show)?;
        Ok(Box::new(store))
    } else {
        Ok(Box::new(DirectoryTree::new(spec)))
    }
}

fn relative_text(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn arguments_object(arguments: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    match arguments {
        Value::Object(map) => Ok(map),
        Value::Null => Err("arguments must be an object".to_string()),
        _ => Err("arguments must be an object".to_string()),
    }
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    match arguments_object(arguments)?.get(key) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(format!("{key} must be a string")),
        None => Err(format!("missing {key}")),
    }
}

fn optional_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments_object(arguments)?.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn optional_cursor(arguments: &Value) -> Result<Option<String>, String> {
    match optional_string(arguments, "cursor")? {
        None => Ok(None),
        Some("") => Err("cursor must not be empty".to_string()),
        Some(text) => Ok(Some(text.to_string())),
    }
}

fn optional_limit(arguments: &Value) -> Result<Option<u64>, String> {
    match arguments_object(arguments)?.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| "limit must be a non-negative integer".to_string()),
        Some(_) => Err("limit must be a non-negative integer".to_string()),
    }
}

fn attach_metadata(file: &mut Value, record: &metadata::Record) {
    file["captured"] = record
        .captured
        .map(|time| Value::String(time.to_text()))
        .unwrap_or(Value::Null);
    file["camera"] = optional_text(record.camera.as_deref());
    file["lens"] = optional_text(record.lens.as_deref());
    file["latitude"] = optional_number(record.latitude);
    file["longitude"] = optional_number(record.longitude);
    file["keywords"] = Value::Array(record.keywords.iter().cloned().map(Value::String).collect());
    file["captions"] = Value::Array(record.captions.iter().cloned().map(Value::String).collect());
}

fn optional_text(text: Option<&str>) -> Value {
    match text {
        Some(text) => Value::String(text.to_string()),
        None => Value::Null,
    }
}

fn optional_number(number: Option<f64>) -> Value {
    match number {
        Some(number) => json!(number),
        None => Value::Null,
    }
}

fn reject_unknown_search_keys(arguments: &Value) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "query",
        "limit",
        "cursor",
        "captured_after",
        "captured_before",
        "camera",
        "lens",
        "keyword",
        "caption",
        "gps",
    ];
    for key in arguments_object(arguments)?.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(format!("unknown search field {key}"));
        }
    }
    Ok(())
}

fn metadata_filters(arguments: &Value) -> Result<Option<Filters>, String> {
    let captured_after = optional_string(arguments, "captured_after")?
        .map(|text| metadata::parse_user_time(text, false))
        .transpose()?;
    let captured_before = optional_string(arguments, "captured_before")?
        .map(|text| metadata::parse_user_time(text, true))
        .transpose()?;
    let camera = optional_nonempty(arguments, "camera")?;
    let lens = optional_nonempty(arguments, "lens")?;
    let keyword = optional_nonempty(arguments, "keyword")?;
    let caption = optional_nonempty(arguments, "caption")?;
    let gps = optional_gps(arguments)?;
    if captured_after.is_none()
        && captured_before.is_none()
        && camera.is_none()
        && lens.is_none()
        && keyword.is_none()
        && caption.is_none()
        && gps.is_none()
    {
        return Ok(None);
    }
    Ok(Some(Filters {
        captured_after,
        captured_before,
        camera,
        lens,
        keyword,
        caption,
        gps,
    }))
}

fn optional_nonempty(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match optional_string(arguments, key)? {
        None => Ok(None),
        Some("") => Err(format!("{key} must not be empty")),
        Some(text) => Ok(Some(text.to_string())),
    }
}

fn optional_gps(arguments: &Value) -> Result<Option<GeoBox>, String> {
    match arguments_object(arguments)?.get("gps") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => {
            for key in map.keys() {
                if !["south", "north", "west", "east"].contains(&key.as_str()) {
                    return Err(format!("unknown gps field {key}"));
                }
            }
            let south = required_f64(map, "south")?;
            let north = required_f64(map, "north")?;
            let west = required_f64(map, "west")?;
            let east = required_f64(map, "east")?;
            metadata::parse_geo_box(south, north, west, east).map(Some)
        }
        Some(_) => Err("gps must be an object".to_string()),
    }
}

fn required_f64(map: &serde_json::Map<String, Value>, key: &str) -> Result<f64, String> {
    match map.get(key) {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("gps {key} must be a finite number")),
        Some(_) => Err(format!("gps {key} must be a number")),
        None => Err(format!("gps is missing {key}")),
    }
}

fn search_def() -> Value {
    json!({
        "name": "search",
        "description": "Find library files by path, capture date, camera, lens, GPS box, keyword, or caption. Metadata is read from the files and their XMP/AAE sidecars. Results are one page. Pass next_cursor to read the next page. Every answer carries freshness {state, checked_at, folders_not_rechecked, label, next}: label and next are the exact lines to show. Nothing is written. On Windows, an edit is picked up by the first search after the editing app closes the file. If an app still has a photo open, search may show its previous version until the app closes it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Case-insensitive substring of the relative path"},
                "captured_after": {"type": "string", "description": "Inclusive capture date or time, YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS"},
                "captured_before": {"type": "string", "description": "Inclusive capture date or time. A date alone includes that whole day."},
                "camera": {"type": "string", "description": "Case-insensitive substring of make and model"},
                "lens": {"type": "string", "description": "Case-insensitive substring of the lens name"},
                "keyword": {"type": "string", "description": "Case-insensitive substring of an XMP subject or IPTC keyword"},
                "caption": {"type": "string", "description": "Case-insensitive substring of an XMP/IPTC caption or description"},
                "gps": {
                    "type": "object",
                    "description": "Inclusive latitude/longitude box. If west is greater than east, the box crosses the antimeridian.",
                    "properties": {
                        "south": {"type": "number"},
                        "north": {"type": "number"},
                        "west": {"type": "number"},
                        "east": {"type": "number"}
                    },
                    "required": ["south", "north", "west", "east"],
                    "additionalProperties": false
                },
                "limit": {"type": "integer", "minimum": 0, "description": "Page size. Soft default is 1000, or ALBUMEN_SEARCH_PAGE. There is no maximum."},
                "cursor": {"type": "string", "description": "Return matches strictly after this relative path."}
            },
            "additionalProperties": false
        }
    })
}

fn thumbnail_def() -> Value {
    json!({
        "name": "thumbnail",
        "description": "Return a small JPEG preview of one library JPEG as an MCP image content block (type image, mimeType image/jpeg), followed by a text block with path, size and cache state. The original is read, not changed. Previews are cached outside the library.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the library"},
                "size": {"type": "integer", "minimum": 1, "description": "Longest edge in pixels. Default 256."}
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

fn ledger_def() -> Value {
    json!({
        "name": "ledger",
        "description": "Read every stored file in the library, every analysis that ran on-device, and every network destination recorded beside the library. The read is local. Nothing is written and no connection is opened.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

fn browse_def() -> Value {
    json!({
        "name": "browse",
        "description": "List library files under an optional relative prefix. Results are one page. Pass next_cursor to read the next page. Reads only. Does not modify the library.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prefix": {"type": "string"},
                "limit": {"type": "integer", "minimum": 0, "description": "Page size. Soft default is 200, or ALBUMEN_BROWSE_PAGE. There is no maximum."},
                "cursor": {"type": "string", "description": "Return files strictly after this relative path."}
            },
            "additionalProperties": false
        }
    })
}

fn export_def() -> Value {
    json!({
        "name": "export",
        "description": "Copy the library, sidecars, and pairs to a directory on --destination, or to an s3:// prefix, with copy_verify. Skips identical files. Never overwrites. Does not modify the library. Returns a plan and writes nothing unless albumen-mcp was started with --apply or --execute.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "manifest": {"type": "string", "description": "Required for s3:// destinations"}
            },
            "required": ["to"],
            "additionalProperties": false
        }
    })
}

fn upload_def() -> Value {
    json!({
        "name": "upload",
        "description": "Copy a file and its discovered sidecars and pairs into --destination with copy_verify. Never overwrites. Does not modify the library or the source. Returns a plan and writes nothing unless albumen-mcp was started with --apply or --execute.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Primary file to copy into --destination"}
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn offsets_are_numeric_never_abbreviations() {
        use super::{days_from_civil, offset_text};
        assert_eq!(offset_text(-420), "UTC-7");
        assert_eq!(offset_text(-480), "UTC-8");
        assert_eq!(offset_text(0), "UTC");
        assert_eq!(offset_text(60), "UTC+1");
        assert_eq!(offset_text(330), "UTC+5:30");
        assert_eq!(offset_text(-570), "UTC-9:30");
        assert_eq!(offset_text(345), "UTC+5:45");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 10, 30), 1_793_320_920 / 86_400);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
    }

    use super::*;

    #[test]
    fn browse_and_search_pages_cover_every_file_once() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..25 {
            fs::write(root.path().join(format!("f{index:02}.jpg")), b"x").unwrap();
        }
        let library = Library::new(root.path().to_path_buf(), false, false, false, None).unwrap();
        let seen = collect_pages(&library, "browse", 10);
        assert_eq!(seen.len(), 25, "{seen:?}");
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique, seen);
        let found = collect_pages(&library, "search", 10);
        assert_eq!(found, seen);
    }

    fn collect_pages(library: &Library, tool: &str, limit: u64) -> Vec<String> {
        let mut cursor = None;
        let mut seen = Vec::new();
        for _ in 0..10 {
            let mut args = json!({"limit": limit});
            if tool == "search" {
                args["query"] = json!("f");
            }
            if let Some(cursor) = &cursor {
                args["cursor"] = json!(cursor);
            }
            let page = library.call(tool, &args).unwrap();
            let files = page["files"].as_array().unwrap();
            assert!(files.len() as u64 <= limit);
            for file in files {
                seen.push(file["path"].as_str().unwrap().to_string());
            }
            if !page["truncated"].as_bool().unwrap() {
                assert!(page.get("next_cursor").is_none());
                return seen;
            }
            cursor = Some(page["next_cursor"].as_str().unwrap().to_string());
        }
        panic!("pagination did not finish: {seen:?}");
    }
}
