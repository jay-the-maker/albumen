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

/// Whether a stored original is hidden on this copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideState {
    Visible,
    Hidden,
}

/// The plan for a hide or unhide. Building it reads only.
#[derive(Debug)]
pub struct HidePlan {
    pub original: PathBuf,
    pub tombstone: PathBuf,
    pub current: HideState,
    /// `false` when the original is already in the requested state.
    pub change: bool,
}

#[derive(Debug)]
pub struct Library {
    root: PathBuf,
    commit_lock: Mutex<()>,
    /// When set, `root` is `members/<id>/` inside a family library.
    /// A PUT whose directory is not `members/<id>` is refused before any library write.
    member_id: Option<String>,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        albumen_ingest::family::refuse_shared_write_root(&root)
            .map_err(|err| io::Error::other(err.to_string()))?;
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
            member_id: None,
        })
    }

    /// Open `members/<member>/` inside a family library.
    ///
    /// [`Library::open`] still refuses that library and anything inside it.
    /// This entry is the phone backup for one joined member. The member id is
    /// an Ed25519 public key, and `members/<member>/devices` must already exist.
    pub fn open_member_namespace(library: impl Into<PathBuf>, member: &str) -> io::Result<Self> {
        let library = library.into();
        let member = albumen_ingest::family::require_joined_member(&library, member)
            .map_err(|err| io::Error::other(err.to_string()))?;
        let root = library.join("members").join(&member);
        dest::create_dir_all(&root).map_err(|err| io::Error::other(err.to_string()))?;
        if !root.is_dir() {
            return Err(io::Error::other(
                "member namespace is not a directory".to_string(),
            ));
        }
        Ok(Self {
            root,
            commit_lock: Mutex::new(()),
            member_id: Some(member),
        })
    }

    /// `Some` when this arrival would write outside the joined member's prefix.
    /// A library opened with [`Library::open`] allows the paths it already allowed.
    pub fn namespace_refusal(&self, arrival: &Arrival) -> Option<String> {
        let member = self.member_id.as_deref()?;
        let expected = format!("members/{member}");
        if arrival.directory == expected {
            return None;
        }
        Some(format!(
            "refusing a write outside members/{member}/. The other member's originals stay in place."
        ))
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
        if let Some(message) = self.namespace_refusal(arrival) {
            return Err(message);
        }
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
        println!(
            "camera-verify protocol={} bytes={} sha256={} verify_ms={}",
            protocol.as_str(),
            verified.byte_len,
            verified.sha256,
            verified.sha256_verify_ms
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        Ok(Commit::Stored {
            sha256: verified.sha256,
            stored_name: verified.filename,
            verify_ms: verified.sha256_verify_ms,
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
        self.stored_original(stored_name)?;
        self.write_tombstone(stored_name, "hide")
    }

    /// Mark `stored_name` visible again. The tombstone is not removed: it is
    /// replaced by an fsynced rename with `"hidden":false`, so an unhide is a
    /// record, not a delete. Returns `false` when it was already visible, and
    /// then writes nothing.
    pub fn unhide(&self, stored_name: &str) -> Result<bool, String> {
        let _guard = self
            .commit_lock
            .lock()
            .map_err(|_| "receive commit lock was poisoned".to_string())?;
        let plan = self.plan_hide(stored_name, false)?;
        if !plan.change {
            return Ok(false);
        }
        let body = format!(
            "{{\"hidden\":false,\"reason\":\"unhide\",\"stored_name\":{}}}\n",
            json_string(stored_name)
        );
        self.write_atomic(&format!("{stored_name}{TOMBSTONE_SUFFIX}"), &body)?;
        Ok(true)
    }

    /// What a hide (`hidden = true`) or unhide would do. Reads only.
    pub fn plan_hide(&self, stored_name: &str, hidden: bool) -> Result<HidePlan, String> {
        let original = self.stored_original(stored_name)?;
        let tombstone = self.root.join(format!("{stored_name}{TOMBSTONE_SUFFIX}"));
        let current = match fs::read_to_string(&tombstone) {
            Ok(text) if text.contains("\"hidden\":true") => HideState::Hidden,
            Ok(_) => HideState::Visible,
            Err(err) if err.kind() == io::ErrorKind::NotFound => HideState::Visible,
            Err(err) => return Err(err.to_string()),
        };
        let change = (current == HideState::Hidden) != hidden;
        Ok(HidePlan {
            original,
            tombstone,
            current,
            change,
        })
    }

    /// The stored original for `stored_name`, which must be a regular file
    /// directly in this library's root. In a member namespace the root is
    /// `members/<id>/`, so a name that resolves anywhere else (another
    /// member's namespace included) is refused before any write.
    fn stored_original(&self, stored_name: &str) -> Result<PathBuf, String> {
        check_stored_name(stored_name)?;
        if stored_name.ends_with(TOMBSTONE_SUFFIX) || stored_name.ends_with(SIDECAR_SUFFIX) {
            return Err(format!("{stored_name} is not a stored original"));
        }
        let original = self.root.join(stored_name);
        let meta = fs::symlink_metadata(&original).map_err(|err| err.to_string())?;
        if !meta.is_file() {
            return Err(format!("{stored_name} is not a stored original"));
        }
        let root = self.root.canonicalize().map_err(|err| err.to_string())?;
        let resolved = original.canonicalize().map_err(|err| err.to_string())?;
        if resolved.parent() != Some(root.as_path()) {
            return Err(format!(
                "refusing {stored_name}: it is not in this library's own namespace"
            ));
        }
        Ok(original)
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

/// Persist the directory entry after a sidecar or tombstone rename.
///
/// The file is already fsynced. On Windows this is `MoveFileExW` write-through
/// or `FlushFileBuffers` on a `FILE_FLAG_BACKUP_SEMANTICS` handle, same as
/// `copy_verify`.
fn sync_dir(dir: &Path) -> Result<(), String> {
    dest::sync_dir(dir).map_err(|err| err.to_string())
}

#[cfg(test)]
static FAIL_SIDECAR_RENAME: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn arm_fail_before_sidecar_rename(sidecar: PathBuf) {
    FAIL_SIDECAR_RENAME
        .lock()
        .expect("sidecar rename hook lock")
        .push(sidecar);
}

fn sidecar_rename_should_fail(dest: &Path) -> bool {
    #[cfg(test)]
    {
        // Two tests arm different sidecars at the same time. One slot would
        // drop the other path, and the commit would succeed.
        let mut armed = FAIL_SIDECAR_RENAME
            .lock()
            .expect("sidecar rename hook lock");
        if let Some(index) = armed.iter().position(|path| path == dest) {
            armed.remove(index);
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
    Stored {
        sha256: String,
        stored_name: String,
        /// Milliseconds spent re-reading the stored bytes and hashing them.
        verify_ms: u128,
    },
    Deduped {
        sha256: String,
        stored_name: String,
    },
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
    use crate::store::HideState;
    use std::path::Path;
    use std::time::UNIX_EPOCH;

    fn file_id(path: &Path) -> u64 {
        let meta = std::fs::metadata(path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            // `file_index` is still unstable. The creation time stays with
            // the file when a tombstone is rewritten beside it.
            meta.creation_time()
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
        let inode = file_id(&photo);
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        let tombstone =
            std::fs::read_to_string(root.path().join("IMG_0001.JPG.albumen-tombstone")).unwrap();
        assert!(tombstone.contains("\"hidden\":true"), "{tombstone}");
        assert!(
            tombstone.contains("receipt sidecar was not written"),
            "{tombstone}"
        );

        library.hide("IMG_0001.JPG").unwrap();
        assert_eq!(file_id(&photo), inode);
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        let hidden =
            std::fs::read_to_string(root.path().join("IMG_0001.JPG.albumen-tombstone")).unwrap();
        assert!(hidden.contains("\"reason\":\"hide\""), "{hidden}");
        library.hide("IMG_0001.JPG").unwrap();
        assert_eq!(std::fs::read(&photo).unwrap(), b"original-bytes");
        assert!(library.hide("../IMG_0001.JPG").is_err());
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
        let mut out: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let bytes = if meta.is_file() {
                    std::fs::read(&path).unwrap()
                } else {
                    Vec::new()
                };
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    bytes,
                    meta.modified().unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    fn stored_original(root: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = root.join(name);
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000))
            .unwrap();
        path
    }

    #[test]
    fn hide_unhide_hide_keeps_the_original_and_never_removes_the_tombstone() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let photo = stored_original(root.path(), "IMG_0002.JPG", b"synthetic-original");
        let inode = file_id(&photo);
        let mtime = std::fs::metadata(&photo).unwrap().modified().unwrap();
        let tombstone = root.path().join("IMG_0002.JPG.albumen-tombstone");
        let unchanged = |step: &str| {
            assert_eq!(
                std::fs::read(&photo).unwrap(),
                b"synthetic-original",
                "{step}"
            );
            assert_eq!(
                std::fs::metadata(&photo).unwrap().modified().unwrap(),
                mtime,
                "{step}"
            );
            assert_eq!(file_id(&photo), inode, "{step}");
        };

        assert_eq!(
            library.plan_hide("IMG_0002.JPG", true).unwrap().current,
            HideState::Visible
        );
        library.hide("IMG_0002.JPG").unwrap();
        unchanged("hide");
        assert!(std::fs::read_to_string(&tombstone)
            .unwrap()
            .contains("\"hidden\":true"));
        let plan = library.plan_hide("IMG_0002.JPG", true).unwrap();
        assert_eq!(plan.current, HideState::Hidden);
        assert!(!plan.change);

        assert!(library.unhide("IMG_0002.JPG").unwrap());
        unchanged("unhide");
        let shown = std::fs::read_to_string(&tombstone).unwrap();
        assert!(shown.contains("\"hidden\":false"), "{shown}");
        assert!(shown.contains("\"reason\":\"unhide\""), "{shown}");
        assert_eq!(
            library.plan_hide("IMG_0002.JPG", false).unwrap().current,
            HideState::Visible
        );
        assert!(
            !library.unhide("IMG_0002.JPG").unwrap(),
            "second unhide is a no-op"
        );

        library.hide("IMG_0002.JPG").unwrap();
        unchanged("hide again");
        assert!(std::fs::read_to_string(&tombstone)
            .unwrap()
            .contains("\"hidden\":true"));
        let names: Vec<_> = snapshot(root.path())
            .into_iter()
            .map(|entry| entry.0)
            .collect();
        assert_eq!(
            names,
            vec!["IMG_0002.JPG", "IMG_0002.JPG.albumen-tombstone"]
        );
    }

    #[test]
    fn unhide_and_plans_write_nothing_when_there_is_nothing_to_change() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        stored_original(root.path(), "IMG_0003.JPG", b"visible");
        let before = snapshot(root.path());
        assert!(!library.unhide("IMG_0003.JPG").unwrap());
        library.plan_hide("IMG_0003.JPG", true).unwrap();
        library.plan_hide("IMG_0003.JPG", false).unwrap();
        assert_eq!(snapshot(root.path()), before);
    }

    #[test]
    fn hide_and_unhide_refuse_names_outside_this_library() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let library = Library::open(&root).unwrap();
        stored_original(&root, "IMG_0004.JPG", b"mine");
        library.hide("IMG_0004.JPG").unwrap();
        let other = parent.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let theirs = stored_original(&other, "THEIRS.JPG", b"theirs");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&theirs, root.join("LINK.JPG")).unwrap();
        let before_root = snapshot(&root);
        let before_other = snapshot(&other);
        for name in [
            "../other/THEIRS.JPG",
            "..",
            "",
            "/etc/hosts",
            "sub/IMG_0004.JPG",
            "IMG_0004.JPG.albumen-tombstone",
            "IMG_0004.JPG.albumen.json",
            "MISSING.JPG",
            "LINK.JPG",
        ] {
            assert!(library.hide(name).is_err(), "hide {name:?}");
            assert!(library.unhide(name).is_err(), "unhide {name:?}");
            assert!(library.plan_hide(name, true).is_err(), "plan {name:?}");
        }
        assert_eq!(snapshot(&root), before_root);
        assert_eq!(snapshot(&other), before_other);
        assert_eq!(std::fs::read(&theirs).unwrap(), b"theirs");
    }
}
