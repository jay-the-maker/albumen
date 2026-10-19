// SPDX-License-Identifier: Apache-2.0

//! Google Takeout import.
//!
//! Split `.zip`, `.tgz`, and `.tar.gz` parts are opened read-only and streamed.
//! Nothing is extracted beside the archives, and the archive bytes are hashed
//! before and after. Each file member is copied with the same write, fsync,
//! and re-read SHA-256 check as [`crate::copy_verify`]. Takeout JSON is stored
//! byte-for-byte next to its original. `photoTakenTime` is read from that
//! sidecar for the reconciliation report and is never written into the original.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_json::json;
use tar::Archive;
use zip::ZipArchive;

use crate::checksum::{sha256_file, sha256_reader, to_hex};
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::local::copy_verify_reader;

/// Counts from a finished Takeout import whose reconciliation matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoutReport {
    pub count_in: u64,
    pub count_out: u64,
    pub mismatched: u64,
    pub report: PathBuf,
}

#[derive(Debug, Default)]
struct Counters {
    count_in: u64,
    count_out: u64,
    mismatched: u64,
}

struct ReportFile {
    file: BufWriter<File>,
    path: PathBuf,
}

struct ArchiveSeal {
    path: PathBuf,
    name: String,
    sha256: String,
    bytes: u64,
}

/// Copy every file member of a Takeout download into `to`.
///
/// `from` is one `.zip`, `.tgz`, or `.tar.gz` archive, or a directory whose
/// entries are only those parts. The destination must not be that directory
/// or a directory that contains it. `report` is a JSONL file outside both.
///
/// The function returns [`IngestError::TakeoutReconciliation`] when any member
/// is missing, extra, or hashed differently, when an archive's bytes change,
/// or when the archives contain no file members. The report is still written.
pub fn import_takeout(from: &Path, to: &Path, report: &Path) -> Result<TakeoutReport, IngestError> {
    let archives = list_archives(from)?;
    let source_root = source_root(from)?;
    refuse_layout(&source_root, to, report)?;
    fs::create_dir_all(to).map_err(|err| IngestError::io(to, err))?;
    if let Some(parent) = report.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| IngestError::io(parent, err))?;
        }
    }

    let seals = seal_archives(&archives)?;
    let mut report_file = ReportFile::create(report)?;
    for seal in &seals {
        report_file.line(&json!({
            "kind": "archive",
            "archive": seal.name,
            "sha256": seal.sha256,
            "bytes": seal.bytes,
            "status": "sealed",
        }))?;
    }

    let mut counters = Counters::default();
    let mut fatal: Option<IngestError> = None;
    for archive in &archives {
        if let Err(err) = import_archive(archive, to, &mut report_file, &mut counters) {
            fatal = Some(err);
            break;
        }
    }

    let archives_unchanged = match confirm_seals(&seals, &mut report_file, &mut counters) {
        Ok(unchanged) => unchanged,
        Err(err) => {
            if fatal.is_none() {
                fatal = Some(err);
            }
            false
        }
    };

    if counters.count_in == 0 && fatal.is_none() {
        counters.mismatched = counters.mismatched.saturating_add(1);
        let _ = report_file.line(&json!({
            "kind": "archive",
            "status": "mismatch",
            "detail": "archives contained no file members",
        }));
    }

    let summary = json!({
        "summary": true,
        "count_in": counters.count_in,
        "count_out": counters.count_out,
        "mismatched": counters.mismatched,
        "archives_unchanged": archives_unchanged,
    });
    report_file.line(&summary)?;
    report_file.finish()?;

    if let Some(err) = fatal {
        return Err(err);
    }
    if counters.count_in != counters.count_out || counters.mismatched != 0 || !archives_unchanged {
        return Err(IngestError::TakeoutReconciliation {
            count_in: counters.count_in,
            count_out: counters.count_out,
            mismatched: counters.mismatched,
            archives_unchanged,
            report: report.display().to_string(),
        });
    }
    Ok(TakeoutReport {
        count_in: counters.count_in,
        count_out: counters.count_out,
        mismatched: counters.mismatched,
        report: report.to_path_buf(),
    })
}

fn import_archive(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    match archive_kind(path) {
        Some(ArchiveKind::Zip) => import_zip(path, dest_root, report, counters),
        Some(ArchiveKind::Tgz) => import_tgz(path, dest_root, report, counters),
        None => Err(IngestError::store(
            "takeout",
            display_name(path),
            "not a .zip, .tgz, or .tar.gz archive",
        )),
    }
}

