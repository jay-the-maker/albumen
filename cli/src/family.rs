// SPDX-License-Identifier: Apache-2.0

//! `albumen family`: per-member namespaces, a signed manifest, invite, and revoke.
//!
//! Writes require `--apply` or `--execute`. A signing key is a file the member
//! keeps outside the library. There is no account.

use std::path::{Path, PathBuf};

use albumen_ingest::family::{self, FamilyKey};
use albumen_ingest::{
    export_family, plan_family_export, plan_family_sync, sync_family, IngestError,
};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a signing key outside the library.
    Keygen {
        /// Key file to create. An existing file is left in place.
        #[arg(long)]
        key: PathBuf,
        /// Write the key file. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Create a family library and sign its genesis record.
    Init {
        /// Parent directory. The library is created at `<to>/albumen-family/<library-id>/`.
        #[arg(long)]
        to: PathBuf,
        /// Signing key. Created when it does not exist yet.
        #[arg(long)]
        key: PathBuf,
        /// Display name stored in this member's hello record.
        #[arg(long, default_value = "founder")]
        name: String,
        /// Write the library. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Copy every member's originals and sidecars out of the library.
    Export {
        /// Family library directory (the directory that contains genesis.json).
        #[arg(long)]
        library: PathBuf,
        /// Destination directory. Must not be inside the library.
        #[arg(long)]
        to: PathBuf,
        /// Checksum manifest. Defaults to `<to>/ALBUMEN-MANIFEST.txt`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Copy the files. Without this or `--execute`, print the plan and write nothing.
        #[arg(long)]
        apply: bool,
        /// Same as `--apply`.
        #[arg(long)]
        execute: bool,
    },
    /// Copy an original into the signing member's namespace.
    Add {
        /// Family library directory (the directory that contains genesis.json).
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Original to copy. It is not modified.
        #[arg(long)]
        from: PathBuf,
        /// Sidecar to copy beside the original, in the same namespace.
        #[arg(long)]
        sidecar: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Sign an invite for another member's public key. Prints the token once.
    Invite {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Invitee's member id, the hex encoding of their public key.
        #[arg(long)]
        member: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Append hello in this member's own namespace.
    Join {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Token printed by `family invite`.
        #[arg(long)]
        token: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Admit a member who has joined with this invite.
    Admit {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        member: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Revoke a member. Their originals, sidecars, and mtimes stay.
    Revoke {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        member: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Tombstone one of your own photos. The original and sidecar stay on every copy.
    Tombstone {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Hide or show a photo in this viewer's library. Other members' files stay.
    Hide {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        sha256: String,
        /// Record hidden=false for this viewer.
        #[arg(long)]
        show: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Add a SHA-256 to an album. Prints the add id.
    AlbumAdd {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        album: String,
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Drop album adds by id. Adds that are not named stay. Bytes stay.
    AlbumOmit {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Add id printed by `family album-add`. Repeat for each id this member has seen.
        #[arg(long = "add")]
        add: Vec<String>,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Rebuild the shared manifest from signed logs, or with --summary print
    /// a read-only text summary of the family library.
    Manifest {
        #[arg(long)]
        library: PathBuf,
        /// When set with `--apply`, append a signed snapshot in this member's log.
        #[arg(long)]
        key: Option<PathBuf>,
        /// Print a plain-text summary instead of JSON: member count (active and
        /// revoked), photos in all members' folders, and photos per member,
        /// listed by name.
        /// Read-only: writes nothing, so it cannot be combined with --key,
        /// --apply or --execute. The GUI's Family view shows these same lines.
        #[arg(long, conflicts_with_all = ["key", "apply", "execute"])]
        summary: bool,
        /// With --summary: a member id (the 64-hex key named under `members/`).
        /// Adds a line with how many photos that member is shown and how many
        /// they have hidden.
        #[arg(long, requires = "summary", value_name = "MEMBER_ID")]
        viewer: Option<String>,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Add to one live family library the files another holds and it lacks.
    /// Additive only: never replaces or removes, refuses when a shared file
    /// differs, and reads `--from` only. Dry-run unless --apply/--execute.
    Sync {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
    /// Print an S3 bucket policy for one member prefix. Nothing is changed.
    Policy {
        #[arg(long)]
        bucket: String,
        /// Library prefix inside the bucket. May be empty.
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long)]
        member: String,
    },
    /// Copy this member's namespace to the configured S3 bucket.
    Publish {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        key: PathBuf,
        /// Prefix inside the bucket from `ALBUMEN_S3_*`.
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        execute: bool,
    },
}

pub fn run(command: Command) -> Result<(), IngestError> {
    match command {
        Command::Keygen {
            key,
            apply,
            execute,
        } => {
            if !(apply || execute) {
                println!("dry-run: no key written. Pass --apply or --execute to create it.");
                println!("family keygen");
                println!("  key: {}", key.display());
                return Ok(());
            }
            let created = family::create_key(&key)?;
            println!("member {}", created.member_id());
            println!("device {}", created.device_id());
            println!("key {}", key.display());
            Ok(())
        }
        Command::Init {
            to,
            key,
            name,
            apply,
            execute,
        } => {
            if !(apply || execute) {
                println!("dry-run: no library written. Pass --apply or --execute to create it.");
                println!("family init");
                println!("  to: {}", to.display());
                println!("  key: {}", key.display());
                println!("  name: {name}");
                return Ok(());
            }
            let signing = load_or_create(&key)?;
            family::refuse_key_in_library(&to, &key)?;
            let library = family::init_library(&to, &signing, &name)?;
            family::refuse_key_in_library(&library, &key)?;
            println!("library {}", library.display());
            println!("member {}", signing.member_id());
            println!("device {}", signing.device_id());
            Ok(())
        }
        Command::Export {
            library,
            to,
            manifest,
            apply,
            execute,
        } => {
            let manifest_path = manifest.unwrap_or_else(|| to.join("ALBUMEN-MANIFEST.txt"));
            let plan = plan_family_export(&library)?;
            if !(apply || execute) {
                println!("dry-run: no files written. Pass --apply or --execute to copy.");
                println!("family export");
                println!("  library: {}", library.display());
                println!("  to: {}", to.display());
                println!("  manifest: {}", manifest_path.display());
                println!("  members: {}", plan.members.len());
                for member in &plan.members {
                    println!("  member: {member}");
                }
                println!("  files: {}", plan.files);
                return Ok(());
            }
            let report = export_family(&library, &to, &manifest_path)?;
            println!("exported {} files to {}", report.count_out, to.display());
            println!("manifest {}", manifest_path.display());
            println!(
                "{{\"summary\":true,\"count_in\":{},\"count_out\":{},\"mismatched\":{}}}",
                report.count_in, report.count_out, report.mismatched
            );
            Ok(())
        }
        Command::Add {
            library,
            key,
            from,
            sidecar,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no files written. Pass --apply or --execute to copy.");
                println!("family add");
                println!("  library: {}", library.display());
                println!("  member: {}", signing.member_id());
                println!("  from: {}", from.display());
                if let Some(sidecar) = &sidecar {
                    println!("  sidecar: {}", sidecar.display());
                }
                return Ok(());
            }
            let added = family::add_object(&library, &signing, &from, sidecar.as_deref())?;
            println!("sha256 {}", added.sha256);
            println!("bytes {}", added.bytes);
            println!("mtime_ns {}", added.mtime_ns);
            match &added.sidecar_sha256 {
                Some(sha) => println!("sidecar_sha256 {sha}"),
                None => println!("sidecar_sha256 none"),
            }
            println!("namespace members/{}", signing.member_id());
            Ok(())
        }
        Command::Invite {
            library,
            key,
            member,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no invite written. Pass --apply or --execute to sign one.");
                println!("family invite");
                println!("  member: {member}");
                return Ok(());
            }
            let token = family::invite(&library, &signing, &member)?;
            println!("token {token}");
            println!("member {member}");
            Ok(())
        }
        Command::Join {
            library,
            key,
            token,
            name,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no hello written. Pass --apply or --execute to join.");
                println!("family join");
                println!("  name: {name}");
                println!("  member: {}", signing.member_id());
                return Ok(());
            }
            family::join(&library, &signing, &token, &name)?;
            println!("joined {}", signing.member_id());
            Ok(())
        }
        Command::Admit {
            library,
            key,
            member,
            token,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no admit written. Pass --apply or --execute to admit.");
                println!("family admit");
                println!("  member: {member}");
                return Ok(());
            }
            family::admit(&library, &signing, &member, &token)?;
            println!("admitted {member}");
            Ok(())
        }
        Command::Revoke {
            library,
            key,
            member,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!(
                    "dry-run: no revoke written. Pass --apply or --execute to revoke. Originals stay."
                );
                println!("family revoke");
                println!("  member: {member}");
                return Ok(());
            }
            family::revoke(&library, &signing, &member)?;
            println!("revoked {member}");
            println!("originals kept");
            Ok(())
        }
        Command::Hide {
            library,
            key,
            sha256,
            show,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no hide written. Pass --apply or --execute to record it.");
                println!("family hide");
                println!("  sha256: {sha256}");
                println!("  hidden: {}", !show);
                return Ok(());
            }
            family::hide(&library, &signing, &sha256, !show)?;
            println!("viewer {}", signing.member_id());
            println!("sha256 {sha256}");
            println!("hidden {}", !show);
            Ok(())
        }
        Command::Tombstone {
            library,
            key,
            sha256,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no tombstone written. Pass --apply or --execute to record it.");
                println!("family tombstone");
                println!("  sha256: {sha256}");
                return Ok(());
            }
            family::tombstone(&library, &signing, &sha256)?;
            println!("member {}", signing.member_id());
            println!("tombstone {sha256}");
            println!("original kept");
            Ok(())
        }
        Command::AlbumAdd {
            library,
            key,
            album,
            sha256,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no album record written. Pass --apply or --execute to add it.");
                println!("family album-add");
                println!("  album: {album}");
                println!("  sha256: {sha256}");
                return Ok(());
            }
            let id = family::album_add(&library, &signing, &album, &sha256)?;
            println!("add {id}");
            Ok(())
        }
        Command::AlbumOmit {
            library,
            key,
            add,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!("dry-run: no album record written. Pass --apply or --execute to omit.");
                println!("family album-omit");
                println!("  adds: {}", add.len());
                return Ok(());
            }
            family::album_omit(&library, &signing, &add)?;
            println!("omitted {}", add.len());
            Ok(())
        }
        Command::Manifest {
            library,
            key,
            summary,
            viewer,
            apply,
            execute,
        } => {
            if summary {
                print!("{}", summary_text(&library, viewer.as_deref())?);
                return Ok(());
            }
            if let Some(key) = key {
                let signing = family::load_key(&key)?;
                if apply || execute {
                    let json = family::sign_manifest(&library, &signing)?;
                    println!("{json}");
                    return Ok(());
                }
                println!("dry-run: snapshot not written. Pass --apply or --execute to sign one.");
            }
            let json = family::manifest_json(&library)?;
            println!("{json}");
            Ok(())
        }
        Command::Sync {
            from,
            to,
            apply,
            execute,
        } => {
            if !(apply || execute) {
                let plan = plan_family_sync(&from, &to)?;
                println!("dry-run: no files written. Pass --apply or --execute to sync.");
                println!("family sync");
                println!("  from: {}", from.display());
                println!("  to: {}", to.display());
                println!("  same: {}", plan.same);
                println!("  add: {}", plan.add.len());
                for name in &plan.add {
                    println!("  + {name}");
                }
                return Ok(());
            }
            let report = sync_family(&from, &to)?;
            println!("family sync");
            println!("  same: {}", report.same);
            println!("  added: {}", report.added.len());
            for name in &report.added {
                println!("  + {name}");
            }
            println!("removed: 0");
            println!("replaced: 0");
            Ok(())
        }
        Command::Policy {
            bucket,
            prefix,
            member,
        } => {
            let policy = family::s3_member_policy(&bucket, &prefix, &member)?;
            println!("{policy}");
            Ok(())
        }
        Command::Publish {
            library,
            key,
            prefix,
            apply,
            execute,
        } => {
            let signing = family::load_key(&key)?;
            if !(apply || execute) {
                println!(
                    "dry-run: no objects written. Pass --apply or --execute to publish this member's namespace."
                );
                println!("family publish");
                println!("  member: {}", signing.member_id());
                println!("  prefix: {prefix}");
                return Ok(());
            }
            let mut config = albumen_ingest::S3Config::from_env()?;
            if !prefix.is_empty() {
                config.prefix = prefix;
            }
            albumen_ingest::ledger::anchor(&library, "export");
            let store = albumen_ingest::S3Store::connect(config)?;
            let count = family::publish_member_s3(&library, signing.member_id(), &store)?;
            println!("published {count}");
            println!("member {}", signing.member_id());
            Ok(())
        }
    }
}

/// The words `albumen family manifest --summary` prints, one line each.
///
/// The GUI's read-only Family view shows exactly this text, so the two never
/// drift. A revoked member's photos stay and are still counted.
pub fn summary_text(library: &Path, viewer: Option<&str>) -> Result<String, IngestError> {
    let json = family::manifest_json(library)?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|err| IngestError::Family {
            message: format!("family manifest: {err}"),
        })?;
    let list = |key: &str| -> Vec<serde_json::Value> {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let text = |entry: &serde_json::Value, key: &str| -> String {
        entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let members: Vec<_> = list("members")
        .into_iter()
        .filter(|m| m.get("admitted").and_then(serde_json::Value::as_bool) == Some(true))
        .collect();
    let objects = list("objects");
    let revoked = members
        .iter()
        .filter(|m| m.get("revoked").and_then(serde_json::Value::as_bool) == Some(true))
        .count();
    let photos = |n: usize| {
        if n == 1 {
            "1 photo".to_string()
        } else {
            format!("{n} photos")
        }
    };
    let mut out = String::new();
    out.push_str(&format!("family library {}\n", text(&value, "library_id")));
    out.push_str(&format!(
        "{} members: {} active, {revoked} revoked\n",
        members.len(),
        members.len() - revoked
    ));
    out.push_str(&format!(
        "{} in all members' folders\n",
        photos(objects.len())
    ));
    // Sorted by the name shown, then id, so the CLI and the GUI list members
    // in the same order.
    let mut rows: Vec<(String, String, &serde_json::Value)> = members
        .iter()
        .map(|member| {
            let id = text(member, "id");
            let name = text(member, "name");
            let label = if name.is_empty() {
                id.chars().take(8).collect()
            } else {
                name
            };
            (label, id, member)
        })
        .collect();
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    for (label, id, member) in rows {
        let short: String = id.chars().take(8).collect();
        let count = objects.iter().filter(|o| text(o, "member") == id).count();
        let mut line = format!("{label} ({short}): {}", photos(count));
        if member.get("revoked").and_then(serde_json::Value::as_bool) == Some(true) {
            line.push_str(", revoked; their photos stay");
        }
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(viewer) = viewer {
        let viewer = family::canonical_member_id(viewer)?;
        let hidden = list("hides")
            .iter()
            .filter(|h| {
                text(h, "viewer") == viewer
                    && h.get("hidden").and_then(serde_json::Value::as_bool) == Some(true)
                    && objects
                        .iter()
                        .any(|o| text(o, "sha256") == text(h, "sha256"))
            })
            .count();
        let short: String = viewer.chars().take(8).collect();
        out.push_str(&format!(
            "viewer {short}: {} of {} photos shown, {hidden} hidden\n",
            objects.len() - hidden,
            objects.len()
        ));
    }
    Ok(out)
}

fn load_or_create(path: &Path) -> Result<FamilyKey, IngestError> {
    if path.is_file() {
        family::load_key(path)
    } else {
        family::create_key(path)
    }
}
