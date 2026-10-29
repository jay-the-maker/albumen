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
        "case-insensitive name collision for {filename}: destination already has {existing} at {destination}"
    )]
    CaseCollision {
        filename: String,
        existing: String,
        destination: String,
    },

    #[error("alternate data stream {name} on {filename}: {message}")]
    AlternateStream {
        filename: String,
        name: String,
        message: String,
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

    #[error("refusing to write {destination} inside source {source_tree}; sources are read-only")]
    DestinationInsideSource {
        destination: String,
        source_tree: String,
    },

    #[error("sources are read-only; copy_verify_quarantine cannot target {path}")]
    SourceReadOnly { path: String },

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

    #[error(
        "icloud reconciliation failed: count_in {count_in} count_out {count_out} mismatched {mismatched} sources_unchanged {sources_unchanged}; report {report}"
    )]
    IcloudReconciliation {
        count_in: u64,
        count_out: u64,
        mismatched: u64,
        sources_unchanged: bool,
        report: String,
    },

    /// A family-library rule refused the operation. No other member's bytes were changed.
    #[error("{message}")]
    Family { message: String },

    /// The arguments cannot be used as given. Nothing was read or written.
    #[error("{message}")]
    Usage { message: String },

    /// The layout is unsafe, so the command refused before writing.
    #[error("{message}")]
    Refused { message: String },
}

/// Process exit code for a verification mismatch or drift.
pub const EXIT_MISMATCH: u8 = 1;
/// Process exit code for unusable arguments. clap uses the same code.
pub const EXIT_USAGE: u8 = 2;
/// Process exit code for a refused unsafe layout.
pub const EXIT_REFUSED: u8 = 3;
/// Process exit code for a read, write, or storage failure.
pub const EXIT_IO: u8 = 4;

