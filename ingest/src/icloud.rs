// SPDX-License-Identifier: Apache-2.0

//! iCloud Photos import.
//!
//! A Photos export, an iCloud download, or a copied Photos library is opened
//! read-only. A download may be several zip or tgz parts plus Photo Details
//! CSVs. Every regular file is `copy_verify`'d. Originals and edited renders
//! are both kept, byte for byte. Live Photo stills and videos stay side by
//! side, and AAE, XMP, and Photo Details CSVs are copied as stored. Album,
//! favorite, and caption metadata from a Photos library is exported beside
//! the original. The library database is opened `mode=ro&immutable=1` and is
//! never written. A capture timestamp read from a sidecar is reported there
//! and is never written into an original.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_json::json;
use tar::Archive;
use zip::ZipArchive;

use crate::checksum::{sha256_file, sha256_reader, to_hex};
use crate::dest;
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::local::{copy_verify_file, copy_verify_reader};
use crate::takeout::dos_to_system_time;

/// Counts from an iCloud import whose reconciliation matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcloudReport {
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
    unmatched: u64,
}

struct ReportFile {
    file: BufWriter<File>,
    path: PathBuf,
}

/// List what [`import_icloud`] would copy. Creates nothing.
pub fn plan_icloud(from: &Path, to: &Path, report: &Path) -> Result<IcloudPlan, IngestError> {
    let source_meta = fs::symlink_metadata(from).map_err(|err| IngestError::io(from, err))?;
    if source_meta.file_type().is_symlink() {
        return Err(IngestError::Symlink(from.to_path_buf()));
    }
    let source_root = if source_meta.is_file() {
        from.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(from)
    } else {
        from
    };
    refuse_layout(source_root, to, report)?;
    let names = if source_meta.is_file() {
        archive_plan_names(from)?
    } else if is_download_dir(from)? {
        download_plan_names(from)?
    } else {
        tree_plan_names(from)?
    };
    let index = name_index(&names);
    let mut unmatched = Vec::new();
    let mut live_photos = Vec::new();
    for member in &names {
        if sidecar_unmatched(
            member,
            sidecar_partner_stem(member).as_deref(),
            from,
            false,
            Some(&index),
        ) {
            unmatched.push(member.clone());
        }
        if let Some(stem) = live_photo_stem(member) {
            if live_pair_present(from, member, stem, false, Some(&index)) {
                live_photos.push(stem.to_string());
            }
        }
    }
    unmatched.sort();
    unmatched.dedup();
    live_photos.sort();
    live_photos.dedup();
    Ok(IcloudPlan {
        files: names.len() as u64,
        unmatched,
        live_photos,
    })
}

/// Counts and names from a dry-run. Nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcloudPlan {
    pub files: u64,
    pub unmatched: Vec<String>,
    pub live_photos: Vec<String>,
}

fn archive_plan_names(path: &Path) -> Result<Vec<String>, IngestError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        tgz_member_names(path)
    } else if name.ends_with(".zip") {
        zip_member_names(path)
    } else {
        Err(IngestError::store(
            "icloud",
            name,
            "a file source must be a .zip, .tgz, or .tar.gz download",
        ))
    }
}

fn download_plan_names(root: &Path) -> Result<Vec<String>, IngestError> {
    let mut files = Vec::new();
    collect_download(root, "", &mut files)?;
    let mut names = Vec::new();
    for (relative, path) in files {
        if is_archive_name(&path) {
            let mut members = archive_plan_names(&path)?;
            names.append(&mut members);
        } else {
            names.push(relative);
        }
    }
    Ok(names)
}

fn tree_plan_names(root: &Path) -> Result<Vec<String>, IngestError> {
    let mut names = Vec::new();
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), IngestError> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
            let entry = entry.map_err(|err| IngestError::io(dir, err))?;
            entries.push(entry.file_name());
        }
        entries.sort();
        for name in entries {
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = dir.join(name);
            let relative = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                walk(&path, &relative, out)?;
            } else {
                out.push(relative);
            }
        }
        Ok(())
    }
    walk(root, "", &mut names)?;
    Ok(names)
}

/// Copy an iCloud Photos download, export, or Photos library into `to`.
///
/// `from` is a directory or one `.zip`, `.tgz`, or `.tar.gz` archive. A
/// directory of archives and CSV files is a multi-part iCloud download. Any
/// other directory is an export folder or a `.photoslibrary` bundle. The
/// destination must not overlap that source. `report` is a new JSONL file
/// outside both.
///
/// The function returns [`IngestError::IcloudReconciliation`] when any file is
/// missing or hashed differently, when a source changes, or when there is
/// nothing to copy. The report is still written.
pub fn import_icloud(from: &Path, to: &Path, report: &Path) -> Result<IcloudReport, IngestError> {
    let source_meta = fs::symlink_metadata(from).map_err(|err| IngestError::io(from, err))?;
    if source_meta.file_type().is_symlink() {
        return Err(IngestError::Symlink(from.to_path_buf()));
    }
    let source_root = if source_meta.is_file() {
        from.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(from)
    } else {
        from
    };
    refuse_layout(source_root, to, report)?;
    dest::create_dir_all(to)?;
    if let Some(parent) = report.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }

    let mut report_file = ReportFile::create(report)?;
    let mut counters = Counters::default();
    let mut fatal: Option<IngestError> = None;
    let mut sources_unchanged = true;

    if source_meta.is_file() {
        let before = sha256_file(from)?;
        let before_len = source_meta.len();
        let name = file_name_str(from)?;
        report_file.line(&json!({
            "kind": "archive",
            "archive": name,
            "sha256": to_hex(&before),
            "bytes": before_len,
            "status": "sealed",
        }))?;
        if let Err(err) = import_archive(from, to, &mut report_file, &mut counters) {
            fatal = Some(err);
        }
        let after = sha256_file(from)?;
        let after_len = fs::metadata(from)
            .map_err(|err| IngestError::io(from, err))?
            .len();
        if after != before || after_len != before_len {
            sources_unchanged = false;
            counters.mismatched = counters.mismatched.saturating_add(1);
            report_file.line(&json!({
                "kind": "archive",
                "archive": name,
                "sha256": to_hex(&after),
                "bytes": after_len,
                "status": "mismatch",
                "detail": format!("archive bytes changed; before {}", to_hex(&before)),
            }))?;
        }
    } else if source_meta.is_dir() {
        match is_download_dir(from) {
            Ok(true) => {
                if let Err(err) = import_download(
                    from,
                    to,
                    &mut report_file,
                    &mut counters,
                    &mut sources_unchanged,
                ) {
                    fatal = Some(err);
                }
            }
            Ok(false) => {
                if let Err(err) = import_tree(from, to, &mut report_file, &mut counters) {
                    fatal = Some(err);
                } else {
                    if let Err(err) =
                        export_library_metadata(from, to, &mut report_file, &mut counters)
                    {
                        fatal = Some(err);
                    }
                    match confirm_tree(from, &mut report_file, &mut counters) {
                        Ok(still) => sources_unchanged = still,
                        Err(err) => {
                            fatal = Some(err);
                            sources_unchanged = false;
                        }
                    }
                }
            }
            Err(err) => fatal = Some(err),
        }
    } else {
        return Err(IngestError::NotAFile(from.to_path_buf()));
    }

    if fatal.is_none() {
        if let Err(err) = restore_icloud_sidecars(to, &mut report_file, &mut counters) {
            fatal = Some(err);
        }
    }

    if counters.count_in == 0 && fatal.is_none() {
        counters.mismatched = counters.mismatched.saturating_add(1);
        let _ = report_file.line(&json!({
            "kind": "file",
            "status": "mismatch",
            "detail": "source contained no files",
        }));
    }
    report_file.line(&json!({
        "summary": true,
        "count_in": counters.count_in,
        "count_out": counters.count_out,
        "mismatched": counters.mismatched,
        "unmatched": counters.unmatched,
        "sources_unchanged": sources_unchanged,
    }))?;
    report_file.finish()?;

    if let Some(err) = fatal {
        return Err(err);
    }
    if counters.count_in != counters.count_out || counters.mismatched != 0 || !sources_unchanged {
        return Err(IngestError::IcloudReconciliation {
            count_in: counters.count_in,
            count_out: counters.count_out,
            mismatched: counters.mismatched,
            sources_unchanged,
            report: report.display().to_string(),
        });
    }
    Ok(IcloudReport {
        count_in: counters.count_in,
        count_out: counters.count_out,
        mismatched: counters.mismatched,
        report: report.to_path_buf(),
    })
}

