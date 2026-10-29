// SPDX-License-Identifier: Apache-2.0

//! `albumen status`: when each destination last had a good backup.
//!
//! Read-only. For each destination it reads the export record Albumen keeps
//! there (ALBUMEN-MANIFEST.txt), re-reads every listed item, and prints one
//! short block in plain words. Nothing at the destination or the source is
//! written, renamed, or touched. Wording follows the status spec v1
//! (2026-10-29): fresh, stale, damaged, not backed up, unreadable.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use albumen_ingest::{parse_manifest, tree_drift, DirectoryTree};

/// Name of the record an export leaves at a directory destination.
pub const RECORD_NAME: &str = "ALBUMEN-MANIFEST.txt";

/// Default for `--max-age`: a weekly backup habit.
pub const DEFAULT_MAX_AGE: &str = "7d";

/// How many changed or missing paths a damaged block lists before "and N more".
const LIST_LIMIT: usize = 20;

/// The state of one destination, worst last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Ok,
    Stale,
    Damaged,
    NotBackedUp,
    Unreadable,
}

impl State {
    pub fn exit_code(self) -> i32 {
        match self {
            State::Ok => 0,
            State::Stale | State::Damaged => 1,
            State::NotBackedUp => 3,
            State::Unreadable => 4,
        }
    }

    /// Rank for the worst-of exit: 4 > 3 > 1 > 0.
    fn rank(self) -> u8 {
        match self {
            State::Ok => 0,
            State::Stale | State::Damaged => 1,
            State::NotBackedUp => 3,
            State::Unreadable => 4,
        }
    }

    fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Stale => "stale",
            State::Damaged => "damaged",
            State::NotBackedUp => "not backed up",
            State::Unreadable => "unreadable",
        }
    }
}

/// One destination's block and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    pub text: String,
    /// The `next:` line, when there is one.
    pub next: Option<String>,
}

fn export_next(dest: &str) -> String {
    format!("next: albumen export --from <your photos folder> --to {dest} --apply")
}

/// Check one destination. `now` and `max_age_secs` come from the caller.
pub fn check(dest: &str, max_age_secs: u64, now: SystemTime) -> Report {
    let root = Path::new(dest);
    let unreadable = |why: String| {
        let next = format!("next: check that {dest} is connected and readable, then run albumen status {dest} again");
        Report {
            state: State::Unreadable,
            text: format!("unreadable: cannot read this destination ({why})\n{dest}\n{next}\n"),
            next: Some(next),
        }
    };
    match std::fs::metadata(root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return unreadable("not a folder".to_string()),
        Err(err) => return unreadable(plain_io(&err)),
    }
    let record = root.join(RECORD_NAME);
    let text = match std::fs::read_to_string(&record) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let next = export_next(dest);
            return Report {
                state: State::NotBackedUp,
                text: format!("not backed up: no Albumen backup found here\n{dest}\n{next}\n"),
                next: Some(next),
            };
        }
        Err(err) => return unreadable(plain_io(&err)),
    };
    let when = match std::fs::metadata(&record).and_then(|meta| meta.modified()) {
        Ok(when) => when,
        Err(err) => return unreadable(plain_io(&err)),
    };
    let Ok(entries) = parse_manifest(&text) else {
        return unreadable("the backup record is not readable".to_string());
    };
    let tree = DirectoryTree::new(root);
    let found = match tree_drift(&entries, &tree, &[RECORD_NAME.to_string()]) {
        Ok(found) => found,
        Err(_) => return unreadable("an item could not be read".to_string()),
    };
    let total = entries.len() as u64;
    let mut changed = Vec::new();
    let mut missing = Vec::new();
    for item in &found {
        match item.kind {
            "changed" => changed.push(item.path.clone()),
            "missing" => missing.push(item.path.clone()),
            _ => {} // extra files at the destination are not damage
        }
    }
    let bad = (changed.len() + missing.len()) as u64;
    if bad > 0 {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "damaged: {} of {} items no longer match what was backed up",
            group_thousands(bad),
            group_thousands(total)
        );
        let _ = writeln!(
            out,
            "{} changed, {} missing, {} fine",
            group_thousands(changed.len() as u64),
            group_thousands(missing.len() as u64),
            group_thousands(total - bad)
        );
        let _ = writeln!(out, "{dest}");
        let _ = writeln!(out, "your originals were not touched");
        let listed: Vec<(&str, &String)> = changed
            .iter()
            .map(|p| ("changed", p))
            .chain(missing.iter().map(|p| ("missing", p)))
            .collect();
        for (kind, path) in listed.iter().take(LIST_LIMIT) {
            let _ = writeln!(out, "  {kind}: {path}");
        }
        if listed.len() > LIST_LIMIT {
            let _ = writeln!(
                out,
                "  and {} more",
                group_thousands((listed.len() - LIST_LIMIT) as u64)
            );
        }
        let next = export_next(dest);
        let _ = writeln!(out, "{next}");
        return Report {
            state: State::Damaged,
            text: out,
            next: Some(next),
        };
    }
    let age = now.duration_since(when).map(|d| d.as_secs()).unwrap_or(0);
    let stamp = utc_stamp(unix_secs(when));
    if age > max_age_secs {
        let next = export_next(dest);
        return Report {
            state: State::Stale,
            text: format!(
                "stale: last good backup was {} ({stamp}), limit is {}\n{} items were verified then; changes since are not checked\n{dest}\n{next}\n",
                relative_age(age),
                days_word(max_age_secs),
                group_thousands(total)
            ),
            next: Some(next),
        };
    }
    Report {
        state: State::Ok,
        text: format!(
            "ok: backup checked {} ({stamp})\n{} items, all match your originals\n{dest}\n",
            relative_age(age),
            group_thousands(total)
        ),
        next: None,
    }
}

