// SPDX-License-Identifier: Apache-2.0

//! Import a mounted camera card (issue #28).
//!
//! The card is a source: it is opened read-only through [`SourceRoot`] and
//! never written, formatted, or emptied. Each original goes with its
//! discovered sidecars and pairs. Bytes are copied by the existing
//! [`copy_verify`] / [`copy_verify_file`] path, so the single-read hash,
//! mtime, durability sync, and read-back hash are those of the store.
//!
//! [`import_card`] works with any [`BlobStore`] and refuses a name the store
//! already has (stores never overwrite). [`import_card_local`] targets a
//! library directory: an asset already there with identical SHA-256 is a
//! dedupe, and an asset whose camera name is taken by different bytes is
//! stored under `{stem}~{sha8}{ext}` names, with the camera filenames
//! recorded in `{renamed primary}.albumen-card.json`.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::checksum::sha256_hex;
use crate::dest;
use crate::discover::{discover, Asset};
use crate::error::IngestError;
use crate::local::copy_verify_file;
use crate::source::SourceRoot;
use crate::store::{copy_verify, BlobStore, StoredFile};

/// Suffix of the record written beside a renamed primary.
pub const CARD_RECORD_SUFFIX: &str = ".albumen-card.json";

/// Staging directory inside the library for renamed copies.
const STAGING_DIR: &str = ".albumen-card-staging";

/// A card file stored under a different library name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    pub camera_filename: String,
    pub library_filename: String,
}

/// Result of a card import.
#[derive(Debug, Clone, Default)]
pub struct CardReport {
    /// Assets copied and verified.
    pub assets: usize,
    /// Every stored file, in import order.
    pub files: Vec<StoredFile>,
    /// Assets skipped because the library already held identical bytes.
    pub deduped: usize,
    /// Files stored under a new name because the camera name was taken.
    pub renamed: Vec<Renamed>,
}

const SIDECAR_EXTS: &[&str] = &["xmp", "aae", "json"];

fn is_sidecar(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SIDECAR_EXTS.iter().any(|s| ext.eq_ignore_ascii_case(s)))
}

/// Discover every asset on the card, media before sidecars, each file once.
fn card_assets(card: &Path) -> Result<Vec<Asset>, IngestError> {
    let root = SourceRoot::open(card)?;
    let mut relatives = Vec::new();
    root.walk(None, &mut |relative| {
        relatives.push(relative.to_string());
        true
    })?;
    relatives.sort_by_key(|relative| is_sidecar(relative));
    let base = root.canonical().to_path_buf();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut assets = Vec::new();
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
        assets.push(asset);
    }
    Ok(assets)
}

/// Count the files an import of `card` would read. Reads only.
pub fn plan_card(card: &Path) -> Result<(usize, usize), IngestError> {
    let assets = card_assets(card)?;
    let files = assets.iter().map(|asset| asset.files().len()).sum();
    Ok((assets.len(), files))
}

/// Copy-verify every original on the mounted card at `card` into `store`.
///
/// A sidecar with no original is imported on its own. The card is only
/// read. The first failure stops the import; that asset is rolled back.
pub fn import_card<S: BlobStore + ?Sized>(
    card: &Path,
    store: &S,
) -> Result<CardReport, IngestError> {
    let mut report = CardReport::default();
    for asset in card_assets(card)? {
        let stored = copy_verify(&asset, store)?;
        report.assets += 1;
        report.files.extend(stored.files);
    }
    Ok(report)
}

/// [`import_card`] into a local library directory, with dedupe and rename.
///
/// For each asset: if every file is already in `library` with the same
/// SHA-256 (under the camera names or the renamed names), it is skipped.
/// If every camera name is free, the asset is copied under those names.
/// Otherwise the whole asset is copied under `{stem}~{sha8}{ext}` names, so
/// pairs and sidecars still share a stem, and a record maps each library
/// name back to its camera filename. Nothing in the library is overwritten.
pub fn import_card_local(card: &Path, library: &Path) -> Result<CardReport, IngestError> {
    SourceRoot::open(card)?.refuse_destination(library)?;
    let mut report = CardReport::default();
    for asset in card_assets(card)? {
        let names: Vec<String> = asset
            .files()
            .iter()
            .map(|file| file_name(&file.path))
            .collect::<Result<_, _>>()?;
        let shas: Vec<String> = asset
            .files()
            .iter()
            .map(|file| sha256_hex(&file.path))
            .collect::<Result<_, _>>()?;

        let camera = states(library, &names, &shas)?;
        if camera.iter().all(|state| *state == State::Same) {
            report.deduped += 1;
            continue;
        }
        if camera.iter().all(|state| *state == State::Free) {
            let stored = copy_verify(&asset, &crate::local::LocalStore::new(library))?;
            report.assets += 1;
            report.files.extend(stored.files);
            continue;
        }

        let suffix = format!("~{}", &shas[0][..8]);
        let renamed: Vec<String> = names
            .iter()
            .map(|name| with_suffix(name, &suffix))
            .collect();
        let target = states(library, &renamed, &shas)?;
        if target.iter().all(|state| *state == State::Same) {
            report.deduped += 1;
            continue;
        }
        if let Some(index) = target.iter().position(|state| *state != State::Free) {
            return Err(IngestError::store(
                "card",
                renamed[index].clone(),
                "renamed target already exists with other bytes; not overwritten",
            ));
        }
        let stored = copy_renamed(&asset, library, &renamed)?;
        write_record(library, &renamed[0], &names, &renamed, &shas)?;
        for (camera_filename, library_filename) in names.iter().zip(&renamed) {
            report.renamed.push(Renamed {
                camera_filename: camera_filename.clone(),
                library_filename: library_filename.clone(),
            });
        }
        report.assets += 1;
        report.files.extend(stored);
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Same,
    Taken,
}

fn states(library: &Path, names: &[String], shas: &[String]) -> Result<Vec<State>, IngestError> {
    names
        .iter()
        .zip(shas)
        .map(|(name, sha)| {
            let path = library.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(_) if sha256_hex(&path)? == *sha => Ok(State::Same),
                Ok(_) => Ok(State::Taken),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(State::Free),
                Err(err) => Err(IngestError::io(&path, err)),
            }
        })
        .collect()
}

