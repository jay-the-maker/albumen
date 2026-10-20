// SPDX-License-Identifier: Apache-2.0

//! Export every member's originals and sidecars in one pass.
//!
//! Each member namespace is opened through [`FamilySource`], which is
//! read-only. Copies go to the destination through `copy_verify`. A
//! destination inside the library is refused. Provenance for a member is
//! that member's public key and the signed manifest, written only at the
//! destination.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::checksum::{sha256_hex, sha256_slice, to_hex};
use crate::error::IngestError;
use crate::export::{write_manifest, ManifestEntry};
use crate::family::manifest_json;
use crate::family_source::{family_object, family_sidecar, FamilySource};
use crate::local::copy_verify_reader;
use crate::source::SourceRoot;

/// Reconciliation of one family export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyExportReport {
    pub count_in: u64,
    pub count_out: u64,
    pub mismatched: u64,
    pub entries: Vec<ManifestEntry>,
}

/// What `family export` would copy. Nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyExportPlan {
    pub members: Vec<String>,
    pub files: u64,
}

/// List originals and sidecars. The library is not modified.
pub fn plan_family_export(library: &Path) -> Result<FamilyExportPlan, IngestError> {
    let source = FamilySource::open(library)?;
    let items = export_items(source.canonical())?;
    Ok(FamilyExportPlan {
        members: source.member_ids().map(str::to_string).collect(),
        files: items.len() as u64,
    })
}

/// Copy every member's originals and sidecars to `dest`, then write the manifest.
///
/// `count_in` is the number of original and sidecar files read. `count_out`
/// is the number whose destination SHA-256 matched. The command's caller
/// exits non-zero unless those counts are equal and non-zero.
pub fn export_family(
    library: &Path,
    dest: &Path,
    manifest: &Path,
) -> Result<FamilyExportReport, IngestError> {
    let source = FamilySource::open(library)?;
    source.refuse_destination(dest)?;
    source.refuse_destination(manifest)?;
    crate::family::refuse_shared_write_root(dest)?;
    let items = export_items(source.canonical())?;
    let manifest_value: Value = serde_json::from_str(&manifest_json(source.canonical())?)
        .map_err(|err| family(format!("manifest: {err}")))?;
    let mut by_member: BTreeMap<String, Vec<ExportItem>> = BTreeMap::new();
    for item in items {
        by_member.entry(item.member.clone()).or_default().push(item);
    }
    let mut entries = Vec::new();
    let mut count_in = 0u64;
    let mut count_out = 0u64;
    for member in source.member_ids() {
        let namespace = source.member(member)?;
        if let Some(list) = by_member.get(member) {
            for item in list {
                count_in += 1;
                let relative = item.relative();
                let dest_path = join_relative(dest, &relative);
                if let Some(byte_len) = already_exported(&dest_path, &item.expected_sha)? {
                    count_out += 1;
                    entries.push(ManifestEntry {
                        relative_path: relative,
                        sha256: item.expected_sha.clone(),
                        byte_len,
                    });
                    continue;
                }
                let verified = copy_verify_reader(
                    &mut namespace.open_read(&item.source)?,
                    &item.source,
                    &dest_path,
                    namespace
                        .metadata_nofollow(&item.source)?
                        .modified()
                        .map_err(|err| IngestError::io(&item.source, err))?,
                )?;
                if verified.sha256 != item.expected_sha {
                    return Err(family(format!(
                        "copied {} hashed {}; the signed record says {}",
                        item.filename, verified.sha256, item.expected_sha
                    )));
                }
                count_out += 1;
                entries.push(ManifestEntry {
                    relative_path: relative,
                    sha256: verified.sha256,
                    byte_len: verified.byte_len,
                });
            }
        }
        let provenance = provenance_bytes(&source, namespace, member, &manifest_value)?;
        let relative = provenance_relative(dest, member, &provenance)?;
        let path = join_relative(dest, &relative);
        if !path.exists() {
            write_bytes(&path, &provenance)?;
        }
        entries.push(ManifestEntry {
            relative_path: relative.clone(),
            sha256: sha256_hex(&path)?,
            byte_len: provenance.len() as u64,
        });
        // Earlier provenance kept by an update is still part of this copy.
        for earlier in earlier_provenance(dest, member)? {
            if earlier == relative {
                continue;
            }
            let path = join_relative(dest, &earlier);
            entries.push(ManifestEntry {
                relative_path: earlier,
                sha256: sha256_hex(&path)?,
                byte_len: std::fs::metadata(&path)
                    .map_err(|err| IngestError::io(&path, err))?
                    .len(),
            });
        }
    }
    if count_in == 0 || count_in != count_out {
        return Err(family(format!(
            "family export reconciliation failed: count_in {count_in} count_out {count_out} mismatched {}",
            count_in.saturating_sub(count_out)
        )));
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    write_manifest(manifest, &entries)?;
    Ok(FamilyExportReport {
        count_in,
        count_out,
        mismatched: 0,
        entries,
    })
}

struct ExportItem {
    member: String,
    /// Object digest. The export directory is named with this, for the original and its sidecar.
    object_sha: String,
    /// SHA-256 the copied bytes must match. A sidecar has its own digest.
    expected_sha: String,
    filename: String,
    source: PathBuf,
    sidecar: bool,
}

impl ExportItem {
    fn relative(&self) -> String {
        let name = if self.sidecar {
            format!("{}.sidecar", self.filename)
        } else {
            self.filename.clone()
        };
        format!("members/{}/{}/{}", self.member, self.object_sha, name)
    }
}

fn export_items(library: &Path) -> Result<Vec<ExportItem>, IngestError> {
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    let Some(objects) = value.get("objects").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    for object in objects {
        let member = object
            .get("member")
            .and_then(Value::as_str)
            .ok_or_else(|| family("manifest object is missing member"))?;
        let sha = object
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| family("manifest object is missing sha256"))?;
        let filename = object
            .get("filename")
            .and_then(Value::as_str)
            .ok_or_else(|| family("manifest object is missing filename"))?;
        if sha.len() != 64 {
            return Err(family(format!("sha256 {sha:?} is not a digest")));
        }
        items.push(ExportItem {
            member: member.to_string(),
            object_sha: sha.to_string(),
            expected_sha: sha.to_string(),
            filename: filename.to_string(),
            source: family_object(library, member, sha),
            sidecar: false,
        });
        if let Some(sidecar_sha) = object.get("sidecar_sha256").and_then(Value::as_str) {
            items.push(ExportItem {
                member: member.to_string(),
                object_sha: sha.to_string(),
                expected_sha: sidecar_sha.to_string(),
                filename: filename.to_string(),
                source: family_sidecar(library, member, sha),
                sidecar: true,
            });
        }
    }
    items.sort_by(|left, right| {
        (&left.member, &left.object_sha, left.sidecar).cmp(&(
            &right.member,
            &right.object_sha,
            right.sidecar,
        ))
    });
    Ok(items)
}

