// SPDX-License-Identifier: Apache-2.0

//! A received upload is a temporary file until `copy_verify_file` accepts it.
//! A different file with the same camera filename is stored under another name.
//! The camera filename and its directory are written to a sidecar. An identical
//! byte string is not stored twice. A failed upload deletes only the spool.
//! If the receipt sidecar cannot be written, the verified original stays and a
//! tombstone is written beside it. `hide` does the same: the original is not
//! unlinked.
//!
//! `source_mtime` and the stored file's mtime are written once, inside the
//! upload that created the file. A later offer is appended to the sidecar and
//! does not replace the first. A sidecar is replaced by an fsynced rename in
//! the same directory, never by deleting it first.

use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use albumen_ingest::{copy_verify_file, dest, sha256_hex, VerifiedObject};

const SIDECAR_SUFFIX: &str = ".albumen.json";
const TOMBSTONE_SUFFIX: &str = ".albumen-tombstone";

#[derive(Debug)]
pub struct Library {
    root: PathBuf,
    commit_lock: Mutex<()>,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        dest::create_dir_all(&root).map_err(|err| io::Error::other(err.to_string()))?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "library path is not a directory",
            ));
        }
        Ok(Self {
            root,
            commit_lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn has(&self, filename: &str) -> bool {
        self.root.join(filename).is_file()
    }

    pub fn is_listed_file(name: &str) -> bool {
        !name.ends_with(SIDECAR_SUFFIX)
    }

    pub fn files(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if Self::is_listed_file(name) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Store `spool` without overwriting any existing bytes.
    ///
    /// The same SHA-256 is returned as [`Commit::Deduped`] and nothing new is
    /// written. A different file whose camera filename is already taken is
    /// stored under a free name, and the camera filename stays in the sidecar.
    pub fn commit(
        &self,
        spool: &mut Spool,
        arrival: &Arrival,
        protocol: MtimeProtocol,
    ) -> Result<Commit, String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        spool.finish()?;
        let digest = sha256_hex(spool.path()).map_err(|err| err.to_string())?;
        if let Some(stored_name) = self.matching_store(&digest)? {
            if let Some(seconds) = spool.source_mtime {
                self.append_mtime_conflict_locked(&stored_name, seconds, protocol)?;
            }
            return Ok(Commit::Deduped {
                sha256: digest,
                stored_name,
            });
        }
        // Writes during the upload refresh the inode mtime. Stamp the session
        // value again so the verified copy keeps the camera time.
        if let Some(seconds) = spool.source_mtime {
            spool.set_mtime(seconds)?;
        }
        let stored_name = self.collision_safe_name(&arrival.camera_filename)?;
        spool.refile(&stored_name)?;
        let verified = copy_verify_file(spool.path(), &self.root).map_err(|err| err.to_string())?;
        let source_mtime = spool
            .source_mtime
            .unwrap_or_else(|| file_mtime_secs(&verified));
        let record = Record {
            sha256: verified.sha256.clone(),
            stored_name: verified.filename.clone(),
            camera_filename: arrival.camera_filename.clone(),
            directory: arrival.directory.clone(),
            source_mtime,
            received_at: unix_now(),
            mtime_conflicts: Vec::new(),
        };
        if let Err(err) = self.write_sidecar(&record) {
            // The original is already copy-verified. A failed receipt is a
            // tombstone, not an unlink. The source upload is not removed.
            let _ = self.write_tombstone(&verified.filename, "receipt sidecar was not written");
            return Err(err);
        }
        Ok(Commit::Stored {
            sha256: verified.sha256,
            stored_name: verified.filename,
        })
    }

    fn matching_store(&self, digest: &str) -> Result<Option<String>, String> {
        for record in self.records()? {
            if record.sha256 != digest {
                continue;
            }
            let path = self.root.join(&record.stored_name);
            if sha256_hex(&path).ok().as_deref() == Some(digest) {
                return Ok(Some(record.stored_name));
            }
        }
        Ok(None)
    }

    fn records(&self) -> Result<Vec<Record>, String> {
        let mut found = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(SIDECAR_SUFFIX) {
                continue;
            }
            let text = fs::read_to_string(entry.path()).map_err(|err| err.to_string())?;
            let Some(record) = Record::parse(&text) else {
                continue;
            };
            if format!("{}{SIDECAR_SUFFIX}", record.stored_name) != name {
                continue;
            }
            found.push(record);
        }
        Ok(found)
    }

    fn collision_safe_name(&self, camera_filename: &str) -> Result<String, String> {
        let mut taken = std::collections::BTreeSet::new();
        for entry in fs::read_dir(&self.root).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            if let Some(name) = entry.file_name().to_str() {
                taken.insert(name.to_string());
            }
        }
        if name_is_free(&taken, camera_filename) {
            return Ok(camera_filename.to_string());
        }
        for n in 2u32..100_000 {
            let candidate = disambiguated(camera_filename, n);
            if name_is_free(&taken, &candidate) {
                return Ok(candidate);
            }
        }
        Err(format!(
            "no free name for camera filename {camera_filename}"
        ))
    }

    fn write_sidecar(&self, record: &Record) -> Result<VerifiedObject, String> {
        let name = format!("{}{SIDECAR_SUFFIX}", record.stored_name);
        let dest = self.root.join(&name);
        if sidecar_rename_should_fail(&dest) {
            return Err("injected failure before sidecar rename".to_string());
        }
        let dir = tempfile::TempDir::new().map_err(|err| err.to_string())?;
        let path = dir.path().join(&name);
        let mut file = dest::create_file(&path).map_err(|err| err.to_string())?;
        file.write_all(record.json().as_bytes())
            .map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        copy_verify_file(&path, &self.root).map_err(|err| err.to_string())
    }

    /// Mark `stored_name` hidden. The original stays where it is.
    ///
    /// The tombstone is `{stored_name}.albumen-tombstone`. A second hide
    /// replaces that sidecar by rename and does not unlink the original.
    pub fn hide(&self, stored_name: &str) -> Result<(), String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        self.hide_locked(stored_name)
    }

    fn hide_locked(&self, stored_name: &str) -> Result<(), String> {
        check_stored_name(stored_name)?;
        let original = self.root.join(stored_name);
        let meta = fs::symlink_metadata(&original).map_err(|err| err.to_string())?;
        if !meta.is_file() {
            return Err(format!("{stored_name} is not a stored original"));
        }
        self.write_tombstone(stored_name, "hide")
    }

    fn write_tombstone(&self, stored_name: &str, reason: &str) -> Result<(), String> {
        let name = format!("{stored_name}{TOMBSTONE_SUFFIX}");
        let body = format!(
            "{{\"hidden\":true,\"reason\":{},\"stored_name\":{}}}\n",
            json_string(reason),
            json_string(stored_name)
        );
        let dest = self.root.join(&name);
        if fs::symlink_metadata(&dest).is_ok() {
            return self.write_atomic(&name, &body);
        }
        let dir = tempfile::TempDir::new().map_err(|err| err.to_string())?;
        let path = dir.path().join(&name);
        let mut file = dest::create_file(&path).map_err(|err| err.to_string())?;
        file.write_all(body.as_bytes())
            .map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        drop(file);
        copy_verify_file(&path, &self.root).map_err(|err| err.to_string())?;
        Ok(())
    }

    fn write_atomic(&self, name: &str, bytes: &str) -> Result<(), String> {
        let dest = self.root.join(name);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let tmp = self
            .root
            .join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
        let result = (|| {
            let mut file = dest::create_file(&tmp).map_err(|err| err.to_string())?;
            file.write_all(bytes.as_bytes())
                .map_err(|err| err.to_string())?;
            file.sync_all().map_err(|err| err.to_string())?;
            drop(file);
            dest::rename(&tmp, &dest).map_err(|err| err.to_string())?;
            sync_dir(&self.root)?;
            let again = fs::read(&dest).map_err(|err| err.to_string())?;
            if again != bytes.as_bytes() {
                return Err(format!("{name} bytes changed during replace"));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = dest::remove_file(&tmp);
        }
        result
    }

    /// Record a camera mtime offered after the file was committed.
    ///
    /// The stored file's mtime and the first `source_mtime` stay. A different
    /// value is appended. The same value as the first is ignored.
    pub fn note_source_mtime(
        &self,
        path: &str,
        seconds: u64,
        protocol: MtimeProtocol,
    ) -> Result<(), String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        let record = self.find_record(path)?;
        self.append_mtime_conflict_locked(&record.stored_name, seconds, protocol)
    }

    fn append_mtime_conflict_locked(
        &self,
        stored_name: &str,
        seconds: u64,
        protocol: MtimeProtocol,
    ) -> Result<(), String> {
        let mut record = self
            .records()?
            .into_iter()
            .find(|record| record.stored_name == stored_name)
            .ok_or_else(|| format!("no sidecar for {stored_name}"))?;
        if record.source_mtime == seconds {
            return Ok(());
        }
        record.mtime_conflicts.push(MtimeConflict {
            value: seconds,
            protocol: protocol.as_str().to_string(),
            received_at: unix_now(),
        });
        self.replace_sidecar(&record)
    }

    /// Replace a sidecar without unlinking it first.
    ///
    /// The new bytes are written to a temporary file in the library directory,
    /// fsynced, and renamed over the old sidecar. The directory is then
    /// fsynced. A failure before the rename leaves the previous sidecar in
    /// place and removes the temporary file.
    fn replace_sidecar(&self, record: &Record) -> Result<(), String> {
        let name = format!("{}{SIDECAR_SUFFIX}", record.stored_name);
        let dest = self.root.join(&name);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let tmp = self
            .root
            .join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
        let result = (|| {
            let mut file = dest::create_file(&tmp).map_err(|err| err.to_string())?;
            file.write_all(record.json().as_bytes())
                .map_err(|err| err.to_string())?;
            file.sync_all().map_err(|err| err.to_string())?;
            drop(file);
            if sidecar_rename_should_fail(&dest) {
                return Err("injected failure before sidecar rename".to_string());
            }
            dest::rename(&tmp, &dest).map_err(|err| err.to_string())?;
            sync_dir(&self.root)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = dest::remove_file(&tmp);
        }
        result
    }

    fn find_record(&self, path: &str) -> Result<Record, String> {
        let arrival = Arrival::parse(path)?;
        let records = self.records()?;
        let mut by_path: Vec<&Record> = records
            .iter()
            .filter(|record| {
                record.camera_filename == arrival.camera_filename
                    && record.directory == arrival.directory
            })
            .collect();
        if by_path.len() == 1 {
            return Ok(by_path.remove(0).clone());
        }
        if by_path.is_empty() {
            if let Some(record) = records
                .iter()
                .find(|record| record.stored_name == arrival.camera_filename)
            {
                return Ok(record.clone());
            }
        }
        Err(format!(
            "no stored file for {}/{}",
            arrival.directory, arrival.camera_filename
        ))
    }
}

fn file_mtime_secs(verified: &VerifiedObject) -> u64 {
    verified
        .mtime
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
static FAIL_SIDECAR_RENAME: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn arm_fail_before_sidecar_rename(sidecar: PathBuf) {
    *FAIL_SIDECAR_RENAME
        .lock()
        .expect("sidecar rename hook lock") = Some(sidecar);
}

fn sidecar_rename_should_fail(dest: &Path) -> bool {
    #[cfg(test)]
    {
        let mut armed = FAIL_SIDECAR_RENAME
            .lock()
            .expect("sidecar rename hook lock");
        if armed.as_deref() == Some(dest) {
            *armed = None;
            return true;
        }
        false
    }
    #[cfg(not(test))]
    {
        let _ = dest;
        false
    }
}

fn name_is_free(taken: &std::collections::BTreeSet<String>, stored_name: &str) -> bool {
    !taken.contains(stored_name) && !taken.contains(&format!("{stored_name}{SIDECAR_SUFFIX}"))
}

fn disambiguated(camera_filename: &str, n: u32) -> String {
    match camera_filename.rfind('.') {
        Some(index) if index > 0 => {
            format!(
                "{}__{n}{}",
                &camera_filename[..index],
                &camera_filename[index..]
            )
        }
        _ => format!("{camera_filename}__{n}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub camera_filename: String,
    pub directory: String,
}

impl Arrival {
    /// Split an upload path into the directory and the filename.
    ///
    /// The filename is the last component, unchanged. `DCIM/100CANON` stays
    /// the directory. `..` is refused.
    pub fn parse(path: &str) -> Result<Self, String> {
        let path = path.trim_matches(['/', '\\']);
        if path.is_empty() {
            return Err("missing filename".to_string());
        }
        if path.split(['/', '\\']).any(|part| part == "..") {
            return Err("refusing a path that climbs out of the library".to_string());
        }
        let mut parts: Vec<&str> = path
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        let Some(filename) = parts.pop() else {
            return Err("missing filename".to_string());
        };
        validate_filename(filename)?;
        Ok(Self {
            camera_filename: filename.to_string(),
            directory: parts.join("/"),
        })
    }
}

fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return Err(format!("refusing filename {name}"));
    }
    Ok(())
}

/// Which upload path offered a camera mtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtimeProtocol {
    Ftp,
    Sftp,
    WebDav,
}

impl MtimeProtocol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ftp => "ftp",
            Self::Sftp => "sftp",
            Self::WebDav => "webdav",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MtimeConflict {
    value: u64,
    protocol: String,
    received_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commit {
    Stored { sha256: String, stored_name: String },
    Deduped { sha256: String, stored_name: String },
}

impl Commit {
    pub fn sha256(&self) -> &str {
        match self {
            Self::Stored { sha256, .. } | Self::Deduped { sha256, .. } => sha256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    sha256: String,
    stored_name: String,
    camera_filename: String,
    directory: String,
    source_mtime: u64,
    received_at: u64,
    mtime_conflicts: Vec<MtimeConflict>,
}

impl Record {
    fn json(&self) -> String {
        format!(
            "{{\"camera_filename\":{},\"directory\":{},\"mtime_conflicts\":{},\"received_at\":{},\"sha256\":{},\"source_mtime\":{},\"stored_name\":{}}}\n",
            json_string(&self.camera_filename),
            json_string(&self.directory),
            conflicts_json(&self.mtime_conflicts),
            json_string(&self.received_at.to_string()),
            json_string(&self.sha256),
            json_string(&self.source_mtime.to_string()),
            json_string(&self.stored_name),
        )
    }

    fn parse(text: &str) -> Option<Self> {
        Some(Self {
            camera_filename: json_field(text, "camera_filename")?,
            directory: json_field(text, "directory")?,
            mtime_conflicts: parse_conflicts(text).ok()?,
            received_at: json_field(text, "received_at")?.parse().ok()?,
            sha256: json_field(text, "sha256")?,
            source_mtime: json_field(text, "source_mtime")?.parse().ok()?,
            stored_name: json_field(text, "stored_name")?,
        })
    }
}

fn conflicts_json(conflicts: &[MtimeConflict]) -> String {
    let mut out = String::from("[");
    for (index, conflict) in conflicts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"protocol\":{},\"received_at\":{},\"value\":{}}}",
            json_string(&conflict.protocol),
            json_string(&conflict.received_at.to_string()),
            json_string(&conflict.value.to_string()),
        ));
    }
    out.push(']');
    out
}

fn check_stored_name(name: &str) -> Result<(), String> {
    let unsafe_name = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control);
    if unsafe_name {
        Err(format!("unsafe stored name {name}"))
    } else {
        Ok(())
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn json_field(text: &str, key: &str) -> Option<String> {
    let value = top_level_value(text, key)?;
    let rest = value.trim_start().strip_prefix('"')?;
    unescape_string(rest)
}

/// Value text of a top-level object field, so a nested `received_at` inside
/// `mtime_conflicts` is not read as the record's own receive time.
fn top_level_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text.trim().strip_prefix('{')?.trim_start();
    loop {
        rest = rest.trim_start();
        if rest.starts_with('}') {
            return None;
        }
        let name = unescape_string(rest.strip_prefix('"')?)?;
        rest = skip_json_string(rest)?;
        rest = rest.trim_start().strip_prefix(':')?.trim_start();
        if name == key {
            return Some(rest);
        }
        rest = skip_json_value(rest)?;
        rest = rest.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
            continue;
        }
        return None;
    }
}

