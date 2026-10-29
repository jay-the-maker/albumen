// SPDX-License-Identifier: Apache-2.0

//! `albumen status` on synthetic backups: fresh, stale, damaged, never
//! backed up, unreadable, and several at once. Wording and exit codes follow
//! the status spec v1. The command is read-only: every destination file's
//! bytes and mtime are the same after it runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

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

/// A synthetic library of `n` files exported to `dest`.
fn backup(tmp: &Path, name: &str, n: usize) -> PathBuf {
    let library = tmp.join(format!("{name}-library"));
    fs::create_dir_all(library.join("roll")).unwrap();
    for i in 0..n {
        fs::write(
            library.join("roll").join(format!("IMG_{i:04}.jpg")),
            format!("synthetic {name} {i}"),
        )
        .unwrap();
    }
    let dest = tmp.join(name);
    let (code, text) = albumen(&["export", "--from", p(&library), "--to", p(&dest), "--apply"]);
    assert_eq!(code, 0, "{text}");
    dest
}

fn age_record(dest: &Path, by: Duration) {
    let record = fs::File::options()
        .write(true)
        .open(dest.join("ALBUMEN-MANIFEST.txt"))
        .unwrap();
    record.set_modified(SystemTime::now() - by).unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = fs::read_dir(&dir) else {
            continue;
        };
        for item in listing.flatten() {
            let path = item.path();
            let meta = fs::metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path.clone());
            }
            let bytes = if meta.is_file() {
                fs::read(&path).unwrap()
            } else {
                Vec::new()
            };
            out.insert(path, (bytes, meta.modified().unwrap()));
        }
    }
    out
}

