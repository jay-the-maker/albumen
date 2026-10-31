// SPDX-License-Identifier: Apache-2.0

//! Hide and delete state that a listing must honour (M7, A8).
//!
//! The files are the only source of truth: receiver
//! `<stored name>.albumen-tombstone` sidecars and the family log (through
//! [`crate::family::manifest_json`]). Nothing here writes anything, and the
//! path index carries no hide flag: a listing asks this module on every
//! read, so a scan, a rescan from a saved index and an index rebuild all see
//! the same state the files hold right now. (The index refreshes by
//! directory mtime; a flag cached there could go stale when a sidecar is
//! rewritten in place.)

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

pub use crate::export::TOMBSTONE_SUFFIX;

/// What [`hidden_paths`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Hidden {
    /// Library-relative paths a listing must not show.
    pub paths: BTreeSet<String>,
    /// Problems reading hide state, in plain words.
    pub errors: Vec<String>,
}

/// The paths among `paths` (library-relative, as the index lists them) that
/// are hidden or deleted for `viewer` (a family member id, or None).
///
/// - A receiver tombstone with `"hidden": true` hides the file it sits
///   beside; `"hidden": false` (an unhide) hides nothing. A tombstone that
///   cannot be read or parsed hides its file (fail closed) and is reported.
/// - In a family library (a `genesis.json` at the root), a family tombstone
///   hides that member's object and its sidecar for every viewer, and a
///   family hide by `viewer` hides every copy of that sha256 for `viewer`
///   only. A manifest that cannot be built is reported.
pub fn hidden_paths(library: &Path, paths: &[String], viewer: Option<&str>) -> Hidden {
    let mut out = Hidden::default();
    receiver_tombstones(library, paths, &mut out);
    if library.join("genesis.json").is_file() {
        family_state(library, paths, viewer, &mut out);
    }
    out
}

fn receiver_tombstones(library: &Path, paths: &[String], out: &mut Hidden) {
    for rel in paths {
        let Some(original) = rel.strip_suffix(TOMBSTONE_SUFFIX) else {
            continue;
        };
        if original.is_empty() {
            continue;
        }
        let hidden = match read_tombstone(&library.join(rel)) {
            Ok(h) => h,
            Err(err) => {
                out.errors.push(format!("{rel}: {err}; hiding it"));
                true
            }
        };
        if hidden {
            out.paths.insert(original.to_string());
        }
    }
}

/// [`read_tombstone`] for callers that cache per-tombstone state (the gui's
/// incremental re-check). Err in plain words; callers fail closed.
pub fn read_receiver_tombstone(path: &Path) -> Result<bool, String> {
    read_tombstone(path)
}

/// Whether a receiver tombstone file says hidden. Err (in plain words) when
/// it cannot be read or parsed; callers fail closed and hide the photo.
fn read_tombstone(path: &Path) -> Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("could not read ({err})"))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("not valid JSON ({err})"))?;
    v.get("hidden")
        .and_then(Value::as_bool)
        .ok_or_else(|| "no \"hidden\" true/false".to_string())
}

/// One photo, as a walk finds it, before the full path list exists: is a
/// receiver tombstone beside `rel` saying hidden? One stat per photo (plus
/// a read when a tombstone exists). `Err` names the tombstone and the
/// problem; the caller hides the photo (fail closed) and says so.
pub fn receiver_hidden(library: &Path, rel: &str) -> Result<bool, String> {
    let tomb = format!("{rel}{TOMBSTONE_SUFFIX}");
    let path = library.join(&tomb);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => read_tombstone(&path).map_err(|err| format!("{tomb}: {err}; hiding it")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("{tomb}: could not check ({err}); hiding it")),
    }
}

