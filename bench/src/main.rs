// SPDX-License-Identifier: Apache-2.0

//! Compare a fresh run with `baseline.json`, or print one without comparing.
//!
//! `--check` is the default. It fails when bytes hashed, files copied, or
//! files returned do not match the scale in the baseline. It does not fail
//! on MB/s or milliseconds. `--measure` prints the run and does not compare.
//! `--write-baseline` replaces the file. `--takeout` times a generated
//! 100,000-member Takeout import and is not part of `--check`. The Takeout
//! hour budget is printed and does not fail the process.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use albumen_bench::{
    load_baseline, measure, measure_icloud, measure_takeout, regression_problems, scale_problems,
    soft_budget_note, soft_budgets_from_env, to_json, Report, DEFAULT_COPY_BYTES,
    DEFAULT_COPY_FILES, DEFAULT_SEARCH_ITEMS, DEFAULT_TAKEOUT_ITEMS, TAKEOUT_BUDGET_SECONDS,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("albumen-bench: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let invocation = parse(&args)?;
    let (search_items, copy_files, copy_bytes) = config_from_env()?;
    match invocation.mode {
        Mode::Check => {
            let baseline = load_baseline(&invocation.baseline)?;
            let probe = Report {
                search_items,
                copy_files,
                copy_bytes_each: copy_bytes,
                copy_verify_mb_s: 0.0,
                copy_verify_items_s: 0.0,
                search_cold_ms: 0.0,
                search_warm_p50_ms: 0.0,
                search_warm_p95_ms: 0.0,
                mcp_browse_p50_ms: 0.0,
                copy_verify_vs_plain: 0.0,
                search_cold_vs_exif: 0.0,
                search_warm_p50_vs_hash: 0.0,
                search_warm_p95_vs_hash: 0.0,
                browse_vs_walk: 0.0,
                bytes_hashed: copy_files.saturating_mul(copy_bytes),
                items_copied: copy_files,
                search_files: search_items.saturating_mul(2),
                browse_files: search_items.saturating_mul(2),
            };
            let mismatch = scale_problems(&baseline, &probe);
            if !mismatch.is_empty() {
                return Err(mismatch.join("\n"));
            }
            let observed = measure(search_items, copy_files, copy_bytes)?;
            print_report(&observed)?;
            eprintln!("{}", soft_budget_note(&observed, &soft_budgets_from_env()?));
            let problems = regression_problems(&baseline, &observed);
            if !problems.is_empty() {
                return Err(problems.join("\n"));
            }
            eprintln!(
                "albumen-bench: counters match {}",
                invocation.baseline.display()
            );
        }
        Mode::Measure => {
            let observed = measure(search_items, copy_files, copy_bytes)?;
            print_report(&observed)?;
            eprintln!("{}", soft_budget_note(&observed, &soft_budgets_from_env()?));
        }
        Mode::Write => {
            let observed = measure(search_items, copy_files, copy_bytes)?;
            let text = pretty(&observed)?;
            fs::write(&invocation.baseline, format!("{text}\n"))
                .map_err(|err| format!("write {}: {err}", invocation.baseline.display()))?;
            print_report(&observed)?;
            eprintln!("wrote {}", invocation.baseline.display());
        }
        Mode::Takeout => {
            let items = env_u64("ALBUMEN_TAKEOUT_ITEMS", DEFAULT_TAKEOUT_ITEMS)?;
            let scale = measure_takeout(items)?;
            let text =
                serde_json::to_string_pretty(&scale.to_json()).map_err(|err| err.to_string())?;
            println!("{text}");
            io::stdout().flush().map_err(|err| err.to_string())?;
            if !scale.reconciled() {
                return Err(format!(
                    "takeout reconciliation failed: count_in {} count_out {} mismatched {}",
                    scale.count_in, scale.count_out, scale.mismatched
                ));
            }
            eprintln!(
                "{}",
                takeout_budget_note(scale.items, scale.seconds, "takeout")?
            );
        }
        Mode::Icloud => {
            let items = env_u64("ALBUMEN_TAKEOUT_ITEMS", DEFAULT_TAKEOUT_ITEMS)?;
            let scale = measure_icloud(items)?;
            let text =
                serde_json::to_string_pretty(&scale.to_json()).map_err(|err| err.to_string())?;
            println!("{text}");
            io::stdout().flush().map_err(|err| err.to_string())?;
            if !scale.reconciled() {
                return Err(format!(
                    "icloud reconciliation failed: count_in {} count_out {} mismatched {}",
                    scale.count_in, scale.count_out, scale.mismatched
                ));
            }
            eprintln!(
                "{}",
                takeout_budget_note(scale.items, scale.seconds, "icloud")?
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Mode {
    Check,
    Measure,
    Write,
    Takeout,
    Icloud,
}

struct Invocation {
    mode: Mode,
    baseline: PathBuf,
}

fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut mode = None;
    let mut baseline = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--check" => set_mode(&mut mode, Mode::Check)?,
            "--measure" => set_mode(&mut mode, Mode::Measure)?,
            "--write-baseline" => set_mode(&mut mode, Mode::Write)?,
            "--takeout" => set_mode(&mut mode, Mode::Takeout)?,
            "--icloud" => set_mode(&mut mode, Mode::Icloud)?,
            "--baseline" => {
                index += 1;
                let path = args
                    .get(index)
                    .ok_or_else(|| "--baseline needs a path".to_string())?;
                baseline = Some(PathBuf::from(path));
            }
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(Invocation {
        mode: mode.unwrap_or(Mode::Check),
        baseline: baseline.unwrap_or_else(default_baseline),
    })
}

fn set_mode(slot: &mut Option<Mode>, mode: Mode) -> Result<(), String> {
    if slot.is_some() {
        return Err(
            "pass only one of --check, --measure, --write-baseline, --takeout, or --icloud"
                .to_string(),
        );
    }
    *slot = Some(mode);
    Ok(())
}

fn takeout_budget_note(items: u64, seconds: f64, kind: &str) -> Result<String, String> {
    let budget = match env::var("ALBUMEN_TAKEOUT_BUDGET_SECONDS") {
        Err(_) => TAKEOUT_BUDGET_SECONDS,
        Ok(text) if text.trim().is_empty() => TAKEOUT_BUDGET_SECONDS,
        Ok(text) => text.trim().parse::<f64>().map_err(|_| {
            format!("ALBUMEN_TAKEOUT_BUDGET_SECONDS must be a number, got {text:?}")
        })?,
    };
    Ok(format!(
        "soft budget (reported only, not a CI failure): {kind} import of {items} items took {seconds:.3}s, budget {budget:.0}s (ALBUMEN_TAKEOUT_BUDGET_SECONDS)"
    ))
}

fn default_baseline() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

fn config_from_env() -> Result<(u64, u64, u64), String> {
    Ok((
        env_u64("ALBUMEN_BENCH_ITEMS", DEFAULT_SEARCH_ITEMS)?,
        env_u64("ALBUMEN_BENCH_COPY_FILES", DEFAULT_COPY_FILES)?,
        env_u64("ALBUMEN_BENCH_COPY_BYTES", DEFAULT_COPY_BYTES)?,
    ))
}

fn env_u64(name: &str, default: u64) -> Result<u64, String> {
    match env::var(name) {
        Err(_) => Ok(default),
        Ok(text) if text.is_empty() => Ok(default),
        Ok(text) => text
            .parse()
            .map_err(|_| format!("{name} must be an integer")),
    }
}

fn pretty(report: &Report) -> Result<String, String> {
    serde_json::to_string_pretty(&to_json(report)).map_err(|err| err.to_string())
}

fn print_report(report: &Report) -> Result<(), String> {
    println!("{}", pretty(report)?);
    io::stdout().flush().map_err(|err| err.to_string())
}
