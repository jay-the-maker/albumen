// SPDX-License-Identifier: Apache-2.0

//! A killed export resumes. `albumen export --apply` is sent SIGKILL at a
//! seeded point, then the same command runs again. The final tree passes
//! verify-manifest, no partial copy or manifest temp file is left, and every
//! file that already had its final name before the rerun is counted as
//! already present (checked against the source by SHA-256), not copied
//! again. Synthetic bytes only; the library is never written.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const FILES: usize = 2_000;
const SEEDS: [u64; 5] = [1, 2, 3, 4, 5];

/// Seeded kill delays in 1..=max_ms (64-bit LCG, PCG constants).
fn kill_delays_ms(seed: u64, count: usize, max_ms: u64) -> Vec<u64> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let bound = max_ms.max(1);
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            1 + ((state >> 33) % bound)
        })
        .collect()
}

fn albumen(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn make_library(root: &Path) {
    for i in 0..FILES {
        let dir = root.join(format!("roll{:02}", i / 200));
        fs::create_dir_all(&dir).unwrap();
        let body = format!("synthetic resume file {i} {}", "x".repeat(i % 97));
        fs::write(dir.join(format!("IMG_{i:05}.jpg")), body).unwrap();
    }
}

/// Every file under `root`, relative, split into final names and leftovers.
fn tree(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut finals = Vec::new();
    let mut leftovers = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = fs::read_dir(&dir) else {
            continue;
        };
        for item in listing.flatten() {
            let path = item.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if name.ends_with(".albumen-partial") || name.ends_with(".tmp") {
                leftovers.push(relative);
            } else if name != "ALBUMEN-MANIFEST.txt" {
                finals.push(relative);
            }
        }
    }
    (finals, leftovers)
}

fn counts(text: &str) -> (usize, usize) {
    let line = text
        .lines()
        .find(|line| line.starts_with("exported "))
        .unwrap_or_else(|| panic!("no summary line: {text}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    (words[1].parse().unwrap(), words[3].parse().unwrap())
}

#[test]
fn delays_are_seeded_in_range_and_differ_by_seed() {
    assert_eq!(kill_delays_ms(7, 20, 900), kill_delays_ms(7, 20, 900));
    assert!(kill_delays_ms(9, 500, 900)
        .iter()
        .all(|d| (1..=900).contains(d)));
    assert!(kill_delays_ms(3, 10, 0).iter().all(|d| *d == 1));
    let firsts: std::collections::BTreeSet<u64> = SEEDS
        .iter()
        .map(|s| kill_delays_ms(*s, 1, 1_000)[0])
        .collect();
    assert_eq!(firsts.len(), SEEDS.len());
}

#[test]
fn a_killed_export_resumes_without_half_files_or_recopies() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    make_library(&library);
    let mut interrupted = 0;
    for seed in SEEDS {
        let dest = tmp.path().join(format!("export-{seed}"));
        let delay = kill_delays_ms(seed, 1, 1_500)[0];
        let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
            .args(["export", "--from", p(&library), "--to", p(&dest), "--apply"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(delay));
        if child.try_wait().unwrap().is_none() {
            interrupted += 1;
        }
        child.kill().ok(); // SIGKILL
        child.wait().unwrap();

        let (before, stale) = tree(&dest);
        eprintln!(
            "seed {seed}: killed at {delay}ms, {} final files, {} leftovers",
            before.len(),
            stale.len()
        );
        let (code, text) = albumen(&["export", "--from", p(&library), "--to", p(&dest), "--apply"]);
        assert_eq!(code, 0, "seed {seed} delay {delay}ms rerun: {text}");
        let (copied, present) = counts(&text);
        assert_eq!(copied + present, FILES, "seed {seed}: {text}");
        assert_eq!(
            present,
            before.len(),
            "seed {seed}: final files before rerun must count as present"
        );

        let manifest = dest.join("ALBUMEN-MANIFEST.txt");
        let (code, text) = albumen(&[
            "verify-manifest",
            "--manifest",
            p(&manifest),
            "--against",
            p(&dest),
        ]);
        assert_eq!(code, 0, "seed {seed} verify: {text}");
        let (finals, leftovers) = tree(&dest);
        assert_eq!(finals.len(), FILES, "seed {seed}");
        assert!(leftovers.is_empty(), "seed {seed} left {leftovers:?}");
    }
    assert!(interrupted >= 1, "no kill landed mid-export; raise FILES");
}