/// Every block, the summary after two or more, and the worst exit code.
pub fn render(reports: &[Report]) -> (String, i32) {
    let mut out = String::new();
    for (i, report) in reports.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&report.text);
    }
    let worst = reports.iter().max_by_key(|r| (r.state.rank(), r.state));
    let code = worst.map(|r| r.state.exit_code()).unwrap_or(0);
    if reports.len() >= 2 {
        let mut parts = Vec::new();
        for state in [
            State::Ok,
            State::Stale,
            State::Damaged,
            State::NotBackedUp,
            State::Unreadable,
        ] {
            let n = reports.iter().filter(|r| r.state == state).count();
            if n > 0 {
                parts.push(format!("{n} {}", state.word()));
            }
        }
        let _ = writeln!(
            out,
            "\n{} destinations: {}",
            reports.len(),
            parts.join(", ")
        );
        // A nonzero exit always ends on a next: line.
        if let Some(next) = worst.and_then(|r| r.next.as_ref()) {
            let _ = writeln!(out, "{next}");
        }
    }
    (out, code)
}

fn plain_io(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "it is not there; is the disk connected?".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => err.to_string(),
    }
}

fn unix_secs(when: SystemTime) -> i64 {
    match when.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Parse `--max-age`: a positive whole number of days (`7d`) or hours (`12h`).
pub fn parse_max_age(s: &str) -> Result<u64, String> {
    let bad = || format!("--max-age {s:?}: use a number of days or hours, like 7d or 12h");
    let unit = s.chars().last().ok_or_else(bad)?;
    let multiplier = match unit {
        'd' => 86_400u64,
        'h' => 3_600u64,
        _ => return Err(bad()),
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    n.checked_mul(multiplier).ok_or_else(bad)
}

/// 15204 -> "15,204".
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Seconds ago in words: "just now", "5 minutes ago", "2 hours ago", "12 days ago".
pub fn relative_age(secs: u64) -> String {
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{} ago", plural(secs / 60, "minute"))
    } else if secs < 86_400 {
        format!("{} ago", plural(secs / 3_600, "hour"))
    } else {
        format!("{} ago", plural(secs / 86_400, "day"))
    }
}

/// A limit in words: whole days when it divides evenly, else hours.
pub fn days_word(secs: u64) -> String {
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        plural(secs / 86_400, "day")
    } else {
        plural(secs / 3_600, "hour")
    }
}

/// Unix seconds as "YYYY-MM-DD HH:MM UTC" (Hinnant's civil-from-days).
pub fn utc_stamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3_600,
        (secs % 3_600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_age_takes_days_or_hours_only() {
        assert_eq!(parse_max_age("7d"), Ok(604_800));
        assert_eq!(parse_max_age("36h"), Ok(129_600));
        assert_eq!(parse_max_age("1h"), Ok(3_600));
        for bad in [
            "",
            "0d",
            "7",
            "7m",
            "d",
            "-1d",
            " 7d",
            "+7d",
            "1.5d",
            "99999999999999999999d",
        ] {
            assert!(parse_max_age(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(15_204), "15,204");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn ages_read_as_words() {
        assert_eq!(relative_age(59), "just now");
        assert_eq!(relative_age(60), "1 minute ago");
        assert_eq!(relative_age(3_599), "59 minutes ago");
        assert_eq!(relative_age(7_200), "2 hours ago");
        assert_eq!(relative_age(86_399), "23 hours ago");
        assert_eq!(relative_age(86_400), "1 day ago");
        assert_eq!(relative_age(12 * 86_400 + 5), "12 days ago");
        assert_eq!(days_word(604_800), "7 days");
        assert_eq!(days_word(86_400), "1 day");
        assert_eq!(days_word(3_600), "1 hour");
        assert_eq!(days_word(129_600), "36 hours");
    }

    #[test]
    fn stamps_are_utc() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(1_793_270_400), "2026-10-29 10:40 UTC");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc_stamp(-60), "1969-12-31 23:59 UTC");
    }

    #[test]
    fn worst_exit_wins_in_rank_order() {
        let r = |state| Report {
            state,
            text: String::new(),
            next: Some("next: x".into()),
        };
        assert_eq!(render(&[r(State::Ok), r(State::Stale)]).1, 1);
        assert_eq!(render(&[r(State::Damaged), r(State::NotBackedUp)]).1, 3);
        assert_eq!(
            render(&[r(State::Unreadable), r(State::NotBackedUp), r(State::Ok)]).1,
            4
        );
        assert_eq!(render(&[r(State::Ok)]).1, 0);
    }
}
