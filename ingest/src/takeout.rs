// SPDX-License-Identifier: Apache-2.0

//! Google Takeout import.
//!
//! Split `.zip`, `.tgz`, and `.tar.gz` parts, or an already extracted folder,
//! are opened read-only. Nothing is written beside the source, and archive
//! bytes are hashed before and after. Each file is copied with the same write,
//! fsync, and re-read SHA-256 check as [`crate::copy_verify`]. Takeout JSON is
//! stored byte-for-byte next to its original. Capture time and GPS are written
//! into a new `.albumen.json` sidecar and are never written into the original.
//! An optional derived XMP file is a new file. It is off unless the caller
//! asks for it, and writing it does not open the original.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_json::json;
use tar::Archive;
use zip::ZipArchive;

use crate::checksum::{sha256_file, sha256_reader, to_hex};
use crate::dest;
use crate::discover::{check_filename, file_name_str, takeout_json_sidecar_names};
use crate::error::IngestError;
use crate::local::{copy_verify_file, copy_verify_reader};
use crate::source::{parent_dir, SourceRoot};

/// Counts from a finished Takeout import whose reconciliation matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoutReport {
    pub count_in: u64,
    pub count_out: u64,
    pub mismatched: u64,
    /// Media with no Takeout JSON, and Takeout JSON with no media partner.
    /// These files are still copied. A non-zero count does not fail the import.
    pub unmatched: u64,
    pub report: PathBuf,
}

/// Names a dry-run would copy. Nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoutPlan {
    pub files: u64,
    pub unmatched: Vec<String>,
    pub motion_photos: Vec<String>,
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

struct ArchiveSeal {
    path: PathBuf,
    name: String,
    sha256: String,
    bytes: u64,
}

/// Copy every file of a Takeout download into `to`.
///
/// `from` is one `.zip`, `.tgz`, or `.tar.gz` archive, a directory whose
/// entries are only those parts, or an extracted Takeout folder. The
/// destination must not be that directory or a directory that contains it.
/// `report` is a JSONL file outside both.
///
/// The function returns [`IngestError::TakeoutReconciliation`] when any member
/// is missing, extra, or hashed differently, when an archive's bytes change,
/// or when the source contains no file members. The report is still written.
/// Files with no metadata partner are listed and still copied.
///
/// Derived XMP is off. [`import_takeout_with`] turns it on.
pub fn import_takeout(from: &Path, to: &Path, report: &Path) -> Result<TakeoutReport, IngestError> {
    import_takeout_with(from, to, report, false)
}

/// List what [`import_takeout`] would copy. Creates nothing.
///
/// A destination or report that overlaps the source is still an error.
pub fn plan_takeout(from: &Path, to: &Path, report: &Path) -> Result<TakeoutPlan, IngestError> {
    let root = source_root(from)?;
    refuse_layout(&root, to, report)?;
    let names = layout_names(from)?;
    Ok(plan_from_names(&names))
}

/// Copy every file member of a Takeout download into `to`.
///
/// `derive_xmp` writes a new XMP sidecar rendered from each Takeout JSON file.
/// The flag is off unless the caller sets it. The original is not opened for
/// write, and its bytes and mtime stay as the archive member stored them.
/// Derived files are extra report lines. They are not archive members, so
/// they are not added to `count_in` or `count_out`.
pub fn import_takeout_with(
    from: &Path,
    to: &Path,
    report: &Path,
    derive_xmp: bool,
) -> Result<TakeoutReport, IngestError> {
    match classify(from)? {
        TakeoutLayout::Archives(archives) => import_archives(&archives, to, report, derive_xmp),
        TakeoutLayout::Extracted => import_extracted(from, to, report, derive_xmp),
    }
}

fn import_archives(
    archives: &[PathBuf],
    to: &Path,
    report: &Path,
    derive_xmp: bool,
) -> Result<TakeoutReport, IngestError> {
    let source_root = source_root(archives.first().map(PathBuf::as_path).unwrap_or(to))?;
    refuse_layout(&source_root, to, report)?;
    dest::create_dir_all(to)?;
    if let Some(parent) = report.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }

    let seals = seal_archives(archives)?;
    let names = archive_names(archives)?;
    let partners = build_partner_index(&names);
    let motion = motion_pairs(&names);
    let mut pending_xmp = Vec::new();
    let mut pending_meta = Vec::new();
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
    {
        let mut ctx = ImportCtx {
            dest_root: to,
            report: &mut report_file,
            counters: &mut counters,
            partners: &partners,
            motion: &motion,
            pending_xmp: &mut pending_xmp,
            pending_meta: &mut pending_meta,
            derive_xmp,
        };
        for archive in archives {
            if let Err(err) = import_archive(archive, &mut ctx) {
                fatal = Some(err);
                break;
            }
        }
    }
    if fatal.is_none() {
        if let Err(err) = write_albumen_sidecars(&pending_meta, to, &mut report_file, &mut counters)
        {
            fatal = Some(err);
        }
    }
    if fatal.is_none() && derive_xmp {
        if let Err(err) = write_pending_xmp(&pending_xmp, to, &mut report_file, &mut counters) {
            fatal = Some(err);
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
        "unmatched": counters.unmatched,
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
        unmatched: counters.unmatched,
        report: report.to_path_buf(),
    })
}

fn import_extracted(
    from: &Path,
    to: &Path,
    report: &Path,
    derive_xmp: bool,
) -> Result<TakeoutReport, IngestError> {
    let source = SourceRoot::open(from)?;
    refuse_layout(from, to, report)?;
    source.refuse_destination(to)?;
    source.refuse_destination(report)?;
    dest::create_dir_all(to)?;
    if let Some(parent) = report.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }
    let names = extracted_names(from)?;
    let partners = build_partner_index(&names);
    let motion = motion_pairs(&names);
    let archive_name = file_name_str(from).unwrap_or_else(|_| from.display().to_string());
    let mut report_file = ReportFile::create(report)?;
    report_file.line(&json!({
        "kind": "archive",
        "archive": archive_name,
        "status": "extracted-folder",
    }))?;
    let mut counters = Counters::default();
    let mut pending_xmp = Vec::new();
    let mut pending_meta = Vec::new();
    let mut sealed = Vec::new();
    let mut fatal = None;
    if let Err(err) = copy_extracted(
        from,
        &archive_name,
        to,
        &partners,
        &motion,
        &mut report_file,
        &mut counters,
        &mut pending_xmp,
        &mut pending_meta,
        &mut sealed,
        derive_xmp,
    ) {
        fatal = Some(err);
    }
    if fatal.is_none() {
        if let Err(err) = write_albumen_sidecars(&pending_meta, to, &mut report_file, &mut counters)
        {
            fatal = Some(err);
        }
    }
    if fatal.is_none() && derive_xmp {
        if let Err(err) = write_pending_xmp(&pending_xmp, to, &mut report_file, &mut counters) {
            fatal = Some(err);
        }
    }
    let mut unchanged = true;
    for (member, sha) in &sealed {
        let source_path = from.join(member);
        let again = match sha256_file(&source_path) {
            Ok(digest) => to_hex(&digest),
            Err(err) => {
                unchanged = false;
                counters.mismatched = counters.mismatched.saturating_add(1);
                let _ = report_file.line(&json!({
                    "kind": "original",
                    "member": member,
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": format!("source changed or could not be re-read: {err}"),
                }));
                continue;
            }
        };
        if again != *sha {
            unchanged = false;
            counters.mismatched = counters.mismatched.saturating_add(1);
            let _ = report_file.line(&json!({
                "kind": "original",
                "member": member,
                "sha256": again,
                "bytes": 0,
                "status": "mismatch",
                "detail": format!("source bytes changed; before {sha}"),
            }));
        }
    }
    if counters.count_in == 0 && fatal.is_none() {
        counters.mismatched = counters.mismatched.saturating_add(1);
        let _ = report_file.line(&json!({
            "kind": "archive",
            "status": "mismatch",
            "detail": "extracted folder contained no files",
        }));
    }
    report_file.line(&json!({
        "summary": true,
        "count_in": counters.count_in,
        "count_out": counters.count_out,
        "mismatched": counters.mismatched,
        "unmatched": counters.unmatched,
        "archives_unchanged": unchanged,
    }))?;
    report_file.finish()?;
    if let Some(err) = fatal {
        return Err(err);
    }
    if counters.count_in != counters.count_out || counters.mismatched != 0 || !unchanged {
        return Err(IngestError::TakeoutReconciliation {
            count_in: counters.count_in,
            count_out: counters.count_out,
            mismatched: counters.mismatched,
            archives_unchanged: unchanged,
            report: report.display().to_string(),
        });
    }
    Ok(TakeoutReport {
        count_in: counters.count_in,
        count_out: counters.count_out,
        mismatched: counters.mismatched,
        unmatched: counters.unmatched,
        report: report.to_path_buf(),
    })
}

