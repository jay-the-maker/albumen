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
use std::io::Write;
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
pub fn load(file: &Path, root_id: &str) -> Result<HashMap<String, Entry>, String> {
    let text = match std::fs::read(file) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| "not utf-8".to_string())?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err("missing".to_string()),
        Err(err) => return Err(format!("unreadable: {err}")),
    };
    let mut head = text.splitn(4, '\n');
    let format = head.next().unwrap_or_default();
    if format != FORMAT {
        return Err(format!("wrong version {format:?}"));
    }
    let root = head.next().unwrap_or_default();
    if root.strip_prefix("root\t") != Some(root_id) {
        return Err("another root or device".to_string());
    }
    let check = head
        .next()
        .and_then(|line| line.strip_prefix("sha256\t"))
        .ok_or("no checksum")?;
    let (want, count) = check.split_once('\t').ok_or("no entry count")?;
    let count: usize = count.parse().map_err(|_| "bad entry count")?;
    let body = head.next().ok_or("truncated")?;
    if hex(&sha2::Sha256::digest(body.as_bytes())) != want {
        return Err("checksum mismatch".to_string());
    }
    let mut entries = HashMap::with_capacity(count);
    for line in body.lines() {
        let (path, entry) = parse_line(line).ok_or("bad entry")?;
        entries.insert(path, entry);
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
    let mut rows: Vec<String> = entries
        .map(|(path, entry)| render_line(path, entry))
        .collect();
    rows.sort_unstable();
    let mut body = String::with_capacity(rows.iter().map(|row| row.len() + 1).sum());
    for row in &rows {
        body.push_str(row);
        body.push('\n');
    }
    let head = format!(
        "{FORMAT}\nroot\t{root_id}\nsha256\t{}\t{}\n",
        hex(&sha2::Sha256::digest(body.as_bytes())),
        rows.len()
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension(format!("search.tmp-{}", std::process::id()));
    let written = (|| {
        let mut out = std::fs::File::create(&temp)?;
        out.write_all(head.as_bytes())?;
        if std::env::var(CRASH_ENV).is_ok_and(|value| value == "before-rename") {
            out.write_all(&body.as_bytes()[..body.len() / 2])?;
            out.sync_all()?;
            std::process::abort();
        }
        out.write_all(body.as_bytes())?;
        out.sync_all()?;
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
