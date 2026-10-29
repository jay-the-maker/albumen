// SPDX-License-Identifier: Apache-2.0

//! `albumen export`, `albumen import-takeout`, `albumen import-card`, `albumen import-icloud`,
//! `albumen scan`, `albumen ledger`, `albumen verify-manifest`, and `albumen family`.
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
    dest, export_tree_counted, import_card_local, import_icloud, import_takeout_with,
    parse_manifest, plan_card, plan_icloud, plan_takeout, platform_dirs, scan_tree,
    verify_scan_manifest, verify_tree, walk_sorted, write_manifest, DirectoryTree, ExportDest,
    IngestError, ManifestEntry, RcloneStore, S3Config, S3Store, SourceRoot, WalkControl,
};
mod proof;

use albumen_ingest::tree_drift;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;

mod family;
mod receive;

#[derive(Debug, Parser)]
#[command(
    name = "albumen",
    about = "Bit-exact photo library export, Takeout import, iCloud import, read-only scan, ledger, and family namespaces. Export, import, and family writes require --apply or --execute. ledger and platform-dirs create nothing."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Copy the library and sidecars to a directory, s3://, sftp://, webdav+http(s)://, or b2://.
    ///
    /// Without `--apply` or `--execute`, print the plan and write nothing.
    Export {
        /// Library root. Files under it are not modified.
        #[arg(long)]
        from: PathBuf,
        /// Destination directory, s3://bucket/prefix, or an rclone remote:
        /// sftp://user@host:port/dir, webdav+https://user@host/path, or
        /// b2://bucket/prefix. The rclone password or key comes from
        /// ALBUMEN_RCLONE_PASSWORD (SFTP, WebDAV) or ALBUMEN_B2_ACCOUNT and
        /// ALBUMEN_B2_KEY, never the command line. ALBUMEN_RCLONE names the binary.
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
    /// Print the platform config and data directories. Creates nothing.
    ///
    /// Linux uses XDG, macOS uses ~/Library/Application Support, and Windows
    /// uses the roaming Known Folder. These paths are not used unless you
    /// pass them to another command.
    PlatformDirs,
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
        /// Ledger directory. Default: under ALBUMEN_HOME or the platform data
        /// directory, keyed by source. Refused inside the source or its parent.
        #[arg(long)]
        ledger: Option<PathBuf>,
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
    /// Copy a mounted camera card into a library directory.
    ///
    /// The card is opened read-only and is never formatted, ejected, or
    /// emptied. Each original goes with its sidecars and pairs through
    /// copy-verify. Identical SHA-256 is skipped; a camera name taken by other
    /// bytes is stored as `{stem}~{sha8}{ext}` with a `.albumen-card.json`
    /// record of the camera filename. Nothing in the library is overwritten.
    ImportCard {
        /// Mounted card, for example its DCIM folder's parent.
        #[arg(long)]
        from: PathBuf,
        /// Destination library. Must not be on or inside the card.
        #[arg(long)]
        to: PathBuf,
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
    /// Per-member namespaces, a signed album manifest, invite, and revoke.
    Family {
        #[command(subcommand)]
        action: family::Command,
    },
    /// Hide or unhide a stored original in a receiver library, on this copy only.
    ///
    /// Writes only `{name}.albumen-tombstone`. Never unlinks the original or
    /// the tombstone. Dry-run unless --apply/--execute.
    Receive {
        #[command(subcommand)]
        action: receive::Command,
    },
    /// List every stored file, every analysis, and every recorded destination.
    ///
    /// The library is opened read-only. The command hashes the files already
    /// in it and reads the logs beside it. It does not open a network connection
    /// and it does not write the library.
    Ledger {
        /// Library root.
        #[arg(long)]
        library: PathBuf,
        /// Extra connection log. The library's own ledger log is always read.
        #[arg(long)]
        connection_log: Option<PathBuf>,
        /// Ledger directory used by an earlier `scan --ledger`.
        #[arg(long)]
        ledger: Option<PathBuf>,
        /// Print JSON. The default is a short text account.
        #[arg(long)]
        json: bool,
    },
    /// Hash the exported files, or a scan manifest, and compare them.
    VerifyManifest {
        #[arg(long)]
        manifest: PathBuf,
        /// Directory, s3://, or rclone remote that was exported. Scan manifests
        /// are compared with a local directory.
        #[arg(long)]
        against: String,
        /// Also write a proof you can keep: `<path>.md` and `<path>.json`.
        ///
        /// Records version and commit, start and end time (UTC), the
        /// manifest's SHA-256, file and byte counts, the SHA-256 of the
        /// sorted per-file hashes, every drift line, and the re-run command.
        /// Refuses to overwrite either file. Drift still exits non-zero.
        #[arg(long)]
        proof: Option<PathBuf>,
    },
    /// Print a shell completion script to stdout.
    ///
    /// Writes nothing except the script. Redirect it into your shell's
    /// completion directory. The README shows each shell.
    Completions {
        /// Shell to generate the script for.
        shell: Shell,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("albumen: {err}");
            eprintln!("next: {}", err.next_step());
            ExitCode::from(err.exit_code())
        }
    }
}

