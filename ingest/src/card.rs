// SPDX-License-Identifier: Apache-2.0

//! Import a mounted camera card (issue #28).
//!
//! The card is a source: it is opened read-only through [`SourceRoot`] and
//! never written, formatted, or emptied. Each original goes through
//! [`copy_verify`] with its discovered sidecars and pairs, so hashing,
//! read-back, mtime and durability are exactly those of the store in use.
//! A failed asset is rolled back by `copy_verify` and the import stops.
//!
//! This first slice refuses a filename that already exists in the library
//! (stores never overwrite). Dedupe by SHA-256 and renaming with a recorded
//! camera filename are follow-ups.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::discover::discover;
use crate::error::IngestError;
use crate::source::SourceRoot;
use crate::store::{copy_verify, BlobStore, StoredFile};

/// Result of a card import.
#[derive(Debug, Clone)]
pub struct CardReport {
    /// Assets copied and verified.
    pub assets: usize,
    /// Every stored file, in import order.
    pub files: Vec<StoredFile>,
}

const SIDECAR_EXTS: &[&str] = &["xmp", "aae", "json"];

fn is_sidecar(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SIDECAR_EXTS.iter().any(|s| ext.eq_ignore_ascii_case(s)))
}

/// Copy-verify every original on the mounted card at `card` into `store`.
///
/// Media files are visited before sidecars so a sidecar travels with its
/// original; a sidecar with no original is imported on its own. The card is
/// only read.
pub fn import_card<S: BlobStore + ?Sized>(
    card: &Path,
    store: &S,
) -> Result<CardReport, IngestError> {
    let root = SourceRoot::open(card)?;
    let mut relatives = Vec::new();
    root.walk(None, &mut |relative| {
        relatives.push(relative.to_string());
        true
    })?;
    // Stable: media first, then sidecars, each in walk order.
    relatives.sort_by_key(|relative| is_sidecar(relative));

    let base = root.canonical().to_path_buf();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut report = CardReport {
        assets: 0,
        files: Vec::new(),
    };
    for relative in relatives {
        let path = base.join(&relative);
        if seen.contains(&path) {
            continue;
        }
        let asset = discover(&path)?;
        if asset.files().iter().any(|file| seen.contains(&file.path)) {
            return Err(IngestError::store(
                "card",
                relative,
                "file already imported as part of another asset",
            ));
        }
        let stored = copy_verify(&asset, store)?;
        seen.extend(asset.files().iter().map(|file| file.path.clone()));
        report.assets += 1;
        report.files.extend(stored.files);
    }
    Ok(report)
}