#[allow(clippy::too_many_arguments)]
fn copy_extracted(
    root: &Path,
    archive_name: &str,
    dest_root: &Path,
    partners: &PartnerIndex,
    motion: &MotionIndex,
    report: &mut ReportFile,
    counters: &mut Counters,
    pending_xmp: &mut Vec<PendingXmp>,
    pending_meta: &mut Vec<PendingMeta>,
    sealed: &mut Vec<(String, String)>,
    derive_xmp: bool,
) -> Result<(), IngestError> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        dir: &Path,
        prefix: &str,
        root: &Path,
        archive_name: &str,
        dest_root: &Path,
        partners: &PartnerIndex,
        motion: &MotionIndex,
        report: &mut ReportFile,
        counters: &mut Counters,
        pending_xmp: &mut Vec<PendingXmp>,
        pending_meta: &mut Vec<PendingMeta>,
        sealed: &mut Vec<(String, String)>,
        derive_xmp: bool,
    ) -> Result<(), IngestError> {
        let source = SourceRoot::open(dir)?;
        let mut names = Vec::new();
        for entry in source.read_dir(dir)? {
            let entry = entry.map_err(|err| IngestError::io(dir, err))?;
            names.push(entry.file_name());
        }
        names.sort();
        for name in names {
            let Some(name) = name.to_str() else {
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
                    "unmatched": true,
                    "detail": "filename is not UTF-8",
                }))?;
                counters.unmatched = counters.unmatched.saturating_add(1);
                continue;
            };
            let path = dir.join(name);
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
                    "kind": "original",
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
                    partners,
                    motion,
                    report,
                    counters,
                    pending_xmp,
                    pending_meta,
                    sealed,
                    derive_xmp,
                )?;
                continue;
            }
            if !meta.is_file() {
                counters.count_in = counters.count_in.saturating_add(1);
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "original",
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
            store_extracted_file(
                root,
                archive_name,
                &path,
                &relative,
                dest_root,
                partners,
                motion,
                report,
                counters,
                pending_xmp,
                pending_meta,
                sealed,
                derive_xmp,
            )?;
        }
        Ok(())
    }
    walk(
        root,
        "",
        root,
        archive_name,
        dest_root,
        partners,
        motion,
        report,
        counters,
        pending_xmp,
        pending_meta,
        sealed,
        derive_xmp,
    )
}

#[allow(clippy::too_many_arguments)]
fn store_extracted_file(
    root: &Path,
    archive_name: &str,
    source: &Path,
    member: &str,
    dest_root: &Path,
    partners: &PartnerIndex,
    motion: &MotionIndex,
    report: &mut ReportFile,
    counters: &mut Counters,
    pending_xmp: &mut Vec<PendingXmp>,
    pending_meta: &mut Vec<PendingMeta>,
    sealed: &mut Vec<(String, String)>,
    derive_xmp: bool,
) -> Result<(), IngestError> {
    counters.count_in = counters.count_in.saturating_add(1);
    let relative = match member_relative(member) {
        Ok(path) => path,
        Err(detail) => {
            counters.mismatched = counters.mismatched.saturating_add(1);
            return report.line(&json!({
                "kind": "original",
                "archive": archive_name,
                "member": member,
                "destination": "",
                "sha256": "",
                "bytes": 0,
                "status": "mismatch",
                "detail": detail,
            }));
        }
    };
    let destination = rel_string(&relative);
    let dest = dest_root.join(&relative);
    let kind = kind_of(&relative);
    let file_name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let directory = relative.parent().map(rel_string).unwrap_or_default();
    let partner = if kind == "sidecar" {
        partners.lookup(&directory, file_name).map(str::to_string)
    } else {
        None
    };
    let stored = match fs::symlink_metadata(&dest) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => {
            counters.mismatched = counters.mismatched.saturating_add(1);
            let mut line = json!({
                "archive": archive_name,
                "member": member,
                "destination": destination,
                "sha256": "",
                "bytes": 0,
                "kind": kind,
                "status": "mismatch",
                "detail": "destination is not a regular file; it was not overwritten",
            });
            mark_takeout_fields(
                &mut line,
                member,
                kind,
                if kind == "sidecar" {
                    Some(partner.as_deref())
                } else {
                    None
                },
                partners,
                motion,
                counters,
            );
            return report.line(&line);
        }
        Ok(_) => {
            let existing = sha256_file(&dest)?;
            let incoming = sha256_file(source)?;
            if existing == incoming {
                let bytes = fs::metadata(&dest)
                    .map_err(|err| IngestError::io(&dest, err))?
                    .len();
                (to_hex(&existing), bytes, "already-verified")
            } else {
                counters.mismatched = counters.mismatched.saturating_add(1);
                let mut line = json!({
                    "archive": archive_name,
                    "member": member,
                    "destination": destination,
                    "sha256": to_hex(&incoming),
                    "bytes": 0,
                    "kind": kind,
                    "status": "mismatch",
                    "detail": "destination already has different bytes; it was not overwritten",
                });
                mark_takeout_fields(
                    &mut line,
                    member,
                    kind,
                    if kind == "sidecar" {
                        Some(partner.as_deref())
                    } else {
                        None
                    },
                    partners,
                    motion,
                    counters,
                );
                return report.line(&line);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent = dest.parent().unwrap_or(dest_root);
            match copy_verify_file(source, parent) {
                Ok(object) => (object.sha256, object.byte_len, "stored"),
                Err(IngestError::ChecksumMismatch { message, .. }) => {
                    counters.mismatched = counters.mismatched.saturating_add(1);
                    let mut line = json!({
                        "archive": archive_name,
                        "member": member,
                        "destination": destination,
                        "sha256": "",
                        "bytes": 0,
                        "kind": kind,
                        "status": "mismatch",
                        "detail": message,
                    });
                    mark_takeout_fields(
                        &mut line,
                        member,
                        kind,
                        if kind == "sidecar" {
                            Some(partner.as_deref())
                        } else {
                            None
                        },
                        partners,
                        motion,
                        counters,
                    );
                    return report.line(&line);
                }
                Err(err) => return Err(err),
            }
        }
        Err(err) => return Err(IngestError::io(&dest, err)),
    };
    counters.count_out = counters.count_out.saturating_add(1);
    sealed.push((member.to_string(), stored.0.clone()));
    let taken = if kind == "sidecar" {
        scan_photo_taken_time(&dest)?
    } else {
        None
    };
    if kind == "sidecar" {
        if let Some(partner_name) = partner.as_deref() {
            let mut media = PathBuf::from(&destination);
            media.pop();
            media.push(partner_name);
            pending_meta.push(PendingMeta {
                archive_name: archive_name.to_string(),
                member: member.to_string(),
                json_path: dest.clone(),
                media_relative: rel_string(&media),
                partner: partner_name.to_string(),
            });
            if derive_xmp {
                let mut xmp = PathBuf::from(&destination);
                xmp.set_file_name(format!("{partner_name}.xmp"));
                pending_xmp.push(PendingXmp {
                    archive_name: archive_name.to_string(),
                    member: member.to_string(),
                    json_path: dest.clone(),
                    xmp_relative: rel_string(&xmp),
                    partner: Some(partner_name.to_string()),
                });
            }
        } else if derive_xmp {
            let mut xmp = PathBuf::from(&destination);
            let xmp_name = format!(
                "{}.xmp",
                xmp.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sidecar.json")
            );
            xmp.set_file_name(xmp_name);
            pending_xmp.push(PendingXmp {
                archive_name: archive_name.to_string(),
                member: member.to_string(),
                json_path: dest,
                xmp_relative: rel_string(&xmp),
                partner: None,
            });
        }
    }
    let mut line = json!({
        "archive": archive_name,
        "member": member,
        "destination": destination,
        "sha256": stored.0,
        "bytes": stored.1,
        "kind": kind,
        "status": stored.2,
    });
    if kind == "sidecar" {
        line["partner"] = json!(partner);
        line["photo_taken_time"] = json!(taken);
    }
    mark_takeout_fields(
        &mut line,
        member,
        kind,
        if kind == "sidecar" {
            Some(partner.as_deref())
        } else {
            None
        },
        partners,
        motion,
        counters,
    );
    let _ = root;
    report.line(&line)
}

struct ImportCtx<'a> {
    dest_root: &'a Path,
    report: &'a mut ReportFile,
    counters: &'a mut Counters,
    partners: &'a PartnerIndex,
    motion: &'a MotionIndex,
    pending_xmp: &'a mut Vec<PendingXmp>,
    pending_meta: &'a mut Vec<PendingMeta>,
    derive_xmp: bool,
}