/// One example per command, shown at the end of its `--help`.
const EXAMPLES: &[(&str, &str)] = &[
    ("export", "albumen export --from ~/Pictures/library --to /Volumes/Backup/export --apply"),
    ("platform-dirs", "albumen platform-dirs"),
    ("scan", "albumen scan ~/Pictures/library --manifest ~/scan.jsonl"),
    ("import-card", "albumen import-card --from /Volumes/CARD --to ~/Pictures/library --apply"),
    ("import-takeout", "albumen import-takeout --from ~/Downloads/takeout --to ~/Pictures/library --report ~/takeout.jsonl --apply"),
    ("import-icloud", "albumen import-icloud --from ~/Downloads/iCloud --to ~/Pictures/library --report ~/icloud.jsonl --apply"),
    ("family", "albumen family init --to ~/family --key ~/founder.key --apply"),
    ("family keygen", "albumen family keygen --key ~/founder.key --apply"),
    ("family init", "albumen family init --to ~/family --key ~/founder.key --apply"),
    ("family export", "albumen family export --library ~/family --to /Volumes/Backup/family --apply"),
    ("family add", "albumen family add --library ~/family --key ~/me.key --from ~/Pictures/IMG_0001.HEIC --apply"),
    ("family invite", "albumen family invite --library ~/family --key ~/founder.key --member sam --apply"),
    ("family join", "albumen family join --library ~/family --key ~/sam.key --token TOKEN --name sam --apply"),
    ("family admit", "albumen family admit --library ~/family --key ~/founder.key --member sam --token TOKEN --apply"),
    ("family revoke", "albumen family revoke --library ~/family --key ~/founder.key --member sam --apply"),
    ("family tombstone", "albumen family tombstone --library ~/family --key ~/me.key --sha256 SHA256 --apply"),
    ("family hide", "albumen family hide --library ~/family --key ~/me.key --sha256 SHA256 --apply"),
    ("family album-add", "albumen family album-add --library ~/family --key ~/me.key --album Summer --sha256 SHA256 --apply"),
    ("family album-omit", "albumen family album-omit --library ~/family --key ~/me.key --add Summer --apply"),
    ("family manifest", "albumen family manifest --library ~/family --key ~/founder.key --apply"),
    ("family sync", "albumen family sync --from ~/family --to /Volumes/Backup/family --apply"),
    ("family policy", "albumen family policy --bucket my-bucket --member sam"),
    ("family publish", "albumen family publish --library ~/family --key ~/founder.key --apply"),
    ("receive", "albumen receive hide --library ~/received --name IMG_0001.HEIC --apply"),
    ("receive hide", "albumen receive hide --library ~/received --name IMG_0001.HEIC --apply"),
    ("receive unhide", "albumen receive unhide --library ~/received --name IMG_0001.HEIC --apply"),
    ("ledger", "albumen ledger --library ~/Pictures/library"),
    ("verify-manifest", "albumen verify-manifest --manifest /Volumes/Backup/export/ALBUMEN-MANIFEST.txt --against /Volumes/Backup/export --proof ~/proof"),
    ("completions", "albumen completions bash > ~/.local/share/bash-completion/completions/albumen"),
];