fn import_zip(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let file = open_read_only(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| zip_err(path, err))?;
    let archive_name = file_name_str(path)?;
    let len = archive.len();
    for index in 0..len {
        let entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(err) => {
                let remaining = u64::try_from(len - index).unwrap_or(u64::MAX);
                counters.count_in = counters.count_in.saturating_add(remaining);
                counters.mismatched = counters.mismatched.saturating_add(remaining);
                report.line(&json!({
                    "kind": "original",
                    "archive": archive_name,
                    "member": format!("index {index}"),
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": format!("zip member {index} of {len} could not be read: {err}"),
                }))?;
                break;
            }
        };
        if entry.is_dir() {
            continue;
        }
        let member_name = match entry.name() {
            Ok(name) => name.into_owned(),
            Err(err) => {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "original",
                    "archive": archive_name,
                    "member": "",
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": format!("zip member name is not usable: {err}"),
                }))?;
                continue;
            }
        };
        let mut part = Part {
            archive_path: path,
            archive_name: &archive_name,
            dest_root,
            report,
            counters,
        };
        if !entry.is_file() {
            part.reject(entry, &member_name, "archive member is not a regular file")?;
            continue;
        }
        let mtime = entry.last_modified().as_ref().and_then(dos_to_system_time);
        part.store_member(entry, &member_name, mtime)?;
    }
    Ok(())
}

fn import_tgz(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let file = open_read_only(path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let archive_name = file_name_str(path)?;
    let entries = archive
        .entries()
        .map_err(|err| IngestError::io(path, err))?;
    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "original",
                    "archive": archive_name,
                    "member": "",
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": format!("tar member could not be read: {err}"),
                }))?;
                break;
            }
        };
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        let member_name = match utf8_path(&entry.path_bytes()) {
            Ok(name) => name,
            Err(detail) => {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "original",
                    "archive": archive_name,
                    "member": "",
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": detail,
                }))?;
                continue;
            }
        };
        let mut part = Part {
            archive_path: path,
            archive_name: &archive_name,
            dest_root,
            report,
            counters,
        };
        if !entry_type.is_file() {
            part.reject(
                &mut entry,
                &member_name,
                "archive member is not a regular file",
            )?;
            continue;
        }
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        part.store_member(&mut entry, &member_name, mtime)?;
    }
    Ok(())
}

struct Part<'a> {
    archive_path: &'a Path,
    archive_name: &'a str,
    dest_root: &'a Path,
    report: &'a mut ReportFile,
    counters: &'a mut Counters,
}

struct Stored<'a> {
    member: &'a str,
    destination: &'a str,
    sha256: &'a str,
    bytes: u64,
    kind: &'a str,
    partner: Option<&'a str>,
    status: &'a str,
}

struct Note<'a> {
    member: &'a str,
    destination: &'a str,
    sha256: &'a str,
    bytes: u64,
    kind: &'a str,
    status: &'a str,
    partner: Option<Option<&'a str>>,
    photo_taken_time: Option<Option<&'a str>>,
    detail: Option<&'a str>,
}