fn provenance_bytes(
    source: &FamilySource,
    namespace: &SourceRoot,
    member: &str,
    manifest: &Value,
) -> Result<Vec<u8>, IngestError> {
    let mut records = Vec::new();
    let devices = namespace.canonical().join("devices");
    if namespace
        .metadata_nofollow(&devices)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        let read = namespace.read_dir(&devices)?;
        let mut device_names = Vec::new();
        for entry in read {
            let entry = entry.map_err(|err| IngestError::io(&devices, err))?;
            let path = entry.path();
            if namespace.metadata_nofollow(&path)?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    device_names.push(name.to_string());
                }
            }
        }
        device_names.sort();
        for device in device_names {
            let log = devices.join(&device).join("log");
            if !namespace
                .metadata_nofollow(&log)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let read = namespace.read_dir(&log)?;
            let mut files = Vec::new();
            for entry in read {
                let entry = entry.map_err(|err| IngestError::io(&log, err))?;
                let path = entry.path();
                if !namespace.metadata_nofollow(&path)?.is_file() {
                    continue;
                }
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    continue;
                };
                if name.ends_with(".json") {
                    files.push((name.to_string(), path));
                }
            }
            files.sort_by(|left, right| left.0.cmp(&right.0));
            for (name, path) in files {
                let bytes = source.read_member(member, &path)?;
                let record: Value = serde_json::from_slice(&bytes).map_err(|err| {
                    family(format!(
                        "signed record {} is not json: {err}",
                        path.display()
                    ))
                })?;
                records.push(json!({
                    "relative": format!("devices/{device}/log/{name}"),
                    "sha256": to_hex(&sha256_slice(&bytes)),
                    "record": record,
                }));
            }
        }
    }
    let body = json!({
        "albumen_family": 1,
        "kind": "provenance",
        "member": member,
        "signed_manifest": manifest,
        "signed_records": records,
    });
    serde_json::to_vec_pretty(&body)
        .map_err(|err| family(format!("could not encode provenance: {err}")))
}

/// An earlier export already holds this file with the expected digest.
///
/// Returns its length so the update can skip it. A file with other bytes is
/// refused: an update never overwrites, and never removes, a destination
/// file.
fn already_exported(path: &Path, expected_sha: &str) -> Result<Option<u64>, IngestError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(IngestError::io(path, err)),
    };
    if !meta.is_file() {
        return Err(family(format!(
            "refusing to overwrite {}: it is not a regular file",
            path.display()
        )));
    }
    let actual = sha256_hex(path)?;
    if actual != expected_sha {
        return Err(family(format!(
            "refusing to overwrite existing file {} (sha256 {actual}, expected {expected_sha})",
            path.display()
        )));
    }
    Ok(Some(meta.len()))
}

/// Where this member's provenance goes.
///
/// The first export writes `members/<member>/provenance.json`. When an
/// update finds different provenance there (a hide, an omit, a revoke), the
/// new state is added beside it as `provenance-<sha256>.json`. Earlier
/// provenance is kept, so hides and revokes reach the copy as added state,
/// never as an overwrite or a deletion.
fn provenance_relative(dest: &Path, member: &str, bytes: &[u8]) -> Result<String, IngestError> {
    let first = format!("members/{member}/provenance.json");
    let path = join_relative(dest, &first);
    if !path.exists() {
        return Ok(first);
    }
    let digest = to_hex(&sha256_slice(bytes));
    if sha256_hex(&path)? == digest {
        return Ok(first);
    }
    Ok(format!("members/{member}/provenance-{digest}.json"))
}

/// Provenance files an earlier export left for `member` in `dest`.
fn earlier_provenance(dest: &Path, member: &str) -> Result<Vec<String>, IngestError> {
    let dir = join_relative(dest, &format!("members/{member}"));
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(IngestError::io(&dir, err)),
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|err| IngestError::io(&dir, err))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let is_provenance = name == "provenance.json"
            || (name.starts_with("provenance-") && name.ends_with(".json"));
        if is_provenance && entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            out.push(format!("members/{member}/{name}"));
        }
    }
    out.sort();
    Ok(out)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::dest::create_dir_all(parent)?;
        }
    }
    let mut file = crate::dest::open_new(path)?;
    std::io::Write::write_all(&mut file, bytes).map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    drop(file);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::dest::sync_dir(parent)?;
        }
    }
    Ok(())
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

fn family(message: impl Into<String>) -> IngestError {
    IngestError::Family {
        message: message.into(),
    }
}
