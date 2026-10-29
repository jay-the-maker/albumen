// SPDX-License-Identifier: Apache-2.0

//! Bit-exact ingest for Albumen.
//!
//! Metadata and original quality are never lost. This crate copies an original
//! and its sidecars without recompressing, transcoding, or stripping them,
//! preserves the filename and mtime, and verifies SHA-256. A mismatch is a
//! hard error: the bad destination is removed, and both hashes are reported.

#![forbid(unsafe_code)]

mod checksum;
mod copy_profile;
pub mod dest;
mod discover;
mod error;
mod exiftool;
mod export;
pub mod family;
mod family_export;
mod family_source;
mod family_sync;
mod icloud;
pub mod index;
pub mod ledger;
mod local;
mod mtime;
mod photos_library;
mod platform;
mod rclone;
mod s3;
mod scan;
mod source;
mod store;
mod takeout;

pub use checksum::{sha256_file, sha256_hex};
pub use copy_profile::{
    enable_thread as enable_copy_profile_thread, reset as reset_copy_profile,
    snapshot as copy_phases, CopyPhases, PHASE_NAMES as COPY_PHASE_NAMES, PREFLIGHT_STEP_NAMES,
};
pub use discover::{discover, Asset, AssetFile, Role};
pub use error::IngestError;
pub use exiftool::exiftool_command;
pub use export::{
    export_tree, export_tree_counted, format_manifest, is_copy_local_tombstone, parse_manifest,
    verify_tree, walk_sorted, write_manifest, DirectoryTree, ExportDest, ExportReport,
    ManifestEntry, Placed, WalkControl,
};
pub use family_export::{export_family, plan_family_export, FamilyExportPlan, FamilyExportReport};
pub use family_source::FamilySource;
pub use family_sync::{plan_family_sync, sync_family, FamilySyncPlan, FamilySyncReport};
pub use icloud::{import_icloud, plan_icloud, IcloudPlan, IcloudReport};
pub use local::{
    copy_verify_file, copy_verify_files_pipelined, pipelined_verifiers, prefetch_source,
    store_prefetched, LocalStore, PrefetchedSource, PREFETCH_MAX_BYTES,
};
pub use platform::platform_dirs;
pub use rclone::RcloneStore;
pub use s3::{S3Config, S3Store};
pub use scan::{scan_tree, verify_scan_manifest, ScanDrift, ScanStats};
pub use source::SourceRoot;
pub use store::{
    copy_verify, copy_verify_quarantine, BlobStore, IngestReport, Quarantine, StoredFile,
    VerifiedObject,
};
pub use takeout::{import_takeout, import_takeout_with, plan_takeout, TakeoutPlan, TakeoutReport};

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
