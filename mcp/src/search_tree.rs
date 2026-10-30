// SPDX-License-Identifier: Apache-2.0

//! The persisted search tree: the metadata cache written beside the index so
//! a restarted server does not re-read every file.
//!
//! Search only. verify-manifest, backup, status and export never open it;
//! they SHA-256 every byte every time.
//!
//! ```text
//! albumen-search-tree/1
//! root<TAB><canonical library path as JSON><TAB><device>
//! sha256<TAB><hex of the body><TAB><entry count>
//! [path, dev, ino, size, "mtime ns", "ctime ns", settled, "checked ns", sha, record]
//! ...
//! ```
//!
//! A file that is missing, truncated, checksum-bad, another version, or for
//! another root or device is discarded whole, never trusted in part. The
//! next search then re-reads every file and writes a new tree: temp file,
//! fsync, rename, so a kill mid-write leaves the old tree or the new one.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::Digest;

use crate::metadata::{Entry, Record, StatKey, WallTime};

pub const FORMAT: &str = "albumen-search-tree/1";

/// Test hook: `ALBUMEN_SEARCH_TREE_CRASH=before-rename` aborts the process
/// after the temp file is half written, as a kill mid-write would.
pub const CRASH_ENV: &str = "ALBUMEN_SEARCH_TREE_CRASH";

/// The tree file for an index file `<key>.idx`: `<key>.search`.
pub fn path_for(index_file: &Path) -> PathBuf {
    index_file.with_extension("search")
}

/// What the root line must say: the canonical library path and its device.
pub fn root_id(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let device = device_of(&canonical);
    format!(
        "{}\t{device}",
        Value::String(canonical.to_string_lossy().into_owned())
    )
}

#[cfg(unix)]
fn device_of(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|meta| meta.dev()).unwrap_or(0)
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> u64 {
    0
}

/// Read a tree. `Err` says why it was discarded; a missing file is
/// `Err("missing")`.
///
/// Streams the file a line at a time and checks the body sha256 as it goes,
/// so the whole file never sits in memory beside the parsed entries (at 380k
/// files the text alone is ~90 MiB). A checksum or count mismatch at the end
/// still discards everything: whole or not at all.
pub fn load(file: &Path, root_id: &str) -> Result<HashMap<String, Entry>, String> {
    let handle = match std::fs::File::open(file) {
        Ok(handle) => handle,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err("missing".to_string()),
        Err(err) => return Err(format!("unreadable: {err}")),
    };
    let mut reader = std::io::BufReader::with_capacity(1 << 20, handle);
    let mut line = String::new();
    let next_line = |reader: &mut std::io::BufReader<std::fs::File>,
                     line: &mut String|
     -> Result<bool, String> {
        line.clear();
        match reader.read_line(line) {
            Ok(0) => Ok(false),
            Ok(_) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                Err("not utf-8".to_string())
            }
            Err(err) => Err(format!("unreadable: {err}")),
        }
    };
    let head_line = |line: &str| line.strip_suffix('\n').unwrap_or(line).to_string();
    next_line(&mut reader, &mut line)?;
    let format = head_line(&line);
    if format != FORMAT {
        return Err(format!("wrong version {format:?}"));
    }
    next_line(&mut reader, &mut line)?;
    if head_line(&line).strip_prefix("root\t") != Some(root_id) {
        return Err("another root or device".to_string());
    }
    if !next_line(&mut reader, &mut line)? {
        return Err("no checksum".to_string());
    }
    let check = head_line(&line);
    let check = check.strip_prefix("sha256\t").ok_or("no checksum")?;
    let (want, count) = check.split_once('\t').ok_or("no entry count")?;
    let want = want.to_string();
    let count: usize = count.parse().map_err(|_| "bad entry count")?;
    let mut hasher = sha2::Sha256::new();
    let mut entries = HashMap::with_capacity(count);
    let mut any = false;
    while next_line(&mut reader, &mut line)? {
        any = true;
        hasher.update(line.as_bytes());
        let row = line.strip_suffix('\n').unwrap_or(&line);
        let row = row.strip_suffix('\r').unwrap_or(row);
        let (path, entry) = parse_line(row).ok_or("bad entry")?;
        entries.insert(path, entry);
    }
    if !any {
        return Err("truncated".to_string());
    }
    if hex(&hasher.finalize()) != want {
        return Err("checksum mismatch".to_string());
    }
    if entries.len() != count {
        return Err("entry count mismatch".to_string());
    }
    Ok(entries)
}