const EXIT_CODES_HELP: &str =
    "Exit codes: 0 ok, 1 verification mismatch or drift, 2 usage, 3 refused unsafe layout, 4 I/O.";

fn with_examples(mut command: clap::Command, prefix: &str) -> clap::Command {
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    for name in names {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        let example = EXAMPLES
            .iter()
            .find(|(key, _)| *key == path)
            .map(|(_, example)| *example);
        command = command.mut_subcommand(&name, |sub| {
            let sub = with_examples(sub, &path);
            match example {
                Some(example) => sub.after_help(format!("Example:\n  {example}")),
                None => sub,
            }
        });
    }
    command
}

fn cli_command() -> clap::Command {
    with_examples(Cli::command(), "").after_help(EXIT_CODES_HELP)
}

fn run() -> Result<(), IngestError> {
    let cli = match cli_command().try_get_matches() {
        Ok(matches) => Cli::from_arg_matches(&matches)
            .map_err(|err| err.exit())
            .unwrap(),
        Err(err) => err.exit(),
    };
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
            albumen_ingest::ledger::anchor(&from, "export");
            let dest = open_target(&to)?;
            let source = SourceRoot::open(&from)?;
            let report = export_tree_counted(&source, dest.as_ref())?;
            let entries = report.entries;
            refuse_clobber(&manifest_path, &to, &entries)?;
            write_manifest(&manifest_path, &entries)?;
            println!(
                "exported {} new, {} already present and verified",
                report.copied, report.already_present
            );
            println!("to {to}");
            println!("manifest {}", manifest_path.display());
            Ok(())
        }
        Command::Scan {
            dir,
            manifest,
            ledger,
        } => {
            let source = SourceRoot::open(&dir)?;
            if let Some(ledger) = ledger.as_deref() {
                albumen_ingest::ledger::set_ledger_dir(ledger, source.canonical())?;
            }
            let stats = scan_tree(&source, &manifest)?;
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
            albumen_ingest::family::refuse_shared_write_root(&to)?;
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
        Command::ImportCard {
            from,
            to,
            apply,
            execute,
        } => {
            albumen_ingest::family::refuse_shared_write_root(&to)?;
            SourceRoot::open(&from)?.refuse_inside(&to)?;
            if !(apply || execute) {
                let (assets, files) = plan_card(&from)?;
                println!("dry-run: no files written. Pass --apply or --execute to copy.");
                println!("import-card");
                println!("  from: {}", from.display());
                println!("  to: {}", to.display());
                println!("  assets: {assets}");
                println!("  files: {files}");
                return Ok(());
            }
            dest::create_dir_all(&to)?;
            let outcome = import_card_local(&from, &to)?;
            println!(
                "imported {} assets ({} files, {} already present) to {}",
                outcome.assets,
                outcome.files.len(),
                outcome.deduped,
                to.display()
            );
            for renamed in &outcome.renamed {
                println!(
                    "  renamed: {} -> {}",
                    renamed.camera_filename, renamed.library_filename
                );
            }
            Ok(())
        }
        Command::ImportIcloud {
            from,
            to,
            report,
            apply,
            execute,
        } => {
            albumen_ingest::family::refuse_shared_write_root(&to)?;
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
        Command::Family { action } => family::run(action),
        Command::Receive { action } => receive::run(action),
        Command::Ledger {
            library,
            connection_log,
            ledger: ledger_dir,
            json,
        } => {
            if let Some(dir) = ledger_dir.as_deref() {
                albumen_ingest::ledger::set_ledger_dir(dir, &library)?;
            }
            let ledger = albumen_ingest::ledger::build(&library, connection_log.as_deref())?;
            if json {
                println!("{}", ledger.to_json()?);
            } else {
                print!("{}", ledger.to_text());
            }
            Ok(())
        }
        Command::PlatformDirs => {
            let (config, data) = platform_dirs()?;
            println!("platform-dirs");
            println!("  config: {}", config.display());
            println!("  data: {}", data.display());
            println!("  creates: nothing");
            Ok(())
        }
        Command::VerifyManifest {
            manifest,
            against,
            proof: Some(proof_path),
        } => verify_with_proof(&manifest, &against, &proof_path),
        Command::VerifyManifest {
            manifest,
            against,
            proof: None,
        } => {
            if manifest_is_jsonl(&manifest)? {
                if is_remote(&against) {
                    return Err(IngestError::Usage {
                        message: format!(
                            "{}: a scan manifest is verified against a local directory",
                            against
                        ),
                    });
                }
                let source = SourceRoot::open(Path::new(&against))?;
                verify_scan_manifest(&manifest, &source, &mut |drift| {
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
            let count = entries.len() as u64;
            println!("verified {count} files in {against}");
            println!(
                "{{\"summary\":true,\"count_in\":{count},\"count_out\":{count},\"mismatched\":0}}"
            );
            Ok(())
        }
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "albumen", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn verify_with_proof(manifest: &Path, against: &str, proof_path: &Path) -> Result<(), IngestError> {
    let (md, json) = proof::proof_paths(proof_path);
    for target in [&md, &json] {
        if target.exists() {
            return Err(IngestError::AlreadyExists {
                filename: target.display().to_string(),
                destination: target.display().to_string(),
            });
        }
    }
    let started = std::time::SystemTime::now();
    let mut drift = Vec::new();
    let mut files = Vec::new();
    if manifest_is_jsonl(manifest)? {
        if is_remote(against) {
            return Err(IngestError::Usage {
                message: format!(
                    "{}: a scan manifest is verified against a local directory",
                    against
                ),
            });
        }
        let text = std::fs::read_to_string(manifest).map_err(|err| IngestError::Io {
            path: manifest.to_path_buf(),
            source: err,
        })?;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let (Some(path), Some(sha), Some(bytes)) = (
                value["path"].as_str(),
                value["sha256"].as_str(),
                value["bytes"].as_u64(),
            ) {
                files.push((path.to_string(), sha.to_string(), bytes));
            }
        }
        let source = SourceRoot::open(Path::new(against))?;
        match verify_scan_manifest(manifest, &source, &mut |item| {
            println!("{} {} {}", item.kind, item.path, item.detail);
            drift.push((item.kind.clone(), item.path.clone(), item.detail.clone()));
        }) {
            Ok(()) | Err(IngestError::ManifestDrift { .. }) => {}
            Err(err) => return Err(err),
        }
    } else {
        let text = std::fs::read_to_string(manifest).map_err(|err| IngestError::Io {
            path: manifest.to_path_buf(),
            source: err,
        })?;
        let entries = parse_manifest(&text)?;
        let dest = open_target(against)?;
        let ignore = manifest_ignore(manifest, against)?;
        for item in tree_drift(&entries, dest.as_ref(), &ignore)? {
            println!("{} {} {}", item.kind, item.path, item.detail);
            drift.push((item.kind.to_string(), item.path, item.detail));
        }
        files = entries
            .into_iter()
            .map(|entry| (entry.relative_path, entry.sha256, entry.byte_len))
            .collect();
    }
    let finished = std::time::SystemTime::now();
    let count = drift.len() as u64;
    let (md, json) = proof::write_proof(
        proof_path,
        &proof::ProofInput {
            manifest,
            against,
            started,
            finished,
            files,
            drift,
        },
    )?;
    println!("proof: {}", md.display());
    println!("proof: {}", json.display());
    if count == 0 {
        println!("verified against {against}");
        Ok(())
    } else {
        Err(IngestError::ManifestDrift { count })
    }
}

fn refuse_export_targets(from: &Path, to: &str, manifest: &Path) -> Result<(), IngestError> {
    let source = SourceRoot::open(from)?;
    if !is_remote(to) {
        let dest = Path::new(to);
        source.refuse_destination(dest)?;
        albumen_ingest::family::refuse_shared_write_root(dest)?;
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
    } else if is_remote(to) {
        println!("  network: on --apply, run rclone against {to} and write objects");
    } else {
        println!("  network: none");
    }
    let source = SourceRoot::open(from)?;
    let local = (!is_remote(to)).then(|| Path::new(to));
    let mut files = 0u64;
    let mut bytes = 0u64;
    let (mut new, mut present) = (0u64, 0u64);
    let mut conflicts: Vec<String> = Vec::new();
    walk_sorted(from, None, &mut |relative| {
        let full = from.join(relative);
        let len = source
            .metadata_nofollow(&full)
            .map(|meta| meta.len())
            .unwrap_or(0);
        files += 1;
        bytes += len;
        let status = match local {
            Some(root) if !albumen_ingest::is_copy_local_tombstone(relative) => {
                Some(plan_status(&source, &full, &root.join(relative), len))
            }
            _ => None,
        };
        match status {
            Some(PlanStatus::Present) => {
                println!("  present: {relative}");
                present += 1;
            }
            Some(PlanStatus::Conflict) => {
                println!("  conflict: {relative}");
                conflicts.push(relative.to_string());
            }
            Some(PlanStatus::Place) => {
                println!("  {relative}");
                new += 1;
            }
            None => println!("  {relative}"),
        }
        WalkControl::Continue
    })?;
    println!("  files: {files}");
    println!("  bytes: {bytes}");
    match local {
        Some(root) => {
            println!("  already present: {present}");
            println!("  conflicts: {}", conflicts.len());
            println!("would export {new} new, {present} already present and verified");
            if let Some(first) = conflicts.first() {
                return Err(IngestError::AlreadyExists {
                    filename: first.clone(),
                    destination: root.join(first).display().to_string(),
                });
            }
        }
        None => println!(
            "would export up to {files} files to {to}; the already-present count needs --apply, because a dry-run opens no network"
        ),
    }
    Ok(())
}

enum PlanStatus {
    Place,
    Present,
    Conflict,
}

/// Mirrors `DirectoryTree::place`: identical bytes are skipped, anything
/// else already at the destination path would fail with `AlreadyExists`.
/// Reads only; nothing at the destination is changed.
fn plan_status(source: &SourceRoot, path: &Path, dest: &Path, source_len: u64) -> PlanStatus {
    let Ok(meta) = std::fs::symlink_metadata(dest) else {
        return PlanStatus::Place;
    };
    if !meta.is_file() || meta.len() != source_len {
        return PlanStatus::Conflict;
    }
    let source_hash = source
        .open_read(path)
        .and_then(|file| albumen_ingest::sha256_reader(file, path));
    match (albumen_ingest::sha256_file(dest), source_hash) {
        (Ok(a), Ok(b)) if a == b => PlanStatus::Present,
        _ => PlanStatus::Conflict,
    }
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
    } else if let Some(store) = open_rclone(spec)? {
        Ok(Box::new(store))
    } else {
        dest::create_dir_all(Path::new(spec))?;
        Ok(Box::new(DirectoryTree::new(spec)))
    }
}

const RCLONE_SCHEMES: [&str; 4] = ["sftp://", "webdav+http://", "webdav+https://", "b2://"];

/// A destination that is not a local directory.
fn is_remote(spec: &str) -> bool {
    spec.starts_with("s3://") || RCLONE_SCHEMES.iter().any(|scheme| spec.starts_with(scheme))
}

fn rclone_error(spec: &str, message: &str) -> IngestError {
    IngestError::Store {
        operation: "rclone",
        filename: spec.to_string(),
        message: message.to_string(),
    }
}

fn env_secret(spec: &str, name: &str) -> Result<String, IngestError> {
    std::env::var(name).map_err(|_| rclone_error(spec, &format!("set {name}")))
}

/// `sftp://user@host:port/dir`, `webdav+https://user@host/path`, `b2://bucket/prefix`.
fn open_rclone(spec: &str) -> Result<Option<RcloneStore>, IngestError> {
    let bin = std::env::var_os("ALBUMEN_RCLONE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rclone"));
    if let Some(rest) = spec.strip_prefix("b2://") {
        let (bucket, prefix) = split_s3(rest)?;
        let account = env_secret(spec, "ALBUMEN_B2_ACCOUNT")?;
        let key = env_secret(spec, "ALBUMEN_B2_KEY")?;
        return Ok(Some(RcloneStore::b2(
            bin, &account, &key, &bucket, &prefix,
        )?));
    }
    if let Some(rest) = spec.strip_prefix("sftp://") {
        let (authority, dir) = rest.split_once('/').unwrap_or((rest, ""));
        let (user, host_port) = authority
            .split_once('@')
            .ok_or_else(|| rclone_error(spec, "expected sftp://user@host:port/dir"))?;
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| rclone_error(spec, "port must be a number"))?,
            ),
            None => (host_port, 22),
        };
        if user.is_empty() || host.is_empty() {
            return Err(rclone_error(spec, "expected sftp://user@host:port/dir"));
        }
        let password = env_secret(spec, "ALBUMEN_RCLONE_PASSWORD")?;
        let store = RcloneStore::sftp(bin, host, port, user, &password)?;
        return Ok(Some(store.under(dir)?));
    }
    if let Some(rest) = spec.strip_prefix("webdav+") {
        let (scheme, after) = rest
            .split_once("://")
            .ok_or_else(|| rclone_error(spec, "expected webdav+https://user@host/path"))?;
        let (authority, path) = after.split_once('/').unwrap_or((after, ""));
        let (user, host) = authority
            .split_once('@')
            .ok_or_else(|| rclone_error(spec, "expected webdav+https://user@host/path"))?;
        let url = format!("{scheme}://{host}/{path}");
        let password = env_secret(spec, "ALBUMEN_RCLONE_PASSWORD")?;
        return Ok(Some(RcloneStore::webdav(bin, &url, user, &password)?));
    }
    Ok(None)
}

