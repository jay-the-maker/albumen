// SPDX-License-Identifier: Apache-2.0

//! Filesystem-write lint for modules that only read a source.
//!
//! Allow-list: empty. `ingest/src/scan.rs`, `ingest/src/discover.rs`,
//! `ingest/src/source.rs`, and `ingest/src/family_source.rs` must not create,
//! write, rename, or remove a file
//! outside `#[cfg(test)]`. `OpenOptions` is a write only when it sets
//! `write`, `append`, `create`, or `create_new`. A hit fails as `file:line`.
//! Add a line to `ALLOW` only when the call cannot reach a source, and say
//! why on that line.

use std::path::{Path, PathBuf};

/// `path:line` entries that are not a source write. Empty on purpose.
const ALLOW: &[&str] = &[];

#[test]
fn source_modules_do_not_call_filesystem_writes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = ["scan.rs", "discover.rs", "source.rs", "family_source.rs"];
    let mut hits = Vec::new();
    for name in files {
        let path = root.join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("read {}: {err}", path.display());
        });
        let stripped = strip_test_modules(&text);
        for (index, line) in stripped.lines().enumerate() {
            let code = strip_line_comment(line);
            if !is_write(&code) {
                continue;
            }
            let hit = format!("{}:{}", display(&path), index + 1);
            if ALLOW.iter().any(|allowed| hit.ends_with(allowed)) {
                continue;
            }
            hits.push(hit);
        }
    }
    assert!(
        hits.is_empty(),
        "source module writes:\n{}",
        hits.join("\n")
    );
}

fn display(path: &Path) -> String {
    let text = path.display().to_string();
    match text.rfind("ingest/src/") {
        Some(index) => text[index..].to_string(),
        None => text,
    }
}

fn is_write(code: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "fs::write",
        "File::create",
        ".write(true)",
        ".append(true)",
        ".create(true)",
        ".create_new(true)",
        "remove_file",
        "remove_dir",
        "fs::rename",
        ".rename(",
        "set_permissions",
    ];
    NEEDLES.iter().any(|needle| code.contains(needle))
}

fn strip_line_comment(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            break;
        }
        out.push(ch);
    }
    out
}

/// Drop `#[cfg(test)] mod` bodies while keeping their newlines, so a hit
/// still names the original line.
fn strip_test_modules(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            let mut next = index + 1;
            while next < lines.len() && lines[next].trim().is_empty() {
                next += 1;
            }
            if next < lines.len() && lines[next].trim_start().starts_with("mod ") {
                let mut depth = 0i32;
                let mut started = false;
                while index < lines.len() {
                    depth += brace_delta(lines[index]);
                    if lines[index].contains('{') {
                        started = true;
                    }
                    out.push('\n');
                    index += 1;
                    if started && depth == 0 {
                        break;
                    }
                }
                continue;
            }
        }
        out.push_str(lines[index]);
        out.push('\n');
        index += 1;
    }
    out
}

fn brace_delta(line: &str) -> i32 {
    let code = strip_line_comment(line);
    let mut delta = 0i32;
    let mut chars = code.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            while let Some(next) = chars.next() {
                if next == '\\' {
                    chars.next();
                    continue;
                }
                if next == '"' {
                    break;
                }
            }
            continue;
        }
        if ch == '\'' {
            let Some(next) = chars.next() else {
                continue;
            };
            if next.is_ascii_alphanumeric() || next == '_' {
                while chars
                    .peek()
                    .is_some_and(|item| item.is_ascii_alphanumeric() || *item == '_')
                {
                    chars.next();
                }
                continue;
            }
            if next == '\\' {
                chars.next();
            }
            if chars.peek() == Some(&'\'') {
                chars.next();
            }
            continue;
        }
        if ch == '{' {
            delta += 1;
        } else if ch == '}' {
            delta -= 1;
        }
    }
    delta
}