fn is_download_dir(root: &Path) -> Result<bool, IngestError> {
    fn walk(dir: &Path, archives: &mut bool, other: &mut bool) -> Result<(), IngestError> {
        if *other {
            return Ok(());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
            let entry = entry.map_err(|err| IngestError::io(dir, err))?;
            names.push(entry.file_name());
        }
        names.sort();
        for name in names {
            if *other {
                break;
            }
            let path = dir.join(&name);
            let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
            if meta.file_type().is_symlink() || meta.is_dir() {
                if meta.is_dir() {
                    walk(&path, archives, other)?;
                }
                continue;
            }
            if !meta.is_file() {
                *other = true;
                break;
            }
            if is_archive_name(&path) {
                *archives = true;
            } else if !is_csv_name(&path) {
                *other = true;
            }
        }
        Ok(())
    }
    let mut archives = false;
    let mut other = false;
    walk(root, &mut archives, &mut other)?;
    Ok(archives && !other)
}

fn is_archive_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".zip") || name.ends_with(".tgz") || name.ends_with(".tar.gz")
}

fn is_csv_name(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
}

fn import_download(
    root: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
    sources_unchanged: &mut bool,
) -> Result<(), IngestError> {
    let archive_name = file_name_str(root).unwrap_or_else(|_| root.display().to_string());
    let mut files = Vec::new();
    collect_download(root, "", &mut files)?;
    files.sort();
    for (relative, path) in files {
        let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
        if meta.file_type().is_symlink() {
            counters.count_in = counters.count_in.saturating_add(1);
            counters.mismatched = counters.mismatched.saturating_add(1);
            report.line(&json!({
                "kind": "file",
                "archive": archive_name,
                "member": relative,
                "destination": "",
                "sha256": "",
                "bytes": 0,
                "status": "mismatch",
                "detail": "refusing to follow a symlink",
            }))?;
            continue;
        }
        if is_archive_name(&path) {
            seal_archive(&path, dest_root, report, counters, sources_unchanged)?;
        } else {
            Session {
                label: root,
                archive_name: &archive_name,
                dest_root,
                report,
                counters,
                on_disk: true,
                names: None,
            }
            .place_file(&path, &relative)?;
        }
    }
    Ok(())
}

fn collect_download(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), IngestError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        names.push(entry.file_name());
    }
    names.sort();
    for name in names {
        let Some(name) = name.to_str() else {
            return Err(IngestError::store(
                "icloud",
                dir.display().to_string(),
                "filename is not UTF-8",
            ));
        };
        let path = dir.join(name);
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            collect_download(&path, &relative, out)?;
            continue;
        }
        out.push((relative, path));
    }
    Ok(())
}

fn seal_archive(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
    sources_unchanged: &mut bool,
) -> Result<(), IngestError> {
    let before = sha256_file(path)?;
    let before_len = fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len();
    let name = file_name_str(path)?;
    report.line(&json!({
        "kind": "archive",
        "archive": name,
        "sha256": to_hex(&before),
        "bytes": before_len,
        "status": "sealed",
    }))?;
    import_archive(path, dest_root, report, counters)?;
    let after = sha256_file(path)?;
    let after_len = fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len();
    if after != before || after_len != before_len {
        *sources_unchanged = false;
        counters.mismatched = counters.mismatched.saturating_add(1);
        report.line(&json!({
            "kind": "archive",
            "archive": name,
            "sha256": to_hex(&after),
            "bytes": after_len,
            "status": "mismatch",
            "detail": format!("archive bytes changed; before {}", to_hex(&before)),
        }))?;
    }
    Ok(())
}

fn export_library_metadata(
    root: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let archive_name = file_name_str(root).unwrap_or_else(|_| root.display().to_string());
    crate::photos_library::for_each_metadata(root, |row| match row {
        crate::photos_library::MetaRow::Missing { filename } => {
            counters.count_in = counters.count_in.saturating_add(1);
            counters.mismatched = counters.mismatched.saturating_add(1);
            report.line(&json!({
                "kind": "sidecar",
                "archive": archive_name,
                "member": filename,
                "destination": "",
                "sha256": "",
                "bytes": 0,
                "status": "mismatch",
                "on_disk": false,
                "detail": "photos library metadata names a file that was not copied",
            }))
        }
        crate::photos_library::MetaRow::Ready {
            relative,
            favorite,
            caption,
            albums,
            capture_time,
            latitude,
            longitude,
        } => {
            let bytes = metadata_json(
                &relative,
                favorite,
                caption.as_deref(),
                &albums,
                capture_time.as_deref(),
                latitude,
                longitude,
            )?;
            let member = format!("{relative}.albumen.json");
            let mtime = crate::mtime::mtime_of(&root.join(&relative))?;
            Session {
                label: root,
                archive_name: &archive_name,
                dest_root,
                report,
                counters,
                on_disk: false,
                names: None,
            }
            .place_reader(&mut std::io::Cursor::new(bytes), &member, Some(mtime))
        }
    })
}

fn metadata_json(
    partner: &str,
    favorite: bool,
    caption: Option<&str>,
    albums: &[String],
    capture_time: Option<&str>,
    latitude: Option<f64>,
    longitude: Option<f64>,
) -> Result<Vec<u8>, IngestError> {
    let mut map = serde_json::Map::new();
    if !albums.is_empty() {
        map.insert("albums".to_string(), json!(albums));
    }
    if let Some(caption) = caption {
        map.insert("caption".to_string(), json!(caption));
    }
    if favorite {
        map.insert("favorite".to_string(), json!(true));
    }
    if let Some(capture_time) = capture_time {
        map.insert("captureTime".to_string(), json!(capture_time));
    }
    if let (Some(latitude), Some(longitude)) = (latitude, longitude) {
        map.insert("latitude".to_string(), json!(latitude));
        map.insert("longitude".to_string(), json!(longitude));
    }
    map.insert("partner".to_string(), json!(partner));
    let mut bytes = serde_json::to_vec(&serde_json::Value::Object(map))
        .map_err(|err| IngestError::store("icloud", partner.to_string(), err))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn import_archive(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        import_tgz(path, dest_root, report, counters)
    } else if lower.ends_with(".zip") {
        import_zip(path, dest_root, report, counters)
    } else {
        Err(IngestError::store(
            "icloud",
            name,
            "a file source must be a .zip, .tgz, or .tar.gz download",
        ))
    }
}

fn import_zip(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let listed = name_index(&zip_member_names(path)?);
    let file = open_read_only(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| {
        IngestError::store("icloud", file_name_str(path).unwrap_or_default(), err)
    })?;
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
                    "kind": "file",
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
                note_broken(
                    report,
                    counters,
                    &archive_name,
                    &format!("zip member name: {err}"),
                )?;
                continue;
            }
        };
        if !entry.is_file() {
            note_broken(
                report,
                counters,
                &archive_name,
                &format!("{member_name} is not a regular file"),
            )?;
            continue;
        }
        let mtime = entry.last_modified().as_ref().and_then(dos_to_system_time);
        Session {
            label: path,
            archive_name: &archive_name,
            dest_root,
            report,
            counters,
            on_disk: false,
            names: Some(&listed),
        }
        .place_reader(entry, &member_name, mtime)?;
    }
    Ok(())
}

