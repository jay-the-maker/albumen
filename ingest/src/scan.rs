// SPDX-License-Identifier: Apache-2.0

//! Read-only library scan.
//!
//! `scan_tree` walks a directory through [`crate::source::SourceRoot`], hashes
//! each file, and appends one JSON object per file to a manifest that must
//! live outside the source. Source files are opened `O_RDONLY` only, and on
//! Linux with `O_NOATIME` when the kernel allows that flag. It does not change
//! a source timestamp. If the size or mtime changes while a file is read, the
//! manifest line records `drift` with the before and after values. The source
//! file is left as it is. The manifest is a destination file and is written by
//! the destination module.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::checksum::{sha256_reader, to_hex};
use crate::dest;
use crate::discover::companion_names;
use crate::error::IngestError;
use crate::exiftool::exiftool_command;
use crate::mtime::to_unix_nanos_string;
use crate::source::SourceRoot;

/// Files and bytes committed by one `scan_tree` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub files: u64,
    pub bytes: u64,
}

/// One difference found by `verify_scan_manifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanDrift {
    pub path: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ScanLine {
    path: String,
    bytes: u64,
    mtime_ns: String,
    sha256: String,
    sidecars: Vec<String>,
    pairs: Vec<String>,
    metadata: KeyMetadata,
    /// Size and mtime at the start and end of the read, when those changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drift: Option<EntryDrift>,
}

/// Size and mtime observed while a file was read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ObservedStat {
    bytes: u64,
    mtime_ns: String,
}

/// A source file changed while scan was reading it.
///
/// `before` is the size and mtime at open. `after` is the size and mtime
/// after the read. Scan records both and leaves the file alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EntryDrift {
    before: ObservedStat,
    after: ObservedStat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct KeyMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    captured: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latitude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    longitude: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    captions: Vec<String>,
}

struct Job {
    index: u64,
    relative: String,
}

struct Hashed {
    index: u64,
    relative: String,
    bytes: u64,
    mtime_ns: String,
    sha256: String,
    sidecars: Vec<String>,
    pairs: Vec<String>,
    drift: Option<EntryDrift>,
}

/// Walk `source` and append a JSONL manifest at `manifest`.
///
/// The manifest is refused when its resolved path is inside `source`. A
/// manifest that already ends with complete lines is continued after the last
/// path. An incomplete final line is truncated back to the last newline.
/// Re-running the same command finishes the walk.
pub fn scan_tree(source: &SourceRoot, manifest: &Path) -> Result<ScanStats, IngestError> {
    if !source.canonical().is_dir() {
        return Err(IngestError::store(
            "scan",
            source.canonical().display().to_string(),
            "source is not a directory",
        ));
    }
    source.refuse_destination(manifest)?;
    if let Some(parent) = manifest.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }
    require_exiftool()?;
    let (after, already) = prepare_resume(manifest)?;
    let workers = scan_workers();
    let (job_tx, job_rx) = sync_channel::<Job>(workers);
    let (done_tx, done_rx) = sync_channel::<Result<Hashed, IngestError>>(workers);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let cancel = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        for _ in 0..workers {
            let rx = Arc::clone(&job_rx);
            let tx = done_tx.clone();
            let cancel = Arc::clone(&cancel);
            let source = source.clone();
            scope.spawn(move || worker_loop(&source, rx, tx, &cancel));
        }
        drop(job_rx);
        drop(done_tx);

        let cancel_writer = Arc::clone(&cancel);
        let source_writer = source.clone();
        let manifest_writer = manifest.to_path_buf();
        let writer = scope.spawn(move || {
            write_ordered(
                &source_writer,
                &manifest_writer,
                done_rx,
                already,
                &cancel_writer,
            )
        });

        let mut index = 0u64;
        let walked = walk_readonly(source, after.as_deref(), &mut |relative| {
            if cancel.load(Ordering::Relaxed) {
                return WalkStop::Stop;
            }
            let mut job = Job {
                index,
                relative: relative.to_string(),
            };
            index += 1;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return WalkStop::Stop;
                }
                match job_tx.try_send(job) {
                    Ok(()) => return WalkStop::Continue,
                    Err(TrySendError::Disconnected(_)) => return WalkStop::Stop,
                    Err(TrySendError::Full(returned)) => {
                        job = returned;
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            }
        });
        drop(job_tx);
        let written = writer.join().unwrap_or_else(|_| {
            Err(IngestError::store(
                "scan",
                manifest.display().to_string(),
                "manifest writer stopped",
            ))
        });
        walked?;
        written
    })
}