/// `members/<member>/objects/sha256/<hh>/<sha>` or its `.sidecar`:
/// (member, sha).
fn family_object(rel: &str) -> Option<(&str, &str)> {
    let mut parts = rel.split('/');
    let (Some("members"), Some(member), Some("objects"), Some("sha256"), Some(hh), Some(name)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if parts.next().is_some() {
        return None;
    }
    let sha = name.strip_suffix(".sidecar").unwrap_or(name);
    (sha.len() == 64 && sha.starts_with(hh)).then_some((member, sha))
}

/// Start of the error (and of the gui's line) when the family log can't be
/// read; the path follows, then a newline and the detail.
pub const FAMILY_LOG_UNREADABLE: &str = "Family photos hidden: can't read the family log ";

/// Start of the error (and the gui's line) when one family log entry can't
/// be read or verified; `<path>/log/<seq>.json` follows.
pub const FAMILY_ENTRY_UNREADABLE: &str =
    "Family photos hidden: a family log entry can't be read: ";

/// Start of the error (and the gui's line) when a family log entry is
/// missing while a later one exists; `<path>/log/<seq>.json` follows.
pub const FAMILY_ENTRY_MISSING: &str = "Family photos hidden: a family log entry is missing: ";

/// Every family fail-closed line starts with this; the gui shows the first
/// line as is.
pub const FAMILY_HIDDEN: &str = "Family photos hidden: ";

fn family_state(library: &Path, paths: &[String], viewer: Option<&str>, out: &mut Hidden) {
    let fail_closed = |out: &mut Hidden, msg: String| {
        // Fail closed: every family object is hidden for everyone until the
        // log reads again, and the message says exactly that.
        out.errors.push(msg);
        for rel in paths {
            if family_object(rel).is_some() {
                out.paths.insert(rel.clone());
            }
        }
    };
    let value: Value = match crate::family::manifest_json(library)
        .map_err(|err| err.to_string())
        .and_then(|json| serde_json::from_str(&json).map_err(|err| err.to_string()))
    {
        Ok(v) => v,
        Err(err) => {
            let members = library.join("members");
            fail_closed(
                out,
                format!("{FAMILY_LOG_UNREADABLE}{}\n{err}", members.display()),
            );
            return;
        }
    };
    // A bad entry would silently drop itself and every later entry from its
    // device (possibly a hide), so the view would unhide: fail closed.
    match crate::family::unreadable_log_entries(library) {
        Ok(bad) if bad.is_empty() => {}
        Ok(bad) => {
            use crate::family::LogProblem;
            let first = match &bad[0] {
                LogProblem::Unreadable(p) => format!("{FAMILY_ENTRY_UNREADABLE}{}", p.display()),
                LogProblem::Missing(p) => format!("{FAMILY_ENTRY_MISSING}{}", p.display()),
            };
            let more = bad.len() - 1;
            fail_closed(
                out,
                if more == 0 {
                    first
                } else {
                    format!("{first}\nand {more} more device log(s) end early")
                },
            );
            return;
        }
        Err(err) => {
            let members = library.join("members");
            fail_closed(
                out,
                format!("{FAMILY_LOG_UNREADABLE}{}\n{err}", members.display()),
            );
            return;
        }
    }
    let list = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let tombstoned: BTreeSet<(String, String)> = list("tombstones")
        .iter()
        .filter_map(|t| Some((s(t, "member")?, s(t, "sha256")?)))
        .collect();
    let mut hidden_for_viewer: BTreeMap<String, bool> = BTreeMap::new();
    if let Some(viewer) = viewer {
        for h in list("hides") {
            if s(&h, "viewer").as_deref() == Some(viewer) {
                if let Some(sha) = s(&h, "sha256") {
                    let on = h.get("hidden").and_then(Value::as_bool).unwrap_or(true);
                    hidden_for_viewer.insert(sha, on);
                }
            }
        }
    }
    for rel in paths {
        let Some((member, sha)) = family_object(rel) else {
            continue;
        };
        if tombstoned.contains(&(member.to_string(), sha.to_string()))
            || hidden_for_viewer.get(sha) == Some(&true)
        {
            out.paths.insert(rel.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstones_hide_unhide_and_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path();
        std::fs::write(lib.join("a.jpg.albumen-tombstone"), br#"{"hidden":true}"#).unwrap();
        std::fs::write(lib.join("b.jpg.albumen-tombstone"), br#"{"hidden":false}"#).unwrap();
        std::fs::write(lib.join("c.jpg.albumen-tombstone"), b"not json").unwrap();
        let paths: Vec<String> = [
            "a.jpg",
            "a.jpg.albumen-tombstone",
            "b.jpg",
            "b.jpg.albumen-tombstone",
            "c.jpg",
            "c.jpg.albumen-tombstone",
            "d.jpg.albumen-tombstone",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let h = hidden_paths(lib, &paths, None);
        let want: BTreeSet<String> = ["a.jpg", "c.jpg", "d.jpg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(h.paths, want);
        assert_eq!(h.errors.len(), 2, "{:?}", h.errors);
    }

    #[test]
    fn receiver_hidden_one_photo() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path();
        std::fs::write(lib.join("a.jpg.albumen-tombstone"), br#"{"hidden":true}"#).unwrap();
        std::fs::write(lib.join("b.jpg.albumen-tombstone"), br#"{"hidden":false}"#).unwrap();
        std::fs::write(lib.join("c.jpg.albumen-tombstone"), b"{").unwrap();
        assert_eq!(receiver_hidden(lib, "a.jpg"), Ok(true));
        assert_eq!(receiver_hidden(lib, "b.jpg"), Ok(false));
        assert_eq!(receiver_hidden(lib, "x.jpg"), Ok(false));
        let err = receiver_hidden(lib, "c.jpg").unwrap_err();
        assert!(err.starts_with("c.jpg.albumen-tombstone: "), "{err}");
    }

    #[test]
    fn family_object_paths() {
        let sha = format!("ab{}", "0".repeat(62));
        let p = format!("members/m1/objects/sha256/ab/{sha}");
        assert_eq!(family_object(&p), Some(("m1", sha.as_str())));
        assert_eq!(
            family_object(&format!("{p}.sidecar")),
            Some(("m1", sha.as_str()))
        );
        assert_eq!(family_object("members/m1/objects/sha256/cd/x"), None);
        assert_eq!(family_object("photos/a.jpg"), None);
    }
}
