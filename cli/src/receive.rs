// SPDX-License-Identifier: Apache-2.0

//! `albumen receive hide|unhide`: copy-local hide records in a receiver
//! library (#196).
//!
//! A hide writes `{name}.albumen-tombstone` beside the stored original. An
//! unhide rewrites that tombstone with `"hidden":false`. Neither unlinks,
//! renames or rewrites the original, and neither removes the tombstone.
//! Without `--apply` or `--execute` the command prints the plan and writes
//! nothing.

use std::path::PathBuf;

use albumen_ingest::IngestError;
use albumen_receive::{HideState, Library};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Hide a stored original on this copy. The original stays.
    Hide(Target),
    /// Show a hidden original again. The tombstone is rewritten, not removed.
    Unhide(Target),
}

#[derive(Debug, clap::Args)]
pub struct Target {
    /// Receiver library root, or the family library with `--member`.
    #[arg(long)]
    library: PathBuf,
    /// Stored file name in the library root (or the member namespace).
    #[arg(long)]
    name: String,
    /// Joined member id. Opens `members/<member>/` inside a family library;
    /// a name outside that namespace is refused.
    #[arg(long)]
    member: Option<String>,
    /// Write the tombstone. Without this or `--execute`, print the plan only.
    #[arg(long)]
    apply: bool,
    /// Same as `--apply`.
    #[arg(long)]
    execute: bool,
}

fn refuse(target: &Target, message: impl Into<String>) -> IngestError {
    IngestError::Refused {
        message: format!("{}: {}", target.library.display(), message.into()),
    }
}

pub fn run(command: Command) -> Result<(), IngestError> {
    let (target, hidden) = match command {
        Command::Hide(target) => (target, true),
        Command::Unhide(target) => (target, false),
    };
    // Library::open creates a missing root, so check first: a dry run must
    // not create anything.
    if !target.library.is_dir() {
        return Err(refuse(&target, "library is not an existing directory"));
    }
    let library = match &target.member {
        Some(member) => Library::open_member_namespace(&target.library, member),
        None => Library::open(&target.library),
    }
    .map_err(|err| refuse(&target, err.to_string()))?;
    let plan = library
        .plan_hide(&target.name, hidden)
        .map_err(|err| refuse(&target, err))?;
    let verb = if hidden { "hide" } else { "unhide" };
    let state = |state: HideState| match state {
        HideState::Hidden => "hidden",
        HideState::Visible => "visible",
    };
    if !(target.apply || target.execute) {
        println!("dry-run: no tombstone written. Pass --apply or --execute to record it.");
        println!("receive {verb}");
        println!("  name: {}", target.name);
        println!("  original: {}", plan.original.display());
        println!("  tombstone: {}", plan.tombstone.display());
        println!("  now: {}", state(plan.current));
        println!("  change: {}", plan.change);
        return Ok(());
    }
    if !plan.change {
        println!("already {} {}", state(plan.current), target.name);
        println!("original kept");
        return Ok(());
    }
    if hidden {
        library.hide(&target.name)
    } else {
        library.unhide(&target.name).map(|_| ())
    }
    .map_err(|err| refuse(&target, err))?;
    println!(
        "{} {}",
        if hidden { "hidden" } else { "visible" },
        target.name
    );
    println!("original kept");
    Ok(())
}