/// Re-hash `source` and report drift against a scan JSONL manifest.
///
/// The walk and the manifest are merged in path order. The source is opened
/// the same way as `scan_tree`. Metadata tags are not compared.
pub fn verify_scan_manifest(
    manifest: &Path,
    source: &SourceRoot,
    on_drift: &mut dyn FnMut(&ScanDrift),
) -> Result<(), IngestError> {
    if !source.canonical().is_dir() {
        return Err(IngestError::store(
            "verify-manifest",
            source.canonical().display().to_string(),
            "source is not a directory",
        ));
    }
    let file = File::open(manifest).map_err(|err| IngestError::io(manifest, err))?;
    let (tx, rx) = sync_channel::<Result<Option<String>, IngestError>>(8);
    let drifts = thread::scope(|scope| {
        let walker = scope.spawn(|| {
            let walked = walk_readonly(source, None, &mut |relative| {
                if tx.send(Ok(Some(relative.to_string()))).is_err() {
                    WalkStop::Stop
                } else {
                    WalkStop::Continue
                }
            });
            let _ = match walked {
                Ok(()) => tx.send(Ok(None)),
                Err(err) => tx.send(Err(err)),
            };
        });
        let compared = compare_manifest(source, manifest, file, rx, on_drift);
        walker.join().ok();
        compared
    })?;
    if drifts == 0 {
        Ok(())
    } else {
        Err(IngestError::ManifestDrift { count: drifts })
    }
}

fn worker_loop(
    source: &SourceRoot,
    jobs: Arc<Mutex<Receiver<Job>>>,
    done: std::sync::mpsc::SyncSender<Result<Hashed, IngestError>>,
    cancel: &AtomicBool,
) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let job = {
            let guard = jobs.lock().unwrap_or_else(|err| err.into_inner());
            guard.recv()
        };
        let Ok(job) = job else {
            break;
        };
        let result = hash_one(source, &job);
        if result.is_err() {
            cancel.store(true, Ordering::Relaxed);
        }
        if done.send(result).is_err() {
            break;
        }
    }
}

fn hash_one(root: &SourceRoot, job: &Job) -> Result<Hashed, IngestError> {
    let path = root.canonical().join(relative_path(&job.relative));
    let file = root.open_read(&path)?;
    let before = observe_file(&path, &file)?;
    mid_scan_touch(&path);
    let sha = to_hex(&sha256_reader(file, &path)?);
    let after = observe_path(&path)?;
    let (sidecar_names, pair_names) = companion_names(&path)?;
    let parent = parent_relative(&job.relative);
    Ok(Hashed {
        index: job.index,
        relative: job.relative.clone(),
        bytes: before.bytes,
        mtime_ns: before.mtime_ns.clone(),
        sha256: sha,
        sidecars: names_to_relative(&parent, &sidecar_names),
        pairs: names_to_relative(&parent, &pair_names),
        drift: drift_if_changed(&before, &after),
    })
}

fn write_ordered(
    source: &SourceRoot,
    manifest: &Path,
    done: Receiver<Result<Hashed, IngestError>>,
    already: u64,
    cancel: &AtomicBool,
) -> Result<ScanStats, IngestError> {
    let mut file = dest::open_append(manifest)?;
    let mut next = 0u64;
    let mut pending: BTreeMap<u64, Hashed> = BTreeMap::new();
    let mut batch = Vec::new();
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut failure: Option<IngestError> = None;
    let mut last_progress = Instant::now();
    let batch_size = exiftool_batch();

    while let Ok(item) = done.recv() {
        if failure.is_some() {
            continue;
        }
        let hashed = match item {
            Ok(hashed) => hashed,
            Err(err) => {
                cancel.store(true, Ordering::Relaxed);
                failure = Some(err);
                continue;
            }
        };
        pending.insert(hashed.index, hashed);
        while let Some(ready) = pending.remove(&next) {
            next += 1;
            bytes += ready.bytes;
            batch.push(ready);
            if batch.len() >= batch_size {
                files += flush_batch(source, &mut file, manifest, &mut batch)?;
                note_progress(already + files, bytes, &mut last_progress, false);
            }
        }
    }
    if failure.is_none() && !batch.is_empty() {
        files += flush_batch(source, &mut file, manifest, &mut batch)?;
    }
    if failure.is_none() && !pending.is_empty() {
        failure = Some(IngestError::store(
            "scan",
            manifest.display().to_string(),
            "manifest writer missed a file",
        ));
    }
    note_progress(already + files, bytes, &mut last_progress, true);
    match failure {
        Some(err) => Err(err),
        None => Ok(ScanStats { files, bytes }),
    }
}

