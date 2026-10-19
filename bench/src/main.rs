// SPDX-License-Identifier: Apache-2.0

//! Compare a fresh run with `baseline.json`, or print one without comparing.
//!
//! `--check` is the default. It fails when the median is more than 10 percent
//! worse than the baseline, and it refuses a library size the baseline was
//! not recorded at. `--measure` prints the run and does not compare.
//! `--write-baseline` replaces the file. `--takeout` times a generated
//! 100,000-member Takeout import and is not part of `--check`.
//! `--throughput` imports 1,000 synthetic files of about 5 MB through
//! `copy_verify` and compares that rate with a raw durable write.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use albumen_bench::{
    load_baseline, measure, measure_icloud, measure_takeout, measure_throughput,
    regression_problems, scale_problems, throughput_problems, to_json, Report, DEFAULT_COPY_BYTES,
    DEFAULT_COPY_FILES, DEFAULT_SEARCH_ITEMS, DEFAULT_TAKEOUT_ITEMS, DEFAULT_THROUGHPUT_BYTES,
    DEFAULT_THROUGHPUT_FILES, TAKEOUT_BUDGET_SECONDS, THROUGHPUT_RATIO_FLOOR,
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
            };
            let mismatch = scale_problems(&baseline, &probe);
            if !mismatch.is_empty() {
                return Err(mismatch.join("\n"));
            }
            let observed = measure(search_items, copy_files, copy_bytes)?;
            print_report(&observed)?;
            let problems = regression_problems(&baseline, &observed);
            if !problems.is_empty() {
                return Err(problems.join("\n"));
            }
            eprintln!(
                "albumen-bench: within 10 percent of {}",
                invocation.baseline.display()
            );
        }
        Mode::Measure => {
            let observed = measure(search_items, copy_files, copy_bytes)?;
            print_report(&observed)?;
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
            if scale.seconds > TAKEOUT_BUDGET_SECONDS {
                return Err(format!(
                    "takeout import of {} items took {:.1}s, budget is {:.0}s",
                    scale.items, scale.seconds, TAKEOUT_BUDGET_SECONDS
                ));
            }
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
            if scale.seconds > TAKEOUT_BUDGET_SECONDS {
                return Err(format!(
                    "icloud import of {} items took {:.1}s, budget is {:.0}s",
                    scale.items, scale.seconds, TAKEOUT_BUDGET_SECONDS
                ));
            }
        }
        Mode::Throughput => {
            let files = env_u64("ALBUMEN_THROUGHPUT_FILES", DEFAULT_THROUGHPUT_FILES)?;
            let bytes = env_u64("ALBUMEN_THROUGHPUT_BYTES", DEFAULT_THROUGHPUT_BYTES)?;
            let allow_small =
                env::var("ALBUMEN_THROUGHPUT_ALLOW_SMALL").ok().as_deref() == Some("1");
            if !allow_small
                && (files < DEFAULT_THROUGHPUT_FILES || bytes < DEFAULT_THROUGHPUT_BYTES)
            {
                return Err(
                    "the throughput gate uses 1000 files of 5000000 bytes. Set ALBUMEN_THROUGHPUT_ALLOW_SMALL=1 for a smaller smoke run."
                        .to_string(),
                );
            }
            let leg = env::var("ALBUMEN_THROUGHPUT_LEG")
                .unwrap_or_else(|_| std::env::consts::OS.to_string());
            let report = measure_throughput(&leg, files, bytes)?;
            let text =
                serde_json::to_string_pretty(&report.to_json()).map_err(|err| err.to_string())?;
            println!("{text}");
            io::stdout().flush().map_err(|err| err.to_string())?;
            eprintln!(
                "import-throughput leg={} files={} bytes_each={} import_mb_s={:.3} raw_mb_s={:.3} ratio={:.4} target={:.3} reconciled_percent={:.1} mismatched={} source_changed={}",
                report.leg,
                report.files,
                report.bytes_each,
                report.import_mb_s,
                report.raw_mb_s,
                report.ratio,
                THROUGHPUT_RATIO_FLOOR,
                report.reconciled_percent(),
                report.mismatched,
                report.source_changed
            );
            let problems = throughput_problems(&report);
            if !problems.is_empty() {
                return Err(problems.join("\n"));
            }
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
    Throughput,
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
            "--throughput" => set_mode(&mut mode, Mode::Throughput)?,
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
            "pass only one of --check, --measure, --write-baseline, --takeout, --icloud, or --throughput"
                .to_string(),
        );
    }
    *slot = Some(mode);
    Ok(())
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