impl Part<'_> {
    fn reject<R: Read>(
        &mut self,
        reader: R,
        member_name: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        self.counters.count_in = self.counters.count_in.saturating_add(1);
        let hashed = sha256_reader(reader, self.archive_path);
        self.counters.mismatched = self.counters.mismatched.saturating_add(1);
        let (sha, detail) = match hashed {
            Ok(digest) => (to_hex(&digest), detail.to_string()),
            Err(err) => (String::new(), format!("{detail}: {err}")),
        };
        self.report.line(&json!({
            "kind": "original",
            "archive": self.archive_name,
            "member": member_name,
            "destination": "",
            "sha256": sha,
            "bytes": 0,
            "status": "mismatch",
            "detail": detail,
        }))
    }

    fn store_member<R: Read>(
        &mut self,
        mut reader: R,
        member_name: &str,
        mtime: Option<SystemTime>,
    ) -> Result<(), IngestError> {
        self.counters.count_in = self.counters.count_in.saturating_add(1);
        let relative = match member_relative(member_name) {
            Ok(path) => path,
            Err(detail) => {
                return self.mismatch(reader, member_name, "", "original", &detail);
            }
        };
        let destination = rel_string(&relative);
        let dest = self.dest_root.join(&relative);
        if !dest.starts_with(self.dest_root) {
            return self.mismatch(
                reader,
                member_name,
                "",
                "original",
                "member path escapes the destination",
            );
        }
        let kind = kind_of(&relative);
        let partner = relative
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(sidecar_partner)
            .map(str::to_string);

        match fs::symlink_metadata(&dest) {
            Ok(meta) if meta.file_type().is_symlink() => self.mismatch(
                reader,
                member_name,
                &destination,
                kind,
                "destination is a symlink",
            ),
            Ok(meta) if meta.is_dir() => self.mismatch(
                reader,
                member_name,
                &destination,
                kind,
                "destination is a directory",
            ),
            Ok(_) => {
                let existing = sha256_file(&dest)?;
                let incoming = sha256_reader(reader, self.archive_path)?;
                if existing == incoming {
                    let bytes = fs::metadata(&dest)
                        .map_err(|err| IngestError::io(&dest, err))?
                        .len();
                    let sha = to_hex(&existing);
                    self.accept(
                        Stored {
                            member: member_name,
                            destination: &destination,
                            sha256: &sha,
                            bytes,
                            kind,
                            partner: partner.as_deref(),
                            status: "already-verified",
                        },
                        &dest,
                    )
                } else {
                    let sha = to_hex(&incoming);
                    self.fail(
                        member_name,
                        &destination,
                        &sha,
                        kind,
                        "destination already has different bytes; it was not overwritten",
                    )
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(mtime) = mtime else {
                    return self.mismatch(
                        reader,
                        member_name,
                        &destination,
                        kind,
                        "archive member has no timestamp",
                    );
                };
                match copy_verify_reader(&mut reader, self.archive_path, &dest, mtime) {
                    Ok(object) => self.accept(
                        Stored {
                            member: member_name,
                            destination: &destination,
                            sha256: &object.sha256,
                            bytes: object.byte_len,
                            kind,
                            partner: partner.as_deref(),
                            status: "stored",
                        },
                        &dest,
                    ),
                    Err(IngestError::ChecksumMismatch { message, .. }) => {
                        self.fail(member_name, &destination, "", kind, &message)
                    }
                    Err(IngestError::AlreadyExists { .. }) => self.fail(
                        member_name,
                        &destination,
                        "",
                        kind,
                        "destination appeared during copy; it was not overwritten",
                    ),
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(IngestError::io(&dest, err)),
        }
    }

    fn accept(&mut self, stored: Stored<'_>, stored_path: &Path) -> Result<(), IngestError> {
        self.counters.count_out = self.counters.count_out.saturating_add(1);
        let taken = if stored.kind == "sidecar" {
            Some(scan_photo_taken_time(stored_path)?)
        } else {
            None
        };
        self.emit(Note {
            member: stored.member,
            destination: stored.destination,
            sha256: stored.sha256,
            bytes: stored.bytes,
            kind: stored.kind,
            status: stored.status,
            partner: if stored.kind == "sidecar" {
                Some(stored.partner)
            } else {
                None
            },
            photo_taken_time: taken.as_ref().map(|value| value.as_deref()),
            detail: None,
        })
    }

    fn mismatch<R: Read>(
        &mut self,
        reader: R,
        member_name: &str,
        destination: &str,
        kind: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        let hashed = sha256_reader(reader, self.archive_path).ok();
        let sha = hashed
            .as_ref()
            .map(|digest| to_hex(digest))
            .unwrap_or_default();
        self.fail(member_name, destination, &sha, kind, detail)
    }

    fn fail(
        &mut self,
        member_name: &str,
        destination: &str,
        sha256: &str,
        kind: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        self.counters.mismatched = self.counters.mismatched.saturating_add(1);
        self.emit(Note {
            member: member_name,
            destination,
            sha256,
            bytes: 0,
            kind,
            status: "mismatch",
            partner: None,
            photo_taken_time: None,
            detail: Some(detail),
        })
    }

    fn emit(&mut self, note: Note<'_>) -> Result<(), IngestError> {
        let mut line = json!({
            "archive": self.archive_name,
            "member": note.member,
            "destination": note.destination,
            "sha256": note.sha256,
            "bytes": note.bytes,
            "kind": note.kind,
            "status": note.status,
        });
        if let Some(partner) = note.partner {
            line["partner"] = json!(partner);
        }
        if let Some(photo_taken_time) = note.photo_taken_time {
            line["photo_taken_time"] = json!(photo_taken_time);
        }
        if let Some(detail) = note.detail {
            line["detail"] = json!(detail);
        }
        self.report.line(&line)
    }
}

fn kind_of(relative: &Path) -> &'static str {
    match relative.file_name().and_then(|name| name.to_str()) {
        Some(name) if ends_with_ignore_ascii_case(name, ".json") => "sidecar",
        _ => "original",
    }
}

/// Media partner of a Takeout JSON name, when the name is a paired sidecar.
///
/// `IMG_0001.jpg.json`, `IMG_0001.jpg.supplemental-metadata.json`, and
/// `IMG_0001.jpg.supplemental-metadata(1).json` pair with `IMG_0001.jpg`.
/// A JSON file whose stem has no dot, such as `metadata.json`, is still copied
/// and has no partner.
fn sidecar_partner(file_name: &str) -> Option<&str> {
    let stem = strip_suffix_ignore_ascii_case(file_name, ".json")?;
    let stem = strip_suffix_ignore_ascii_case(stem, ".supplemental-metadata")
        .or_else(|| strip_numbered_supplemental(stem))
        .unwrap_or(stem);
    if stem.is_empty() || !stem.contains('.') {
        None
    } else {
        Some(stem)
    }
}

fn strip_numbered_supplemental(stem: &str) -> Option<&str> {
    let marker = ".supplemental-metadata(";
    let index = stem.rfind(marker)?;
    let rest = &stem[index + marker.len()..];
    let close = rest.find(')')?;
    if close == 0 || close + 1 != rest.len() {
        return None;
    }
    if !rest[..close].chars().all(|char| char.is_ascii_digit()) {
        return None;
    }
    Some(&stem[..index])
}

fn member_relative(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err("empty member name".to_string());
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(format!("absolute member path {name}"));
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(format!("drive member path {name}"));
    }
    if name.contains('\0') || name.chars().any(char::is_control) || name.contains('\\') {
        return Err(format!("unsafe member path {name}"));
    }
    let mut relative = PathBuf::new();
    for component in name.split('/') {
        if component.is_empty() || component == "." {
            if component.is_empty() {
                return Err(format!("empty path component in {name}"));
            }
            continue;
        }
        if component == ".." {
            return Err(format!("member path escapes the archive: {name}"));
        }
        check_filename(component).map_err(|_| format!("unsafe member path {name}"))?;
        relative.push(component);
    }
    if relative.as_os_str().is_empty() {
        return Err(format!("empty member path {name}"));
    }
    Ok(relative)
}