fn flush_batch(
    source: &SourceRoot,
    file: &mut File,
    manifest: &Path,
    batch: &mut Vec<Hashed>,
) -> Result<u64, IngestError> {
    let paths: Vec<String> = batch.iter().map(|item| item.relative.clone()).collect();
    let metadata = read_metadata(source, &paths)?;
    for item in batch.iter_mut() {
        let path = source.canonical().join(relative_path(&item.relative));
        let after = observe_path(&path)?;
        extend_drift(item, after);
    }
    if metadata.len() != batch.len() {
        return Err(IngestError::store(
            "exiftool",
            source.canonical().display().to_string(),
            format!(
                "exiftool returned {} records for {} files",
                metadata.len(),
                batch.len()
            ),
        ));
    }
    let mut written = 0u64;
    for (item, meta) in batch.drain(..).zip(metadata) {
        let line = ScanLine {
            path: item.relative,
            bytes: item.bytes,
            mtime_ns: item.mtime_ns,
            sha256: item.sha256,
            sidecars: item.sidecars,
            pairs: item.pairs,
            metadata: meta,
            drift: item.drift,
        };
        writeln!(
            file,
            "{}",
            serde_json::to_string(&line).map_err(|err| {
                IngestError::store("scan", line.path.clone(), err.to_string())
            })?
        )
        .map_err(|err| IngestError::io(manifest, err))?;
        written += 1;
    }
    file.sync_all()
        .map_err(|err| IngestError::io(manifest, err))?;
    Ok(written)
}

fn note_progress(files: u64, bytes: u64, last: &mut Instant, force: bool) {
    if force || last.elapsed() >= Duration::from_secs(1) {
        eprintln!("albumen scan: {files} files, {bytes} bytes hashed");
        *last = Instant::now();
    }
}

fn compare_manifest(
    source: &SourceRoot,
    manifest_path: &Path,
    file: File,
    rx: Receiver<Result<Option<String>, IngestError>>,
    on_drift: &mut dyn FnMut(&ScanDrift),
) -> Result<u64, IngestError> {
    let mut lines = BufReader::new(file).lines();
    let mut next_disk = next_disk_path(&rx)?;
    let mut next_line = read_scan_line(manifest_path, &mut lines)?;
    let mut drifts = 0u64;
    loop {
        match (next_disk, next_line) {
            (None, None) => return Ok(drifts),
            (Some(disk), None) => {
                emit(
                    on_drift,
                    &mut drifts,
                    &disk,
                    "extra",
                    "present on disk and absent from the manifest",
                );
                next_disk = next_disk_path(&rx)?;
                next_line = None;
            }
            (None, Some(line)) => {
                emit(
                    on_drift,
                    &mut drifts,
                    &line.path,
                    "missing",
                    "present in the manifest and absent from the source",
                );
                next_disk = None;
                next_line = read_scan_line(manifest_path, &mut lines)?;
            }
            (Some(disk), Some(line)) => match disk.cmp(&line.path) {
                std::cmp::Ordering::Equal => {
                    drifts += compare_file(source, &line, on_drift)?;
                    next_disk = next_disk_path(&rx)?;
                    next_line = read_scan_line(manifest_path, &mut lines)?;
                }
                std::cmp::Ordering::Less => {
                    emit(
                        on_drift,
                        &mut drifts,
                        &disk,
                        "extra",
                        "present on disk and absent from the manifest",
                    );
                    next_disk = next_disk_path(&rx)?;
                    next_line = Some(line);
                }
                std::cmp::Ordering::Greater => {
                    emit(
                        on_drift,
                        &mut drifts,
                        &line.path,
                        "missing",
                        "present in the manifest and absent from the source",
                    );
                    next_disk = Some(disk);
                    next_line = read_scan_line(manifest_path, &mut lines)?;
                }
            },
        }
    }
}

fn next_disk_path(
    rx: &Receiver<Result<Option<String>, IngestError>>,
) -> Result<Option<String>, IngestError> {
    match rx.recv() {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(err)) => Err(err),
        Err(_) => Ok(None),
    }
}