fn import_archive(path: &Path, ctx: &mut ImportCtx<'_>) -> Result<(), IngestError> {
    match archive_kind(path) {
        Some(ArchiveKind::Zip) => import_zip(path, ctx),
        Some(ArchiveKind::Tgz) => import_tgz(path, ctx),
        None => Err(IngestError::store(
            "takeout",
            display_name(path),
            "not a .zip, .tgz, or .tar.gz archive",
        )),
    }
}

fn import_zip(path: &Path, ctx: &mut ImportCtx<'_>) -> Result<(), IngestError> {
    let file = open_read_only(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| zip_err(path, err))?;
    let archive_name = file_name_str(path)?;
    let len = archive.len();
    for index in 0..len {
        let entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(err) => {
                let remaining = u64::try_from(len - index).unwrap_or(u64::MAX);
                ctx.counters.count_in = ctx.counters.count_in.saturating_add(remaining);
                ctx.counters.mismatched = ctx.counters.mismatched.saturating_add(remaining);
                ctx.report.line(&json!({
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
                ctx.counters.count_in = ctx.counters.count_in.saturating_add(1);
                ctx.counters.mismatched = ctx.counters.mismatched.saturating_add(1);
                ctx.report.line(&json!({
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
        let mut part = Part::from_ctx(path, &archive_name, ctx);
        if !entry.is_file() {
            part.reject(entry, &member_name, "archive member is not a regular file")?;
            continue;
        }
        let mtime = entry.last_modified().as_ref().and_then(dos_to_system_time);
        part.store_member(entry, &member_name, mtime)?;
    }
    Ok(())
}

fn import_tgz(path: &Path, ctx: &mut ImportCtx<'_>) -> Result<(), IngestError> {
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
                ctx.counters.count_in = ctx.counters.count_in.saturating_add(1);
                ctx.counters.mismatched = ctx.counters.mismatched.saturating_add(1);
                ctx.report.line(&json!({
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
                ctx.counters.count_in = ctx.counters.count_in.saturating_add(1);
                ctx.counters.mismatched = ctx.counters.mismatched.saturating_add(1);
                ctx.report.line(&json!({
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
        let mut part = Part::from_ctx(path, &archive_name, ctx);
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

struct PendingXmp {
    archive_name: String,
    member: String,
    json_path: PathBuf,
    xmp_relative: String,
    partner: Option<String>,
}

struct PendingMeta {
    archive_name: String,
    member: String,
    json_path: PathBuf,
    media_relative: String,
    partner: String,
}

struct Part<'a> {
    archive_path: &'a Path,
    archive_name: &'a str,
    dest_root: &'a Path,
    report: &'a mut ReportFile,
    counters: &'a mut Counters,
    partners: &'a PartnerIndex,
    motion: &'a MotionIndex,
    pending_xmp: &'a mut Vec<PendingXmp>,
    pending_meta: &'a mut Vec<PendingMeta>,
    derive_xmp: bool,
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

impl<'a> Part<'a> {
    fn from_ctx(archive_path: &'a Path, archive_name: &'a str, ctx: &'a mut ImportCtx<'_>) -> Self {
        Self {
            archive_path,
            archive_name,
            dest_root: ctx.dest_root,
            report: ctx.report,
            counters: ctx.counters,
            partners: ctx.partners,
            motion: ctx.motion,
            pending_xmp: ctx.pending_xmp,
            pending_meta: ctx.pending_meta,
            derive_xmp: ctx.derive_xmp,
        }
    }

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
        let file_name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let directory = relative.parent().map(rel_string).unwrap_or_default();
        let partner = if kind == "sidecar" {
            self.partners
                .lookup(&directory, file_name)
                .map(str::to_string)
        } else {
            None
        };

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
        if self.derive_xmp && stored.kind == "sidecar" {
            let xmp_name = match stored.partner {
                Some(partner) => format!("{partner}.xmp"),
                None => format!(
                    "{}.xmp",
                    Path::new(stored.destination)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("sidecar.json")
                ),
            };
            let mut relative = PathBuf::from(stored.destination);
            relative.set_file_name(&xmp_name);
            self.pending_xmp.push(PendingXmp {
                archive_name: self.archive_name.to_string(),
                member: stored.member.to_string(),
                json_path: stored_path.to_path_buf(),
                xmp_relative: rel_string(&relative),
                partner: stored.partner.map(str::to_string),
            });
        }
        if stored.kind == "sidecar" {
            if let Some(partner) = stored.partner {
                let mut media = PathBuf::from(stored.destination);
                media.pop();
                media.push(partner);
                self.pending_meta.push(PendingMeta {
                    archive_name: self.archive_name.to_string(),
                    member: stored.member.to_string(),
                    json_path: stored_path.to_path_buf(),
                    media_relative: rel_string(&media),
                    partner: partner.to_string(),
                });
            }
        }
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
        mark_takeout_fields(
            &mut line,
            note.member,
            note.kind,
            note.partner,
            self.partners,
            self.motion,
            self.counters,
        );
        self.report.line(&line)
    }
}

fn kind_of(relative: &Path) -> &'static str {
    match relative.file_name().and_then(|name| name.to_str()) {
        Some(name) if ends_with_ignore_ascii_case(name, ".json") => "sidecar",
        _ => "original",
    }
}

struct PartnerIndex {
    /// `{directory}\n{json name in ascii lowercase}` → media file name.
    partners: HashMap<String, String>,
    /// `{directory}\n{media file name}` for media that has exactly one JSON.
    paired_media: HashSet<String>,
}

struct MotionIndex {
    /// `{directory}\n{stem lowercase}` → the still's stem, when a video shares it.
    stems: HashMap<String, String>,
}

impl PartnerIndex {
    fn lookup(&self, directory: &str, json_name: &str) -> Option<&str> {
        self.partners
            .get(&partner_key(directory, json_name))
            .map(String::as_str)
    }
}

fn partner_key(directory: &str, json_name: &str) -> String {
    format!("{directory}\n{}", json_name.to_ascii_lowercase())
}

fn archive_member_names(path: &Path) -> Result<Vec<String>, IngestError> {
    match archive_kind(path) {
        Some(ArchiveKind::Zip) => zip_member_names(path),
        Some(ArchiveKind::Tgz) => tgz_member_names(path),
        None => Err(IngestError::store(
            "takeout",
            display_name(path),
            "not a .zip, .tgz, or .tar.gz archive",
        )),
    }
}

fn zip_member_names(path: &Path) -> Result<Vec<String>, IngestError> {
    let file = open_read_only(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| zip_err(path, err))?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|err| zip_err(path, err))?;
        if entry.is_dir() {
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
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut names = Vec::new();
    let entries = archive
        .entries()
        .map_err(|err| IngestError::io(path, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| IngestError::io(path, err))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if let Ok(name) = utf8_path(&entry.path_bytes()) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Pair each JSON name with the one media file in the same directory whose
/// Takeout JSON name is that file. Matching runs from the media name to the
/// JSON name. A cut that removes part of the media name is not reversible
/// from the JSON alone, so the directory listing decides it. Two media files
/// that would share one JSON name are left unpaired.
fn build_partner_index(members: &[String]) -> PartnerIndex {
    let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
    for member in members {
        let Ok(relative) = member_relative(member) else {
            continue;
        };
        let Some(file_name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let directory = relative.parent().map(rel_string).unwrap_or_default();
        dirs.entry(directory)
            .or_default()
            .push(file_name.to_string());
    }
    let mut partners = HashMap::new();
    let mut paired_media = HashSet::new();
    for (directory, mut names) in dirs {
        names.sort();
        names.dedup();
        let mut claims: HashMap<String, Vec<String>> = HashMap::new();
        for name in &names {
            if ends_with_ignore_ascii_case(name, ".json") {
                continue;
            }
            for expected in expected_json_names(name) {
                let slot = claims.entry(expected.to_ascii_lowercase()).or_default();
                if !slot.iter().any(|existing| existing == name) {
                    slot.push(name.clone());
                }
            }
        }
        for name in &names {
            if !ends_with_ignore_ascii_case(name, ".json") {
                continue;
            }
            let Some(media) = claims.get(&name.to_ascii_lowercase()) else {
                continue;
            };
            if media.len() == 1 {
                partners.insert(partner_key(&directory, name), media[0].clone());
                paired_media.insert(format!("{directory}\n{}", media[0]));
            }
        }
    }
    PartnerIndex {
        partners,
        paired_media,
    }
}

fn expected_json_names(media: &str) -> [String; 2] {
    takeout_json_sidecar_names(media)
}

fn write_albumen_sidecars(
    pending: &[PendingMeta],
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    for item in pending {
        let parsed = {
            let file = match File::open(&item.json_path) {
                Ok(file) => file,
                Err(err) => {
                    counters.mismatched = counters.mismatched.saturating_add(1);
                    report.line(&json!({
                        "kind": "albumen-sidecar",
                        "archive": item.archive_name,
                        "member": item.member,
                        "destination": format!("{}.albumen.json", item.media_relative),
                        "partner": item.partner,
                        "sha256": "",
                        "bytes": 0,
                        "status": "mismatch",
                        "detail": err.to_string(),
                    }))?;
                    continue;
                }
            };
            match serde_json::from_reader::<_, serde_json::Value>(file) {
                Ok(value) => value,
                Err(err) => {
                    counters.mismatched = counters.mismatched.saturating_add(1);
                    report.line(&json!({
                        "kind": "albumen-sidecar",
                        "archive": item.archive_name,
                        "member": item.member,
                        "destination": format!("{}.albumen.json", item.media_relative),
                        "partner": item.partner,
                        "sha256": "",
                        "bytes": 0,
                        "status": "mismatch",
                        "detail": format!("Takeout JSON could not be read for the Albumen sidecar: {err}"),
                    }))?;
                    continue;
                }
            }
        };
        let Some(bytes) = albumen_sidecar_bytes(&parsed, &item.media_relative) else {
            continue;
        };
        let relative = format!("{}.albumen.json", item.media_relative);
        let dest = dest_root.join(&relative);
        let mtime = fs::metadata(&item.json_path)
            .and_then(|meta| meta.modified())
            .map_err(|err| IngestError::io(&item.json_path, err))?;
        let outcome = place_derived(&item.json_path, &dest, &bytes, mtime)?;
        if outcome.status == "mismatch" {
            counters.mismatched = counters.mismatched.saturating_add(1);
        }
        let mut line = json!({
            "kind": "albumen-sidecar",
            "archive": item.archive_name,
            "member": item.member,
            "destination": relative,
            "partner": item.media_relative,
            "sha256": outcome.sha256,
            "bytes": outcome.bytes,
            "status": outcome.status,
        });
        if let Some(detail) = outcome.detail {
            line["detail"] = json!(detail);
        }
        report.line(&line)?;
    }
    Ok(())
}

fn albumen_sidecar_bytes(value: &serde_json::Value, partner: &str) -> Option<Vec<u8>> {
    let taken = value
        .pointer("/photoTakenTime/timestamp")
        .and_then(|item| item.as_str())
        .filter(|text| !text.is_empty());
    let geo = value.get("geoData").and_then(sidecar_geo);
    if taken.is_none() && geo.is_none() {
        return None;
    }
    let mut map = serde_json::Map::new();
    map.insert("partner".to_string(), json!(partner));
    if let Some(taken) = taken {
        map.insert("photoTakenTime".to_string(), json!(taken));
        if let Some(utc) = xmp_datetime(taken) {
            map.insert("photoTakenTimeUtc".to_string(), json!(utc));
        }
    }
    if let Some((lat, lon, alt)) = geo {
        map.insert("latitude".to_string(), json!(lat));
        map.insert("longitude".to_string(), json!(lon));
        if let Some(alt) = alt {
            map.insert("altitude".to_string(), json!(alt));
        }
    }
    let mut bytes = serde_json::to_vec(&serde_json::Value::Object(map)).ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

fn sidecar_geo(geo: &serde_json::Value) -> Option<(f64, f64, Option<f64>)> {
    let lat = geo.get("latitude")?.as_f64()?;
    let lon = geo.get("longitude")?.as_f64()?;
    if lat == 0.0 && lon == 0.0 {
        return None;
    }
    let alt = geo.get("altitude").and_then(|item| item.as_f64());
    Some((lat, lon, alt))
}

fn place_derived(
    label: &Path,
    dest: &Path,
    bytes: &[u8],
    mtime: SystemTime,
) -> Result<DerivedWrite, IngestError> {
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => Ok(DerivedWrite {
            sha256: String::new(),
            bytes: 0,
            status: "mismatch",
            detail: Some("Albumen sidecar destination is not a regular file".to_string()),
        }),
        Ok(_) => {
            let existing = sha256_file(dest)?;
            let incoming = sha256_reader(Cursor::new(bytes), label)?;
            if existing == incoming {
                Ok(DerivedWrite {
                    sha256: to_hex(&existing),
                    bytes: fs::metadata(dest)
                        .map_err(|err| IngestError::io(dest, err))?
                        .len(),
                    status: "already-verified",
                    detail: None,
                })
            } else {
                Ok(DerivedWrite {
                    sha256: to_hex(&incoming),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(
                        "Albumen sidecar destination already has different bytes; it was not overwritten"
                            .to_string(),
                    ),
                })
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut reader = Cursor::new(bytes);
            match copy_verify_reader(&mut reader, label, dest, mtime) {
                Ok(object) => Ok(DerivedWrite {
                    sha256: object.sha256,
                    bytes: object.byte_len,
                    status: "stored",
                    detail: None,
                }),
                Err(IngestError::ChecksumMismatch { message, .. }) => Ok(DerivedWrite {
                    sha256: String::new(),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(message),
                }),
                Err(IngestError::AlreadyExists { .. }) => Ok(DerivedWrite {
                    sha256: String::new(),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(
                        "Albumen sidecar destination appeared during copy; it was not overwritten"
                            .to_string(),
                    ),
                }),
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(IngestError::io(dest, err)),
    }
}

fn write_pending_xmp(
    pending: &[PendingXmp],
    dest_root: &Path,
    report: &mut ReportFile,
    counters: &mut Counters,
) -> Result<(), IngestError> {
    for item in pending {
        let xmp_path = dest_root.join(&item.xmp_relative);
        let outcome = match render_stored_xmp(&item.json_path, &xmp_path) {
            Ok(outcome) => outcome,
            Err(err) => {
                counters.mismatched = counters.mismatched.saturating_add(1);
                report.line(&json!({
                    "kind": "derived-xmp",
                    "archive": item.archive_name,
                    "member": item.member,
                    "destination": item.xmp_relative,
                    "partner": item.partner,
                    "sha256": "",
                    "bytes": 0,
                    "status": "mismatch",
                    "detail": err.to_string(),
                }))?;
                continue;
            }
        };
        if outcome.status == "mismatch" {
            counters.mismatched = counters.mismatched.saturating_add(1);
        }
        let mut line = json!({
            "kind": "derived-xmp",
            "archive": item.archive_name,
            "member": item.member,
            "destination": item.xmp_relative,
            "partner": item.partner,
            "sha256": outcome.sha256,
            "bytes": outcome.bytes,
            "status": outcome.status,
        });
        if let Some(detail) = outcome.detail {
            line["detail"] = json!(detail);
        }
        report.line(&line)?;
    }
    Ok(())
}

struct DerivedWrite {
    sha256: String,
    bytes: u64,
    status: &'static str,
    detail: Option<String>,
}

/// Write one derived XMP next to a stored JSON file.
///
/// The original is not a path this function opens. `json_path` is read, and
/// `xmp_path` is created. An existing XMP with the same SHA-256 is left in
/// place. Different bytes are left in place and reported as a mismatch.
fn render_stored_xmp(json_path: &Path, xmp_path: &Path) -> Result<DerivedWrite, IngestError> {
    let json_mtime = fs::metadata(json_path)
        .and_then(|meta| meta.modified())
        .map_err(|err| IngestError::io(json_path, err))?;
    let parsed: serde_json::Value = {
        let file = File::open(json_path).map_err(|err| IngestError::io(json_path, err))?;
        serde_json::from_reader(file).map_err(|err| {
            IngestError::store("takeout-xmp", json_path.display().to_string(), err)
        })?
    };
    let bytes = render_xmp(&parsed).into_bytes();
    match fs::symlink_metadata(xmp_path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => Ok(DerivedWrite {
            sha256: String::new(),
            bytes: 0,
            status: "mismatch",
            detail: Some("derived XMP destination is not a regular file".to_string()),
        }),
        Ok(_) => {
            let existing = sha256_file(xmp_path)?;
            let incoming = sha256_reader(Cursor::new(&bytes), json_path)?;
            if existing == incoming {
                Ok(DerivedWrite {
                    sha256: to_hex(&existing),
                    bytes: fs::metadata(xmp_path)
                        .map_err(|err| IngestError::io(xmp_path, err))?
                        .len(),
                    status: "already-verified",
                    detail: None,
                })
            } else {
                Ok(DerivedWrite {
                    sha256: to_hex(&incoming),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(
                        "derived XMP destination already has different bytes; it was not overwritten"
                            .to_string(),
                    ),
                })
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut reader = Cursor::new(bytes);
            match copy_verify_reader(&mut reader, json_path, xmp_path, json_mtime) {
                Ok(object) => Ok(DerivedWrite {
                    sha256: object.sha256,
                    bytes: object.byte_len,
                    status: "stored",
                    detail: None,
                }),
                Err(IngestError::ChecksumMismatch { message, .. }) => Ok(DerivedWrite {
                    sha256: String::new(),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(message),
                }),
                Err(IngestError::AlreadyExists { .. }) => Ok(DerivedWrite {
                    sha256: String::new(),
                    bytes: 0,
                    status: "mismatch",
                    detail: Some(
                        "derived XMP destination appeared during copy; it was not overwritten"
                            .to_string(),
                    ),
                }),
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(IngestError::io(xmp_path, err)),
    }
}

fn render_xmp(value: &serde_json::Value) -> String {
    let title = value.get("title").and_then(|item| item.as_str());
    let description = value.get("description").and_then(|item| item.as_str());
    let taken = value
        .pointer("/photoTakenTime/timestamp")
        .and_then(|item| item.as_str())
        .and_then(xmp_datetime);
    let geo = value.get("geoData").and_then(xmp_geo);
    let mut attrs = String::new();
    if let Some(taken) = taken {
        attrs.push_str(&format!(" exif:DateTimeOriginal=\"{taken}\""));
    }
    if let Some((lat, lon, alt)) = geo {
        attrs.push_str(&format!(
            " exif:GPSLatitude=\"{lat}\" exif:GPSLongitude=\"{lon}\""
        ));
        if let Some(alt) = alt {
            attrs.push_str(&format!(" exif:GPSAltitude=\"{alt}\""));
        }
    }
    let mut body = String::new();
    if let Some(title) = title {
        body.push_str(&format!(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            xml_escape(title)
        ));
    }
    if let Some(description) = description {
        body.push_str(&format!(
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>",
            xml_escape(description)
        ));
    }
    format!(
        "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"{attrs}>{body}</rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    )
}

fn xmp_geo(geo: &serde_json::Value) -> Option<(String, String, Option<String>)> {
    let lat = geo.get("latitude")?.as_f64()?;
    let lon = geo.get("longitude")?.as_f64()?;
    if lat == 0.0 && lon == 0.0 {
        return None;
    }
    let alt = geo.get("altitude").and_then(|item| item.as_f64());
    Some((format_coord(lat), format_coord(lon), alt.map(format_coord)))
}

fn format_coord(value: f64) -> String {
    let raw = format!("{value:.6}");
    let trimmed = raw.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn xmp_datetime(timestamp: &str) -> Option<String> {
    let secs: i64 = timestamp.parse().ok()?;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days)?;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

/// Inverse of [`days_from_civil`]. Public-domain civil-from-days conversion.
fn civil_from_days(mut z: i64) -> Option<(i32, u32, u32)> {
    z = z.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let year = i32::try_from(year).ok()?;
    Some((year, u32::try_from(month).ok()?, u32::try_from(day).ok()?))
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other if other.is_control() => {}
            other => out.push(other),
        }
    }
    out
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

enum TakeoutLayout {
    Archives(Vec<PathBuf>),
    Extracted,
}

fn classify(from: &Path) -> Result<TakeoutLayout, IngestError> {
    let meta = fs::symlink_metadata(from).map_err(|err| IngestError::io(from, err))?;
    if meta.file_type().is_symlink() {
        return Err(IngestError::Symlink(from.to_path_buf()));
    }
    if meta.is_file() {
        if archive_kind(from).is_none() {
            return Err(IngestError::store(
                "takeout",
                display_name(from),
                "not a .zip, .tgz, or .tar.gz archive or an extracted folder",
            ));
        }
        return Ok(TakeoutLayout::Archives(vec![from.to_path_buf()]));
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
    let mut nested = false;
    let mut other = false;
    for name in names {
        let path = from.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(|err| IngestError::io(&path, err))?;
        if meta.file_type().is_symlink() {
            if archives.is_empty() {
                other = true;
            } else {
                return Err(IngestError::Symlink(path));
            }
            continue;
        }
        if meta.is_dir() {
            nested = true;
            continue;
        }
        if meta.is_file() && archive_kind(&path).is_some() {
            archives.push(path);
            continue;
        }
        if meta.is_file() {
            other = true;
            continue;
        }
        return Err(IngestError::store(
            "takeout",
            display_name(&path),
            "not a regular file; it was not skipped",
        ));
    }
    if !archives.is_empty() && (nested || other) {
        return Err(IngestError::store(
            "takeout",
            display_name(from),
            "directory mixes archives with other files or folders; nothing was skipped",
        ));
    }
    if !archives.is_empty() {
        return Ok(TakeoutLayout::Archives(archives));
    }
    if nested || other {
        return Ok(TakeoutLayout::Extracted);
    }
    Err(IngestError::store(
        "takeout",
        display_name(from),
        "directory has no Takeout archives or extracted files",
    ))
}

fn layout_names(from: &Path) -> Result<Vec<String>, IngestError> {
    match classify(from)? {
        TakeoutLayout::Archives(archives) => archive_names(&archives),
        TakeoutLayout::Extracted => extracted_names(from),
    }
}

fn archive_names(archives: &[PathBuf]) -> Result<Vec<String>, IngestError> {
    let mut names = Vec::new();
    for archive in archives {
        names.extend(archive_member_names(archive)?);
    }
    Ok(names)
}

fn extracted_names(root: &Path) -> Result<Vec<String>, IngestError> {
    let mut names = Vec::new();
    walk_extracted_names(root, "", &mut names)?;
    Ok(names)
}

fn walk_extracted_names(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), IngestError> {
    let source = SourceRoot::open(dir)?;
    let mut names = Vec::new();
    for entry in source.read_dir(dir)? {
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
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => out.push(relative),
            Ok(meta) if meta.is_dir() => walk_extracted_names(&path, &relative, out)?,
            Ok(meta) if meta.is_file() => out.push(relative),
            Ok(_) => out.push(relative),
            Err(err) => return Err(IngestError::io(&path, err)),
        }
    }
    Ok(())
}

fn plan_from_names(names: &[String]) -> TakeoutPlan {
    let partners = build_partner_index(names);
    let motion = motion_pairs(names);
    let mut unmatched = Vec::new();
    for member in names {
        let Ok(relative) = member_relative(member) else {
            unmatched.push(member.clone());
            continue;
        };
        let file_name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let directory = relative.parent().map(rel_string).unwrap_or_default();
        if ends_with_ignore_ascii_case(file_name, ".json") {
            if partners.lookup(&directory, file_name).is_none() && takeout_json_name(file_name) {
                unmatched.push(member.clone());
            }
        } else if is_media_name(file_name)
            && !partners
                .paired_media
                .contains(&format!("{directory}\n{file_name}"))
        {
            unmatched.push(member.clone());
        }
    }
    unmatched.sort();
    unmatched.dedup();
    let mut motion_photos = motion.stems.values().cloned().collect::<Vec<_>>();
    motion_photos.sort();
    motion_photos.dedup();
    TakeoutPlan {
        files: names.len() as u64,
        unmatched,
        motion_photos,
    }
}

fn takeout_json_name(name: &str) -> bool {
    let Some(stem) = strip_suffix_ignore_ascii_case(name, ".json") else {
        return false;
    };
    if stem.to_ascii_lowercase().contains("supplemental-metadata")
        || stem.to_ascii_lowercase().contains(".supple")
    {
        return true;
    }
    let core = stem.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    is_media_ext(core)
}

fn mark_takeout_fields(
    line: &mut serde_json::Value,
    member: &str,
    kind: &str,
    partner: Option<Option<&str>>,
    partners: &PartnerIndex,
    motion: &MotionIndex,
    counters: &mut Counters,
) {
    let unmatched = member_unmatched(member, kind, partner, partners);
    if unmatched {
        line["unmatched"] = json!(true);
        counters.unmatched = counters.unmatched.saturating_add(1);
    }
    if let Some(stem) = motion_stem(member, motion) {
        line["motion_photo_stem"] = json!(stem);
    }
}

fn member_unmatched(
    member: &str,
    kind: &str,
    partner: Option<Option<&str>>,
    partners: &PartnerIndex,
) -> bool {
    let Ok(relative) = member_relative(member) else {
        return false;
    };
    let Some(file_name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let directory = relative.parent().map(rel_string).unwrap_or_default();
    if kind == "sidecar" {
        return partner == Some(None) && takeout_json_name(file_name);
    }
    kind == "original"
        && is_media_name(file_name)
        && !partners
            .paired_media
            .contains(&format!("{directory}\n{file_name}"))
}

fn motion_stem(member: &str, motion: &MotionIndex) -> Option<String> {
    let relative = member_relative(member).ok()?;
    let file_name = relative.file_name().and_then(|name| name.to_str())?;
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !is_still_ext(ext) && !is_video_ext(ext) {
        return None;
    }
    let directory = relative.parent().map(rel_string).unwrap_or_default();
    let key = format!("{directory}\n{}", stem.to_ascii_lowercase());
    motion.stems.get(&key).cloned()
}

fn motion_pairs(members: &[String]) -> MotionIndex {
    let mut stills: HashMap<String, String> = HashMap::new();
    let mut videos: HashSet<String> = HashSet::new();
    for member in members {
        let Ok(relative) = member_relative(member) else {
            continue;
        };
        let Some(file_name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((stem, ext)) = file_name.rsplit_once('.') else {
            continue;
        };
        let directory = relative.parent().map(rel_string).unwrap_or_default();
        let key = format!("{directory}\n{}", stem.to_ascii_lowercase());
        if is_still_ext(ext) {
            stills.entry(key).or_insert_with(|| stem.to_string());
        } else if is_video_ext(ext) {
            videos.insert(key);
        }
    }
    let mut stems = HashMap::new();
    for (key, stem) in stills {
        if videos.contains(&key) {
            stems.insert(key, stem);
        }
    }
    MotionIndex { stems }
}

fn is_media_name(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| is_media_ext(ext))
}

fn is_media_ext(ext: &str) -> bool {
    is_still_ext(ext) || is_video_ext(ext) || is_raw_ext(ext)
}

fn is_still_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "heic" | "heif" | "hif" | "png" | "tif" | "tiff" | "avif" | "webp" | "gif"
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
    let root = SourceRoot::open(parent_dir(path))?;
    root.open_read(path)
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
    if suffix.len() > name.len() {
        return None;
    }
    let split = name.len() - suffix.len();
    if !name.is_char_boundary(split) {
        return None;
    }
    let (head, tail) = name.split_at(split);
    if tail.eq_ignore_ascii_case(suffix) {
        Some(head)
    } else {
        None
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

    fn partner_of(json_name: &str, files: &[&str]) -> Option<String> {
        let members = files
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        build_partner_index(&members)
            .lookup("", json_name)
            .map(str::to_string)
    }

    #[test]
    fn partner_names_cover_takeout_json_spellings() {
        let plain = ["IMG_0001.jpg", "IMG_0001.jpg.json"];
        assert_eq!(
            partner_of("IMG_0001.jpg.json", &plain).as_deref(),
            Some("IMG_0001.jpg")
        );
        let clip = ["CLIP.mp4", "CLIP.mp4.supplemental-metadata.json"];
        assert_eq!(
            partner_of("CLIP.mp4.supplemental-metadata.json", &clip).as_deref(),
            Some("CLIP.mp4")
        );
        let numbered = [
            "CLIP.mp4",
            "CLIP(1).mp4",
            "CLIP.mp4.supplemental-metadata.json",
            "CLIP.mp4.supplemental-metadata(1).json",
        ];
        assert_eq!(
            partner_of("CLIP.mp4.supplemental-metadata(1).json", &numbered).as_deref(),
            Some("CLIP(1).mp4")
        );
        assert_eq!(
            partner_of("CLIP.mp4.supplemental-metadata.json", &numbered).as_deref(),
            Some("CLIP.mp4")
        );
        assert_eq!(partner_of("metadata.json", &["metadata.json"]), None);
        assert_eq!(
            partner_of(
                "user-generated-memory-titles.json",
                &["user-generated-memory-titles.json"]
            ),
            None
        );
    }

    #[test]
    fn expected_json_names_follow_the_51_character_cut() {
        assert_eq!(
            expected_json_names("Screenshot_20231027_123303_Facebook.jpg")[0],
            "Screenshot_20231027_123303_Facebook.jpg.supple.json"
        );
        assert_eq!(
            expected_json_names("PXL_20240101_120000_EXTRA.jpg")[0],
            "PXL_20240101_120000_EXTRA.jpg.supplemental-met.json"
        );
        assert_eq!(
            expected_json_names("20161105_170829.jpg")[0],
            "20161105_170829.jpg.supplemental-metadata.json"
        );
        let forty_five = format!("{}.jpg", "n".repeat(41));
        assert_eq!(utf16_units(&forty_five), 45);
        assert_eq!(
            expected_json_names(&forty_five)[0],
            format!("{forty_five}..json")
        );
        let snap = "Screenshot_2024-08-31-00-53-48-647_com.snapchat.jpg";
        assert_eq!(utf16_units(snap), 51);
        assert_eq!(
            expected_json_names(snap)[0],
            "Screenshot_2024-08-31-00-53-48-647_com.snapcha.json"
        );
        assert_eq!(
            expected_json_names("Screenshot_20231027_123303_Facebook(1).jpg")[0],
            "Screenshot_20231027_123303_Facebook.jpg.supple(1).json"
        );
        assert_eq!(
            expected_json_names("IMG_1234(1).jpg")[0],
            "IMG_1234.jpg.supplemental-metadata(1).json"
        );
        assert_eq!(
            expected_json_names("IMG_1234(1).jpg")[1],
            "IMG_1234.jpg(1).json"
        );
        assert_eq!(
            expected_json_names("IMG_0001-edited.jpg")[0],
            "IMG_0001-edited.jpg.supplemental-metadata.json"
        );
        let edited = "Screenshot_20231027_123303_Facebook-edited.jpg";
        assert_eq!(utf16_units(edited), 46);
        assert_eq!(
            expected_json_names(edited)[0],
            "Screenshot_20231027_123303_Facebook-edited.jpg.json"
        );
    }

    fn utf16_units(value: &str) -> usize {
        value.chars().map(char::len_utf16).sum()
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
            .filter(|line| {
                line.get("summary").is_none()
                    && line["kind"] != "archive"
                    && line["kind"] != "albumen-sidecar"
            })
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

    #[test]
    fn each_truncated_json_name_pairs_and_unpaired_json_is_copied() {
        let tmp = Tmp::new();
        let from = tmp.path().join("takeout");
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        fs::create_dir(&from).unwrap();

        let forty_five = format!("{}.jpg", "n".repeat(41));
        let media = [
            "IMG_0001.jpg",
            "IMG_0002.jpg",
            "IMG_0002-edited.jpg",
            "IMG_1234.jpg",
            "IMG_1234(1).jpg",
            "Screenshot_20231027_123303_Facebook.jpg",
            "Screenshot_20231027_123303_Facebook(1).jpg",
            "Screenshot_20231027_123303_Facebook-edited.jpg",
            "PXL_20240101_120000_EXTRA.jpg",
            "Screenshot_2024-08-31-00-53-48-647_com.snapchat.jpg",
            forty_five.as_str(),
        ];
        let mut zip_files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut expected: Vec<(String, String, Vec<u8>)> = Vec::new();
        for (index, name) in media.iter().enumerate() {
            let bytes = format!("synthetic-media-{index}-{name}").into_bytes();
            let json_name = if *name == "IMG_0001.jpg" || *name == "IMG_0002.jpg" {
                expected_json_names(name)[1].clone()
            } else {
                expected_json_names(name)[0].clone()
            };
            let json = taken_json(name, "1590000000", "1600000000");
            zip_files.push((format!("Takeout/Album/{name}"), bytes.clone()));
            if *name == "Screenshot_20231027_123303_Facebook.jpg" {
                expected.push((name.to_string(), json_name, json));
            } else {
                zip_files.push((format!("Takeout/Album/{json_name}"), json.clone()));
                expected.push((name.to_string(), json_name, json));
            }
        }
        let album_meta = br#"{"title":"synthetic album"}"#.to_vec();
        let memory = br#"{"title":"synthetic memory"}"#.to_vec();
        zip_files.push((
            "Takeout/Album/metadata.json".to_string(),
            album_meta.clone(),
        ));
        zip_files.push((
            "Takeout/Album/user-generated-memory-titles.json".to_string(),
            memory.clone(),
        ));

        let zip_path = from.join("takeout-001.zip");
        let tgz_path = from.join("takeout-002.tgz");
        write_zip_owned(&zip_path, &zip_files);
        let facebook_json = expected
            .iter()
            .find(|(name, _, _)| name == "Screenshot_20231027_123303_Facebook.jpg")
            .unwrap();
        let facebook_member = format!("Takeout/Album/{}", facebook_json.1);
        write_tgz(
            &tgz_path,
            &[(facebook_member.as_str(), facebook_json.2.as_slice())],
            1_590_000_000,
        );
        let zip_before = fs::read(&zip_path).unwrap();
        let tgz_before = fs::read(&tgz_path).unwrap();

        let outcome = import_takeout(&from, &to, &report).unwrap();
        let file_count = zip_files.len() as u64 + 1;
        assert_eq!(outcome.count_in, file_count);
        assert_eq!(outcome.count_out, file_count);
        assert_eq!(outcome.mismatched, 0);

        let lines = report_lines(&report);
        for (name, json_name, json_bytes) in &expected {
            let media_path = to.join(format!("Takeout/Album/{name}"));
            let json_path = to.join(format!("Takeout/Album/{json_name}"));
            assert_eq!(
                fs::read(&json_path).unwrap(),
                *json_bytes,
                "{json_name} bytes"
            );
            assert!(media_path.is_file(), "{name}");
            let line = lines
                .iter()
                .find(|line| line["destination"] == format!("Takeout/Album/{json_name}"))
                .unwrap_or_else(|| panic!("missing report line for {json_name}"));
            assert_eq!(line["kind"], "sidecar", "{json_name}");
            assert_eq!(line["partner"], name.as_str(), "{json_name}");
            assert_eq!(line["status"], "stored");
        }
        let plain = lines
            .iter()
            .find(|line| {
                line["destination"] == "Takeout/Album/IMG_1234.jpg.supplemental-metadata.json"
            })
            .unwrap();
        let numbered = lines
            .iter()
            .find(|line| {
                line["destination"] == "Takeout/Album/IMG_1234.jpg.supplemental-metadata(1).json"
            })
            .unwrap();
        assert_eq!(plain["partner"], "IMG_1234.jpg");
        assert_eq!(numbered["partner"], "IMG_1234(1).jpg");
        assert_ne!(plain["partner"], numbered["partner"]);

        for (name, bytes) in [
            ("metadata.json", album_meta.as_slice()),
            ("user-generated-memory-titles.json", memory.as_slice()),
        ] {
            let path = to.join(format!("Takeout/Album/{name}"));
            assert_eq!(fs::read(&path).unwrap(), bytes);
            let line = lines
                .iter()
                .find(|line| line["destination"] == format!("Takeout/Album/{name}"))
                .unwrap();
            assert_eq!(line["kind"], "sidecar");
            assert!(line["partner"].is_null(), "{line}");
            assert_eq!(line["status"], "stored");
        }
        assert!(
            !to.join("Takeout/Album/IMG_0001.jpg.xmp").exists(),
            "derived XMP stays off unless requested"
        );
        assert_eq!(fs::read(&zip_path).unwrap(), zip_before);
        assert_eq!(fs::read(&tgz_path).unwrap(), tgz_before);
        assert!(!from.join("Takeout").exists());
    }

    #[test]
    fn derived_xmp_is_a_new_file_and_leaves_the_original() {
        let tmp = Tmp::new();
        let dir = tmp.path().join("loose");
        fs::create_dir(&dir).unwrap();
        let original = dir.join("IMG_0001.jpg");
        let original_bytes = b"synthetic-original-not-rewritten";
        fs::write(&original, original_bytes).unwrap();
        let sentinel = UNIX_EPOCH + Duration::from_secs(1_111_111_111);
        crate::mtime::set_mtime(&original, sentinel).unwrap();
        let before_mtime = mtime_of(&original).unwrap();
        let json_bytes = taken_json("IMG_0001.jpg", "1590000000", "1600000000");
        let json_path = dir.join("IMG_0001.jpg.json");
        fs::write(&json_path, &json_bytes).unwrap();
        let xmp_path = dir.join("IMG_0001.jpg.xmp");
        let written = render_stored_xmp(&json_path, &xmp_path).unwrap();
        assert_eq!(written.status, "stored");
        assert_eq!(fs::read(&original).unwrap(), original_bytes);
        assert_eq!(mtime_of(&original).unwrap(), before_mtime);
        assert_eq!(fs::read(&json_path).unwrap(), json_bytes);
        let xmp = fs::read_to_string(&xmp_path).unwrap();
        assert!(xmp.contains("2020-05-20T18:40:00Z"), "{xmp}");
        assert!(!xmp.contains("2020-09-13T12:26:40Z"), "{xmp}");
        assert!(!xmp.contains("GPSLatitude"), "{xmp}");
        assert!(xmp.contains("synthetic fixture"));
        let placed = render_xmp(&serde_json::json!({
            "title": "A&B",
            "photoTakenTime": {"timestamp": "1590000000"},
            "geoData": {"latitude": 37.5, "longitude": -122.25, "altitude": 10.0}
        }));
        assert!(placed.contains("2020-05-20T18:40:00Z"), "{placed}");
        assert!(placed.contains("exif:GPSLatitude=\"37.5\""), "{placed}");
        assert!(placed.contains("exif:GPSLongitude=\"-122.25\""), "{placed}");
        assert!(placed.contains("A&amp;B"), "{placed}");

        let incoming = tmp.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let archive = incoming.join("takeout.zip");
        let photo = b"synthetic-import-original";
        let caption = taken_json("IMG_0001.jpg", "1590000000", "1600000000");
        write_zip(
            &archive,
            CompressionMethod::Stored,
            &[
                ("Takeout/IMG_0001.jpg", photo.as_slice()),
                ("Takeout/IMG_0001.jpg.json", caption.as_slice()),
            ],
        );
        let library = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let outcome = import_takeout_with(&archive, &library, &report, true).unwrap();
        assert_eq!(outcome.count_in, 2);
        assert_eq!(outcome.count_out, 2);
        assert_eq!(outcome.mismatched, 0);
        let stored = library.join("Takeout/IMG_0001.jpg");
        assert_eq!(fs::read(&stored).unwrap(), photo);
        let expected_zip_mtime =
            dos_to_system_time(&zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap())
                .unwrap();
        assert_eq!(mtime_of(&stored).unwrap(), expected_zip_mtime);
        assert_eq!(
            fs::read(library.join("Takeout/IMG_0001.jpg.json")).unwrap(),
            caption
        );
        let derived = fs::read_to_string(library.join("Takeout/IMG_0001.jpg.xmp")).unwrap();
        assert!(derived.contains("2020-05-20T18:40:00Z"), "{derived}");
        let lines = report_lines(&report);
        let xmp_line = lines
            .iter()
            .find(|line| line["kind"] == "derived-xmp")
            .unwrap();
        assert_eq!(xmp_line["destination"], "Takeout/IMG_0001.jpg.xmp");
        assert_eq!(xmp_line["partner"], "IMG_0001.jpg");
        assert_eq!(xmp_line["status"], "stored");
        let summary = lines.last().unwrap();
        assert_eq!(summary["count_in"], 2);
        assert_eq!(summary["count_out"], 2);
        assert_eq!(summary["mismatched"], 0);

        let plain = tmp.path().join("plain");
        let plain_report = tmp.path().join("plain.jsonl");
        import_takeout(&archive, &plain, &plain_report).unwrap();
        assert!(!plain.join("Takeout/IMG_0001.jpg.xmp").exists());
        assert_eq!(fs::read(plain.join("Takeout/IMG_0001.jpg")).unwrap(), photo);
    }

    #[test]
    fn extracted_folder_restores_gps_and_reports_unmatched_motion_pairs() {
        let tmp = Tmp::new();
        let from = tmp.path().join("Takeout");
        let album = from.join("Google Photos/Trip");
        fs::create_dir_all(&album).unwrap();
        let jpeg = b"\xff\xd8\xff\xd9synthetic-extracted-jpeg";
        let motion = b"synthetic-extracted-motion";
        let orphan = b"\xff\xd8\xff\xd9synthetic-unmatched-jpeg";
        let json = br#"{"title":"IMG_0001.jpg","photoTakenTime":{"timestamp":"1590000000"},"geoData":{"latitude":37.5,"longitude":-122.25,"altitude":10.0}}"#;
        fs::write(album.join("IMG_0001.jpg"), jpeg).unwrap();
        fs::write(album.join("IMG_0001.jpg.supplemental-metadata.json"), json).unwrap();
        fs::write(album.join("IMG_0001.mp4"), motion).unwrap();
        fs::write(album.join("IMG_0002.jpg"), orphan).unwrap();
        let before = fs::read(album.join("IMG_0001.jpg")).unwrap();
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        let plan = plan_takeout(&from, &to, &report).unwrap();
        assert!(!to.exists());
        assert!(!report.exists());
        assert!(plan
            .unmatched
            .iter()
            .any(|name| name.ends_with("IMG_0002.jpg")));
        assert!(plan.motion_photos.iter().any(|stem| stem == "IMG_0001"));
        let outcome = import_takeout(&from, &to, &report).unwrap();
        assert_eq!(outcome.count_in, outcome.count_out);
        assert_eq!(outcome.mismatched, 0);
        assert!(outcome.unmatched >= 1);
        assert_eq!(fs::read(album.join("IMG_0001.jpg")).unwrap(), before);
        assert_eq!(
            fs::read(to.join("Google Photos/Trip/IMG_0001.jpg")).unwrap(),
            jpeg
        );
        let sidecar: serde_json::Value = serde_json::from_slice(
            &fs::read(to.join("Google Photos/Trip/IMG_0001.jpg.albumen.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar["photoTakenTime"], "1590000000");
        assert_eq!(sidecar["photoTakenTimeUtc"], "2020-05-20T18:40:00Z");
        assert_eq!(sidecar["latitude"], 37.5);
        assert_eq!(sidecar["longitude"], -122.25);
        assert!(!jpeg.windows(4).any(|window| window == b"37.5"));
        let lines = report_lines(&report);
        let photo = lines
            .iter()
            .find(|line| line["member"] == "Google Photos/Trip/IMG_0001.jpg")
            .unwrap();
        assert_eq!(photo["motion_photo_stem"], "IMG_0001");
        assert!(photo.get("unmatched").is_none(), "{photo}");
        let missing = lines
            .iter()
            .find(|line| line["member"] == "Google Photos/Trip/IMG_0002.jpg")
            .unwrap();
        assert_eq!(missing["unmatched"], true);
        let summary = lines.last().unwrap();
        assert_eq!(summary["count_in"], summary["count_out"]);
        assert!(summary["unmatched"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn synthetic_gate_fixture_round_trips_and_discover_pairs_json() {
        let tmp = Tmp::new();
        let from = tmp.path().join("takeout");
        let to = tmp.path().join("library");
        let report = tmp.path().join("report.jsonl");
        fs::create_dir(&from).unwrap();

        let long = format!("{}.jpg", "n".repeat(43));
        assert_eq!(long.chars().count(), 47);
        let long_json_name = expected_json_names(&long)[0].clone();
        assert_ne!(long_json_name, format!("{long}.json"));
        let jpeg = b"\xff\xd8\xff\xdb synthetic-gate-jpeg";
        let edited = b"\xff\xd8\xff\xdb synthetic-gate-edited";
        let heic = b"synthetic-gate-heic";
        let mov = b"synthetic-gate-mov";
        let clip = b"synthetic-gate-clip";
        let long_bytes = b"synthetic-gate-truncated-47";
        let orphan_media = b"synthetic-gate-orphan-media";
        let plain_json = taken_json("IMG_0001.jpg", "1590000000", "1600000000");
        let edited_json = taken_json("IMG_0001-edited.jpg", "1590000001", "1600000001");
        let live_json = taken_json("IMG_LIVE.HEIC", "1590000002", "1600000002");
        let clip_json = taken_json("CLIP.mp4", "1590000003", "1600000003");
        let long_json = taken_json(&long, "1590000004", "1600000004");
        let orphan_json = br#"{"title":"synthetic album"}"#.to_vec();
        assert!(!jpeg.windows(10).any(|window| window == b"1590000000"));

        let zip_path = from.join("takeout-001.zip");
        let tgz_path = from.join("takeout-002.tgz");
        write_zip(
            &zip_path,
            CompressionMethod::Stored,
            &[
                ("Takeout/Album/IMG_0001.jpg", jpeg.as_slice()),
                ("Takeout/Album/IMG_0001-edited.jpg", edited.as_slice()),
                ("Takeout/Album/IMG_LIVE.HEIC", heic.as_slice()),
                ("Takeout/Album/IMG_LIVE.MOV", mov.as_slice()),
                ("Takeout/Album/CLIP.mp4", clip.as_slice()),
                (&format!("Takeout/Album/{long}"), long_bytes.as_slice()),
                ("Takeout/Album/ORPHAN.jpg", orphan_media.as_slice()),
            ],
        );
        write_tgz(
            &tgz_path,
            &[
                ("Takeout/Album/IMG_0001.jpg.json", plain_json.as_slice()),
                (
                    "Takeout/Album/IMG_0001-edited.jpg.supplemental-metadata.json",
                    edited_json.as_slice(),
                ),
                (
                    "Takeout/Album/IMG_LIVE.HEIC.supplemental-metadata.json",
                    live_json.as_slice(),
                ),
                (
                    "Takeout/Album/CLIP.mp4.supplemental-metadata.json",
                    clip_json.as_slice(),
                ),
                (
                    &format!("Takeout/Album/{long_json_name}"),
                    long_json.as_slice(),
                ),
                ("Takeout/Album/metadata.json", orphan_json.as_slice()),
            ],
            1_590_000_000,
        );
        let zip_before = fs::read(&zip_path).unwrap();
        let tgz_before = fs::read(&tgz_path).unwrap();

        let outcome = import_takeout(&from, &to, &report).unwrap();
        assert_eq!(outcome.count_in, outcome.count_out);
        assert_eq!(outcome.mismatched, 0);
        assert!(outcome.unmatched >= 1);
        assert_eq!(fs::read(&zip_path).unwrap(), zip_before);
        assert_eq!(fs::read(&tgz_path).unwrap(), tgz_before);
        assert!(!from.join("Takeout").exists());
        assert_eq!(
            fs::read(to.join("Takeout/Album/IMG_0001.jpg")).unwrap(),
            jpeg
        );
        assert_eq!(
            fs::read(to.join("Takeout/Album/IMG_0001.jpg.json")).unwrap(),
            plain_json
        );
        assert_eq!(
            fs::read(to.join(format!("Takeout/Album/{long_json_name}"))).unwrap(),
            long_json
        );
        assert!(!to.join("Takeout/Album/IMG_0001.jpg.xmp").exists());

        let album = to.join("Takeout/Album");
        let sidecar: serde_json::Value =
            serde_json::from_slice(&fs::read(album.join("IMG_0001.jpg.albumen.json")).unwrap())
                .unwrap();
        assert_eq!(sidecar["photoTakenTime"], "1590000000");
        let stored_mtime = mtime_of(&album.join("IMG_0001.jpg")).unwrap();
        let stored_secs = stored_mtime
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_ne!(stored_secs, 1_590_000_000);

        let lines = report_lines(&report);
        for member in [
            "Takeout/Album/IMG_0001.jpg",
            "Takeout/Album/IMG_0001-edited.jpg",
            "Takeout/Album/IMG_LIVE.HEIC",
            "Takeout/Album/IMG_LIVE.MOV",
            "Takeout/Album/CLIP.mp4",
        ] {
            let line = lines
                .iter()
                .find(|line| line["member"] == member)
                .unwrap_or_else(|| panic!("missing {member}"));
            assert!(line["sha256"].as_str().unwrap().len() == 64, "{line}");
        }
        let live = lines
            .iter()
            .find(|line| line["member"] == "Takeout/Album/IMG_LIVE.HEIC")
            .unwrap();
        assert_eq!(live["motion_photo_stem"], "IMG_LIVE");
        let orphan_line = lines
            .iter()
            .find(|line| line["member"] == "Takeout/Album/ORPHAN.jpg")
            .unwrap();
        assert_eq!(orphan_line["unmatched"], true);
        let metadata = lines
            .iter()
            .find(|line| line["member"] == "Takeout/Album/metadata.json")
            .unwrap();
        assert!(metadata["partner"].is_null(), "{metadata}");
        assert_eq!(metadata["sha256"].as_str().unwrap().len(), 64);
        let summary = lines.last().unwrap();
        assert_eq!(summary["count_in"], summary["count_out"]);
        assert_eq!(summary["mismatched"], 0);

        let discovered = crate::discover::discover(&album.join("IMG_0001.jpg")).unwrap();
        assert!(discovered.files().iter().any(|file| {
            file.role == crate::discover::Role::Sidecar
                && file.path.file_name().unwrap() == "IMG_0001.jpg.json"
        }));
        let edited_asset = crate::discover::discover(&album.join("IMG_0001-edited.jpg")).unwrap();
        let edited_names: Vec<_> = edited_asset
            .files()
            .iter()
            .skip(1)
            .map(|file| file.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            edited_names,
            vec!["IMG_0001-edited.jpg.supplemental-metadata.json".to_string()]
        );
        let clipped = crate::discover::discover(&album.join(&long)).unwrap();
        assert_eq!(
            clipped.files()[1]
                .path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            long_json_name
        );
        let clip_asset = crate::discover::discover(&album.join("CLIP.mp4")).unwrap();
        assert!(clip_asset.files().iter().any(|file| {
            file.path.file_name().unwrap() == "CLIP.mp4.supplemental-metadata.json"
        }));
    }

    fn write_zip_owned(path: &Path, files: &[(String, Vec<u8>)]) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = zip_options(CompressionMethod::Stored);
        for (name, bytes) in files {
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
}