fn import_tgz(
    path: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let listed = name_index(&tgz_member_names(path)?);
    let file = open_read_only(path)?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let archive_name = file_name_str(path)?;
    let entries = archive
        .entries()
        .map_err(|err| IngestError::io(path, err))?;
    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                note_broken(
                    report,
                    counters,
                    &archive_name,
                    &format!("tar member: {err}"),
                )?;
                break;
            }
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let bytes = entry.path_bytes();
        let member_name = match std::str::from_utf8(&bytes) {
            Ok(name) if !name.contains('\\') => name.to_string(),
            _ => {
                note_broken(
                    report,
                    counters,
                    &archive_name,
                    "tar member path is not a safe UTF-8 path",
                )?;
                continue;
            }
        };
        if !kind.is_file() {
            note_broken(
                report,
                counters,
                &archive_name,
                &format!("{member_name} is not a regular file"),
            )?;
            continue;
        }
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        Session {
            label: path,
            archive_name: &archive_name,
            dest_root,
            report,
            counters,
            on_disk: false,
            names: Some(&listed),
        }
        .place_reader(&mut entry, &member_name, mtime)?;
    }
    Ok(())
}

fn note_broken(
    report: &mut ReportFile,
    counters: &mut Counters,
    archive_name: &str,
    detail: &str,
) -> Result<(), IngestError> {
    counters.count_in = counters.count_in.saturating_add(1);
    counters.mismatched = counters.mismatched.saturating_add(1);
    report.line(&json!({
        "kind": "file",
        "archive": archive_name,
        "member": "",
        "destination": "",
        "sha256": "",
        "bytes": 0,
        "status": "mismatch",
        "detail": detail,
    }))
}

fn import_tree(
    root: &Path,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let archive_name = file_name_str(root).unwrap_or_else(|_| root.display().to_string());
    walk_tree(root, &archive_name, dest_root, report, counters)
}

fn walk_tree(
    root: &Path,
    archive_name: &str,
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    fn walk(
        dir: &Path,
        prefix: &str,
        root: &Path,
        archive_name: &str,
        dest_root: &Path,
        report: &mut ReportFile,
        counters: &mut Counters,
    ) -> Result<(), IngestError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
            let entry = entry.map_err(|err| IngestError::io(dir, err))?;
            names.push(entry.file_name());
        }
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let Some(name) = name.to_str() else {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "file",
                    "archive": archive_name,
                    "member": "",
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": "filename is not UTF-8",
                }))?;
                continue;
            };
            let relative = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
            if meta.file_type().is_symlink() {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "file",
                    "archive": archive_name,
                    "member": relative,
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": "refusing to follow a symlink",
                }))?;
                continue;
            }
            if meta.is_dir() {
                walk(
                    &path,
                    &relative,
                    root,
                    archive_name,
                    dest_root,
                    report,
                    counters,
                )?;
                continue;
            }
            if !meta.is_file() {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "file",
                    "archive": archive_name,
                    "member": relative,
                    "destination": "",
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": "not a regular file",
                }))?;
                continue;
            }
            Session {
                label: root,
                archive_name,
                dest_root,
                report,
                counters,
                on_disk: true,
                names: None,
            }
            .place_file(&path, &relative)?;
        }
        Ok(())
    }
    walk(root, "", root, archive_name, dest_root, report, counters)
}

fn confirm_tree(
    root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<bool, IngestError> {
    report.flush_to_disk()?;
    let file = File::open(&report.path).map_err(|err| IngestError::io(&report.path, err))?;
    let reader = BufReader::new(file);
    let mut unchanged = true;
    for line in reader.lines() {
        let line = line.map_err(|err| IngestError::io(&report.path, err))?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|err| {
            IngestError::store("icloud-report", report.path.display().to_string(), err)
        })?;
        if value.get("summary").is_some() || value["kind"] == "archive" || value["on_disk"] == false
        {
            continue;
        }
        let Some(sha) = value["sha256"].as_str() else {
            continue;
        };
        if sha.is_empty() {
            continue;
        }
        let Some(member) = value["member"].as_str() else {
            continue;
        };
        if member.is_empty() {
            continue;
        }
        let source = root.join(member);
        let again = match sha256_file(&source) {
            Ok(digest) => to_hex(&digest),
            Err(err) => {
                unchanged = false;
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "file",
                    "member": member,
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": format!("source changed or could not be re-read: {err}"),
                }))?;
                continue;
            }
        };
        if again != sha {
            unchanged = false;
            counters.mismatched = counters.mismatched.saturating_add(1);
            report.line(&json!({
                "kind": "file",
                "member": member,
                "sha256": again,
                "bytes": 0,
                "status": "mismatch",
                "detail": format!("source bytes changed; before {sha}"),
            }))?;
        }
    }
    Ok(unchanged)
}

struct Session<'a> {
    label: &'a Path,
    archive_name: &'a str,
    dest_root: &'a Path,
    report: &'a mut ReportFile,
    counters: &'a mut Counters,
    on_disk: bool,
    names: Option<&'a NameIndex>,
}

struct NameIndex {
    pairs: HashSet<String>,
    media: HashSet<String>,
}

struct Filed<'a> {
    member: &'a str,
    destination: &'a str,
    sha256: &'a str,
    bytes: u64,
    status: &'a str,
}

struct Note<'a> {
    member: &'a str,
    destination: &'a str,
    sha256: &'a str,
    bytes: u64,
    status: &'a str,
    capture_time: Option<&'a str>,
    detail: Option<&'a str>,
    on_disk: bool,
}