fn parse_conflicts(text: &str) -> Result<Vec<MtimeConflict>, ()> {
    let Some(value) = top_level_value(text, "mtime_conflicts") else {
        return Ok(Vec::new());
    };
    let mut rest = value.trim_start().strip_prefix('[').ok_or(())?;
    let mut found = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') {
            return Ok(found);
        }
        if !rest.starts_with('{') {
            return Err(());
        }
        let after = skip_json_value(rest).ok_or(())?;
        let object = &rest[..rest.len() - after.len()];
        found.push(MtimeConflict {
            protocol: json_field(object, "protocol").ok_or(())?,
            received_at: json_field(object, "received_at")
                .ok_or(())?
                .parse()
                .map_err(|_| ())?,
            value: json_field(object, "value")
                .ok_or(())?
                .parse()
                .map_err(|_| ())?,
        });
        rest = after.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
            continue;
        }
        if rest.starts_with(']') {
            return Ok(found);
        }
        return Err(());
    }
}

fn skip_json_string(text: &str) -> Option<&str> {
    let body = text.strip_prefix('"')?;
    let mut escaped = false;
    for (index, ch) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(&body[index + ch.len_utf8()..]);
        }
    }
    None
}

fn skip_json_value(text: &str) -> Option<&str> {
    let text = text.trim_start();
    if text.starts_with('"') {
        return skip_json_string(text);
    }
    if text.starts_with('[') {
        return skip_brackets(text, '[', ']');
    }
    if text.starts_with('{') {
        return skip_brackets(text, '{', '}');
    }
    let end = text.find([',', '}', ']']).unwrap_or(text.len());
    Some(&text[end..])
}

