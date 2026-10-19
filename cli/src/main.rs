// SPDX-License-Identifier: Apache-2.0

//! `albumen export`, `albumen import-takeout`, `albumen import-icloud`,
//! `albumen scan`, and `albumen verify-manifest`.
//!
//! Export is `copy_verify` across a library. It does not modify sources and
//! it does not overwrite a destination that differs. Without `--apply` or
//! `--execute` it prints the plan and writes nothing. Takeout and iCloud
//! import read the download or library and copy into a directory you choose.
//! Scan only reads the source and writes a JSONL manifest outside that tree.

// IngestError is the library's error. The CLI prints it; it does not reshape it.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use albumen_ingest::{
    dest, export_tree, import_icloud, import_takeout_with, parse_manifest, plan_icloud,
    plan_takeout, scan_tree, verify_scan_manifest, verify_tree, walk_sorted, write_manifest,
    DirectoryTree, ExportDest, IngestError, ManifestEntry, S3Config, S3Store, SourceRoot,
    WalkControl,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "albumen",
    about = "Bit-exact photo library export, Takeout import, iCloud import, and read-only scan. Export and import writes require --apply or --execute."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Copy the library and sidecars to a directory or s3://bucket/prefix.
    ///
    /// Without `--apply` or `--execute`, print the plan and write nothing.
    Export {
        /// Library root. Files under it are not modified.
        #[arg(long)]
        from: PathBuf,
        /// Destination directory, or s3://bucket/prefix.
        #[arg(long)]
        to: String,
        /// SHA-256 manifest to write. Defaults to <to>/ALBUMEN-MANIFEST.txt for a directory.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Copy the files. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Read a library and write a JSONL manifest outside it.
    ///
    /// The source is opened read-only. Scan does not write the source.
    /// A size or mtime change during the read is recorded on that line as
    /// `drift`, with the before and after values. The manifest is refused
    /// when it resolves inside the library. Running the command again
    /// continues after the last complete line.
    Scan {
        /// Library root.
        dir: PathBuf,
        /// JSONL manifest to write. Must not resolve inside the library.
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Copy a Google Takeout archive or extracted folder into a library.
    ///
    /// The source is opened read-only. Each file is copy-verified. Takeout
    /// JSON stays byte-for-byte beside its original. Capture time and GPS are
    /// written into a new Albumen sidecar and are not written into the
    /// original. Without `--apply` or `--execute`, print the plan and write
    /// nothing. The command exits non-zero when the reconciliation report
    /// does not match.
    ImportTakeout {
        /// Directory of split .zip and .tgz parts, one archive, or an extracted folder.
        #[arg(long)]
        from: PathBuf,
        /// Destination library. Must not be the archive directory.
        #[arg(long)]
        to: PathBuf,
        /// JSONL reconciliation report, outside the archives and the library.
        #[arg(long)]
        report: PathBuf,
        /// Write a new XMP file rendered from each Takeout JSON.
        ///
        /// Off by default. The original's bytes and mtime are not changed.
        #[arg(long)]
        derive_xmp: bool,
        /// Copy the files. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Copy an iCloud Photos download, export, or Photos library.
    ///
    /// The source is opened read-only. A folder of zip parts and Photo Details
    /// CSVs is a multi-part download: the archives are streamed and the CSVs
    /// are copied byte-for-byte. A Photos library database is opened in
    /// SQLite read-only immutable mode. Album, favorite, caption, capture time,
    /// and GPS metadata is written beside the original and is not written back
    /// into the library. Without `--apply` or `--execute`, print the plan and
    /// write nothing. The command exits non-zero when the reconciliation
    /// report does not match.
    ImportIcloud {
        /// Export directory, `.photoslibrary` bundle, or one `.zip`/`.tgz` download.
        #[arg(long)]
        from: PathBuf,
        /// Destination library. Must not overlap the source.
        #[arg(long)]
        to: PathBuf,
        /// JSONL reconciliation report, outside the source and the library.
        #[arg(long)]
        report: PathBuf,
        /// Copy the files. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Hash the exported files, or a scan manifest, and compare them.
    VerifyManifest {
        #[arg(long)]
        manifest: PathBuf,
        /// Directory or s3://bucket/prefix that was exported. Scan manifests
        /// are compared with a local directory.
        #[arg(long)]
        against: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("albumen: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), IngestError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Export {
            from,
            to,
            manifest,
            apply,
            execute,
        } => {
            let manifest_path = match manifest {
                Some(path) => path,
                None => default_manifest(&to)?,
            };
            refuse_export_targets(&from, &to, &manifest_path)?;
            if !(apply || execute) {
                return print_export_plan(&from, &to, &manifest_path);
            }
            let dest = open_target(&to)?;
            let entries = export_tree(&from, dest.as_ref())?;
            refuse_clobber(&manifest_path, &to, &entries)?;
            write_manifest(&manifest_path, &entries)?;
            println!("exported {} files to {to}", entries.len());
            println!("manifest {}", manifest_path.display());
            Ok(())
        }
        Command::Scan { dir, manifest } => {
            let stats = scan_tree(&dir, &manifest)?;
            println!(
                "scanned {} files ({} bytes) into {}",
                stats.files,
                stats.bytes,
                manifest.display()
            );
            Ok(())
        }
        Command::ImportTakeout {
            from,
            to,
            report,
            derive_xmp,
            apply,
            execute,
        } => {
            if !(apply || execute) {
                let plan = plan_takeout(&from, &to, &report)?;
                println!("dry-run: no files written. Pass --apply or --execute to copy.");
                println!("import-takeout");
                println!("  from: {}", from.display());
                println!("  to: {}", to.display());
                println!("  report: {}", report.display());
                println!("  files: {}", plan.files);
                println!("  unmatched: {}", plan.unmatched.len());
                for name in &plan.unmatched {
                    println!("  unmatched-file: {name}");
                }
                for stem in &plan.motion_photos {
                    println!("  motion-photo: {stem}");
                }
                if derive_xmp {
                    println!("  derive-xmp: on --apply");
                }
                return Ok(());
            }
            let outcome = import_takeout_with(&from, &to, &report, derive_xmp)?;
            println!(
                "imported {} files ({} in, {} unmatched) to {}",
                outcome.count_out,
                outcome.count_in,
                outcome.unmatched,
                to.display()
            );
            println!("report {}", report.display());
            Ok(())
        }
        Command::ImportIcloud {
            from,
            to,
            report,
            apply,
            execute,
        } => {
            if !(apply || execute) {
                let plan = plan_icloud(&from, &to, &report)?;
                println!("dry-run: no files written. Pass --apply or --execute to copy.");
                println!("import-icloud");
                println!("  from: {}", from.display());
                println!("  to: {}", to.display());
                println!("  report: {}", report.display());
                println!("  files: {}", plan.files);
                println!("  unmatched: {}", plan.unmatched.len());
                for name in &plan.unmatched {
                    println!("  unmatched-file: {name}");
                }
                for stem in &plan.live_photos {
                    println!("  live-photo: {stem}");
                }
                return Ok(());
            }
            let outcome = import_icloud(&from, &to, &report)?;
            println!(
                "imported {} files ({} in) to {}",
                outcome.count_out,
                outcome.count_in,
                to.display()
            );
            println!("report {}", report.display());
            Ok(())
        }
        Command::VerifyManifest { manifest, against } => {
            if manifest_is_jsonl(&manifest)? {
                if against.starts_with("s3://") {
                    return Err(IngestError::Store {
                        operation: "verify-manifest",
                        filename: against,
                        message: "a scan manifest is verified against a local directory"
                            .to_string(),
                    });
                }
                verify_scan_manifest(&manifest, Path::new(&against), &mut |drift| {
                    println!("{} {} {}", drift.kind, drift.path, drift.detail);
                })?;
                println!("verified scan manifest against {against}");
                return Ok(());
            }
            let text = std::fs::read_to_string(&manifest).map_err(|err| IngestError::Io {
                path: manifest.clone(),
                source: err,
            })?;
            let entries = parse_manifest(&text)?;
            let dest = open_target(&against)?;
            let ignore = manifest_ignore(&manifest, &against)?;
            verify_tree(&entries, dest.as_ref(), &ignore)?;
            println!("verified {} files in {against}", entries.len());
            Ok(())
        }
    }
}

fn refuse_export_targets(from: &Path, to: &str, manifest: &Path) -> Result<(), IngestError> {
    let source = SourceRoot::open(from)?;
    if !to.starts_with("s3://") {
        source.refuse_destination(Path::new(to))?;
    }
    source.refuse_destination(manifest)
}

fn print_export_plan(from: &Path, to: &str, manifest: &Path) -> Result<(), IngestError> {
    if !from.is_dir() {
        return Err(IngestError::NotAFile(from.to_path_buf()));
    }
    println!(
        "dry-run: no files written and no network connections opened. Pass --apply or --execute to copy."
    );
    println!("export");
    println!("  from: {}", from.display());
    println!("  to: {to}");
    println!("  manifest: {}", manifest.display());
    if to.starts_with("s3://") {
        println!("  network: on --apply, connect to ALBUMEN_S3_ENDPOINT and write objects");
    } else {
        println!("  network: none");
    }
    let source = SourceRoot::open(from)?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    walk_sorted(from, None, &mut |relative| {
        let full = from.join(relative);
        let len = source
            .metadata_nofollow(&full)
            .map(|meta| meta.len())
            .unwrap_or(0);
        println!("  {relative}");
        files += 1;
        bytes += len;
        WalkControl::Continue
    })?;
    println!("  files: {files}");
    println!("  bytes: {bytes}");
    Ok(())
}

fn manifest_is_jsonl(path: &Path) -> Result<bool, IngestError> {
    let mut file = std::fs::File::open(path).map_err(|err| IngestError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    let mut buf = [0u8; 64];
    let n = std::io::Read::read(&mut file, &mut buf).map_err(|err| IngestError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    let text = String::from_utf8_lossy(&buf[..n]);
    Ok(text.trim_start().starts_with('{'))
}

fn open_target(spec: &str) -> Result<Box<dyn ExportDest>, IngestError> {
    if let Some(rest) = spec.strip_prefix("s3://") {
        let (bucket, prefix) = split_s3(rest)?;
        let mut config = S3Config::from_env()?;
        config.bucket = bucket;
        config.prefix = prefix;
        let store = S3Store::connect(config)?;
        Ok(Box::new(store))
    } else {
        dest::create_dir_all(Path::new(spec))?;
        Ok(Box::new(DirectoryTree::new(spec)))
    }
}

fn split_s3(rest: &str) -> Result<(String, String), IngestError> {
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.contains(' ') {
        return Err(IngestError::Store {
            operation: "s3",
            filename: rest.to_string(),
            message: "expected s3://bucket or s3://bucket/prefix".to_string(),
        });
    }
    let (bucket, prefix) = match rest.split_once('/') {
        Some((bucket, prefix)) => (bucket.to_string(), prefix.to_string()),
        None => (rest.to_string(), String::new()),
    };
    if bucket.is_empty() || prefix.contains("//") || prefix.split('/').any(|part| part == "..") {
        return Err(IngestError::UnsafeFilename {
            filename: rest.to_string(),
        });
    }
    Ok((bucket, prefix))
}

fn default_manifest(to: &str) -> Result<PathBuf, IngestError> {
    if to.starts_with("s3://") {
        return Err(IngestError::Store {
            operation: "manifest",
            filename: to.to_string(),
            message: "pass --manifest when exporting to S3".to_string(),
        });
    }
    Ok(Path::new(to).join("ALBUMEN-MANIFEST.txt"))
}

fn refuse_clobber(manifest: &Path, to: &str, entries: &[ManifestEntry]) -> Result<(), IngestError> {
    if to.starts_with("s3://") {
        return Ok(());
    }
    let root = Path::new(to);
    let Ok(relative) = manifest.strip_prefix(root) else {
        return Ok(());
    };
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if entries.iter().any(|entry| entry.relative_path == relative) {
        return Err(IngestError::AlreadyExists {
            filename: relative,
            destination: manifest.display().to_string(),
        });
    }
    Ok(())
}

fn manifest_ignore(manifest: &Path, against: &str) -> Result<Vec<String>, IngestError> {
    if against.starts_with("s3://") {
        return Ok(Vec::new());
    }
    let root = Path::new(against);
    let Ok(relative) = manifest.strip_prefix(root) else {
        return Ok(Vec::new());
    };
    if relative.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    let text = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(vec![text])
}