fn compare_file(
    source: &SourceRoot,
    line: &ScanLine,
    on_drift: &mut dyn FnMut(&ScanDrift),
) -> Result<u64, IngestError> {
    let path = source.canonical().join(relative_path(&line.path));
    let file = source.open_read(&path)?;
    let before = observe_file(&path, &file)?;
    let sha = to_hex(&sha256_reader(file, &path)?);
    let after = observe_path(&path)?;
    let mut drifts = 0u64;
    if let Some(drift) = drift_if_changed(&before, &after) {
        emit(
            on_drift,
            &mut drifts,
            &line.path,
            "drift",
            &format!(
                "bytes {} -> {}, mtime {} -> {}",
                drift.before.bytes, drift.after.bytes, drift.before.mtime_ns, drift.after.mtime_ns
            ),
        );
    }
    let bytes = before.bytes;
    let mtime_ns = before.mtime_ns;
    if sha != line.sha256 {
        emit(
            on_drift,
            &mut drifts,
            &line.path,
            "sha256",
            &format!("manifest {} source {sha}", line.sha256),
        );
    }
    if bytes != line.bytes {
        emit(
            on_drift,
            &mut drifts,
            &line.path,
            "bytes",
            &format!("manifest {} source {bytes}", line.bytes),
        );
    }
    if mtime_ns != line.mtime_ns {
        emit(
            on_drift,
            &mut drifts,
            &line.path,
            "mtime",
            &format!("manifest {} source {mtime_ns}", line.mtime_ns),
        );
    }
    Ok(drifts)
}

fn emit(
    on_drift: &mut dyn FnMut(&ScanDrift),
    drifts: &mut u64,
    path: &str,
    kind: &str,
    detail: &str,
) {
    *drifts += 1;
    on_drift(&ScanDrift {
        path: path.to_string(),
        kind: kind.to_string(),
        detail: detail.to_string(),
    });
}

fn read_scan_line<R: BufRead>(
    manifest: &Path,
    lines: &mut std::io::Lines<R>,
) -> Result<Option<ScanLine>, IngestError> {
    let Some(line) = lines.next() else {
        return Ok(None);
    };
    let line = line.map_err(|err| IngestError::io(manifest, err))?;
    if line.is_empty() {
        return read_scan_line(manifest, lines);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|err| IngestError::ManifestParse {
            line_number: 0,
            line: format!("{err}: {line}"),
        })
}

fn prepare_resume(manifest: &Path) -> Result<(Option<String>, u64), IngestError> {
    if !manifest.exists() {
        return Ok((None, 0));
    }
    let mut file = dest::open_read_write(manifest)?;
    let mut len = file
        .metadata()
        .map_err(|err| IngestError::io(manifest, err))?
        .len();
    if len == 0 {
        return Ok((None, 0));
    }
    let mut window = 256 * 1024;
    loop {
        if window > len {
            window = len;
        }
        let start = len - window;
        file.seek(SeekFrom::Start(start))
            .map_err(|err| IngestError::io(manifest, err))?;
        let mut buf = vec![0u8; usize::try_from(window).unwrap_or(usize::MAX)];
        file.read_exact(&mut buf)
            .map_err(|err| IngestError::io(manifest, err))?;
        let Some(last_nl) = buf.iter().rposition(|byte| *byte == b'\n') else {
            if start == 0 {
                dest::set_len(&file, manifest, 0)?;
                file.sync_all()
                    .map_err(|err| IngestError::io(manifest, err))?;
                return Ok((None, 0));
            }
            window = (window.saturating_mul(2)).min(len);
            continue;
        };
        let complete_end = last_nl + 1;
        let body_end = last_nl;
        let line_start = buf[..body_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        if line_start == 0 && start > 0 {
            window = (window.saturating_mul(2)).min(len);
            continue;
        }
        if complete_end != buf.len() {
            let new_len = start + complete_end as u64;
            dest::set_len(&file, manifest, new_len)?;
            file.sync_all()
                .map_err(|err| IngestError::io(manifest, err))?;
            len = new_len;
        }
        let line = &buf[line_start..body_end];
        let parsed: ScanLine =
            serde_json::from_slice(line).map_err(|err| IngestError::ManifestParse {
                line_number: 0,
                line: format!("{err}: {}", String::from_utf8_lossy(line)),
            })?;
        let count = count_newlines(&mut file, manifest, len)?;
        return Ok((Some(parsed.path), count));
    }
}

fn count_newlines(file: &mut File, manifest: &Path, len: u64) -> Result<u64, IngestError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|err| IngestError::io(manifest, err))?;
    let mut remaining = len;
    let mut count = 0u64;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        file.read_exact(&mut buf[..want])
            .map_err(|err| IngestError::io(manifest, err))?;
        count += buf[..want].iter().filter(|byte| **byte == b'\n').count() as u64;
        remaining -= want as u64;
    }
    Ok(count)
}

