// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

/// Failure while copying or verifying an original.
///
/// Checksum failures name both hashes. When Albumen created the bad
/// destination, it is removed so a corrupt copy is not kept.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{message}")]
    ChecksumMismatch {
        filename: String,
        expected_sha256: String,
        actual_sha256: String,
        destination: String,
        removed: bool,
        message: String,
    },

    #[error(
        "mtime was not preserved for {filename}: expected unix nanos {expected_unix_nanos}, got {actual_unix_nanos}; destination {destination} removed={removed}"
    )]
    MtimeNotPreserved {
        filename: String,
        expected_unix_nanos: String,
        actual_unix_nanos: String,
        destination: String,
        removed: bool,
    },

    #[error("refusing to overwrite existing file {filename} at {destination}")]
    AlreadyExists {
        filename: String,
        destination: String,
    },

    #[error(
        "filename was not preserved: source name is {source_name:?}, requested name is {requested:?}"
    )]
    FilenameMismatch {
        source_name: String,
        requested: String,
    },

    #[error("unsafe filename {filename:?}; names must be a single path component")]
    UnsafeFilename { filename: String },

    #[error("filename is not valid UTF-8: {path}")]
    NonUtf8Filename { path: PathBuf },

    #[error("not a file: {0}")]
    NotAFile(PathBuf),

    #[error("asset is empty")]
    EmptyAsset,

    #[error("asset contains duplicate filename {0}")]
    DuplicateFilename(String),

    #[error("asset is missing a primary file")]
    MissingPrimary,

    #[error(
        "S3 object {key} is missing {field} metadata; refusing to pretend the original was preserved"
    )]
    MissingObjectMetadata { key: String, field: String },

    #[error("storage {operation} failed for {filename}: {message}")]
    Store {
        operation: &'static str,
        filename: String,
        message: String,
    },

    #[error("refusing to follow symlink {0}")]
    Symlink(PathBuf),

    #[error(
        "manifest mismatch for {relative_path}: expected sha256 {expected_sha256} ({expected_len} bytes), got {actual_sha256} ({actual_len} bytes)"
    )]
    ManifestMismatch {
        relative_path: String,
        expected_sha256: String,
        actual_sha256: String,
        expected_len: u64,
        actual_len: u64,
    },

    #[error("manifest entry missing at destination: {0}")]
    ManifestMissing(String),

    #[error("destination has {0}, which is not in the manifest")]
    ManifestExtra(String),

    #[error("could not parse manifest line {line_number}: {line}")]
    ManifestParse { line_number: usize, line: String },

    #[error(
        "refusing to write the manifest inside the source tree: {manifest} is inside {source_tree}"
    )]
    ManifestInsideSource {
        manifest: String,
        source_tree: String,
    },

    #[error("scan manifest drift: {count} difference(s)")]
    ManifestDrift { count: u64 },

    #[error(
        "takeout reconciliation failed: count_in {count_in} count_out {count_out} mismatched {mismatched} archives_unchanged {archives_unchanged}; report {report}"
    )]
    TakeoutReconciliation {
        count_in: u64,
        count_out: u64,
        mismatched: u64,
        archives_unchanged: bool,
        report: String,
    },
}

impl IngestError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn checksum_mismatch(
        filename: impl Into<String>,
        expected_sha256: impl Into<String>,
        actual_sha256: impl Into<String>,
        destination: impl Into<String>,
        removed: bool,
    ) -> Self {
        let filename = filename.into();
        let expected_sha256 = expected_sha256.into();
        let actual_sha256 = actual_sha256.into();
        let destination = destination.into();
        let message = if removed {
            format!(
                "checksum mismatch for {filename}: expected sha256 {expected_sha256}, got {actual_sha256}; removed destination {destination} so a corrupt copy was not kept"
            )
        } else {
            format!(
                "checksum mismatch for {filename}: expected sha256 {expected_sha256}, got {actual_sha256}; FAILED to remove corrupt destination {destination}"
            )
        };
        Self::ChecksumMismatch {
            filename,
            expected_sha256,
            actual_sha256,
            destination,
            removed,
            message,
        }
    }

    pub(crate) fn store(
        operation: &'static str,
        filename: impl Into<String>,
        err: impl std::fmt::Display,
    ) -> Self {
        Self::Store {
            operation,
            filename: filename.into(),
            message: err.to_string(),
        }
    }
}
