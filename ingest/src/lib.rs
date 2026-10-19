// SPDX-License-Identifier: Apache-2.0

//! Bit-exact ingest for Albumen.
//!
//! Metadata and original quality are never lost. This crate copies an original
//! and its sidecars without recompressing, transcoding, or stripping them,
//! preserves the filename and mtime, and verifies SHA-256. A mismatch is a
//! hard error: the bad destination is removed, and both hashes are reported.

#![forbid(unsafe_code)]

mod checksum;
mod discover;
mod error;
mod export;
mod icloud;
mod local;
mod mtime;
mod photos_library;
mod rclone;
mod s3;
mod scan;
mod store;
mod takeout;

pub use checksum::{sha256_file, sha256_hex};
pub use discover::{discover, Asset, AssetFile, Role};
pub use error::IngestError;
pub use export::{
    export_tree, format_manifest, parse_manifest, verify_tree, walk_sorted, write_manifest,
    DirectoryTree, ExportDest, ManifestEntry, Placed, WalkControl,
};
pub use icloud::{import_icloud, IcloudReport};
pub use local::{copy_verify_file, LocalStore};
pub use rclone::RcloneStore;
pub use s3::{S3Config, S3Store};
pub use scan::{scan_tree, verify_scan_manifest, ScanDrift, ScanStats};
pub use store::{
    copy_verify, copy_verify_quarantine, BlobStore, IngestReport, Quarantine, StoredFile,
    VerifiedObject,
};
pub use takeout::{import_takeout, TakeoutReport};

#[cfg(test)]
mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::mtime::set_mtime;

    pub struct Tmp {
        path: PathBuf,
    }

    impl Tmp {
        pub fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("albumen-test-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    pub fn write_file(dir: &Path, name: &str, bytes: &[u8], mtime: SystemTime) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write fixture");
        set_mtime(&path, mtime).expect("set mtime");
        path
    }
}
