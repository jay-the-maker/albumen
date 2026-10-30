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

/// The stamp before `tail` is "YYYY-MM-DD HH:MM UTC", "UTC-7" or
/// "UTC+5:30": a numeric offset, never an abbreviation such as PDT.
fn labelled(line: &str, tail: &str) -> bool {
    let Some(head) = line.strip_suffix(tail) else {
        return false;
    };
    let Some((_, zone)) = head.rsplit_once(' ') else {
        return false;
    };
    let Some(offset) = zone.strip_prefix("UTC") else {
        return false;
    };
    let date = &head[head.len() - zone.len() - 17..head.len() - zone.len() - 1];
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    date.len() == 16
        && digits(&date[0..4])
        && digits(&date[11..13])
        && (offset.is_empty()
            || (offset.starts_with(['+', '-'])
                && offset[1..].chars().all(|c| c.is_ascii_digit() || c == ':')))
}

/// Linux and macOS honour `TZ`: India shows "UTC+5:30", UTC shows "UTC".
#[cfg(unix)]
#[test]
fn stamps_follow_tz_with_numeric_offsets() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = backup(tmp.path(), "zoned", 1);
    for (tz, zone) in [
        ("Asia/Kolkata", " UTC+5:30)"),
        ("UTC", " UTC)"),
        ("America/Los_Angeles", " UTC-"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
            .args(["status", p(&dest)])
            .env("TZ", tz)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next().unwrap();
        assert!(first.contains(zone), "TZ={tz}: {text}");
        assert!(labelled(first, ")"), "TZ={tz}: {text}");
    }
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
        l[0].starts_with("ok: backup checked 2 hours ago (") && labelled(l[0], ")"),
        "{text}"
    );
    assert_eq!(l[1], "12 items, all match what was backed up");
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
    assert!(labelled(l[0], "), limit is 7 days"), "{text}");
    assert_eq!(
        l[1],
        "4 items were verified then; changes since are not checked"
    );
    assert_eq!(l[2], p(&stale));
    assert_eq!(
        l[3],
        format!(
            "next: albumen backup --from <your photos folder> --to {}",
            sq(p(&stale))
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
            "status only read the backup; your photos folder was not opened",
            "  changed: roll/IMG_0001.jpg",
            "  missing: roll/IMG_0004.jpg",
            &format!(
                "next: move the changed copy aside, for example: mv {} {}",
                sq(&format!("{}/roll/IMG_0001.jpg", p(&damaged))),
                sq(&format!("{}/roll/IMG_0001.jpg.changed", p(&damaged)))
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
                "next: to back up this folder, run albumen backup --from {} --to <your backup disk>",
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
        blocks[3].starts_with("3 backups: 1 ok, 1 stale, 1 not backed up\n"),
        "{text}"
    );
    assert_plain_words(&text);
    assert_next_rules(code, &text);

    // An unreadable destination does not stop the others, and 4 wins.
    let gone = tmp.path().join("gone");
    let (code, text) = status(&[p(&gone), p(&fresh)]);
    assert_eq!(code, 4, "{text}");
    assert!(text.contains("\n\nok: backup checked"), "{text}");
    assert!(text.contains("2 backups: 1 ok, 1 unreadable"), "{text}");
    assert_next_rules(code, &text);
}

/// Single-quoted for a shell, as status prints destinations in next: lines.
/// What the CLI's shell quoting gives `text`: bare when it is only safe
/// characters, else single-quoted. Windows temp paths carry backslashes, so they
/// are quoted there and bare on Unix.
fn sq(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:=+@".contains(c))
    {
        text.to_string()
    } else {
        q(text)
    }
}

fn q(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[test]
fn next_lines_quote_the_destination_and_changed_copies_are_moved_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = backup(tmp.path(), "Photo Backup's", 4);
    let d = p(&dest).to_string();

    // Only missing items: rerunning the backup puts them back.
    fs::remove_file(dest.join("roll/IMG_0002.jpg")).unwrap();
    let (code, text) = status(&[&d]);
    assert_eq!(code, 1, "{text}");
    assert_eq!(
        text.lines().last().unwrap(),
        format!(
            "next: albumen backup --from <your photos folder> --to {}",
            q(&d)
        )
    );
    assert_next_rules(code, &text);

    // A changed copy: a rerun would refuse it, so the next step is to move
    // it aside, and no albumen command is suggested.
    for name in ["IMG_0000.jpg", "IMG_0003.jpg"] {
        let flip = dest.join("roll").join(name);
        let mut bytes = fs::read(&flip).unwrap();
        bytes[0] ^= 1;
        fs::write(&flip, bytes).unwrap();
    }
    let before = snapshot(&dest);
    let (code, text) = status(&[&d]);
    assert_eq!(code, 1, "{text}");
    let first = format!("{d}/roll/IMG_0000.jpg");
    assert_eq!(
        text.lines().last().unwrap(),
        format!(
            "next: move the changed copy aside (and 1 other changed file), for example: mv {} {}",
            q(&first),
            q(&format!("{first}.changed"))
        )
    );
    // The temp path itself may contain "albumen" (e.g. a checkout under
    // sl-albumen), so look only at the words around the quoted paths.
    let words = text.lines().last().unwrap().replace(&q(&first), "");
    assert!(!words.contains("albumen"), "{text}");
    assert_plain_words(&text);
    assert_next_rules(code, &text);
    assert_eq!(before, snapshot(&dest), "status changed the destination");

    // Never backed up and unreadable also quote it.
    let never = tmp.path().join("new disk");
    fs::create_dir_all(&never).unwrap();
    let (_, text) = status(&[p(&never)]);
    assert!(
        text.ends_with(&format!(
            "--from {} --to <your backup disk>\n",
            q(p(&never))
        )),
        "{text}"
    );
    let gone = tmp.path().join("gone disk");
    let (_, text) = status(&[p(&gone)]);
    assert!(
        text.contains(&format!("albumen status {} again", q(p(&gone)))),
        "{text}"
    );
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

/// Folders `albumen-receive` writes have no backup record. They carry no
/// definitive receive marker, so status gives the same neutral hint as for
/// any other folder, keeps exit 3, and only reads.
#[test]
fn receive_libraries_get_the_neutral_hint_and_stay_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain receive");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("IMG_0001.jpg"), b"photo").unwrap();
    fs::write(
        plain.join("IMG_0001.jpg.albumen.json"),
        "{\"camera_filename\":\"IMG_0001.jpg\",\"directory\":\"DCIM\",\"mtime_conflicts\":[],\"received_at\":\"1\",\"sha256\":\"00\",\"source_mtime\":\"1\",\"stored_name\":\"IMG_0001.jpg\"}\n",
    )
    .unwrap();
    let family = tmp.path().join("family");
    let member = family.join("members").join("ab".repeat(32));
    fs::create_dir_all(member.join("devices/0e74/log")).unwrap();
    fs::write(
        family.join("genesis.json"),
        "{\"payload\":{\"albumen_family\":1}}\n",
    )
    .unwrap();
    fs::write(member.join("IMG_0002.jpg"), b"photo").unwrap();
    fs::write(member.join("IMG_0002.jpg.albumen.json"), "{}\n").unwrap();
    let empty = tmp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    for dir in [&plain, &family, &empty] {
        let before = snapshot(dir);
        let (code, text) = status(&[p(dir)]);
        assert_eq!(code, 3, "{text}");
        let quoted = sq(p(dir));
        assert_eq!(
            lines(&text),
            vec![
                "not backed up: no Albumen backup found here",
                p(dir),
                &format!(
                    "next: to back up this folder, run albumen backup --from {quoted} --to <your backup disk>"
                ),
            ]
        );
        assert_plain_words(&text);
        assert_next_rules(code, &text);
        assert_eq!(before, snapshot(dir), "status changed {}", p(dir));
    }
}
