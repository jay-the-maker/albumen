// SPDX-License-Identifier: Apache-2.0

//! `albumen backup`: one repeatable run for a scheduler.
//!
//! 1. Incremental scan of the library into a state folder outside it
//!    (`scan.jsonl`, with `--previous` set to the last run's scan).
//! 2. Resumable export into DEST, writing `DEST/ALBUMEN-MANIFEST.txt`.
//! 3. Verify every item at DEST against what was exported, and check the
//!    export's hashes against the scan's for files the scan saw unchanged.
//! 4. Write a dated proof pair, `DEST/ALBUMEN-PROOFS/backup-<UTC>.md` and
//!    `.json`, new files only.
//!
//! Running it twice with no changes copies 0 files and writes a second proof
//! with the same counts and combined hash. A killed run is finished by the
//! next one: the scan resumes `scan.next.jsonl`, the export skips verified
//! files and clears partial copies. The library is only read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use albumen_ingest::{
    dest, ledger, scan_tree_with, tree_drift, DirectoryTree, IngestError, ScanOptions, SourceRoot,
};

use crate::proof;

/// Folder under DEST that holds backup proofs. Ignored by verify and status.
pub const PROOF_DIR: &str = "ALBUMEN-PROOFS";

pub struct BackupArgs<'a> {
    pub from: &'a Path,
    pub to: &'a str,
    pub state: Option<&'a Path>,
}

/// Where `scan.jsonl` is kept between runs: `--state`, or a `backup` folder
/// in this library's ledger directory (never inside or beside the library).
fn state_dir(from: &Path, state: Option<&Path>) -> PathBuf {
    match state {
        Some(path) => path.to_path_buf(),
        None => ledger::directory(from).join("backup"),
    }
}

/// `backup-20261029T104000Z`, plus `-2`, `-3` if a proof with that name exists.
fn proof_base(root: &Path, started: SystemTime) -> PathBuf {
    let stamp: String = proof::utc(started)
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    let dir = root.join(PROOF_DIR);
    let mut base = dir.join(format!("backup-{stamp}"));
    let mut n = 2;
    while base.with_extension("json").exists() || base.with_extension("md").exists() {
        base = dir.join(format!("backup-{stamp}-{n}"));
        n += 1;
    }
    base
}

fn scan_hashes(manifest: &Path) -> Result<HashMap<String, String>, IngestError> {
    let text = std::fs::read_to_string(manifest).map_err(|source| IngestError::Io {
        path: manifest.to_path_buf(),
        source,
    })?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("drift").is_some() {
            continue;
        }
        if let (Some(path), Some(sha)) = (value["path"].as_str(), value["sha256"].as_str()) {
            out.insert(path.to_string(), sha.to_string());
        }
    }
    Ok(out)
}

pub fn run(args: &BackupArgs<'_>) -> Result<(), IngestError> {
    if crate::is_remote(args.to) {
        return Err(IngestError::Usage {
            message: format!(
                "{}: backup writes to a local or mounted folder; use albumen export for S3 or rclone",
                args.to
            ),
        });
    }
    let started = SystemTime::now();
    let root = Path::new(args.to);
    let manifest = root.join(crate::status::RECORD_NAME);
    let source = SourceRoot::open(args.from)?;
    let state = state_dir(args.from, args.state);
    source.refuse_destination(&state)?;
    dest::create_dir_all(&state)?;

    // 1. Incremental scan. A killed run left scan.next.jsonl; scan resumes it.
    let previous = state.join("scan.jsonl");
    let next = state.join("scan.next.jsonl");
    let options = ScanOptions {
        previous: previous.is_file().then(|| previous.clone()),
        rehash_all: false,
    };
    let scanned = scan_tree_with(&source, &next, &options)?;
    dest::rename(&next, &previous)?;
    println!(
        "scan: {} files, {} hashed, {} unchanged",
        scanned.files, scanned.hashed, scanned.reused
    );

    // 2. Resumable export.
    let report = crate::export_apply(args.from, args.to, &manifest)?;
    println!(
        "export: {} copied, {} already there and verified",
        report.copied, report.already_present
    );

    // 3. Verify DEST, and cross-check against the scan.
    let tree = DirectoryTree::new(root);
    let ignore = vec![
        crate::status::RECORD_NAME.to_string(),
        format!("{PROOF_DIR}/"),
    ];
    let mut drift: Vec<(String, String, String)> = tree_drift(&report.entries, &tree, &ignore)?
        .into_iter()
        .map(|item| (item.kind.to_string(), item.path, item.detail))
        .collect();
    let scan = scan_hashes(&previous)?;
    for entry in &report.entries {
        if let Some(sha) = scan.get(&entry.relative_path) {
            if *sha != entry.sha256 {
                drift.push((
                    "changed-during-run".to_string(),
                    entry.relative_path.clone(),
                    "the original changed between scan and export".to_string(),
                ));
            }
        }
    }
    let finished = SystemTime::now();

    // 4. Proof.
    dest::create_dir_all(&root.join(PROOF_DIR))?;
    let files = report
        .entries
        .iter()
        .map(|entry| {
            (
                entry.relative_path.clone(),
                entry.sha256.clone(),
                entry.byte_len,
            )
        })
        .collect();
    let count = drift.len() as u64;
    let (md, json) = proof::write_proof(
        &proof_base(root, started),
        &proof::ProofInput {
            manifest: &manifest,
            against: args.to,
            started,
            finished,
            files,
            drift,
        },
    )?;
    println!("verify: {} items, {} problems", report.entries.len(), count);
    println!("proof: {}", md.display());
    println!("proof: {}", json.display());
    if count == 0 {
        println!("backup ok: {}", args.to);
        Ok(())
    } else {
        Err(IngestError::ManifestDrift { count })
    }
}