impl Session<'_> {
    fn place_file(&mut self, source: &Path, member_name: &str) -> Result<(), IngestError> {
        self.counters.count_in = self.counters.count_in.saturating_add(1);
        let relative = match safe_relative(member_name) {
            Ok(path) => path,
            Err(detail) => return self.fail_file(source, member_name, "", &detail),
        };
        let destination = rel_string(&relative);
        let dest = self.dest_root.join(&relative);
        if !dest.starts_with(self.dest_root) {
            return self.fail_file(
                source,
                member_name,
                "",
                "member path escapes the destination",
            );
        }
        match fs::symlink_metadata(&dest) {
            Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => self.fail_file(
                source,
                member_name,
                &destination,
                "destination is not a regular file; it was not overwritten",
            ),
            Ok(_) => {
                let existing = sha256_file(&dest)?;
                let incoming = sha256_file(source)?;
                if existing == incoming {
                    let bytes = meta_len(&dest)?;
                    let sha = to_hex(&existing);
                    self.accept(
                        Filed {
                            member: member_name,
                            destination: &destination,
                            sha256: &sha,
                            bytes,
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
                        "destination already has different bytes; it was not overwritten",
                    )
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let parent = dest.parent().unwrap_or(self.dest_root);
                match copy_verify_file(source, parent) {
                    Ok(object) => self.accept(
                        Filed {
                            member: member_name,
                            destination: &destination,
                            sha256: &object.sha256,
                            bytes: object.byte_len,
                            status: "stored",
                        },
                        &dest,
                    ),
                    Err(IngestError::ChecksumMismatch { message, .. }) => {
                        self.fail(member_name, &destination, "", &message)
                    }
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(IngestError::io(&dest, err)),
        }
    }

    fn place_reader<R: Read>(
        &mut self,
        mut reader: R,
        member_name: &str,
        mtime: Option<SystemTime>,
    ) -> Result<(), IngestError> {
        self.counters.count_in = self.counters.count_in.saturating_add(1);
        let relative = match safe_relative(member_name) {
            Ok(path) => path,
            Err(detail) => return self.fail_reader(reader, member_name, "", &detail),
        };
        let destination = rel_string(&relative);
        let dest = self.dest_root.join(&relative);
        if !dest.starts_with(self.dest_root) {
            return self.fail_reader(
                reader,
                member_name,
                "",
                "member path escapes the destination",
            );
        }
        match fs::symlink_metadata(&dest) {
            Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => self.fail_reader(
                reader,
                member_name,
                &destination,
                "destination is not a regular file; it was not overwritten",
            ),
            Ok(_) => {
                let existing = sha256_file(&dest)?;
                let incoming = sha256_reader(&mut reader, self.label)?;
                if existing == incoming {
                    let bytes = meta_len(&dest)?;
                    let sha = to_hex(&existing);
                    self.accept(
                        Filed {
                            member: member_name,
                            destination: &destination,
                            sha256: &sha,
                            bytes,
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
                        "destination already has different bytes; it was not overwritten",
                    )
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(mtime) = mtime else {
                    return self.fail_reader(
                        reader,
                        member_name,
                        &destination,
                        "archive member has no timestamp",
                    );
                };
                match copy_verify_reader(&mut reader, self.label, &dest, mtime) {
                    Ok(object) => self.accept(
                        Filed {
                            member: member_name,
                            destination: &destination,
                            sha256: &object.sha256,
                            bytes: object.byte_len,
                            status: "stored",
                        },
                        &dest,
                    ),
                    Err(IngestError::ChecksumMismatch { message, .. }) => {
                        self.fail(member_name, &destination, "", &message)
                    }
                    Err(IngestError::AlreadyExists { .. }) => self.fail(
                        member_name,
                        &destination,
                        "",
                        "destination appeared during copy; it was not overwritten",
                    ),
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(IngestError::io(&dest, err)),
        }
    }

    fn accept(&mut self, filed: Filed<'_>, stored: &Path) -> Result<(), IngestError> {
        self.counters.count_out = self.counters.count_out.saturating_add(1);
        let captured = if kind_of(filed.member) == "sidecar" {
            scan_capture_time(stored)?
        } else {
            None
        };
        self.emit(Note {
            member: filed.member,
            destination: filed.destination,
            sha256: filed.sha256,
            bytes: filed.bytes,
            status: filed.status,
            capture_time: captured.as_deref(),
            detail: None,
            on_disk: self.on_disk,
        })
    }

    fn fail_file(
        &mut self,
        source: &Path,
        member_name: &str,
        destination: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        let hashed = sha256_file(source).ok();
        let sha = hashed
            .as_ref()
            .map(|digest| to_hex(digest))
            .unwrap_or_default();
        self.fail(member_name, destination, &sha, detail)
    }

    fn fail_reader<R: Read>(
        &mut self,
        reader: R,
        member_name: &str,
        destination: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        let hashed = sha256_reader(reader, self.label).ok();
        let sha = hashed
            .as_ref()
            .map(|digest| to_hex(digest))
            .unwrap_or_default();
        self.fail(member_name, destination, &sha, detail)
    }

    fn fail(
        &mut self,
        member_name: &str,
        destination: &str,
        sha256: &str,
        detail: &str,
    ) -> Result<(), IngestError> {
        self.counters.mismatched = self.counters.mismatched.saturating_add(1);
        self.emit(Note {
            member: member_name,
            destination,
            sha256,
            bytes: 0,
            status: "mismatch",
            capture_time: None,
            detail: Some(detail),
            on_disk: self.on_disk,
        })
    }

    fn emit(&mut self, note: Note<'_>) -> Result<(), IngestError> {
        let kind = if note.member.is_empty() {
            "file"
        } else {
            kind_of(note.member)
        };
        let mut line = json!({
            "archive": self.archive_name,
            "member": note.member,
            "destination": note.destination,
            "sha256": note.sha256,
            "bytes": note.bytes,
            "kind": kind,
            "status": note.status,
        });
        if let Some(stem) = live_photo_stem(note.member) {
            if live_pair_present(self.label, note.member, stem, self.on_disk, self.names) {
                line["live_photo_stem"] = json!(stem);
            }
        }
        if kind == "sidecar" {
            let partner = sidecar_partner_stem(note.member);
            line["partner_stem"] = json!(partner);
            line["capture_time"] = json!(note.capture_time);
            if sidecar_unmatched(
                note.member,
                partner.as_deref(),
                self.label,
                self.on_disk,
                self.names,
            ) {
                line["unmatched"] = json!(true);
                self.counters.unmatched = self.counters.unmatched.saturating_add(1);
            }
        }
        if !note.on_disk {
            line["on_disk"] = json!(false);
        }
        if let Some(detail) = note.detail {
            line["detail"] = json!(detail);
        } else if note.member.to_ascii_lowercase().ends_with(".albumen.json") {
            line["detail"] =
                json!("photos library metadata export; source database was not modified");
        }
        self.report.line(&line)
    }
}

fn kind_of(member: &str) -> &'static str {
    let name = member.rsplit('/').next().unwrap_or(member);
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".aae")
        || lower.ends_with(".xmp")
        || lower.ends_with(".albumen.json")
        || lower.ends_with(".csv")
    {
        return "sidecar";
    }
    if is_edited_path(member) {
        return "edited";
    }
    if is_media_name(name) || is_original_path(member) {
        return "original";
    }
    "file"
}

fn is_edited_path(member: &str) -> bool {
    let name = member.rsplit('/').next().unwrap_or(member);
    let lower_name = name.to_ascii_lowercase();
    if let Some(stem) = lower_name.rsplit_once('.').map(|(stem, _)| stem) {
        if stem.ends_with("-edited") {
            return true;
        }
    }
    member.split('/').any(|part| {
        part == "renders"
            || part == "derivatives"
            || part == "Edited"
            || part.eq_ignore_ascii_case("edited")
    })
}

fn is_original_path(member: &str) -> bool {
    member
        .split('/')
        .any(|part| part == "originals" || part == "Masters")
}

fn is_media_name(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    is_media_ext(ext)
}

fn is_media_ext(ext: &str) -> bool {
    const EXT: &[&str] = &[
        "jpg", "jpeg", "heic", "heif", "hif", "png", "tif", "tiff", "avif", "webp", "dng", "cr2",
        "cr3", "nef", "arw", "raf", "orf", "rw2", "raw", "mov", "mp4", "m4v",
    ];
    EXT.iter().any(|item| ext.eq_ignore_ascii_case(item))
}

fn name_index(members: &[String]) -> NameIndex {
    let mut stills: HashSet<String> = HashSet::new();
    let mut videos: HashSet<String> = HashSet::new();
    let mut media: HashSet<String> = HashSet::new();
    for member in members {
        let Some(stem) = live_photo_stem(member) else {
            continue;
        };
        let name = member.rsplit('/').next().unwrap_or(member);
        let Some((_, ext)) = name.rsplit_once('.') else {
            continue;
        };
        let directory = member.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        let key = format!("{directory}\n{}", stem.to_ascii_lowercase());
        media.insert(key.clone());
        if is_video_ext(ext) {
            videos.insert(key);
        } else {
            stills.insert(key);
        }
    }
    let pairs = stills.intersection(&videos).cloned().collect();
    NameIndex { pairs, media }
}

fn zip_member_names(path: &Path) -> Result<Vec<String>, IngestError> {
    let file = open_read_only(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| {
        IngestError::store("icloud", file_name_str(path).unwrap_or_default(), err)
    })?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|err| {
            IngestError::store("icloud", file_name_str(path).unwrap_or_default(), err)
        })?;
        if entry.is_dir() || !entry.is_file() {
            continue;
        }
        if let Ok(name) = entry.name() {
            names.push(name.into_owned());
        }
    }
    Ok(names)
}

fn tgz_member_names(path: &Path) -> Result<Vec<String>, IngestError> {
    let file = open_read_only(path)?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let mut names = Vec::new();
    let entries = archive
        .entries()
        .map_err(|err| IngestError::io(path, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| IngestError::io(path, err))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if let Ok(name) = std::str::from_utf8(&entry.path_bytes()) {
            if !name.contains('\\') {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

fn live_pair_present(
    root: &Path,
    member: &str,
    stem: &str,
    on_disk: bool,
    names: Option<&NameIndex>,
) -> bool {
    let directory = member.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let key = format!("{directory}\n{}", stem.to_ascii_lowercase());
    if let Some(names) = names {
        return names.pairs.contains(&key);
    }
    if !on_disk {
        return false;
    }
    sibling_has(root, member, stem, true)
}

fn sidecar_unmatched(
    member: &str,
    partner: Option<&str>,
    root: &Path,
    on_disk: bool,
    names: Option<&NameIndex>,
) -> bool {
    let lower = member.to_ascii_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".albumen.json") {
        return false;
    }
    let Some(partner) = partner else {
        return false;
    };
    let directory = member.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let key = format!("{directory}\n{}", partner.to_ascii_lowercase());
    if let Some(names) = names {
        return !names.media.contains(&key);
    }
    if !on_disk {
        return false;
    }
    !sibling_has(root, member, partner, false)
}

fn sibling_has(root: &Path, member: &str, stem: &str, opposite_kind: bool) -> bool {
    let source = root.join(member);
    let Some(parent) = source.parent() else {
        return false;
    };
    let source_video = source
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(is_video_ext);
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name
            == source
                .file_name()
                .and_then(|item| item.to_str())
                .unwrap_or("")
        {
            continue;
        }
        let Some((other, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if !other.eq_ignore_ascii_case(stem) {
            continue;
        }
        if opposite_kind {
            if source_video && is_still_ext(ext) || !source_video && is_video_ext(ext) {
                return true;
            }
        } else if is_still_ext(ext) || is_video_ext(ext) || is_raw_ext(ext) {
            return true;
        }
    }
    false
}

fn is_still_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "heic" | "heif" | "hif" | "png" | "tif" | "tiff" | "avif" | "webp"
    )
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "mov" | "mp4" | "m4v")
}

fn is_raw_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "dng" | "cr2" | "cr3" | "nef" | "arw" | "raf" | "orf" | "rw2" | "raw"
    )
}

fn live_photo_stem(member: &str) -> Option<&str> {
    let name = member.rsplit('/').next().unwrap_or(member);
    let (stem, ext) = name.rsplit_once('.')?;
    let still = matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "heic" | "heif" | "hif" | "png" | "tif" | "tiff" | "avif" | "webp"
    );
    let video = matches!(ext.to_ascii_lowercase().as_str(), "mov" | "mp4" | "m4v");
    if still || video {
        Some(stem)
    } else {
        None
    }
}

fn sidecar_partner_stem(name: &str) -> Option<String> {
    let file = name.rsplit('/').next().unwrap_or(name);
    let lower = file.to_ascii_lowercase();
    let stem = if lower.ends_with(".albumen.json") {
        &file[..file.len() - ".albumen.json".len()]
    } else if lower.ends_with(".xmp") || lower.ends_with(".aae") {
        &file[..file.len() - 4]
    } else {
        return None;
    };
    if let Some((base, ext)) = stem.rsplit_once('.') {
        if is_media_ext(ext) {
            return Some(base.to_string());
        }
    }
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn scan_capture_time(path: &Path) -> Result<Option<String>, IngestError> {
    const KEY: &[u8] = b"DateTimeOriginal";
    let mut file = File::open(path).map_err(|err| IngestError::io(path, err))?;
    let mut buf = [0u8; 8 * 1024];
    let mut prefix = [0u8; 16];
    let mut prefix_len = 0usize;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        if n == 0 {
            return Ok(None);
        }
        let mut search = Vec::with_capacity(prefix_len + n);
        search.extend_from_slice(&prefix[..prefix_len]);
        search.extend_from_slice(&buf[..n]);
        if let Some(index) = find_subslice(&search, KEY) {
            let start = index + KEY.len();
            return finish_timestamp(path, &search[start..], &mut file);
        }
        let keep = KEY.len() - 1;
        if search.len() > keep {
            prefix[..keep].copy_from_slice(&search[search.len() - keep..]);
            prefix_len = keep;
        } else {
            prefix[..search.len()].copy_from_slice(&search);
            prefix_len = search.len();
        }
    }
}

fn finish_timestamp(
    path: &Path,
    first: &[u8],
    file: &mut File,
) -> Result<Option<String>, IngestError> {
    let mut value = String::new();
    let mut started = false;
    if !take_timestamp(first, &mut value, &mut started) {
        return Ok(finished_timestamp(&value));
    }
    let mut buf = [0u8; 8 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        if n == 0 {
            return Ok(finished_timestamp(&value));
        }
        if !take_timestamp(&buf[..n], &mut value, &mut started) {
            return Ok(finished_timestamp(&value));
        }
    }
}

fn take_timestamp(bytes: &[u8], value: &mut String, started: &mut bool) -> bool {
    for &byte in bytes {
        if !*started {
            if byte.is_ascii_digit() {
                *started = true;
                value.push(byte as char);
            }
            continue;
        }
        if is_timestamp_byte(byte) {
            value.push(byte as char);
            continue;
        }
        return false;
    }
    true
}

fn finished_timestamp(value: &str) -> Option<String> {
    if value.len() >= 8 {
        Some(value.to_string())
    } else {
        None
    }
}

fn is_timestamp_byte(byte: u8) -> bool {
    byte.is_ascii_digit()
        || matches!(
            byte,
            b'-' | b':' | b'T' | b't' | b' ' | b'+' | b'Z' | b'z' | b'.'
        )
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn safe_relative(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return Err(format!("unsafe member path {name}"));
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
        if component.is_empty() {
            return Err(format!("empty path component in {name}"));
        }
        if component == "." {
            continue;
        }
        if component == ".." {
            return Err(format!("member path escapes the source: {name}"));
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

fn meta_len(path: &Path) -> Result<u64, IngestError> {
    Ok(fs::metadata(path)
        .map_err(|err| IngestError::io(path, err))?
        .len())
}

fn open_read_only(path: &Path) -> Result<File, IngestError> {
    let root = crate::source::SourceRoot::open(crate::source::parent_dir(path))?;
    root.open_read(path)
}

fn restore_icloud_sidecars(
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    if !dest_root.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    collect_dest_files(dest_root, "", &mut files)?;
    for (relative, path) in files {
        let lower = relative.to_ascii_lowercase();
        let file_name = lower.rsplit('/').next().unwrap_or(&lower);
        if file_name == "photo details.csv" {
            restore_photo_details_csv(dest_root, &path, report, counters)?;
            continue;
        }
        if !(lower.ends_with(".xmp") || lower.ends_with(".aae")) {
            continue;
        }
        let Some(capture) = scan_capture_time(&path)? else {
            continue;
        };
        let Some(media) = relative
            .strip_suffix(".xmp")
            .or_else(|| relative.strip_suffix(".XMP"))
            .or_else(|| relative.strip_suffix(".aae"))
            .or_else(|| relative.strip_suffix(".AAE"))
        else {
            continue;
        };
        let sidecar_rel = format!("{media}.albumen.json");
        if dest_root.join(&sidecar_rel).exists() {
            continue;
        }
        let bytes = metadata_json(media, false, None, &[], Some(&capture), None, None)?;
        let mtime = crate::mtime::mtime_of(&path)?;
        place_restored_sidecar(dest_root, &sidecar_rel, &bytes, mtime, report, counters)?;
    }
    Ok(())
}

fn restore_photo_details_csv(
    dest_root: &Path,
    csv_path: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let text = std::fs::read_to_string(csv_path).map_err(|err| IngestError::io(csv_path, err))?;
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Ok(());
    };
    let headers = split_csv(header);
    let index = |name: &str| {
        headers
            .iter()
            .position(|item| item.eq_ignore_ascii_case(name))
    };
    let name_at = index("imgName")
        .or_else(|| index("filename"))
        .or_else(|| index("photo"));
    let date_at = index("dateTaken")
        .or_else(|| index("date"))
        .or_else(|| index("photoTakenTime"));
    let lat_at = index("latitude");
    let lon_at = index("longitude");
    let Some(name_at) = name_at else {
        return Ok(());
    };
    if date_at.is_none() && (lat_at.is_none() || lon_at.is_none()) {
        return Ok(());
    }
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut files = Vec::new();
    collect_dest_files(dest_root, "", &mut files)?;
    for (relative, _) in &files {
        if let Some(file_name) = relative.rsplit('/').next() {
            by_name
                .entry(file_name.to_ascii_lowercase())
                .or_default()
                .push(relative.clone());
        }
    }
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv(line);
        let Some(file_name) = fields.get(name_at).map(|item| item.trim()) else {
            continue;
        };
        if file_name.is_empty() {
            continue;
        }
        let capture = date_at
            .and_then(|at| fields.get(at))
            .map(|item| item.trim().to_string());
        let latitude = lat_at
            .and_then(|at| fields.get(at))
            .and_then(|item| item.trim().parse::<f64>().ok());
        let longitude = lon_at
            .and_then(|at| fields.get(at))
            .and_then(|item| item.trim().parse::<f64>().ok());
        let gps = match (latitude, longitude) {
            (Some(lat), Some(lon)) if !(lat == 0.0 && lon == 0.0) => Some((lat, lon)),
            _ => None,
        };
        let capture = capture.filter(|text| !text.is_empty());
        if capture.is_none() && gps.is_none() {
            continue;
        }
        let matches = by_name
            .get(&file_name.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default();
        if matches.len() != 1 {
            counters.unmatched = counters.unmatched.saturating_add(1);
            report.line(&json!({
                "kind": "sidecar",
                "member": file_name,
                "destination": "",
                "sha256": "",
                "bytes": 0,
                "status": "mismatch",
                "unmatched": true,
                "detail": "Photo Details row did not match exactly one copied file",
            }))?;
            counters.mismatched = counters.mismatched.saturating_add(1);
            continue;
        }
        let media = &matches[0];
        let sidecar_rel = format!("{media}.albumen.json");
        if dest_root.join(&sidecar_rel).exists() {
            continue;
        }
        let bytes = metadata_json(
            media,
            false,
            None,
            &[],
            capture.as_deref(),
            gps.map(|(lat, _)| lat),
            gps.map(|(_, lon)| lon),
        )?;
        let mtime = crate::mtime::mtime_of(csv_path)?;
        place_restored_sidecar(dest_root, &sidecar_rel, &bytes, mtime, report, counters)?;
    }
    Ok(())
}

fn place_restored_sidecar(
    dest_root: &Path,
    relative: &str,
    bytes: &[u8],
    mtime: SystemTime,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    let dest = dest_root.join(relative);
    let mut reader = std::io::Cursor::new(bytes);
    match copy_verify_reader(&mut reader, dest_root, &dest, mtime) {
        Ok(object) => report.line(&json!({
            "kind": "albumen-sidecar",
            "member": relative,
            "destination": relative,
            "sha256": object.sha256,
            "bytes": object.byte_len,
            "status": "stored",
            "on_disk": false,
            "detail": "capture time and GPS restored into an Albumen sidecar; original bytes were not modified",
        })),
        Err(err) => {
            counters.mismatched = counters.mismatched.saturating_add(1);
            report.line(&json!({
                "kind": "albumen-sidecar",
                "member": relative,
                "destination": relative,
                "sha256": "",
                "bytes": 0,
                "status": "mismatch",
                "on_disk": false,
                "detail": err.to_string(),
            }))
        }
    }
}

fn collect_dest_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), IngestError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        names.push(entry.file_name());
    }
    names.sort();
    for name in names {
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = dir.join(name);
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            collect_dest_files(&path, &relative, out)?;
        } else if meta.is_file() {
            out.push((relative, path));
        }
    }
    Ok(())
}

