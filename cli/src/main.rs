// SPDX-License-Identifier: Apache-2.0

//! `albumen export` and `albumen verify-manifest`.
//!
//! Export is `copy_verify` across a library. It does not modify sources and
//! it does not overwrite a destination that differs.

// IngestError is the library's error. The CLI prints it; it does not reshape it.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use albumen_ingest::{
    export_tree, parse_manifest, verify_tree, write_manifest, DirectoryTree, ExportDest,
    IngestError, ManifestEntry, S3Config, S3Store,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "albumen", about = "Bit-exact photo library export")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Copy the library and sidecars to a directory or s3://bucket/prefix.
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
    },
    /// Hash the exported files and compare them to a manifest.
    VerifyManifest {
        #[arg(long)]
        manifest: PathBuf,
        /// Directory or s3://bucket/prefix that was exported.
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
        Command::Export { from, to, manifest } => {
            let dest = open_target(&to)?;
            let entries = export_tree(&from, dest.as_ref())?;
            let manifest_path = match manifest {
                Some(path) => path,
                None => default_manifest(&to)?,
            };
            refuse_clobber(&manifest_path, &to, &entries)?;
            write_manifest(&manifest_path, &entries)?;
            println!("exported {} files to {to}", entries.len());
            println!("manifest {}", manifest_path.display());
            Ok(())
        }
        Command::VerifyManifest { manifest, against } => {
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

fn open_target(spec: &str) -> Result<Box<dyn ExportDest>, IngestError> {
    if let Some(rest) = spec.strip_prefix("s3://") {
        let (bucket, prefix) = split_s3(rest)?;
        let mut config = S3Config::from_env()?;
        config.bucket = bucket;
        config.prefix = prefix;
        let store = S3Store::connect(config)?;
        Ok(Box::new(store))
    } else {
        std::fs::create_dir_all(spec).map_err(|err| IngestError::Io {
            path: PathBuf::from(spec),
            source: err,
        })?;
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