/// `IMG_0004.jpg` -> `IMG_0004~ab12cd34.jpg`; `IMG_0004.jpg.xmp` ->
/// `IMG_0004~ab12cd34.jpg.xmp`. The suffix goes after the first stem so a
/// renamed asset still pairs by stem.
fn with_suffix(name: &str, suffix: &str) -> String {
    match name.find('.') {
        Some(dot) if dot > 0 => format!("{}{suffix}{}", &name[..dot], &name[dot..]),
        _ => format!("{name}{suffix}"),
    }
}

/// Copy each file with `copy_verify_file` into a staging directory inside
/// the library, then hard-link it to its new name (which fails rather than
/// replace an existing file) and drop the staging name. The bytes, mtime
/// and verification are those of `copy_verify_file`; the link only adds a
/// name. A failure removes every file this asset added.
fn copy_renamed(
    asset: &Asset,
    library: &Path,
    renamed: &[String],
) -> Result<Vec<StoredFile>, IngestError> {
    let staging = library.join(STAGING_DIR);
    dest::create_dir_all(&staging)?;
    let mut done: Vec<PathBuf> = Vec::new();
    let mut stored = Vec::new();
    let result = (|| {
        for (file, new_name) in asset.files().iter().zip(renamed) {
            let object = copy_verify_file(&file.path, &staging)?;
            let staged = staging.join(&object.filename);
            let target = library.join(new_name);
            let linked = std::fs::hard_link(&staged, &target);
            dest::remove_file(&staged)?;
            linked.map_err(|err| IngestError::io(&target, err))?;
            done.push(target.clone());
            dest::sync_dir(library)?;
            let read_back = sha256_hex(&target)?;
            if read_back != object.sha256 {
                return Err(IngestError::ChecksumMismatch {
                    filename: new_name.clone(),
                    expected_sha256: object.sha256,
                    actual_sha256: read_back,
                    destination: target.display().to_string(),
                    removed: false,
                    message: "renamed copy re-read".to_string(),
                });
            }
            stored.push(StoredFile {
                filename: new_name.clone(),
                sha256: object.sha256,
                byte_len: object.byte_len,
                mtime: object.mtime,
                role: file.role,
            });
        }
        Ok(())
    })();
    dest::remove_empty_dir_best_effort(&staging);
    if let Err(err) = result {
        for path in done.iter().rev() {
            dest::remove_file_best_effort(path);
        }
        let _ = dest::sync_dir(library);
        return Err(err);
    }
    Ok(stored)
}

fn write_record(
    library: &Path,
    primary: &str,
    names: &[String],
    renamed: &[String],
    shas: &[String],
) -> Result<(), IngestError> {
    let files: Vec<serde_json::Value> = names
        .iter()
        .zip(renamed)
        .zip(shas)
        .map(|((camera, library_name), sha)| {
            serde_json::json!({
                "camera_filename": camera,
                "library_filename": library_name,
                "sha256": sha,
            })
        })
        .collect();
    let body = serde_json::json!({ "albumen_card_import": 1, "files": files });
    let path = library.join(format!("{primary}{CARD_RECORD_SUFFIX}"));
    let mut bytes = serde_json::to_vec_pretty(&body)
        .map_err(|err| IngestError::store("card", primary.to_string(), err))?;
    bytes.push(b'\n');
    let mut file = dest::open_new(&path)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| IngestError::io(&path, err))?;
    drop(file);
    dest::sync_dir(library)
}

fn file_name(path: &Path) -> Result<String, IngestError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| IngestError::NonUtf8Filename {
            path: path.to_path_buf(),
        })
}

#[cfg(test)]
mod tests {
    use super::with_suffix;

    #[test]
    fn suffix_goes_after_the_first_stem() {
        assert_eq!(with_suffix("IMG_0004.jpg", "~ab"), "IMG_0004~ab.jpg");
        assert_eq!(
            with_suffix("IMG_0004.jpg.xmp", "~ab"),
            "IMG_0004~ab.jpg.xmp"
        );
        assert_eq!(with_suffix("README", "~ab"), "README~ab");
        assert_eq!(with_suffix(".hidden", "~ab"), ".hidden~ab");
    }
}