fn split_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    fields.push(current);
    fields
}

fn refuse_layout(source_root: &Path, to: &Path, report: &Path) -> Result<(), IngestError> {
    let source = normalize_existing(source_root)?;
    let dest = normalize_path(to)?;
    let report_path = normalize_path(report)?;
    if dest.starts_with(&source) || source.starts_with(&dest) {
        return Err(IngestError::store(
            "icloud",
            dest.display().to_string(),
            format!(
                "refusing to import in place: destination {} overlaps source {}",
                dest.display(),
                source.display()
            ),
        ));
    }
    if report_path.starts_with(&source) || report_path.starts_with(&dest) || report.exists() {
        return Err(IngestError::store(
            "icloud",
            report.display().to_string(),
            "reconciliation report must be a new file outside the source and the library",
        ));
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
            Ok(meta) if meta.file_type().is_symlink() => return Err(IngestError::Symlink(current)),
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
                        "icloud",
                        path.display().to_string(),
                        "path does not exist",
                    ));
                };
                rest.push(name);
                if !current.pop() {
                    return Err(IngestError::store(
                        "icloud",
                        path.display().to_string(),
                        "path does not exist",
                    ));
                }
            }
            Err(err) => return Err(IngestError::io(&current, err)),
        }
    }
}

impl ReportFile {
    fn create(path: &Path) -> Result<Self, IngestError> {
        let file = dest::open_new(path)?;
        Ok(Self {
            file: BufWriter::new(file),
            path: path.to_path_buf(),
        })
    }