/// Write `entries` atomically: temp file beside `file`, fsync, rename.
pub fn save<'a>(
    file: &Path,
    root_id: &str,
    entries: impl Iterator<Item = (&'a String, &'a Entry)>,
) -> std::io::Result<()> {
    // Sort references by path and render each row when it is needed (once
    // for the hash, once for the write) instead of holding every rendered
    // row: at 380k files the rows alone were ~90 MiB on top of the map.
    let mut rows: Vec<(&'a String, &'a Entry)> = entries.collect();
    rows.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut hasher = sha2::Sha256::new();
    for (path, entry) in &rows {
        hasher.update(render_line(path, entry).as_bytes());
        hasher.update(b"\n");
    }
    let head = format!(
        "{FORMAT}\nroot\t{root_id}\nsha256\t{}\t{}\n",
        hex(&hasher.finalize()),
        rows.len()
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension(format!("search.tmp-{}", std::process::id()));
    let written = (|| {
        let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&temp)?);
        out.write_all(head.as_bytes())?;
        let crash = std::env::var(CRASH_ENV).is_ok_and(|value| value == "before-rename");
        let stop = if crash { rows.len() / 2 } else { rows.len() };
        for (path, entry) in &rows[..stop] {
            out.write_all(render_line(path, entry).as_bytes())?;
            out.write_all(b"\n")?;
        }
        let out = out.into_inner().map_err(|err| err.into_error())?;
        out.sync_all()?;
        if crash {
            std::process::abort();
        }
        drop(out);
        std::fs::rename(&temp, file)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn render_line(path: &str, entry: &Entry) -> String {
    let record = &entry.record;
    let time = record
        .captured
        .map(|t| json!([t.year, t.month, t.day, t.hour, t.minute, t.second, t.nanos]));
    let row = json!([
        path,
        entry.stat.device,
        entry.stat.inode,
        entry.stat.size,
        entry.stat.mtime_ns.to_string(),
        entry.stat.ctime_ns.to_string(),
        entry.settled,
        entry.checked_ns.to_string(),
        entry.sha,
        {
            "t": time,
            "c": record.camera,
            "l": record.lens,
            "la": record.latitude.map(f64::to_bits),
            "lo": record.longitude.map(f64::to_bits),
            "k": record.keywords,
            "p": record.captions,
        }
    ]);
    row.to_string()
}

fn parse_line(line: &str) -> Option<(String, Entry)> {
    let row: Value = serde_json::from_str(line).ok()?;
    let row = row.as_array()?;
    if row.len() != 10 {
        return None;
    }
    let ns = |value: &Value| value.as_str()?.parse::<i128>().ok();
    let strings = |value: &Value| -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect()
    };
    let opt_string = |value: &Value| -> Option<Option<String>> {
        match value {
            Value::Null => Some(None),
            Value::String(text) => Some(Some(text.clone())),
            _ => None,
        }
    };
    let opt_f64 = |value: &Value| -> Option<Option<f64>> {
        match value {
            Value::Null => Some(None),
            other => Some(Some(f64::from_bits(other.as_u64()?))),
        }
    };
    let record = row[9].as_object()?;
    let captured = match record.get("t")? {
        Value::Null => None,
        Value::Array(parts) if parts.len() == 7 => {
            let n = |i: usize| parts[i].as_i64();
            Some(WallTime {
                year: i32::try_from(n(0)?).ok()?,
                month: u8::try_from(n(1)?).ok()?,
                day: u8::try_from(n(2)?).ok()?,
                hour: u8::try_from(n(3)?).ok()?,
                minute: u8::try_from(n(4)?).ok()?,
                second: u8::try_from(n(5)?).ok()?,
                nanos: u32::try_from(n(6)?).ok()?,
            })
        }
        _ => return None,
    };
    let entry = Entry {
        stat: StatKey {
            device: row[1].as_u64()?,
            inode: row[2].as_u64()?,
            size: row[3].as_u64()?,
            mtime_ns: ns(&row[4])?,
            ctime_ns: ns(&row[5])?,
        },
        settled: row[6].as_bool()?,
        checked_ns: ns(&row[7])?,
        sha: row[8].as_str()?.to_string(),
        record: Record {
            captured,
            camera: opt_string(record.get("c")?)?,
            lens: opt_string(record.get("l")?)?,
            latitude: opt_f64(record.get("la")?)?,
            longitude: opt_f64(record.get("lo")?)?,
            keywords: strings(record.get("k")?)?,
            captions: strings(record.get("p")?)?,
        },
    };
    Some((row[0].as_str()?.to_string(), entry))
}
