// SPDX-License-Identifier: Apache-2.0

//! A received upload is a temporary file until `copy_verify_file` accepts it.
//! A different file with the same camera filename is stored under another name.
//! The camera filename and its directory are written to a sidecar. An identical
//! byte string is not stored twice. A failed upload deletes only the spool,
//! and a sidecar failure deletes the destination this call just wrote.

use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use albumen_ingest::{copy_verify_file, sha256_hex, VerifiedObject};

const SIDECAR_SUFFIX: &str = ".albumen.json";

#[derive(Debug)]
pub struct Library {
    root: PathBuf,
    commit_lock: Mutex<()>,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
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

    pub fn files(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if !name.ends_with(SIDECAR_SUFFIX) {
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
    pub fn commit(&self, spool: &mut Spool, arrival: &Arrival) -> Result<Commit, String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        spool.finish()?;
        let digest = sha256_hex(spool.path()).map_err(|err| err.to_string())?;
        if let Some(stored_name) = self.matching_store(&digest)? {
            if let Some(seconds) = spool.source_mtime {
                self.apply_stored_mtime_locked(&stored_name, seconds)?;
            }
            return Ok(Commit::Deduped {
                sha256: digest,
                stored_name,
            });
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
        };
        if let Err(err) = self.write_sidecar(&record) {
            let _ = fs::remove_file(self.root.join(&verified.filename));
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
        let dir = tempfile::TempDir::new().map_err(|err| err.to_string())?;
        let path = dir.path().join(&name);
        let mut file = File::create(&path).map_err(|err| err.to_string())?;
        file.write_all(record.json().as_bytes())
            .map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        copy_verify_file(&path, &self.root).map_err(|err| err.to_string())
    }

    /// Set the stored file's mtime from FTP MFMT/MDTM or SFTP setstat.
    /// The photo bytes stay. `received_at` in the sidecar stays.
    pub fn note_source_mtime(&self, path: &str, seconds: u64) -> Result<(), String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        self.apply_source_mtime_locked(path, seconds)
    }

    fn apply_source_mtime_locked(&self, path: &str, seconds: u64) -> Result<(), String> {
        let record = self.find_record(path)?;
        self.apply_stored_mtime_locked(&record.stored_name, seconds)
    }

    fn apply_stored_mtime_locked(&self, stored_name: &str, seconds: u64) -> Result<(), String> {
        let mut record = self
            .records()?
            .into_iter()
            .find(|record| record.stored_name == stored_name)
            .ok_or_else(|| format!("no sidecar for {stored_name}"))?;
        let stored = self.root.join(&record.stored_name);
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        fs::File::options()
            .write(true)
            .open(&stored)
            .and_then(|file| file.set_modified(when))
            .map_err(|err| err.to_string())?;
        record.source_mtime = seconds;
        let name = format!("{}{SIDECAR_SUFFIX}", record.stored_name);
        let sidecar = self.root.join(&name);
        fs::remove_file(&sidecar).map_err(|err| err.to_string())?;
        self.write_sidecar(&record)?;
        Ok(())
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
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
}

impl Record {
    fn json(&self) -> String {
        format!(
            "{{\"camera_filename\":{},\"directory\":{},\"received_at\":{},\"sha256\":{},\"source_mtime\":{},\"stored_name\":{}}}\n",
            json_string(&self.camera_filename),
            json_string(&self.directory),
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
            received_at: json_field(text, "received_at")?.parse().ok()?,
            sha256: json_field(text, "sha256")?,
            source_mtime: json_field(text, "source_mtime")?.parse().ok()?,
            stored_name: json_field(text, "stored_name")?,
        })
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
    let needle = format!("\"{key}\":");
    let index = text.find(&needle)?;
    let rest = text[index + needle.len()..].trim_start();
    let rest = rest.strip_prefix('"')?;
    unescape_string(rest)
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
        let file = File::create(&path).map_err(|err| err.to_string())?;
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
        fs::rename(&self.path, &dest).map_err(|err| err.to_string())?;
        self.path = dest;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{disambiguated, Arrival};

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
}
