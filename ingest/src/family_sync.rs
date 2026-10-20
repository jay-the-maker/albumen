// SPDX-License-Identifier: Apache-2.0

//! `albumen family sync`: bring one live family library up to date with
//! another by adding the files it lacks. Additive only.
//!
//! The source library is opened read-only and is never written. Every
//! file is checked before anything is written: a file both copies hold
//! must have the same bytes, or the whole sync is refused. Nothing in the
//! destination is ever replaced or removed. New files go through
//! `copy_verify` (fsync, device read-back, SHA-256). Hides, tombstones and
//! revokes travel as the signed log files they already are, and both
//! libraries' signed records must verify before and after the merge.
//! Copy-local receiver tombstones (`*.albumen-tombstone`) are not carried.

use std::path::{Path, PathBuf};

use crate::export::is_copy_local_tombstone;
use crate::family::manifest_json;
use crate::local::copy_verify_reader;
use crate::source::SourceRoot;
use crate::{sha256_hex, IngestError};

/// What a sync would do.
#[derive(Debug, Clone, Default)]
pub struct FamilySyncPlan {
    /// Relative paths the destination lacks, in walk order.
    pub add: Vec<String>,
    /// Files both copies already hold with the same bytes.
    pub same: u64,
}

/// What a sync did.
#[derive(Debug, Clone, Default)]
pub struct FamilySyncReport {
    pub added: Vec<String>,
    pub same: u64,
}

fn family(message: impl Into<String>) -> IngestError {
    IngestError::Family {
        message: message.into(),
    }
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

fn dest_is_empty(to: &Path) -> Result<bool, IngestError> {
    match std::fs::read_dir(to) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => Err(IngestError::io(to, err)),
    }
}

/// Check both libraries and list what `to` lacks. Writes nothing.
pub fn plan_family_sync(from: &Path, to: &Path) -> Result<FamilySyncPlan, IngestError> {
    let source = SourceRoot::open(from)?;
    source.refuse_destination(to)?;
    if to.exists() {
        SourceRoot::open(to)?.refuse_destination(from)?;
    }
    // Both copies' signed records must verify before anything is merged.
    manifest_json(source.canonical())
        .map_err(|err| family(format!("source library does not verify: {err}")))?;
    if !dest_is_empty(to)? {
        manifest_json(to)
            .map_err(|err| family(format!("destination library does not verify: {err}")))?;
    }
    let mut relatives = Vec::new();
    source.walk(None, &mut |relative| {
        relatives.push(relative.to_string());
        true
    })?;
    let mut plan = FamilySyncPlan::default();
    for relative in relatives {
        if is_copy_local_tombstone(&relative) {
            continue;
        }
        let dest_path = join_relative(to, &relative);
        match std::fs::symlink_metadata(&dest_path) {
            Ok(meta) => {
                if !meta.is_file() {
                    return Err(family(format!(
                        "refusing to sync: {} exists in the destination and is not a regular file",
                        dest_path.display()
                    )));
                }
                let theirs = sha256_hex(&dest_path)?;
                let ours = sha256_hex(&join_relative(source.canonical(), &relative))?;
                if theirs != ours {
                    return Err(family(format!(
                        "refusing to sync: {relative} has different bytes in the two libraries (source {ours}, destination {theirs}); nothing was written"
                    )));
                }
                plan.same += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => plan.add.push(relative),
            Err(err) => return Err(IngestError::io(&dest_path, err)),
        }
    }
    Ok(plan)
}

/// Add every file `to` lacks from `from`, verified. Refuses before writing
/// when any shared file differs. The merged manifest of `to` must verify
/// afterwards.
pub fn sync_family(from: &Path, to: &Path) -> Result<FamilySyncReport, IngestError> {
    let plan = plan_family_sync(from, to)?;
    let source = SourceRoot::open(from)?;
    let mut report = FamilySyncReport {
        added: Vec::new(),
        same: plan.same,
    };
    for relative in plan.add {
        let source_path = join_relative(source.canonical(), &relative);
        let dest_path = join_relative(to, &relative);
        let mtime = source
            .metadata_nofollow(&source_path)?
            .modified()
            .map_err(|err| IngestError::io(&source_path, err))?;
        let verified = copy_verify_reader(
            &mut source.open_read(&source_path)?,
            &source_path,
            &dest_path,
            mtime,
        )?;
        let expected = sha256_hex(&source_path)?;
        if verified.sha256 != expected {
            return Err(family(format!(
                "{relative} changed in the source during sync (copied {}, now {expected})",
                verified.sha256
            )));
        }
        report.added.push(relative);
    }
    manifest_json(to).map_err(|err| family(format!("merged library does not verify: {err}")))?;
    Ok(report)
}
