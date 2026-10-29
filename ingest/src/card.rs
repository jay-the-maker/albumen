// SPDX-License-Identifier: Apache-2.0

//! Import a mounted camera card (issue #28).
//!
//! The card is a source: it is opened read-only through [`SourceRoot`] and
//! never written, formatted, or emptied. Each original goes through
//! [`copy_verify`] with its discovered sidecars and pairs, so hashing,
//! read-back, mtime and durability are exactly those of the store in use.
//! A failed asset is rolled back by `copy_verify` and the import stops.
//!
//! [`import_card`] refuses a filename that already exists in the store
//! (stores never overwrite). [`import_card_local`] also skips an asset whose
//! files are already in a local library with identical SHA-256 (dedupe) and
//! refuses one whose name is taken by different bytes. Renaming with a
//! recorded camera filename is a follow-up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::checksum::sha256_hex;
use crate::discover::discover;
use crate::error::IngestError;
use crate::local::LocalStore;
use crate::source::SourceRoot;
use crate::store::{copy_verify, BlobStore, StoredFile};

/// Result of a card import.
#[derive(Debug, Clone)]
pub struct CardReport {
    /// Assets copied and verified.
    pub assets: usize,
    /// Every stored file, in import order.
    pub files: Vec<StoredFile>,
    /// Assets skipped because the library already held identical bytes.
    pub deduped: usize,
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
    import_card_with(card, store, &|_| Ok(None))
}

/// [`import_card`] into a local library directory, with SHA-256 dedupe.
///
/// An asset whose every file already exists in `library` with the same
/// SHA-256 is skipped. A file whose name exists with different bytes stops
/// the import before anything of that asset is copied; the library file is
/// never overwritten.
pub fn import_card_local(card: &Path, library: &Path) -> Result<CardReport, IngestError> {
    let store = LocalStore::new(library);
    import_card_with(card, &store, &|filename| {
        let path = library.join(filename);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => sha256_hex(&path).map(Some),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(IngestError::io(&path, err)),
        }
    })
}

type Existing<'a> = dyn Fn(&str) -> Result<Option<String>, IngestError> + 'a;

fn import_card_with<S: BlobStore + ?Sized>(
    card: &Path,
    store: &S,
    existing: &Existing<'_>,
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
        deduped: 0,
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
        seen.extend(asset.files().iter().map(|file| file.path.clone()));
        let mut present = 0;
        for file in asset.files() {
            let name = file_name(&file.path)?;
            if let Some(library_sha) = existing(&name)? {
                let card_sha = sha256_hex(&file.path)?;
                if card_sha != library_sha {
                    return Err(IngestError::store(
                        "card",
                        name,
                        format!(
                            "library already has different bytes under this name \
                             (card {card_sha}, library {library_sha}); not overwritten"
                        ),
                    ));
                }
                present += 1;
            }
        }
        if present == asset.files().len() {
            report.deduped += 1;
            continue;
        }
        let stored = copy_verify(&asset, store)?;
        report.assets += 1;
        report.files.extend(stored.files);
    }
    Ok(report)
}

fn file_name(path: &Path) -> Result<String, IngestError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| IngestError::NonUtf8Filename {
            path: path.to_path_buf(),
        })
}