impl IngestError {
    /// The process exit code for this error.
    ///
    /// 1 is a verification mismatch or drift, 2 is usage, 3 is a refused
    /// unsafe layout, and 4 is I/O. The match has no wildcard arm, so a new
    /// variant must pick a code.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ChecksumMismatch { .. }
            | Self::MtimeNotPreserved { .. }
            | Self::FilenameMismatch { .. }
            | Self::MissingObjectMetadata { .. }
            | Self::ManifestMismatch { .. }
            | Self::ManifestMissing(_)
            | Self::ManifestExtra(_)
            | Self::ManifestDrift { .. }
            | Self::TakeoutReconciliation { .. }
            | Self::IcloudReconciliation { .. } => EXIT_MISMATCH,
            Self::NotAFile(_)
            | Self::EmptyAsset
            | Self::DuplicateFilename(_)
            | Self::MissingPrimary
            | Self::ManifestParse { .. }
            | Self::Usage { .. } => EXIT_USAGE,
            Self::AlreadyExists { .. }
            | Self::CaseCollision { .. }
            | Self::AlternateStream { .. }
            | Self::UnsafeFilename { .. }
            | Self::NonUtf8Filename { .. }
            | Self::Symlink(_)
            | Self::DestinationInsideSource { .. }
            | Self::SourceReadOnly { .. }
            | Self::Family { .. }
            | Self::Refused { .. } => EXIT_REFUSED,
            Self::Io { .. } | Self::Store { .. } => EXIT_IO,
        }
    }

    /// What to do next. The CLI prints it as the last line, after `next: `.
    ///
    /// The match has no wildcard arm, so a new variant must say what to do.
    pub fn next_step(&self) -> String {
        match self {
            Self::Io { path, .. } => format!(
                "check that {} exists and is readable, and that the disk has space, then run the command again",
                path.display()
            ),
            Self::ChecksumMismatch { filename, .. } => format!(
                "the source was not changed; check the disk or network holding {filename}, then run the command again"
            ),
            Self::MtimeNotPreserved { .. } => {
                "use a destination file system that keeps modification times, then run the command again".to_string()
            }
            Self::AlreadyExists { destination, .. } => format!(
                "choose a new destination, or move {destination} aside yourself; Albumen does not overwrite"
            ),
            Self::CaseCollision { destination, .. } => format!(
                "rename one of the two files in a copy of the source, or export to a case-sensitive file system instead of {destination}"
            ),
            Self::AlternateStream { filename, .. } => format!(
                "copy {filename} with its alternate stream removed, or export to NTFS"
            ),
            Self::FilenameMismatch { .. } => {
                "pass the file's own name; Albumen keeps original file names".to_string()
            }
            Self::UnsafeFilename { .. } | Self::NonUtf8Filename { .. } => {
                "rename the file in a copy of the source to a single UTF-8 path component, then run the command again".to_string()
            }
            Self::NotAFile(path) => format!("pass a path that exists; {} is not usable here", path.display()),
            Self::EmptyAsset | Self::MissingPrimary | Self::DuplicateFilename(_) => {
                "pass one photo or video, with any sidecars beside it".to_string()
            }
            Self::MissingObjectMetadata { key, .. } => format!(
                "re-export {key} with this version of Albumen so its metadata is written"
            ),
            Self::Store { operation, .. } => format!(
                "check the storage credentials and network for {operation}, then run the command again"
            ),
            Self::Symlink(path) => format!(
                "pass the real path instead of the symlink {}",
                path.display()
            ),
            Self::ManifestMismatch { relative_path, .. } => format!(
                "re-export {relative_path} from the library, then run verify-manifest again"
            ),
            Self::ManifestMissing(path) => format!(
                "re-export so {path} is written, then run verify-manifest again"
            ),
            Self::ManifestExtra(path) => format!(
                "move {path} out of the export or re-export into an empty directory, then run verify-manifest again"
            ),
            Self::ManifestParse { .. } => {
                "pass the manifest file Albumen wrote, unedited".to_string()
            }
            Self::DestinationInsideSource { .. } | Self::SourceReadOnly { .. } => {
                "pick a destination outside the source; sources are never written".to_string()
            }
            Self::ManifestDrift { .. } => {
                "read the drift lines above, re-export the changed files, then run verify-manifest again".to_string()
            }
            Self::TakeoutReconciliation { report, .. } => format!(
                "read the mismatch lines in {report}, then run import-takeout again; files already copied are kept"
            ),
            Self::IcloudReconciliation { report, .. } => format!(
                "read the mismatch lines in {report}, then run import-icloud again; files already copied are kept"
            ),
            Self::Family { .. } => {
                "check the member and key you passed; no other member's files were changed".to_string()
            }
            Self::Usage { .. } => "run the command with --help to see an example".to_string(),
            Self::Refused { .. } => {
                "pick a path that does not overlap the library or source, then run the command again".to_string()
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn every_variant() -> Vec<IngestError> {
        let text = || String::from("x");
        vec![
            IngestError::io("p", std::io::Error::other("x")),
            IngestError::checksum_mismatch("f", "a", "b", "d", true),
            IngestError::MtimeNotPreserved {
                filename: text(),
                expected_unix_nanos: text(),
                actual_unix_nanos: text(),
                destination: text(),
                removed: true,
            },
            IngestError::AlreadyExists {
                filename: text(),
                destination: text(),
            },
            IngestError::CaseCollision {
                filename: text(),
                existing: text(),
                destination: text(),
            },
            IngestError::AlternateStream {
                filename: text(),
                name: text(),
                message: text(),
            },
            IngestError::FilenameMismatch {
                source_name: text(),
                requested: text(),
            },
            IngestError::UnsafeFilename { filename: text() },
            IngestError::NonUtf8Filename { path: "p".into() },
            IngestError::NotAFile("p".into()),
            IngestError::EmptyAsset,
            IngestError::DuplicateFilename(text()),
            IngestError::MissingPrimary,
            IngestError::MissingObjectMetadata {
                key: text(),
                field: text(),
            },
            IngestError::store("op", "f", "m"),
            IngestError::Symlink("p".into()),
            IngestError::ManifestMismatch {
                relative_path: text(),
                expected_sha256: text(),
                actual_sha256: text(),
                expected_len: 1,
                actual_len: 2,
            },
            IngestError::ManifestMissing(text()),
            IngestError::ManifestExtra(text()),
            IngestError::ManifestParse {
                line_number: 1,
                line: text(),
            },
            IngestError::DestinationInsideSource {
                destination: text(),
                source_tree: text(),
            },
            IngestError::SourceReadOnly { path: text() },
            IngestError::ManifestDrift { count: 1 },
            IngestError::TakeoutReconciliation {
                count_in: 1,
                count_out: 0,
                mismatched: 0,
                archives_unchanged: true,
                report: text(),
            },
            IngestError::IcloudReconciliation {
                count_in: 1,
                count_out: 0,
                mismatched: 0,
                sources_unchanged: true,
                report: text(),
            },
            IngestError::Family { message: text() },
            IngestError::Usage { message: text() },
            IngestError::Refused { message: text() },
        ]
    }

    /// Variant names declared in this file's `IngestError` enum.
    fn declared_variants() -> usize {
        let source = include_str!("error.rs");
        let start = source.find("pub enum IngestError {").unwrap();
        let body = &source[start..];
        let end = body.find("\n}\n").unwrap();
        body[..end]
            .lines()
            .filter(|line| {
                let rest = line.strip_prefix("    ").unwrap_or("");
                rest.starts_with(|c: char| c.is_ascii_uppercase())
            })
            .count()
    }

    #[test]
    fn every_variant_has_a_next_line_and_a_documented_code() {
        let all = every_variant();
        let kinds: HashSet<_> = all.iter().map(std::mem::discriminant).collect();
        assert_eq!(kinds.len(), all.len(), "a variant is listed twice");
        assert_eq!(
            all.len(),
            declared_variants(),
            "every IngestError variant must be listed here with a next step"
        );
        for err in &all {
            let next = err.next_step();
            assert!(next.len() > 10, "{err:?} has no next step");
            assert!(!next.contains('\n'), "{err:?} next step is one line");
            assert!(
                [EXIT_MISMATCH, EXIT_USAGE, EXIT_REFUSED, EXIT_IO].contains(&err.exit_code()),
                "{err:?}"
            );
        }
    }

    #[test]
    fn codes_follow_the_documented_table() {
        assert_eq!(IngestError::ManifestDrift { count: 1 }.exit_code(), 1);
        assert_eq!(
            IngestError::Usage {
                message: "x".into()
            }
            .exit_code(),
            2
        );
        assert_eq!(IngestError::Symlink("p".into()).exit_code(), 3);
        assert_eq!(
            IngestError::io("p", std::io::Error::other("x")).exit_code(),
            4
        );
    }
}