    fn line(&mut self, value: &serde_json::Value) -> Result<(), IngestError> {
        serde_json::to_writer(&mut self.file, value).map_err(|err| {
            IngestError::store("icloud-report", self.path.display().to_string(), err)
        })?;
        self.file
            .write_all(b"\n")
            .map_err(|err| IngestError::io(&self.path, err))
    }

    fn flush_to_disk(&mut self) -> Result<(), IngestError> {
        self.file
            .flush()
            .map_err(|err| IngestError::io(&self.path, err))?;
        self.file
            .get_ref()
            .sync_all()
            .map_err(|err| IngestError::io(&self.path, err))
    }

    fn finish(mut self) -> Result<(), IngestError> {
        self.flush_to_disk()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtime::{mtime_of, set_mtime};
    use crate::test_support::Tmp;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    use zip::ZipWriter;

    fn stamp() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_546_300_800)
    }

    fn write_stamped(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
        set_mtime(path, stamp()).unwrap();
    }

    fn xmp() -> &'static [u8] {
        br#"<x:xmpmeta><rdf:Description exif:DateTimeOriginal="2020-05-20T16:00:00"/></x:xmpmeta>"#
    }

    fn aae() -> &'static [u8] {
        br#"<?xml version="1.0"?><plist><dict><key>adjustmentFormatIdentifier</key><string>synthetic</string></dict></plist>"#
    }

    fn lock_readonly(root: &Path) {
        fn walk(dir: &Path) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path);
                } else {
                    let mut perms = fs::metadata(&path).unwrap().permissions();
                    perms.set_readonly(true);
                    fs::set_permissions(&path, perms).unwrap();
                }
            }
        }
        walk(root);
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut names = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            names.sort();
            for path in names {
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        walk(root, root, &mut out);
        out
    }

    #[test]
    fn library_and_export_keep_originals_edits_pairs_and_sidecars() {
        let tmp = Tmp::new();
        let from = tmp.path().join("Photos Library.photoslibrary");
        let heic = b"synthetic-albumen-icloud-heic-original";
        let mov = b"synthetic-albumen-icloud-live-video";
        let edited = b"synthetic-albumen-icloud-edited-render";
        let sqlite = b"synthetic-sqlite-not-a-database";
        write_stamped(&from.join("originals/0/IMG_0001.HEIC"), heic);
        write_stamped(&from.join("originals/0/IMG_0001.MOV"), mov);
        write_stamped(&from.join("originals/0/IMG_0001.AAE"), aae());
        write_stamped(&from.join("originals/0/IMG_0001.HEIC.xmp"), xmp());
        write_stamped(&from.join("resources/renders/0/IMG_0001.jpeg"), edited);
        write_stamped(&from.join("database/Photos.sqlite"), sqlite);
        let before = snapshot(&from);
        lock_readonly(&from);

        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let outcome = import_icloud(&from, &to, &report).unwrap();
        assert_eq!(outcome.count_in, 6);
        assert_eq!(outcome.count_out, 6);
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.HEIC")).unwrap(),
            heic
        );
        assert_eq!(fs::read(to.join("originals/0/IMG_0001.MOV")).unwrap(), mov);
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.AAE")).unwrap(),
            aae()
        );
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.HEIC.xmp")).unwrap(),
            xmp()
        );
        assert_eq!(
            fs::read(to.join("resources/renders/0/IMG_0001.jpeg")).unwrap(),
            edited
        );
        assert_eq!(fs::read(to.join("database/Photos.sqlite")).unwrap(), sqlite);
        assert!(!heic.windows(10).any(|window| window == b"2020-05-20"));
        assert_eq!(
            mtime_of(&to.join("originals/0/IMG_0001.HEIC")).unwrap(),
            stamp()
        );

        let lines = fs::read_to_string(&report)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let summary = lines.last().unwrap();
        assert_eq!(summary["count_in"], 6);
        assert_eq!(summary["count_out"], 6);
        assert_eq!(summary["mismatched"], 0);
        assert_eq!(summary["sources_unchanged"], true);
        let heic_line = lines
            .iter()
            .find(|line| line["member"] == "originals/0/IMG_0001.HEIC")
            .unwrap();
        assert_eq!(heic_line["kind"], "original");
        assert_eq!(heic_line["live_photo_stem"], "IMG_0001");
        assert!(heic_line.get("capture_time").is_none());
        let mov_line = lines
            .iter()
            .find(|line| line["member"] == "originals/0/IMG_0001.MOV")
            .unwrap();
        assert_eq!(mov_line["live_photo_stem"], "IMG_0001");
        let xmp_line = lines
            .iter()
            .find(|line| line["member"] == "originals/0/IMG_0001.HEIC.xmp")
            .unwrap();
        assert_eq!(xmp_line["kind"], "sidecar");
        assert_eq!(xmp_line["partner_stem"], "IMG_0001");
        assert_eq!(xmp_line["capture_time"], "2020-05-20T16:00:00");
        let aae_line = lines
            .iter()
            .find(|line| line["member"] == "originals/0/IMG_0001.AAE")
            .unwrap();
        assert_eq!(aae_line["kind"], "sidecar");
        assert_eq!(aae_line["partner_stem"], "IMG_0001");
        assert!(aae_line["capture_time"].is_null());
        let edit_line = lines
            .iter()
            .find(|line| line["member"] == "resources/renders/0/IMG_0001.jpeg")
            .unwrap();
        assert_eq!(edit_line["kind"], "edited");
        assert!(edit_line.get("capture_time").is_none());
        for line in lines.iter().filter(|line| line.get("summary").is_none()) {
            let dest = to.join(line["destination"].as_str().unwrap());
            assert_eq!(line["sha256"], crate::sha256_hex(&dest).unwrap());
        }
        assert_eq!(snapshot(&from), before);

        let again = tmp.path().join("report-again.jsonl");
        let second = import_icloud(&from, &to, &again).unwrap();
        assert_eq!(second.count_in, second.count_out);
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.HEIC")).unwrap(),
            heic
        );
        assert_eq!(
            mtime_of(&to.join("originals/0/IMG_0001.HEIC")).unwrap(),
            stamp()
        );
    }

    #[test]
    fn zip_download_is_streamed_and_left_unchanged() {
        let tmp = Tmp::new();
        let incoming = tmp.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let zip_path = incoming.join("iCloud Photos.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
        let heic = b"synthetic-zip-heic";
        let xmp = xmp();
        zip.start_file("iCloud Photos/IMG_0002.HEIC", options)
            .unwrap();
        zip.write_all(heic).unwrap();
        zip.start_file("iCloud Photos/IMG_0002.HEIC.xmp", options)
            .unwrap();
        zip.write_all(xmp).unwrap();
        zip.finish().unwrap();
        let before = fs::read(&zip_path).unwrap();
        let mut perms = fs::metadata(&zip_path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&zip_path, perms).unwrap();

        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let outcome = import_icloud(&zip_path, &to, &report).unwrap();
        assert_eq!(outcome.count_in, 2);
        assert_eq!(outcome.count_out, 2);
        assert_eq!(
            fs::read(to.join("iCloud Photos/IMG_0002.HEIC")).unwrap(),
            heic
        );
        assert_eq!(
            fs::read(to.join("iCloud Photos/IMG_0002.HEIC.xmp")).unwrap(),
            xmp
        );
        assert!(!heic.windows(4).any(|window| window == b"2020"));
        assert_eq!(fs::read(&zip_path).unwrap(), before);
        assert!(!incoming.join("iCloud Photos").exists());
    }

    #[test]
    fn different_bytes_are_not_overwritten() {
        let tmp = Tmp::new();
        let from = tmp.path().join("export");
        write_stamped(&from.join("IMG_0001.HEIC"), b"synthetic-original");
        let to = tmp.path().join("library");
        let dest = to.join("IMG_0001.HEIC");
        fs::create_dir_all(&to).unwrap();
        fs::write(&dest, b"different").unwrap();
        let err = import_icloud(&from, &to, &tmp.path().join("report.jsonl")).unwrap_err();
        assert!(
            matches!(err, IngestError::IcloudReconciliation { .. }),
            "{err}"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"different");
    }

    #[test]
    fn symlink_is_not_followed() {
        let tmp = Tmp::new();
        let from = tmp.path().join("export");
        fs::create_dir(&from).unwrap();
        let outside = tmp.path().join("outside.HEIC");
        fs::write(&outside, b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, from.join("link.HEIC")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, from.join("link.HEIC")).unwrap();
        write_stamped(&from.join("IMG_0001.HEIC"), b"kept");
        let to = tmp.path().join("library");
        let err = import_icloud(&from, &to, &tmp.path().join("report.jsonl")).unwrap_err();
        assert!(
            matches!(err, IngestError::IcloudReconciliation { .. }),
            "{err}"
        );
        assert!(!to.join("link.HEIC").exists());
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert_eq!(fs::read(to.join("IMG_0001.HEIC")).unwrap(), b"kept");
    }

    #[test]
    fn in_place_destination_is_refused() {
        let tmp = Tmp::new();
        let from = tmp.path().join("export");
        write_stamped(&from.join("IMG_0001.HEIC"), b"kept");
        let err =
            import_icloud(&from, &from.join("out"), &tmp.path().join("report.jsonl")).unwrap_err();
        assert!(matches!(err, IngestError::Store { .. }), "{err}");
        assert!(!from.join("out").exists());
    }

    #[test]
    fn multipart_download_keeps_photo_details_byte_exact() {
        let tmp = Tmp::new();
        let incoming = tmp.path().join("iCloud Photos");
        fs::create_dir(&incoming).unwrap();
        let first = b"synthetic-part-one-heic";
        let second = b"synthetic-part-two-heic";
        let photo_csv = b"imgName,favorite,caption\nIMG_0001.HEIC,yes,synthetic caption\n";
        let album_csv = b"album,photo\nSynthetic Album,IMG_0002.HEIC\n";
        write_zip_part(
            &incoming.join("iCloud Photos.zip"),
            &[("iCloud Photos/IMG_0001.HEIC", first)],
        );
        write_zip_part(
            &incoming.join("iCloud Photos-1.zip"),
            &[("iCloud Photos/IMG_0002.HEIC", second)],
        );
        fs::write(incoming.join("Photo Details.csv"), photo_csv).unwrap();
        fs::write(incoming.join("Album Details.csv"), album_csv).unwrap();
        let zip_before = fs::read(incoming.join("iCloud Photos.zip")).unwrap();
        let zip2_before = fs::read(incoming.join("iCloud Photos-1.zip")).unwrap();

        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let outcome = import_icloud(&incoming, &to, &report).unwrap();
        assert_eq!(outcome.count_in, 4);
        assert_eq!(outcome.count_out, 4);
        assert_eq!(
            fs::read(to.join("iCloud Photos/IMG_0001.HEIC")).unwrap(),
            first
        );
        assert_eq!(
            fs::read(to.join("iCloud Photos/IMG_0002.HEIC")).unwrap(),
            second
        );
        assert_eq!(fs::read(to.join("Photo Details.csv")).unwrap(), photo_csv);
        assert_eq!(fs::read(to.join("Album Details.csv")).unwrap(), album_csv);
        assert_eq!(
            fs::read(incoming.join("iCloud Photos.zip")).unwrap(),
            zip_before
        );
        assert_eq!(
            fs::read(incoming.join("iCloud Photos-1.zip")).unwrap(),
            zip2_before
        );
        assert!(!incoming.join("iCloud Photos").exists());
        let report_text = fs::read_to_string(&report).unwrap();
        let csv_line = report_text
            .lines()
            .find(|line| line.contains("Photo Details.csv"))
            .unwrap();
        assert!(csv_line.contains("\"kind\":\"sidecar\""), "{csv_line}");
        assert!(report_text.contains("\"count_in\":4"));
        assert!(report_text.contains("\"sources_unchanged\":true"));
    }

    #[test]
    fn photos_library_metadata_is_a_read_only_sidecar() {
        let tmp = Tmp::new();
        let from = tmp.path().join("Photos Library.photoslibrary");
        let heic = b"synthetic-library-heic-original";
        let plain = b"synthetic-library-heic-unmarked";
        write_stamped(&from.join("originals/0/IMG_0001.HEIC"), heic);
        write_stamped(&from.join("originals/0/IMG_0002.HEIC"), plain);
        let db_path = from.join("database/Photos.sqlite");
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "
            CREATE TABLE ZASSET (
                Z_PK INTEGER PRIMARY KEY,
                ZFILENAME TEXT,
                ZDIRECTORY TEXT,
                ZFAVORITE INTEGER
            );
            CREATE TABLE ZADDITIONALASSETATTRIBUTES (
                Z_PK INTEGER PRIMARY KEY,
                ZASSET INTEGER,
                ZTITLE TEXT
            );
            CREATE TABLE ZGENERICALBUM (
                Z_PK INTEGER PRIMARY KEY,
                ZTITLE TEXT
            );
            CREATE TABLE Z_1ASSETS (
                Z_1ALBUMS INTEGER,
                Z_2ASSETS INTEGER
            );
            INSERT INTO ZASSET (Z_PK, ZFILENAME, ZDIRECTORY, ZFAVORITE) VALUES
                (1, 'IMG_0001.HEIC', '0', 1),
                (2, 'IMG_0002.HEIC', '0', 0);
            INSERT INTO ZADDITIONALASSETATTRIBUTES (Z_PK, ZASSET, ZTITLE) VALUES
                (1, 1, 'synthetic caption');
            INSERT INTO ZGENERICALBUM (Z_PK, ZTITLE) VALUES
                (7, 'Zebra'),
                (3, 'Album A');
            INSERT INTO Z_1ASSETS (Z_1ALBUMS, Z_2ASSETS) VALUES
                (7, 1),
                (3, 1);
            ",
        )
        .unwrap();
        drop(db);
        let before = snapshot(&from);
        lock_readonly(&from);

        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let outcome = import_icloud(&from, &to, &report).unwrap();
        assert_eq!(
            outcome.count_in, 4,
            "two originals, the database, and one metadata sidecar"
        );
        assert_eq!(outcome.count_out, 4);
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.HEIC")).unwrap(),
            heic
        );
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0002.HEIC")).unwrap(),
            plain
        );
        assert!(!heic.windows(8).any(|window| window == b"favorite"));
        assert!(!heic.windows(8).any(|window| window == b"caption"));
        let sidecar: serde_json::Value = serde_json::from_slice(
            &fs::read(to.join("originals/0/IMG_0001.HEIC.albumen.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar["favorite"], true);
        assert_eq!(sidecar["caption"], "synthetic caption");
        assert_eq!(sidecar["albums"], serde_json::json!(["Album A", "Zebra"]));
        assert_eq!(sidecar["partner"], "originals/0/IMG_0001.HEIC");
        assert!(!to.join("originals/0/IMG_0002.HEIC.albumen.json").exists());
        assert_eq!(snapshot(&from), before);
        assert!(!from.join("database/Photos.sqlite-wal").exists());
        assert!(!from.join("database/Photos.sqlite-shm").exists());
        let lines = fs::read_to_string(&report).unwrap();
        let meta = lines
            .lines()
            .find(|line| line.contains("IMG_0001.HEIC.albumen.json"))
            .unwrap();
        assert!(meta.contains("\"kind\":\"sidecar\""), "{meta}");
        assert!(meta.contains("\"partner_stem\":\"IMG_0001\""), "{meta}");
        assert!(meta.contains("\"on_disk\":false"), "{meta}");
        assert!(lines.contains("\"count_in\":4"));

        let again = tmp.path().join("report-again.jsonl");
        let second = import_icloud(&from, &to, &again).unwrap();
        assert_eq!(second.count_in, second.count_out);
        assert_eq!(snapshot(&from), before);
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0001.HEIC.albumen.json")).unwrap(),
            fs::read(to.join("originals/0/IMG_0001.HEIC.albumen.json")).unwrap()
        );
    }

    #[test]
    fn photos_library_capture_time_and_gps_land_in_the_sidecar() {
        let tmp = Tmp::new();
        let from = tmp.path().join("Photos Library.photoslibrary");
        let heic = b"synthetic-gps-heic-original";
        write_stamped(&from.join("originals/0/IMG_0009.HEIC"), heic);
        write_stamped(
            &from.join("originals/0/IMG_0009.MOV"),
            b"synthetic-live-mov",
        );
        let db_path = from.join("database/Photos.sqlite");
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "
            CREATE TABLE ZASSET (
                Z_PK INTEGER PRIMARY KEY,
                ZFILENAME TEXT,
                ZDIRECTORY TEXT,
                ZFAVORITE INTEGER,
                ZDATECREATED REAL,
                ZLATITUDE REAL,
                ZLONGITUDE REAL
            );
            INSERT INTO ZASSET (Z_PK, ZFILENAME, ZDIRECTORY, ZFAVORITE, ZDATECREATED, ZLATITUDE, ZLONGITUDE)
            VALUES (1, 'IMG_0009.HEIC', '0', 0, 610000000, 37.5, -122.25);
            ",
        )
        .unwrap();
        drop(db);
        let before = fs::read(from.join("originals/0/IMG_0009.HEIC")).unwrap();
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let plan = plan_icloud(&from, &to, &report).unwrap();
        assert!(!to.exists());
        assert!(plan.live_photos.iter().any(|stem| stem == "IMG_0009"));
        let outcome = import_icloud(&from, &to, &report).unwrap();
        assert_eq!(outcome.count_in, outcome.count_out);
        assert_eq!(
            fs::read(from.join("originals/0/IMG_0009.HEIC")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(to.join("originals/0/IMG_0009.HEIC")).unwrap(),
            heic
        );
        let sidecar: serde_json::Value = serde_json::from_slice(
            &fs::read(to.join("originals/0/IMG_0009.HEIC.albumen.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar["latitude"], 37.5);
        assert_eq!(sidecar["longitude"], -122.25);
        assert!(sidecar["captureTime"].as_str().unwrap().ends_with('Z'));
        let report_text = fs::read_to_string(&report).unwrap();
        assert!(report_text.contains("\"live_photo_stem\":\"IMG_0009\""));
        assert!(!heic.windows(4).any(|window| window == b"37.5"));
    }

    fn write_zip_part(path: &Path, files: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
        for (name, bytes) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
}