fn require_exiftool() -> Result<(), IngestError> {
    match exiftool_command().arg("-ver").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(IngestError::store(
            "exiftool",
            "exiftool".to_string(),
            format!(
                "exiftool -ver failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )),
        Err(err) => Err(IngestError::store(
            "exiftool",
            "exiftool".to_string(),
            format!("exiftool is required to record key metadata: {err}"),
        )),
    }
}

fn read_metadata(
    root: &SourceRoot,
    relative_paths: &[String],
) -> Result<Vec<KeyMetadata>, IngestError> {
    let output = exiftool_command()
        .current_dir(root.canonical())
        .arg("-json")
        .arg("-n")
        .arg("-s")
        .arg("-api")
        .arg("largefilesupport=1")
        .args(relative_paths)
        .output()
        .map_err(|err| {
            IngestError::store(
                "exiftool",
                root.canonical().display().to_string(),
                format!("exiftool is required to record key metadata: {err}"),
            )
        })?;
    // ExifTool exits 1 for an empty file or a file of zeros, and still prints
    // one JSON object per path. A complete array is a successful read.
    let items = match serde_json::from_slice::<Vec<Map<String, Value>>>(&output.stdout) {
        Ok(items) if items.len() == relative_paths.len() => items,
        _ => {
            return Err(IngestError::store(
                "exiftool",
                root.canonical().display().to_string(),
                format!(
                    "exiftool failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
    };
    if items.len() != relative_paths.len() {
        return Err(IngestError::store(
            "exiftool",
            root.canonical().display().to_string(),
            format!(
                "exiftool returned {} records for {} files",
                items.len(),
                relative_paths.len()
            ),
        ));
    }
    let mut records = Vec::with_capacity(items.len());
    for (relative, item) in relative_paths.iter().zip(items) {
        let source = item
            .get("SourceFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches("./");
        if source != relative {
            return Err(IngestError::store(
                "exiftool",
                relative.clone(),
                format!("exiftool reported {source}"),
            ));
        }
        records.push(metadata_from_map(&item));
    }
    Ok(records)
}

fn metadata_from_map(item: &Map<String, Value>) -> KeyMetadata {
    let make = first_string(item, &["Make"]);
    let model = first_string(item, &["Model", "CameraModelName", "UniqueCameraModel"]);
    let camera = match (make, model) {
        (Some(make), Some(model))
            if !model
                .to_ascii_lowercase()
                .contains(&make.to_ascii_lowercase()) =>
        {
            Some(format!("{make} {model}"))
        }
        (Some(make), None) => Some(make),
        (_, Some(model)) => Some(model),
        (None, None) => None,
    };
    KeyMetadata {
        captured: first_string(
            item,
            &[
                "DateTimeOriginal",
                "CreateDate",
                "DateCreated",
                "MediaCreateDate",
                "CreationDate",
                "DateTimeDigitized",
            ],
        ),
        camera,
        lens: first_string(item, &["LensModel", "LensMake", "Lens", "LensType"]),
        latitude: first_number(item, &["GPSLatitude"]),
        longitude: first_number(item, &["GPSLongitude"]),
        keywords: string_list(item, &["Keywords", "Subject", "HierarchicalSubject"]),
        captions: string_list(
            item,
            &[
                "Description",
                "ImageDescription",
                "Caption-Abstract",
                "Title",
            ],
        ),
    }
}

fn first_string(item: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(Value::String(text)) = item.get(*key) {
            if !text.is_empty() {
                return Some(text.clone());
            }
        }
    }
    None
}

fn first_number(item: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        match item.get(*key) {
            Some(Value::Number(number)) => return Some(number.to_string()),
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            _ => {}
        }
    }
    None
}

fn string_list(item: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        if let Some(value) = item.get(*key) {
            push_strings(value, &mut out);
        }
    }
    out
}

fn push_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !text.is_empty() && !out.iter().any(|existing| existing == text) {
                out.push(text.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                push_strings(item, out);
            }
        }
        Value::Number(number) => {
            let text = number.to_string();
            if !out.iter().any(|existing| existing == &text) {
                out.push(text);
            }
        }
        _ => {}
    }
}

fn exiftool_batch() -> usize {
    match std::env::var("ALBUMEN_EXIFTOOL_BATCH") {
        Ok(text) => text
            .parse::<usize>()
            .ok()
            .filter(|value| *value >= 1)
            .unwrap_or(64),
        Err(_) => 64,
    }
}

fn scan_workers() -> usize {
    if let Ok(text) = std::env::var("ALBUMEN_SCAN_WORKERS") {
        if let Ok(value) = text.parse::<usize>() {
            if value >= 1 {
                return value;
            }
        }
    }
    thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
}

fn observe_file(path: &Path, file: &File) -> Result<ObservedStat, IngestError> {
    let meta = file.metadata().map_err(|err| IngestError::io(path, err))?;
    observed(path, &meta)
}

fn observe_path(path: &Path) -> Result<ObservedStat, IngestError> {
    let meta = fs::symlink_metadata(path).map_err(|err| IngestError::io(path, err))?;
    observed(path, &meta)
}

fn observed(path: &Path, meta: &std::fs::Metadata) -> Result<ObservedStat, IngestError> {
    let mtime = meta.modified().map_err(|err| IngestError::io(path, err))?;
    Ok(ObservedStat {
        bytes: meta.len(),
        mtime_ns: to_unix_nanos_string(mtime),
    })
}

fn drift_if_changed(before: &ObservedStat, after: &ObservedStat) -> Option<EntryDrift> {
    if before.bytes == after.bytes && before.mtime_ns == after.mtime_ns {
        None
    } else {
        Some(EntryDrift {
            before: before.clone(),
            after: after.clone(),
        })
    }
}

fn extend_drift(item: &mut Hashed, after: ObservedStat) {
    let started = ObservedStat {
        bytes: item.bytes,
        mtime_ns: item.mtime_ns.clone(),
    };
    if started.bytes == after.bytes && started.mtime_ns == after.mtime_ns {
        return;
    }
    let before = item
        .drift
        .as_ref()
        .map(|drift| drift.before.clone())
        .unwrap_or(started);
    item.drift = Some(EntryDrift { before, after });
}

/// Test seam. Production scans do not use it. A unit test sets it so a file
/// can change after the open stat and before the hash, without scan writing
/// the source back.
#[cfg(test)]
type MidScanHook = Box<dyn FnMut(&Path) + Send>;

#[cfg(test)]
static MID_SCAN: Mutex<Option<MidScanHook>> = Mutex::new(None);

#[cfg(test)]
fn mid_scan_touch(path: &Path) {
    let mut guard = MID_SCAN.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(hook) = guard.as_mut() {
        hook(path);
    }
}

#[cfg(not(test))]
fn mid_scan_touch(_path: &Path) {}

fn parent_relative(relative: &str) -> String {
    match relative.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

fn names_to_relative(parent: &str, names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| {
            if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            }
        })
        .collect()
}

fn relative_path(relative: &str) -> PathBuf {
    relative.split('/').collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkStop {
    Continue,
    Stop,
}

fn walk_readonly(
    root: &SourceRoot,
    after: Option<&str>,
    visit: &mut dyn FnMut(&str) -> WalkStop,
) -> Result<(), IngestError> {
    root.walk(after, &mut |relative| visit(relative) == WalkStop::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_file, Tmp};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn as_root(path: &Path) -> SourceRoot {
        SourceRoot::open(path).unwrap()
    }

    struct ModeGuard {
        #[cfg(unix)]
        saved: Vec<(PathBuf, u32)>,
        #[cfg(not(unix))]
        saved: Vec<PathBuf>,
    }

    impl Drop for ModeGuard {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for (path, mode) in self.saved.iter().rev() {
                    let Ok(meta) = fs::symlink_metadata(path) else {
                        continue;
                    };
                    let mut perms = meta.permissions();
                    perms.set_mode(*mode);
                    let _ = fs::set_permissions(path, perms);
                }
            }
            #[cfg(not(unix))]
            {
                for path in self.saved.iter().rev() {
                    let Ok(meta) = fs::symlink_metadata(path) else {
                        continue;
                    };
                    if meta.is_file() {
                        let mut perms = meta.permissions();
                        // This block is not compiled on Unix. Clearing the
                        // Windows read-only attribute restores the fixture.
                        #[allow(clippy::permissions_set_readonly_false)]
                        {
                            perms.set_readonly(false);
                        }
                        let _ = fs::set_permissions(path, perms);
                    }
                }
            }
        }
    }

    fn lock_tree(root: &Path) -> ModeGuard {
        let mut stack = vec![root.to_path_buf()];
        let mut paths = Vec::new();
        while let Some(path) = stack.pop() {
            let meta = fs::symlink_metadata(&path).unwrap();
            paths.push(path.clone());
            if meta.is_dir() {
                for entry in fs::read_dir(&path).unwrap() {
                    stack.push(entry.unwrap().path());
                }
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let saved = paths
                .iter()
                .map(|path| {
                    let mode = fs::symlink_metadata(path).unwrap().mode();
                    (path.clone(), mode)
                })
                .collect::<Vec<_>>();
            for (path, mode) in &saved {
                let mut perms = fs::symlink_metadata(path).unwrap().permissions();
                perms.set_mode(mode & !0o222);
                fs::set_permissions(path, perms).unwrap();
            }
            ModeGuard { saved }
        }
        #[cfg(not(unix))]
        {
            for path in &paths {
                let meta = fs::symlink_metadata(path).unwrap();
                if meta.is_file() {
                    let mut perms = meta.permissions();
                    perms.set_readonly(true);
                    fs::set_permissions(path, perms).unwrap();
                }
            }
            ModeGuard { saved: paths }
        }
    }

    fn allow_write(path: &Path) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o644);
        }
        #[cfg(not(unix))]
        {
            // This block is not compiled on Unix. Clearing the Windows
            // read-only attribute lets the test change the fixture bytes.
            #[allow(clippy::permissions_set_readonly_false)]
            {
                perms.set_readonly(false);
            }
        }
        fs::set_permissions(path, perms).unwrap();
    }

    /// What scan must leave untouched. A read may update access time, and
    /// scan does not write that time back, so access time is not compared.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Snap {
        rel: String,
        is_dir: bool,
        len: u64,
        sha: String,
        mtime: SystemTime,
        #[cfg(unix)]
        mode: u32,
        #[cfg(not(unix))]
        readonly: bool,
    }

    fn list_rels(root: &Path) -> Vec<String> {
        let mut rels = Vec::new();
        fn walk(dir: &Path, prefix: &str, rels: &mut Vec<String>) {
            let mut names: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            for name in names {
                let name = name.to_str().unwrap().to_string();
                let rel = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                rels.push(rel.clone());
                let path = dir.join(&name);
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    walk(&path, &rel, rels);
                }
            }
        }
        walk(root, "", &mut rels);
        rels
    }

    fn capture(root: &Path, rels: &[String]) -> Vec<Snap> {
        let mut paths = vec![String::new()];
        paths.extend(rels.iter().cloned());
        paths
            .into_iter()
            .map(|rel| {
                let path = if rel.is_empty() {
                    root.to_path_buf()
                } else {
                    root.join(relative_path(&rel))
                };
                let meta = fs::symlink_metadata(&path).unwrap();
                let sha = if meta.is_file() {
                    let opened = SourceRoot::open(root).unwrap();
                    let file = opened.open_read(&path).unwrap();
                    to_hex(&sha256_reader(file, &path).unwrap())
                } else {
                    String::new()
                };
                Snap {
                    rel,
                    is_dir: meta.is_dir(),
                    len: meta.len(),
                    sha,
                    mtime: meta.modified().unwrap(),
                    #[cfg(unix)]
                    mode: {
                        use std::os::unix::fs::MetadataExt;
                        meta.mode()
                    },
                    #[cfg(not(unix))]
                    readonly: meta.permissions().readonly(),
                }
            })
            .collect()
    }

    struct HookGuard;

    impl HookGuard {
        fn install(hook: impl FnMut(&Path) + Send + 'static) -> Self {
            *MID_SCAN.lock().unwrap_or_else(|err| err.into_inner()) = Some(Box::new(hook));
            Self
        }
    }

    impl Drop for HookGuard {
        fn drop(&mut self) {
            *MID_SCAN.lock().unwrap_or_else(|err| err.into_inner()) = None;
        }
    }

    fn fixture(dir: &Path) {
        let mtime = UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700);
        fs::create_dir_all(dir.join("trip")).unwrap();
        write_file(dir, "a.txt", b"alpha", mtime);
        write_file(&dir.join("trip"), "IMG.jpg", b"jpeg-bytes", mtime);
        write_file(&dir.join("trip"), "IMG.dng", b"raw-bytes", mtime);
        write_file(&dir.join("trip"), "IMG.xmp", b"<xmp/>", mtime);
    }

    #[test]
    fn refuses_a_manifest_inside_the_source() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        write_file(
            &source,
            "a.txt",
            b"alpha",
            UNIX_EPOCH + Duration::from_secs(1_400_000_000),
        );
        let manifest = source.join("nested").join("out.jsonl");
        let err = scan_tree(&as_root(&source), &manifest).unwrap_err();
        assert!(
            matches!(err, IngestError::DestinationInsideSource { .. }),
            "{err}"
        );
        assert!(!source.join("nested").exists());
    }

    #[test]
    fn scan_of_a_read_only_tree_does_not_change_it() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        fixture(&source);
        let manifest = tmp.path().join("out").join("scan.jsonl");
        let _guard = lock_tree(&source);
        let rels = list_rels(&source);
        let before = capture(&source, &rels);
        let stats = scan_tree(&as_root(&source), &manifest).unwrap();
        let after = capture(&source, &rels);
        assert_eq!(before, after);
        assert_eq!(stats.files, 4);
        let lines = fs::read_to_string(&manifest).unwrap();
        let records: Vec<ScanLine> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            records
                .iter()
                .map(|line| line.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt", "trip/IMG.dng", "trip/IMG.jpg", "trip/IMG.xmp"]
        );
        let jpeg = records
            .iter()
            .find(|line| line.path == "trip/IMG.jpg")
            .unwrap();
        assert_eq!(jpeg.pairs, vec!["trip/IMG.dng".to_string()]);
        assert_eq!(jpeg.sidecars, vec!["trip/IMG.xmp".to_string()]);
        assert_eq!(jpeg.bytes, b"jpeg-bytes".len() as u64);
        assert!(jpeg.drift.is_none(), "{jpeg:?}");
        verify_scan_manifest(&manifest, &as_root(&source), &mut |_| ()).unwrap();
    }

    #[test]
    fn file_modified_mid_scan_keeps_its_new_mtime_and_records_drift() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        let mtime = UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700);
        let path = write_file(&source, "a.txt", b"alpha", mtime)
            .canonicalize()
            .unwrap();
        let manifest = tmp.path().join("scan.jsonl");
        let new_mtime = UNIX_EPOCH + Duration::new(1_700_000_000, 100);
        let _hook = HookGuard::install({
            let path = path.clone();
            move |opened: &Path| {
                let Ok(opened) = opened.canonicalize() else {
                    return;
                };
                if opened == path {
                    fs::write(&path, b"alphabet").unwrap();
                    crate::mtime::set_mtime(&path, new_mtime).unwrap();
                }
            }
        });

        let stats = scan_tree(&as_root(&source), &manifest).unwrap();
        drop(_hook);

        assert_eq!(stats.files, 1);
        assert_eq!(fs::read(&path).unwrap(), b"alphabet");
        let left = fs::symlink_metadata(&path).unwrap().modified().unwrap();
        assert_eq!(left, new_mtime);
        let line: ScanLine = serde_json::from_str(
            fs::read_to_string(&manifest)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let drift = line.drift.expect("manifest records drift");
        assert_eq!(drift.before.bytes, b"alpha".len() as u64);
        assert_eq!(drift.before.mtime_ns, to_unix_nanos_string(mtime));
        assert_eq!(drift.after.bytes, b"alphabet".len() as u64);
        assert_eq!(drift.after.mtime_ns, to_unix_nanos_string(new_mtime));
        assert_eq!(line.bytes, drift.before.bytes);
        assert_eq!(line.mtime_ns, drift.before.mtime_ns);
    }

    #[test]
    fn scan_resumes_after_the_last_complete_line() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        write_file(&source, "a.txt", b"alpha", mtime);
        write_file(&source, "b.txt", b"beta", mtime);
        let manifest = tmp.path().join("scan.jsonl");
        let _guard = lock_tree(&source);
        let rels = list_rels(&source);
        let before = capture(&source, &rels);
        scan_tree(&as_root(&source), &manifest).unwrap();
        let text = fs::read_to_string(&manifest).unwrap();
        let end = text.trim_end_matches('\n').rfind('\n').unwrap();
        fs::write(&manifest, &text[..=end]).unwrap();
        let stats = scan_tree(&as_root(&source), &manifest).unwrap();
        assert_eq!(stats.files, 1);
        let paths: Vec<String> = fs::read_to_string(&manifest)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<ScanLine>(line).unwrap().path)
            .collect();
        assert_eq!(paths, vec!["a.txt".to_string(), "b.txt".to_string()]);
        assert_eq!(capture(&source, &rels), before);
    }

    #[test]
    fn scan_records_an_empty_file() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("empty.jpg"), b"").unwrap();
        let manifest = tmp.path().join("scan.jsonl");
        let stats = scan_tree(&as_root(&source), &manifest).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 0);
        verify_scan_manifest(&manifest, &as_root(&source), &mut |_| ()).unwrap();
    }

    #[test]
    fn verify_reports_sha256_drift() {
        let tmp = Tmp::new();
        let source = tmp.path().join("library");
        fs::create_dir_all(&source).unwrap();
        let mtime = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        write_file(&source, "a.txt", b"alpha", mtime);
        let manifest = tmp.path().join("scan.jsonl");
        {
            let _guard = lock_tree(&source);
            scan_tree(&as_root(&source), &manifest).unwrap();
        }
        allow_write(&source.join("a.txt"));
        fs::write(source.join("a.txt"), b"alphb").unwrap();
        let mut seen = Vec::new();
        let err = verify_scan_manifest(&manifest, &as_root(&source), &mut |drift| {
            seen.push(drift.clone())
        })
        .unwrap_err();
        assert!(matches!(err, IngestError::ManifestDrift { .. }), "{err}");
        assert!(
            seen.iter()
                .any(|drift| drift.kind == "sha256" && drift.path == "a.txt"),
            "{seen:?}"
        );
    }
}