fn status(args: &[&str]) -> (i32, String) {
    let mut all = vec!["status"];
    all.extend_from_slice(args);
    albumen(&all)
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

fn assert_plain_words(text: &str) {
    let lower = text.to_lowercase();
    for word in ["manifest", "hash", "sha", "drift"] {
        assert!(
            !lower.contains(word),
            "{word:?} in user-facing output:\n{text}"
        );
    }
}

fn assert_next_rules(code: i32, text: &str) {
    let last = text.lines().last().unwrap_or("");
    if code == 0 {
        assert!(!text.lines().any(|l| l.starts_with("next:")), "{text}");
    } else {
        assert!(
            last.starts_with("next: "),
            "nonzero exit must end on next:\n{text}"
        );
    }
    let (_, completions) = albumen(&["completions", "bash"]);
    for line in text.lines().filter(|l| l.starts_with("next:")) {
        if let Some(rest) = line.split("albumen ").nth(1) {
            let command = rest.split_whitespace().next().unwrap();
            assert!(
                completions.contains(command),
                "next: names {command:?}, which completions do not list"
            );
        }
    }
}

#[test]
fn fresh_stale_damaged_never_and_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let fresh = backup(tmp.path(), "fresh", 12);
    age_record(&fresh, Duration::from_secs(2 * 3_600 + 30));
    let before = snapshot(&fresh);
    let (code, text) = status(&[p(&fresh)]);
    assert_eq!(code, 0, "{text}");
    let l = lines(&text);
    assert_eq!(l.len(), 3, "{text}");
    assert!(
        l[0].starts_with("ok: backup checked 2 hours ago (") && l[0].ends_with(" UTC)"),
        "{text}"
    );
    assert_eq!(l[1], "12 items, all match your originals");
    assert_eq!(l[2], p(&fresh));
    assert_plain_words(&text);
    assert_next_rules(code, &text);

    // --max-age 1h makes the fresh fixture stale.
    let (code, text) = status(&[p(&fresh), "--max-age", "1h"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.starts_with("stale: last good backup was 2 hours ago ("),
        "{text}"
    );
    assert!(
        text.lines().next().unwrap().ends_with("), limit is 1 hour"),
        "{text}"
    );
    assert_eq!(before, snapshot(&fresh), "status changed the destination");

    let stale = backup(tmp.path(), "stale", 4);
    age_record(&stale, Duration::from_secs(12 * 86_400 + 60));
    let (code, text) = status(&[p(&stale)]);
    assert_eq!(code, 1, "{text}");
    let l = lines(&text);
    assert!(
        l[0].starts_with("stale: last good backup was 12 days ago ("),
        "{text}"
    );
    assert!(l[0].ends_with(" UTC), limit is 7 days"), "{text}");
    assert_eq!(
        l[1],
        "4 items were verified then; changes since are not checked"
    );
    assert_eq!(l[2], p(&stale));
    assert_eq!(
        l[3],
        format!(
            "next: albumen backup --from <your photos folder> --to {}",
            p(&stale)
        )
    );
    assert_eq!(l.len(), 4);
    assert_plain_words(&text);
    assert_next_rules(code, &text);

    let damaged = backup(tmp.path(), "damaged", 6);
    let flip = damaged.join("roll/IMG_0001.jpg");
    let mut bytes = fs::read(&flip).unwrap();
    bytes[0] ^= 1;
    fs::write(&flip, bytes).unwrap();
    fs::remove_file(damaged.join("roll/IMG_0004.jpg")).unwrap();
    let before = snapshot(&damaged);
    let (code, text) = status(&[p(&damaged)]);
    assert_eq!(code, 1, "{text}");
    assert_eq!(
        lines(&text),
        vec![
            "damaged: 2 of 6 items no longer match what was backed up",
            "1 changed, 1 missing, 4 fine",
            p(&damaged),
            "your originals were not touched",
            "  changed: roll/IMG_0001.jpg",
            "  missing: roll/IMG_0004.jpg",
            &format!(
                "next: albumen backup --from <your photos folder> --to {}",
                p(&damaged)
            ),
        ]
    );
    assert_plain_words(&text);
    assert_next_rules(code, &text);
    assert_eq!(before, snapshot(&damaged), "status changed the destination");

    let never = tmp.path().join("never");
    fs::create_dir_all(&never).unwrap();
    let (code, text) = status(&[p(&never)]);
    assert_eq!(code, 3, "{text}");
    assert_eq!(
        lines(&text),
        vec![
            "not backed up: no Albumen backup found here",
            p(&never),
            &format!(
                "next: albumen backup --from <your photos folder> --to {}",
                p(&never)
            ),
        ]
    );
    assert_plain_words(&text);
    assert_next_rules(code, &text);
    assert!(
        fs::read_dir(&never).unwrap().next().is_none(),
        "status wrote into an empty folder"
    );

    let gone = tmp.path().join("unplugged");
    let (code, text) = status(&[p(&gone)]);
    assert_eq!(code, 4, "{text}");
    assert!(text.starts_with("unreadable: "), "{text}");
    assert_next_rules(code, &text);
    assert!(!gone.exists());
}

#[test]
fn several_destinations_report_each_and_exit_with_the_worst() {
    let tmp = tempfile::tempdir().unwrap();
    let fresh = backup(tmp.path(), "a", 3);
    let stale = backup(tmp.path(), "b", 3);
    age_record(&stale, Duration::from_secs(9 * 86_400));
    let never = tmp.path().join("c");
    fs::create_dir_all(&never).unwrap();
    let (code, text) = status(&[p(&fresh), p(&stale), p(&never)]);
    assert_eq!(code, 3, "{text}");
    let blocks: Vec<&str> = text.split("\n\n").collect();
    assert_eq!(blocks.len(), 4, "{text}");
    assert!(blocks[0].starts_with("ok: "));
    assert!(blocks[1].starts_with("stale: "));
    assert!(blocks[2].starts_with("not backed up: "));
    assert!(
        blocks[3].starts_with("3 destinations: 1 ok, 1 stale, 1 not backed up\n"),
        "{text}"
    );
    assert_plain_words(&text);
    assert_next_rules(code, &text);

    // An unreadable destination does not stop the others, and 4 wins.
    let gone = tmp.path().join("gone");
    let (code, text) = status(&[p(&gone), p(&fresh)]);
    assert_eq!(code, 4, "{text}");
    assert!(text.contains("\n\nok: backup checked"), "{text}");
    assert!(
        text.contains("2 destinations: 1 ok, 1 unreadable"),
        "{text}"
    );
    assert_next_rules(code, &text);
}

#[test]
fn bad_max_age_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["7m", "0d", "week"] {
        let (code, text) = status(&[p(tmp.path()), "--max-age", bad]);
        assert_eq!(code, 2, "{bad}: {text}");
    }
    let (code, _) = status(&[]);
    assert_eq!(code, 2);
}