fn rel_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn utf8_path(bytes: &[u8]) -> Result<String, String> {
    let name =
        std::str::from_utf8(bytes).map_err(|_| "tar member path is not UTF-8".to_string())?;
    if name.contains('\\') {
        return Err(format!("unsafe member path {name}"));
    }
    Ok(name.to_string())
}

#[derive(Clone, Copy)]
enum ArchiveKind {
    Zip,
    Tgz,
}

fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveKind::Tgz)
    } else if name.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

fn list_archives(from: &Path) -> Result<Vec<PathBuf>, IngestError> {
    let meta = fs::symlink_metadata(from).map_err(|err| IngestError::io(from, err))?;
    if meta.file_type().is_symlink() {
        return Err(IngestError::Symlink(from.to_path_buf()));
    }
    if meta.is_file() {
        if archive_kind(from).is_none() {
            return Err(IngestError::store(
                "takeout",
                display_name(from),
                "not a .zip, .tgz, or .tar.gz archive",
            ));
        }
        return Ok(vec![from.to_path_buf()]);
    }
    if !meta.is_dir() {
        return Err(IngestError::NotAFile(from.to_path_buf()));
    }
    let mut archives = Vec::new();
    let mut names = Vec::new();
    for entry in fs::read_dir(from).map_err(|err| IngestError::io(from, err))? {
        let entry = entry.map_err(|err| IngestError::io(from, err))?;
        names.push(entry.file_name());
    }
    names.sort();
    for name in names {
        let path = from.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
        if meta.file_type().is_symlink() {
            return Err(IngestError::Symlink(path));
        }
        if meta.is_dir() {
            return Err(IngestError::store(
                "takeout",
                display_name(&path),
                "nested directory is not a Takeout part and was not skipped",
            ));
        }
        if !meta.is_file() || archive_kind(&path).is_none() {
            return Err(IngestError::store(
                "takeout",
                display_name(&path),
                "only .zip, .tgz, and .tar.gz parts are read; other files are not skipped",
            ));
        }
        archives.push(path);
    }
    if archives.is_empty() {
        return Err(IngestError::store(
            "takeout",
            display_name(from),
            "directory has no .zip, .tgz, or .tar.gz parts",
        ));
    }
    Ok(archives)
}

fn source_root(from: &Path) -> Result<PathBuf, IngestError> {
    if from.is_file() {
        from.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                IngestError::store("takeout", display_name(from), "archive has no parent")
            })
    } else {
        Ok(from.to_path_buf())
    }
}

fn refuse_layout(source_root: &Path, to: &Path, report: &Path) -> Result<(), IngestError> {
    let source = normalize_existing(source_root)?;
    let dest = normalize_path(to)?;
    let report_path = normalize_path(report)?;
    if dest.starts_with(&source) || source.starts_with(&dest) {
        return Err(IngestError::store(
            "takeout",
            dest.display().to_string(),
            format!(
                "refusing to extract in place: destination {} overlaps archive directory {}",
                dest.display(),
                source.display()
            ),
        ));
    }
    if report_path.starts_with(&source) || report_path.starts_with(&dest) {
        return Err(IngestError::store(
            "takeout",
            report_path.display().to_string(),
            "reconciliation report must be outside the archives and the library",
        ));
    }
    if report.exists() {
        return Err(IngestError::AlreadyExists {
            filename: display_name(report),
            destination: report.display().to_string(),
        });
    }
    Ok(())
}

fn normalize_existing(path: &Path) -> Result<PathBuf, IngestError> {
    let meta = fs::symlink_metadata(path).map_err(|err| IngestError::io(path, err))?;
    if meta.file_type().is_symlink() {
        return Err(IngestError::Symlink(path.to_path_buf()));
    }
    path.canonicalize()
        .map_err(|err| IngestError::io(path, err))
}

fn normalize_path(path: &Path) -> Result<PathBuf, IngestError> {
    let mut rest = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(IngestError::Symlink(current));
            }
            Ok(_) => {
                let mut canon = current
                    .canonicalize()
                    .map_err(|err| IngestError::io(&current, err))?;
                for part in rest.iter().rev() {
                    canon.push(part);
                }
                return Ok(canon);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = current.file_name().map(|name| name.to_os_string()) else {
                    return Err(IngestError::store(
                        "takeout",
                        path.display().to_string(),
                        "path does not exist",
                    ));
                };
                rest.push(name);
                if !current.pop() {
                    return Err(IngestError::store(
                        "takeout",
                        path.display().to_string(),
                        "path does not exist",
                    ));
                }
            }
            Err(err) => return Err(IngestError::io(&current, err)),
        }
    }
}

