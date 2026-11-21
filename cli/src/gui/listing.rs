// SPDX-License-Identifier: Apache-2.0

//! The library listing behind the photo grid.
//!
//! `SourceRoot::walk` stops at the first entry it will not read (a symlink,
//! a name with a control character, a name that is not UTF-8). That is the
//! right answer for a copy, but for browsing it turned one odd file into an
//! empty grid. This walk reads the same way (read-only, no symlink
//! followed) and keeps going: every entry it leaves out is counted with its
//! reason, so the page can say so instead of showing 0 photos.

use std::path::Path;

use albumen_ingest::SourceRoot;

/// Why an entry was left out of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Skip {
    /// A symbolic link. Albumen never follows one inside a library.
    Symlink,
    /// A name with a control character, a slash, or another unsafe byte.
    UnsafeName,
    /// A name that is not valid UTF-8.
    NonUtf8Name,
    /// A folder or file that could not be read.
    Unreadable,
    /// Not a regular file or folder (a socket, a device, a pipe).
    Special,
}

impl Skip {
    pub fn word(self) -> &'static str {
        match self {
            Skip::Symlink => "symlink",
            Skip::UnsafeName => "unsafe name",
            Skip::NonUtf8Name => "name not UTF-8",
            Skip::Unreadable => "unreadable",
            Skip::Special => "not a regular file",
        }
    }
}

/// One entry that was left out, by its path as far as it could be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Relative path with any control character shown as `\u{..}`.
    pub shown: String,
    pub why: Skip,
}

#[derive(Debug, Default)]
pub struct Listing {
    /// Relative paths of every regular file, sorted.
    pub files: Vec<String>,
    pub skipped: Vec<Skipped>,
}

/// List every regular file under `library`, read-only. Never fails: a root
/// that cannot be opened is one `Unreadable` entry.
pub fn list(library: &Path) -> Listing {
    let mut out = Listing::default();
    match SourceRoot::open(library) {
        Ok(root) => {
            let start = root.canonical().to_path_buf();
            walk(&root, &start, "", &mut out);
        }
        Err(_) => out.skipped.push(Skipped {
            shown: library.display().to_string(),
            why: Skip::Unreadable,
        }),
    }
    out.files.sort();
    out
}

fn show(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

fn safe_name(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control))
}

fn walk(root: &SourceRoot, dir: &Path, prefix: &str, out: &mut Listing) {
    let Ok(read) = root.read_dir(dir) else {
        out.skipped.push(Skipped {
            shown: if prefix.is_empty() {
                ".".into()
            } else {
                show(prefix)
            },
            why: Skip::Unreadable,
        });
        return;
    };
    let mut dirs = Vec::new();
    for entry in read {
        let Ok(entry) = entry else {
            out.skipped.push(Skipped {
                shown: show(prefix),
                why: Skip::Unreadable,
            });
            continue;
        };
        let raw = entry.file_name();
        let Some(name) = raw.to_str() else {
            out.skipped.push(Skipped {
                shown: join(prefix, &show(&raw.to_string_lossy())),
                why: Skip::NonUtf8Name,
            });
            continue;
        };
        let relative = join(prefix, name);
        if !safe_name(name) {
            out.skipped.push(Skipped {
                shown: join(prefix, &show(name)),
                why: Skip::UnsafeName,
            });
            continue;
        }
        let path = entry.path();
        let Ok(kind) = std::fs::symlink_metadata(&path).map(|m| m.file_type()) else {
            out.skipped.push(Skipped {
                shown: relative,
                why: Skip::Unreadable,
            });
            continue;
        };
        if kind.is_symlink() {
            out.skipped.push(Skipped {
                shown: relative,
                why: Skip::Symlink,
            });
        } else if kind.is_dir() {
            dirs.push((path, relative));
        } else if kind.is_file() {
            out.files.push(relative);
        } else {
            out.skipped.push(Skipped {
                shown: relative,
                why: Skip::Special,
            });
        }
    }
    dirs.sort();
    for (path, relative) in dirs {
        walk(root, &path, &relative, out);
    }
}

/// Counts per reason, for the page, and the first few paths.
pub fn summary(skipped: &[Skipped]) -> serde_json::Value {
    let mut counts = std::collections::BTreeMap::<&str, u64>::new();
    for s in skipped {
        *counts.entry(s.why.word()).or_default() += 1;
    }
    let examples: Vec<serde_json::Value> = skipped
        .iter()
        .take(20)
        .map(|s| serde_json::json!({ "path": s.shown, "why": s.why.word() }))
        .collect();
    serde_json::json!({ "count": skipped.len(), "by_reason": counts, "examples": examples })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_control_characters_visibly() {
        assert_eq!(show("a\u{7}b.jpg"), "a\\u{7}b.jpg");
        assert!(!safe_name("a\nb.jpg"));
        assert!(safe_name("2015 06 Reunion"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_control_name_are_skipped_and_counted_not_fatal() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        albumen_ingest::dest::create_dir_all(&lib.join("ev")).unwrap();
        for name in ["ev/a.jpg", "b.jpg", "bad\u{7}name.jpg"] {
            let mut f = albumen_ingest::dest::create_file(&lib.join(name)).unwrap();
            f.write_all(b"x").unwrap();
        }
        std::os::unix::fs::symlink(lib.join("b.jpg"), lib.join("ev/link.jpg")).unwrap();
        let l = list(&lib);
        assert_eq!(l.files, ["b.jpg", "ev/a.jpg"]);
        let mut whys: Vec<Skip> = l.skipped.iter().map(|s| s.why).collect();
        whys.sort();
        assert_eq!(whys, [Skip::Symlink, Skip::UnsafeName]);
        let s = summary(&l.skipped);
        assert_eq!(s["count"], 2);
        assert_eq!(s["by_reason"]["symlink"], 1);
    }
}