fn skip_brackets(text: &str, open: char, close: char) -> Option<&str> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(&text[index + ch.len_utf8()..]);
            }
        }
    }
    None
}

fn unescape_string(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            return Some(out);
        }
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    None
}

pub struct Spool {
    _dir: tempfile::TempDir,
    path: PathBuf,
    file: File,
    len: u64,
    source_mtime: Option<u64>,
}

impl Spool {
    pub fn create(filename: &str) -> Result<Self, String> {
        validate_filename(filename)?;
        let dir = tempfile::TempDir::new().map_err(|err| err.to_string())?;
        let path = dir.path().join(filename);
        let file = dest::create_file(&path).map_err(|err| err.to_string())?;
        Ok(Self {
            _dir: dir,
            path,
            file,
            len: 0,
            source_mtime: None,
        })
    }

    pub fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), String> {
        if offset > self.len {
            return Err("upload has a gap".to_string());
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|err| err.to_string())?;
        self.file.write_all(data).map_err(|err| err.to_string())?;
        let end = offset + data.len() as u64;
        if end > self.len {
            self.len = end;
        }
        Ok(())
    }

    pub fn set_mtime(&mut self, seconds: u64) -> Result<(), String> {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        self.file
            .set_modified(when)
            .map_err(|err| err.to_string())?;
        self.source_mtime = Some(seconds);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.file.flush().map_err(|err| err.to_string())?;
        self.file.sync_all().map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn refile(&mut self, stored_name: &str) -> Result<(), String> {
        validate_filename(stored_name)?;
        if self.path.file_name().and_then(|name| name.to_str()) == Some(stored_name) {
            return Ok(());
        }
        let dest = self
            .path
            .parent()
            .ok_or_else(|| "spool has no directory".to_string())?
            .join(stored_name);
        dest::rename(&self.path, &dest).map_err(|err| err.to_string())?;
        self.path = dest;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        arm_fail_before_sidecar_rename, disambiguated, Arrival, Library, MtimeProtocol, Record,
        Spool,
    };
    use std::time::UNIX_EPOCH;

    fn file_index(meta: &std::fs::Metadata) -> u64 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            meta.file_index().unwrap_or(0)
        }
    }

    #[test]
    fn arrival_keeps_the_camera_filename_and_directory() {
        let arrival = Arrival::parse("/DCIM/100CANON/IMG_9999.JPG").unwrap();
        assert_eq!(arrival.camera_filename, "IMG_9999.JPG");
        assert_eq!(arrival.directory, "DCIM/100CANON");
        assert!(Arrival::parse("../IMG_0001.JPG").is_err());
    }

    #[test]
    fn disambiguated_name_keeps_the_extension() {
        assert_eq!(disambiguated("IMG_9999.JPG", 2), "IMG_9999__2.JPG");
        assert_eq!(disambiguated("IMG_9999", 2), "IMG_9999__2");
    }

    #[test]
    fn sidecar_without_conflicts_still_parses() {
        let text = "{\"camera_filename\":\"a.jpg\",\"directory\":\"DCIM\",\"received_at\":\"2\",\"sha256\":\"ab\",\"source_mtime\":\"1\",\"stored_name\":\"a.jpg\"}\n";
        let record = Record::parse(text).unwrap();
        assert!(record.mtime_conflicts.is_empty());
        assert_eq!(record.source_mtime, 1);
        assert_eq!(record.received_at, 2);
    }

    #[test]
    fn conflict_fields_do_not_replace_the_first_record_fields() {
        let record = Record {
            sha256: "ab".into(),
            stored_name: "a.jpg".into(),
            camera_filename: "a.jpg".into(),
            directory: "DCIM".into(),
            source_mtime: 1,
            received_at: 2,
            mtime_conflicts: vec![super::MtimeConflict {
                value: 9,
                protocol: "ftp".into(),
                received_at: 8,
            }],
        };
        let parsed = Record::parse(&record.json()).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn injected_rename_failure_leaves_a_valid_sidecar() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let mut spool = Spool::create("IMG_0001.JPG").unwrap();
        spool.write_at(0, b"original-bytes").unwrap();
        spool.set_mtime(1_000_000_000).unwrap();
        library
            .commit(
                &mut spool,
                &Arrival {
                    camera_filename: "IMG_0001.JPG".into(),
                    directory: "DCIM".into(),
                },
                MtimeProtocol::Ftp,
            )
            .unwrap();
        let photo = root.path().join("IMG_0001.JPG");
        let sidecar = root.path().join("IMG_0001.JPG.albumen.json");
        let before = std::fs::read(&sidecar).unwrap();
        let mtime_before = std::fs::metadata(&photo).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_000_000_000
        );

        library
            .note_source_mtime("DCIM/IMG_0001.JPG", 1_000_000_000, MtimeProtocol::Ftp)
            .unwrap();
        assert_eq!(std::fs::read(&sidecar).unwrap(), before);

        arm_fail_before_sidecar_rename(sidecar.clone());
        let err = library.note_source_mtime("IMG_0001.JPG", 1_000_000_060, MtimeProtocol::Ftp);
        assert!(err.is_err(), "{err:?}");
        assert_eq!(std::fs::read(&sidecar).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&photo).unwrap().modified().unwrap(),
            mtime_before
        );
        let names: Vec<String> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
        let parsed = Record::parse(&String::from_utf8(before.clone()).unwrap()).unwrap();
        assert_eq!(parsed.source_mtime, 1_000_000_000);
        assert!(parsed.mtime_conflicts.is_empty());

        library
            .note_source_mtime("IMG_0001.JPG", 1_000_000_060, MtimeProtocol::Sftp)
            .unwrap();
        library
            .note_source_mtime("IMG_0001.JPG", 1_000_000_060, MtimeProtocol::WebDav)
            .unwrap();
        let text = std::fs::read_to_string(&sidecar).unwrap();
        let updated = Record::parse(&text).unwrap();
        assert_eq!(updated.source_mtime, 1_000_000_000);
        assert_eq!(updated.received_at, parsed.received_at);
        assert_eq!(updated.mtime_conflicts.len(), 2);
        assert_eq!(updated.mtime_conflicts[0].value, 1_000_000_060);
        assert_eq!(updated.mtime_conflicts[0].protocol, "sftp");
        assert_eq!(updated.mtime_conflicts[1].protocol, "webdav");
        assert_ne!(updated.mtime_conflicts[0].received_at, updated.source_mtime);
        assert_eq!(
            std::fs::metadata(&photo).unwrap().modified().unwrap(),
            mtime_before
        );
    }

    #[test]
    fn sidecar_failure_and_hide_keep_the_original() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let mut spool = Spool::create("IMG_0001.JPG").unwrap();
        spool.write_at(0, b"original-bytes").unwrap();
        spool.set_mtime(1_000_000_000).unwrap();
        let sidecar = root.path().join("IMG_0001.JPG.albumen.json");
        arm_fail_before_sidecar_rename(sidecar);
        let err = library
            .commit(
                &mut spool,
                &Arrival {
                    camera_filename: "IMG_0001.JPG".into(),
                    directory: "DCIM".into(),
                },
                MtimeProtocol::WebDav,
            )
            .unwrap_err();
        assert!(err.contains("injected failure"), "{err}");
        let photo = root.path().join("IMG_0001.JPG");
        let inode = file_index(&std::fs::metadata(&photo).unwrap());
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        let tombstone =
            std::fs::read_to_string(root.path().join("IMG_0001.JPG.albumen-tombstone")).unwrap();
        assert!(tombstone.contains("\"hidden\":true"), "{tombstone}");
        assert!(
            tombstone.contains("receipt sidecar was not written"),
            "{tombstone}"
        );

        library.hide("IMG_0001.JPG").unwrap();
        assert_eq!(file_index(&std::fs::metadata(&photo).unwrap()), inode);
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        let hidden =
            std::fs::read_to_string(root.path().join("IMG_0001.JPG.albumen-tombstone")).unwrap();
        assert!(hidden.contains("\"reason\":\"hide\""), "{hidden}");
        library.hide("IMG_0001.JPG").unwrap();
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        assert!(library.hide("../IMG_0001.JPG").is_err());
    }
}