fn seal_archives(archives: &[PathBuf]) -> Result<Vec<ArchiveSeal>, IngestError> {
    let mut seals = Vec::with_capacity(archives.len());
    for path in archives {
        let sha = sha256_file(path)?;
        let bytes = fs::metadata(path)
            .map_err(|err| IngestError::io(path, err))?
            .len();
        seals.push(ArchiveSeal {
            name: file_name_str(path)?,
            path: path.clone(),
            sha256: to_hex(&sha),
            bytes,
        });
    }
    Ok(seals)
}

fn confirm_seals(
    seals: &[ArchiveSeal],
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<bool, IngestError> {
    let mut unchanged = true;
    for seal in seals {
        let sha = to_hex(&sha256_file(&seal.path)?);
        let bytes = fs::metadata(&seal.path)
            .map_err(|err| IngestError::io(&seal.path, err))?
            .len();
        if sha != seal.sha256 || bytes != seal.bytes {
            unchanged = false;
            counters.mismatched = counters.mismatched.saturating_add(1);
            report.line(&json!({
                "kind": "archive",
                "archive": seal.name,
                "sha256": sha,
                "bytes": bytes,
                "status": "mismatch",
                "detail": format!("archive bytes changed; before {}", seal.sha256),
            }))?;
        }
    }
    Ok(unchanged)
}

fn open_read_only(path: &Path) -> Result<File, IngestError> {
    OpenOptions::new()
        .read(true)
        .write(false)
        .open(path)
        .map_err(|err| IngestError::io(path, err))
}

fn zip_err(path: &Path, err: zip::result::ZipError) -> IngestError {
    IngestError::store("takeout", display_name(path), err)
}

fn display_name(path: &Path) -> String {
    file_name_str(path).unwrap_or_else(|_| path.display().to_string())
}

pub(crate) fn dos_to_system_time(dt: &zip::DateTime) -> Option<SystemTime> {
    let days = days_from_civil(
        i32::from(dt.year()),
        i32::from(dt.month()),
        i32::from(dt.day()),
    )?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(i64::from(dt.hour()) * 3_600)?
        .checked_add(i64::from(dt.minute()) * 60)?
        .checked_add(i64::from(dt.second()))?;
    if secs < 0 {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    }
}

/// Days since the Unix epoch for a civil date. Public-domain civil-from-days
/// conversion. Invalid month or day returns `None`.
fn days_from_civil(mut year: i32, month: i32, day: i32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    year -= i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year as u32;
    Some(i64::from(era) * 146_097 + i64::from(day_of_era) - 719_468)
}

struct PhotoTakenScan {
    key_matched: usize,
    timestamp_matched: usize,
    phase: ScanPhase,
    value: Vec<u8>,
    escape: bool,
    done: Option<String>,
}

enum ScanPhase {
    SeekKey,
    SeekTimestamp,
    SeekColon,
    SeekQuote,
    Capture,
    Done,
}

const PHOTO_TAKEN_KEY: &[u8] = b"\"photoTakenTime\"";
const TIMESTAMP_KEY: &[u8] = b"\"timestamp\"";

impl PhotoTakenScan {
    fn new() -> Self {
        Self {
            key_matched: 0,
            timestamp_matched: 0,
            phase: ScanPhase::SeekKey,
            value: Vec::new(),
            escape: false,
            done: None,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.done.is_some() {
            return;
        }
        match self.phase {
            ScanPhase::SeekKey => self.seek(byte, PHOTO_TAKEN_KEY, true),
            ScanPhase::SeekTimestamp => self.seek(byte, TIMESTAMP_KEY, false),
            ScanPhase::SeekColon => {
                if byte == b':' {
                    self.phase = ScanPhase::SeekQuote;
                } else if !byte.is_ascii_whitespace() {
                    self.phase = ScanPhase::SeekTimestamp;
                    self.timestamp_matched = 0;
                    self.seek(byte, TIMESTAMP_KEY, false);
                }
            }
            ScanPhase::SeekQuote => {
                if byte == b'"' {
                    self.phase = ScanPhase::Capture;
                    self.value.clear();
                    self.escape = false;
                } else if !byte.is_ascii_whitespace() {
                    self.phase = ScanPhase::SeekTimestamp;
                    self.timestamp_matched = 0;
                    self.seek(byte, TIMESTAMP_KEY, false);
                }
            }
            ScanPhase::Capture => {
                if self.escape {
                    self.value.push(byte);
                    self.escape = false;
                } else if byte == b'\\' {
                    self.escape = true;
                } else if byte == b'"' {
                    self.done = String::from_utf8(self.value.clone()).ok();
                    self.phase = ScanPhase::Done;
                } else {
                    self.value.push(byte);
                }
            }
            ScanPhase::Done => {}
        }
    }

    fn seek(&mut self, byte: u8, pattern: &[u8], key: bool) {
        let matched = if key {
            &mut self.key_matched
        } else {
            &mut self.timestamp_matched
        };
        if byte == pattern[*matched] {
            *matched += 1;
            if *matched == pattern.len() {
                *matched = 0;
                self.phase = if key {
                    ScanPhase::SeekTimestamp
                } else {
                    ScanPhase::SeekColon
                };
            }
        } else {
            *matched = usize::from(byte == pattern[0]);
        }
    }

    fn push_slice(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.push(*byte);
            if self.done.is_some() {
                break;
            }
        }
    }
}

fn scan_photo_taken_time(path: &Path) -> Result<Option<String>, IngestError> {
    let mut file = File::open(path).map_err(|err| IngestError::io(path, err))?;
    let mut scanner = PhotoTakenScan::new();
    let mut buf = [0u8; 8 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        if n == 0 {
            break;
        }
        scanner.push_slice(&buf[..n]);
        if scanner.done.is_some() {
            break;
        }
    }
    Ok(scanner.done)
}

fn ends_with_ignore_ascii_case(name: &str, suffix: &str) -> bool {
    strip_suffix_ignore_ascii_case(name, suffix).is_some()
}

fn strip_suffix_ignore_ascii_case<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    if name.len() < suffix.len() {
        return None;
    }
    let (head, tail) = name.split_at(name.len() - suffix.len());
    if tail.eq_ignore_ascii_case(suffix) {
        Some(head)
    } else {
        None
    }
}