fn split_s3(rest: &str) -> Result<(String, String), IngestError> {
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.contains(' ') {
        return Err(IngestError::Usage {
            message: format!("{rest}: expected s3://bucket or s3://bucket/prefix"),
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
    if is_remote(to) {
        return Err(IngestError::Usage {
            message: format!("{to}: pass --manifest when exporting to S3 or an rclone remote"),
        });
    }
    Ok(Path::new(to).join("ALBUMEN-MANIFEST.txt"))
}

fn refuse_clobber(manifest: &Path, to: &str, entries: &[ManifestEntry]) -> Result<(), IngestError> {
    if is_remote(to) {
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
    if is_remote(against) {
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

#[cfg(test)]
mod help_tests {
    use super::*;

    fn walk(command: &clap::Command, prefix: &str, seen: &mut Vec<String>) {
        for sub in command.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            let after = sub
                .get_after_help()
                .map(|text| text.to_string())
                .unwrap_or_default();
            let examples = after.matches("Example:").count();
            assert_eq!(examples, 1, "`albumen {path} --help` needs one example");
            assert!(
                after.contains(&format!("albumen {path}")),
                "{path}: {after}"
            );
            seen.push(path.clone());
            walk(sub, &path, seen);
        }
    }

    #[test]
    fn every_command_help_shows_one_example() {
        let command = cli_command();
        let mut seen = Vec::new();
        walk(&command, "", &mut seen);
        for (key, _) in EXAMPLES {
            assert!(seen.iter().any(|path| path == key), "unused example {key}");
        }
        let top = command
            .get_after_help()
            .map(|text| text.to_string())
            .unwrap();
        assert!(top.contains("3 refused unsafe layout"));
    }
}