impl ReportFile {
    fn create(path: &Path) -> Result<Self, IngestError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|err| IngestError::io(path, err))?;
        Ok(Self {
            file: BufWriter::new(file),
            path: path.to_path_buf(),
        })
    }

    fn line(&mut self, value: &serde_json::Value) -> Result<(), IngestError> {
        serde_json::to_writer(&mut self.file, value).map_err(|err| {
            IngestError::store("takeout-report", self.path.display().to_string(), err)
        })?;
        self.file
            .write_all(b"\n")
            .map_err(|err| IngestError::io(&self.path, err))?;
        Ok(())
    }

    fn finish(mut self) -> Result<(), IngestError> {
        self.file
            .flush()
            .map_err(|err| IngestError::io(&self.path, err))?;
        self.file
            .get_ref()
            .sync_all()
            .map_err(|err| IngestError::io(&self.path, err))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtime::mtime_of;
    use crate::test_support::Tmp;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn taken_json(title: &str, taken: &str, created: &str) -> Vec<u8> {
        format!(
            "{{\n  \"title\": \"{title}\",\n  \"description\": \"synthetic fixture\",\n  \"creationTime\": {{\"timestamp\": \"{created}\", \"formatted\": \"synthetic\"}},\n  \"photoTakenTime\": {{\"timestamp\": \"{taken}\", \"formatted\": \"synthetic\"}},\n  \"geoData\": {{\"latitude\": 0.0, \"longitude\": 0.0, \"altitude\": 0.0, \"latitudeSpan\": 0.0, \"longitudeSpan\": 0.0}}\n}}\n"
        )
        .into_bytes()
    }

    fn synthetic_bytes(prefix: &[u8], len: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(prefix);
        while bytes.len() < len {
            bytes.push((bytes.len() % 251) as u8);
        }
        bytes.truncate(len);
        bytes
    }

    fn zip_options(method: CompressionMethod) -> SimpleFileOptions {
        SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap())
    }

    fn write_zip(path: &Path, method: CompressionMethod, files: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = zip_options(method);
        for (name, bytes) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_tgz(path: &Path, files: &[(&str, &[u8])], mtime: u64) {
        let file = File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(bytes.len() as u64);
            header.set_mtime(mtime);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *bytes).unwrap();
        }
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();
    }

    fn report_lines(path: &Path) -> Vec<serde_json::Value> {
        let text = fs::read_to_string(path).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn lock_readonly(paths: &[PathBuf]) {
        for path in paths {
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_readonly(true);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn unix_epoch_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), Some(0));
    }

    #[test]
    fn partner_names_cover_takeout_json_spellings() {
        assert_eq!(sidecar_partner("IMG_0001.jpg.json"), Some("IMG_0001.jpg"));
        assert_eq!(
            sidecar_partner("CLIP.mp4.supplemental-metadata.json"),
            Some("CLIP.mp4")
        );
        assert_eq!(
            sidecar_partner("CLIP.mp4.supplemental-metadata(1).json"),
            Some("CLIP.mp4")
        );
        assert_eq!(sidecar_partner("metadata.json"), None);
        assert_eq!(sidecar_partner("user-generated-memory-titles.json"), None);
    }

    #[test]
    fn photo_taken_time_ignores_creation_time_and_split_chunks() {
        let json = taken_json("IMG_0001.jpg", "1590000000", "1600000000");
        let mut scanner = PhotoTakenScan::new();
        for chunk in json.chunks(3) {
            scanner.push_slice(chunk);
        }
        assert_eq!(scanner.done.as_deref(), Some("1590000000"));
    }

    #[test]
    fn member_paths_reject_escape_and_keep_album_names() {
        let relative =
            member_relative("Takeout/Google Photos/Photos from 2020/IMG_0001.jpg").unwrap();
        assert_eq!(
            rel_string(&relative),
            "Takeout/Google Photos/Photos from 2020/IMG_0001.jpg"
        );
        assert!(member_relative("../outside.jpg").is_err());
        assert!(member_relative("/etc/passwd").is_err());
        assert!(member_relative("album/../../outside.jpg").is_err());
        assert_eq!(
            rel_string(&member_relative("./Takeout/./IMG_0001.jpg").unwrap()),
            "Takeout/IMG_0001.jpg"
        );
    }

    #[test]
    fn split_zip_and_tgz_round_trip_is_byte_exact() {
        let tmp = Tmp::new();
        let from = tmp.path().join("takeout");
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        fs::create_dir(&from).unwrap();

        let jpeg = synthetic_bytes(
            b"\xff\xd8\xff\xdb synthetic-albumen-takeout-jpeg\n",
            2_500_000,
        );
        let html = b"<html>synthetic takeout index</html>\n".to_vec();
        let json = taken_json("IMG_0001.jpg", "1590000000", "1600000000");
        let mp4 = synthetic_bytes(b"synthetic-albumen-takeout-mp4\n", 64_000);
        let supplemental = taken_json("CLIP.mp4", "1590001111", "1600001111");
        assert!(!jpeg.windows(10).any(|window| window == b"1590000000"));

        let zip_a = from.join("takeout-20200520T160000Z-001.zip");
        let zip_b = from.join("takeout-20200520T160000Z-002.zip");
        let tgz = from.join("takeout-20200520T160000Z-003.tgz");
        write_zip(
            &zip_a,
            CompressionMethod::Deflated,
            &[
                (
                    "Takeout/Google Photos/Photos from 2020/IMG_0001.jpg",
                    jpeg.as_slice(),
                ),
                (
                    "Takeout/Google Photos/Photos from 2020/index.html",
                    html.as_slice(),
                ),
            ],
        );
        write_zip(
            &zip_b,
            CompressionMethod::Stored,
            &[(
                "Takeout/Google Photos/Photos from 2020/IMG_0001.jpg.json",
                json.as_slice(),
            )],
        );
        write_tgz(
            &tgz,
            &[
                ("Takeout/Google Photos/Album/CLIP.mp4", mp4.as_slice()),
                (
                    "Takeout/Google Photos/Album/CLIP.mp4.supplemental-metadata.json",
                    supplemental.as_slice(),
                ),
            ],
            1_590_001_111,
        );
        let before = [zip_a.clone(), zip_b.clone(), tgz.clone()]
            .into_iter()
            .map(|path| fs::read(&path).unwrap())
            .collect::<Vec<_>>();
        lock_readonly(&[zip_a.clone(), zip_b.clone(), tgz.clone()]);

        let outcome = import_takeout(&from, &to, &report).unwrap();
        assert_eq!(outcome.count_in, 5);
        assert_eq!(outcome.count_out, 5);
        assert_eq!(outcome.mismatched, 0);

        let photo = to.join("Takeout/Google Photos/Photos from 2020/IMG_0001.jpg");
        let sidecar = to.join("Takeout/Google Photos/Photos from 2020/IMG_0001.jpg.json");
        let page = to.join("Takeout/Google Photos/Photos from 2020/index.html");
        let clip = to.join("Takeout/Google Photos/Album/CLIP.mp4");
        let clip_json = to.join("Takeout/Google Photos/Album/CLIP.mp4.supplemental-metadata.json");
        assert_eq!(fs::read(&photo).unwrap(), jpeg);
        assert_eq!(fs::read(&sidecar).unwrap(), json);
        assert_eq!(fs::read(&page).unwrap(), html);
        assert_eq!(fs::read(&clip).unwrap(), mp4);
        assert_eq!(fs::read(&clip_json).unwrap(), supplemental);
        assert!(!fs::read(&photo)
            .unwrap()
            .windows(10)
            .any(|window| window == b"1590000000"));

        let expected_zip_mtime =
            dos_to_system_time(&zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap())
                .unwrap();
        assert_eq!(mtime_of(&photo).unwrap(), expected_zip_mtime);
        assert_eq!(
            mtime_of(&clip).unwrap(),
            UNIX_EPOCH + Duration::from_secs(1_590_001_111)
        );

        let lines = report_lines(&report);
        let summary = lines.last().unwrap();
        assert_eq!(summary["count_in"], 5);
        assert_eq!(summary["count_out"], 5);
        assert_eq!(summary["mismatched"], 0);
        assert_eq!(summary["archives_unchanged"], true);
        let file_lines = lines
            .iter()
            .filter(|line| line.get("summary").is_none() && line["kind"] != "archive")
            .collect::<Vec<_>>();
        assert_eq!(file_lines.len(), 5);
        for line in &file_lines {
            let dest = to.join(line["destination"].as_str().unwrap());
            assert_eq!(line["member"], line["destination"]);
            assert_eq!(line["sha256"], crate::sha256_hex(&dest).unwrap());
            assert_eq!(
                line["bytes"].as_u64().unwrap(),
                fs::metadata(&dest).unwrap().len()
            );
            if line["kind"] == "original" {
                assert!(line.get("photo_taken_time").is_none());
            }
        }
        let sidecar_line = file_lines
            .iter()
            .find(|line| {
                line["destination"]
                    .as_str()
                    .unwrap()
                    .ends_with("IMG_0001.jpg.json")
            })
            .unwrap();
        assert_eq!(sidecar_line["kind"], "sidecar");
        assert_eq!(sidecar_line["partner"], "IMG_0001.jpg");
        assert_eq!(sidecar_line["photo_taken_time"], "1590000000");
        let supplemental_line = file_lines
            .iter()
            .find(|line| {
                line["destination"]
                    .as_str()
                    .unwrap()
                    .ends_with("supplemental-metadata.json")
            })
            .unwrap();
        assert_eq!(supplemental_line["partner"], "CLIP.mp4");
        assert_eq!(supplemental_line["photo_taken_time"], "1590001111");

        let kept = mtime_of(&photo).unwrap();
        let again = tmp.path().join("report-again.jsonl");
        let outcome = import_takeout(&from, &to, &again).unwrap();
        assert_eq!(outcome.count_in, outcome.count_out);
        assert_eq!(fs::read(&photo).unwrap(), jpeg);
        assert_eq!(mtime_of(&photo).unwrap(), kept);
        let again_lines = report_lines(&again);
        assert!(again_lines
            .iter()
            .any(|line| line["status"] == "already-verified"));

        for (path, bytes) in [&zip_a, &zip_b, &tgz].iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        let extracted = from.join("Takeout");
        assert!(!extracted.exists());
    }

    #[test]
    fn different_destination_bytes_are_not_overwritten() {
        let tmp = Tmp::new();
        let incoming = tmp.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let from = incoming.join("one.zip");
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let jpeg = b"synthetic-original".to_vec();
        write_zip(
            &from,
            CompressionMethod::Stored,
            &[("Takeout/IMG_0001.jpg", jpeg.as_slice())],
        );
        let dest = to.join("Takeout/IMG_0001.jpg");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, b"different-bytes").unwrap();
        let err = import_takeout(&from, &to, &report).unwrap_err();
        assert!(
            matches!(err, IngestError::TakeoutReconciliation { .. }),
            "{err}"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"different-bytes");
        let summary = report_lines(&report).pop().unwrap();
        assert_eq!(summary["count_in"].as_u64(), Some(1));
        assert_eq!(summary["count_out"].as_u64(), Some(0));
        assert!(summary["mismatched"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn path_escape_is_not_written() {
        let tmp = Tmp::new();
        let incoming = tmp.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let from = incoming.join("slip.zip");
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        write_zip(
            &from,
            CompressionMethod::Stored,
            &[
                ("../outside.jpg", b"nope".as_slice()),
                ("Takeout/ok.jpg", b"ok".as_slice()),
            ],
        );
        let err = import_takeout(&from, &to, &report).unwrap_err();
        assert!(
            matches!(err, IngestError::TakeoutReconciliation { .. }),
            "{err}"
        );
        assert!(!tmp.path().join("outside.jpg").exists());
        assert!(!to.join("outside.jpg").exists());
        assert_eq!(fs::read(to.join("Takeout/ok.jpg")).unwrap(), b"ok");
    }

    #[test]
    fn tar_symlink_is_not_materialized() {
        let tmp = Tmp::new();
        let incoming = tmp.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let from = incoming.join("link.tgz");
        let file = File::create(&from).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path("Takeout/link.jpg").unwrap();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("elsewhere").unwrap();
        header.set_size(0);
        header.set_mtime(1_590_000_000);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_path("Takeout/ok.jpg").unwrap();
        header.set_size(2);
        header.set_mtime(1_590_000_000);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"ok"[..]).unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let err = import_takeout(&from, &to, &report).unwrap_err();
        assert!(
            matches!(err, IngestError::TakeoutReconciliation { .. }),
            "{err}"
        );
        assert!(!to.join("Takeout/link.jpg").exists());
        assert_eq!(fs::read(to.join("Takeout/ok.jpg")).unwrap(), b"ok");
    }

    #[test]
    fn in_place_destination_is_refused() {
        let tmp = Tmp::new();
        let from = tmp.path().join("takeout");
        fs::create_dir(&from).unwrap();
        write_zip(
            &from.join("part.zip"),
            CompressionMethod::Stored,
            &[("Takeout/ok.jpg", b"ok".as_slice())],
        );
        let err =
            import_takeout(&from, &from.join("out"), &tmp.path().join("report.jsonl")).unwrap_err();
        assert!(matches!(err, IngestError::Store { .. }));
        assert!(!from.join("out").exists());
        assert!(!from.join("Takeout").exists());
    }
}
