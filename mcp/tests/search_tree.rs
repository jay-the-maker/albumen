// SPDX-License-Identifier: Apache-2.0

//! M3 v2: the persisted search tree. Warm search re-hashes only files whose
//! stat key changed, sees adds/deletes/renames, catches a same-size edit with
//! the mtime put back (ctime/inode), labels anything it could not recheck,
//! and throws away any tree it cannot fully trust. Every answer carries the
//! freshness object of the label copy spec v1, asserted word for word.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use albumen_ingest::{walk_sorted, WalkControl};
use albumen_mcp::Library;
use serde_json::{json, Value};

/// A minimal JPEG whose XMP dc:subject is `keyword`.
fn jpeg(keyword: &str) -> Vec<u8> {
    let xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject><rdf:Bag><rdf:li>{keyword}</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#
    );
    let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    payload.extend_from_slice(xmp.as_bytes());
    let mut out = vec![0xff, 0xd8, 0xff, 0xe1];
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}

fn put(root: &Path, relative: &str, keyword: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, jpeg(keyword)).unwrap();
}

/// `count` files per dir, all tagged `keyword`.
fn fixture(root: &Path, dirs: &[&str], count: usize, keyword: &str) -> Vec<String> {
    let mut all = Vec::new();
    for dir in dirs {
        for n in 0..count {
            let relative = format!("{dir}/IMG_{n:04}.jpg");
            put(root, &relative, keyword);
            all.push(relative);
        }
    }
    all
}

/// Wait out the 2 s racy window so the next hash is trusted next time.
fn settle() {
    std::thread::sleep(Duration::from_millis(2_100));
}

/// T pinned for every label: 2026-10-29 17:42 PDT = 2026-10-30T00:42:00Z
/// ("2026-10-29 17:42 UTC-7" on a Pacific-time Windows PC).
const PINNED_NS: &str = "1793320920000000000";
/// Labels use local time from `TZ` on Linux and macOS. On Windows `TZ` has
/// no effect: the label is the OS zone's local time with a numeric offset
/// (`local_text`), e.g. "2026-10-29 17:42 UTC-7" on a Pacific-time PC, never
/// an abbreviation such as PDT.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const T: &str = "2026-10-29 17:42 PDT";
#[cfg(windows)]
const T: OsLocal = OsLocal;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const T: &str = "2026-10-30 00:42 UTC";

/// Windows expected label, worked out independently of the server through
/// .NET's TimeZoneInfo for the pinned instant.
#[cfg(windows)]
struct OsLocal;

#[cfg(windows)]
impl std::fmt::Display for OsLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        static LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let label = LABEL.get_or_init(|| {
            let script = "$u=[DateTime]::new(2026,10,30,0,42,0,[DateTimeKind]::Utc);\
                $z=[TimeZoneInfo]::Local;$l=[TimeZoneInfo]::ConvertTimeFromUtc($u,$z);\
                $m=[int]$z.GetUtcOffset($u).TotalMinutes;\
                $l.ToString('yyyy-MM-dd HH:mm',[Globalization.CultureInfo]::InvariantCulture)+' '+$m";
            let out = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .output()
                .expect("powershell for the expected local label");
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let (when, minutes) = text.rsplit_once(' ').expect("label and offset");
            let minutes: i64 = minutes.parse().expect("offset minutes");
            let zone = if minutes == 0 {
                "UTC".to_string()
            } else {
                let sign = if minutes < 0 { '-' } else { '+' };
                let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
                if m == 0 {
                    format!("UTC{sign}{h}")
                } else {
                    format!("UTC{sign}{h}:{m:02}")
                }
            };
            let label = format!("{when} {zone}");
            // CI sets this after `tzutil /s "India Standard Time"` so the
            // half-hour offset is checked against a fixed string too.
            if let Ok(want) = std::env::var("ALBUMEN_EXPECTED_LOCAL_LABEL") {
                assert_eq!(label, want, "OS zone label");
            }
            label
        });
        f.write_str(label)
    }
}
const T_UTC: &str = "2026-10-30T00:42:00Z";

fn open(root: &Path, index_dir: &Path) -> Library {
    std::env::set_var("ALBUMEN_SEARCH_STATS", "1");
    // Same values in every test, so parallel tests agree.
    std::env::set_var("ALBUMEN_SEARCH_CLOCK_NS", PINNED_NS);
    std::env::set_var("TZ", "America/Los_Angeles");
    Library::new(root.to_path_buf(), false, false, false, None)
        .unwrap()
        .with_index_dir(Some(index_dir.to_path_buf()))
}

struct Answer {
    paths: Vec<String>,
    hashed: u64,
    unchecked: u64,
    tree: String,
    freshness: Value,
}

fn search(library: &Library, keyword: &str) -> Answer {
    let page = library
        .call("search", &json!({"keyword": keyword, "limit": 100000}))
        .unwrap();
    assert_eq!(page["truncated"], json!(false));
    Answer {
        paths: page["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap().to_string())
            .collect(),
        hashed: page["stats"]["hashed"].as_u64().unwrap(),
        unchecked: page["stats"]["unchecked_files"].as_u64().unwrap(),
        tree: page["stats"]["tree"].as_str().unwrap().to_string(),
        freshness: page["freshness"].clone(),
    }
}

fn walk(root: &Path) -> Vec<String> {
    let mut seen = Vec::new();
    walk_sorted(root, None, &mut |relative| {
        seen.push(relative.to_string());
        WalkControl::Continue
    })
    .unwrap();
    seen
}

fn tree_path(root: &Path, index_dir: &Path) -> PathBuf {
    let index = albumen_ingest::index::LibraryIndex::open(root, Some(index_dir)).unwrap();
    index.file().unwrap().with_extension("search")
}

fn sorted(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths
}

#[test]
fn unchanged_rehashes_zero_and_100_changed_rehash_exactly_100() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    let all = fixture(&root, &["2020/01", "2020/02", "2021/07"], 100, "alpha-01");
    settle();

    let library = open(&root, &index_dir);
    let first = search(&library, "alpha-01");
    assert_eq!(first.paths, all);
    assert_eq!(first.hashed, 300);
    assert_eq!(first.tree, "discarded: missing");
    assert_eq!(first.freshness["state"], "rebuilt");
    assert_eq!(
        first.freshness["label"],
        format!("search: 300 matches, read every folder at {T} (no saved index yet)")
    );
    let again = search(&library, "alpha-01");
    assert_eq!((again.hashed, again.paths.len()), (0, 300));
    assert_eq!(again.freshness["state"], "fresh");

    // A restarted server loads the tree instead of re-reading 300 files.
    let library = open(&root, &index_dir);
    let restarted = search(&library, "alpha-01");
    assert_eq!(restarted.tree, "loaded");
    assert_eq!((restarted.hashed, restarted.paths), (0, all.clone()));

    // 100 files rewritten in place (directory mtimes untouched): new bytes,
    // new size, new keyword.
    let dirs_before: Vec<SystemTime> = ["2020/01", "2020/02"]
        .iter()
        .map(|dir| fs::metadata(root.join(dir)).unwrap().modified().unwrap())
        .collect();
    let changed: Vec<String> = all[..50].iter().chain(&all[100..150]).cloned().collect();
    for relative in &changed {
        put(&root, relative, "bravo-012");
    }
    let dirs_after: Vec<SystemTime> = ["2020/01", "2020/02"]
        .iter()
        .map(|dir| fs::metadata(root.join(dir)).unwrap().modified().unwrap())
        .collect();
    assert_eq!(dirs_before, dirs_after, "in-place edits moved a dir mtime");
    settle();
    let probe = search(&library, "bravo-012");
    assert_eq!(probe.hashed, 100);
    assert_eq!(probe.paths, changed);
    let rest = search(&library, "alpha-01");
    assert_eq!(rest.hashed, 0);
    assert_eq!(rest.paths.len(), 200);
    assert!(rest.paths.iter().all(|path| !changed.contains(path)));
}

#[test]
fn add_delete_and_rename_50_each_return_exactly_the_new_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    let all = fixture(&root, &["a", "b", "c", "d"], 50, "alpha-01");
    settle();
    let library = open(&root, &index_dir);
    assert_eq!(search(&library, "alpha-01").paths, all);

    for n in 0..25 {
        put(&root, &format!("a/NEW_{n:04}.jpg"), "alpha-01");
        put(&root, &format!("e/new/NEW_{n:04}.jpg"), "alpha-01");
    }
    for n in 0..50 {
        fs::remove_file(root.join(format!("b/IMG_{n:04}.jpg"))).unwrap();
    }
    for n in 0..25 {
        // within a directory, and across directories
        fs::rename(
            root.join(format!("c/IMG_{n:04}.jpg")),
            root.join(format!("c/RENAMED_{n:04}.jpg")),
        )
        .unwrap();
        fs::rename(
            root.join(format!("d/IMG_{n:04}.jpg")),
            root.join(format!("a/MOVED_{n:04}.jpg")),
        )
        .unwrap();
    }
    let expected = walk(&root);
    assert_eq!(expected.len(), 200 + 50 - 50);
    let after = search(&library, "alpha-01");
    let unique: BTreeSet<&String> = after.paths.iter().collect();
    assert_eq!(unique.len(), after.paths.len(), "duplicate paths");
    assert_eq!(sorted(after.paths), sorted(expected.clone()));

    // Same answer from a restarted server reading the saved tree.
    let library = open(&root, &index_dir);
    let restarted = search(&library, "alpha-01");
    assert_eq!(restarted.tree, "loaded");
    assert_eq!(sorted(restarted.paths), sorted(expected));
}

#[cfg(unix)]
#[test]
fn a_same_size_edit_with_the_mtime_put_back_is_caught_by_ctime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    let all = fixture(&root, &["roll"], 20, "alpha-01");
    settle();
    let library = open(&root, &index_dir);
    assert_eq!(search(&library, "alpha-01").hashed, 20);

    let target = root.join(&all[7]);
    let before = fs::metadata(&target).unwrap();
    fs::write(&target, jpeg("bravo-01")).unwrap();
    fs::File::options()
        .write(true)
        .open(&target)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
    let after = fs::metadata(&target).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());

    // A fresh server too, so the catch comes from the persisted stat key.
    let library = open(&root, &index_dir);
    let probe = search(&library, "bravo-01");
    assert_eq!(probe.tree, "loaded");
    assert_eq!(probe.hashed, 1);
    assert_eq!(probe.paths, vec![all[7].clone()]);
}

// Windows: std exposes no change time, so the stat key there is
// (size, mtime, creation time). A same-size rewrite that restores the mtime
// keeps all three and is NOT caught by warm search until the bytes are read
// again. This is listed as unclaimed in the PR note; integrity commands are
// unaffected because they hash every byte.

#[test]
fn a_search_that_runs_out_of_recheck_budget_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    let all = fixture(&root, &["2020/01", "2020/02", "2021/07"], 10, "alpha-01");
    settle();
    let library = open(&root, &index_dir);
    assert_eq!(search(&library, "alpha-01").paths, all);

    // Zero budget: nothing is compared with the disk, so every answer comes
    // from the tree, and it is labeled.
    let library = open(&root, &index_dir).with_recheck_budget(Duration::ZERO);
    let stale = search(&library, "alpha-01");
    assert_eq!(stale.tree, "loaded");
    assert_eq!(
        (stale.paths.clone(), stale.hashed, stale.unchecked),
        (all.clone(), 0, 30)
    );
    assert_eq!(
        stale.freshness,
        json!({
            "state": "partial",
            "checked_at": T_UTC,
            "folders_not_rechecked": 3,
            "label": format!("search: 30 matches, checked against disk at {T}; 3 folders not rechecked"),
            "next": "next: run the same search again to recheck those folders",
        })
    );

    // Two folders under 2020, then one folder: singular.
    let two = open(&root.join("2020"), &index_dir);
    search(&two, "alpha-01");
    let two = open(&root.join("2020"), &index_dir).with_recheck_budget(Duration::ZERO);
    let answer = search(&two, "alpha-01").freshness;
    assert_eq!(
        answer["label"],
        format!("search: 20 matches, checked against disk at {T}; 2 folders not rechecked")
    );
    assert_eq!(
        answer["next"],
        "next: run the same search again to recheck those folders"
    );
    let single = open(&root.join("2021"), &index_dir);
    search(&single, "alpha-01");
    let single = open(&root.join("2021"), &index_dir).with_recheck_budget(Duration::ZERO);
    let answer = search(&single, "alpha-01").freshness;
    assert_eq!(
        answer["label"],
        format!("search: 10 matches, checked against disk at {T}; 1 folder not rechecked")
    );
    assert_eq!(
        answer["next"],
        "next: run the same search again to recheck that folder"
    );
    assert_eq!(answer["folders_not_rechecked"], 1);

    // Every answer with an unchecked file is labeled partial; a full
    // recheck is labeled fresh. No answer goes out without freshness.
    let library = open(&root, &index_dir);
    let fresh = search(&library, "alpha-01");
    assert_eq!(fresh.unchecked, 0);
    assert_eq!(fresh.freshness["state"], "fresh");
    let library = open(&root, &index_dir).with_recheck_budget(Duration::ZERO);
    for keyword in ["alpha-01", "no-such-keyword", "alpha"] {
        let answer = search(&library, keyword);
        assert!(answer.unchecked > 0);
        assert_eq!(
            answer.freshness["state"], "partial",
            "unlabeled stale answer for {keyword}"
        );
    }
}

fn corrupt(case: &str, tree: &Path, other: &Path) {
    let bytes = fs::read(tree).unwrap();
    match case {
        "missing" => fs::remove_file(tree).unwrap(),
        "truncated" => fs::write(tree, &bytes[..bytes.len() / 2]).unwrap(),
        "checksum" => {
            let mut bytes = bytes;
            let at = bytes.len() - 3;
            bytes[at] = if bytes[at] == b'0' { b'1' } else { b'0' };
            fs::write(tree, bytes).unwrap();
        }
        "version" => {
            let text = String::from_utf8(bytes).unwrap();
            fs::write(
                tree,
                text.replacen("albumen-search-tree/1", "albumen-search-tree/9", 1),
            )
            .unwrap();
        }
        "other-root" => {
            fs::copy(other, tree).unwrap();
        }
        _ => unreachable!(),
    }
}

#[test]
fn a_bad_tree_is_discarded_whole_and_rewritten() {
    for case in ["missing", "truncated", "checksum", "version", "other-root"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let twin = tmp.path().join("twin");
        let index_dir = tmp.path().join("index");
        let all = fixture(&root, &["x", "y"], 30, "alpha-01");
        fixture(&twin, &["x", "y"], 30, "bravo-01");
        settle();
        search(&open(&root, &index_dir), "alpha-01");
        search(&open(&twin, &index_dir), "bravo-01");
        let tree = tree_path(&root, &index_dir);
        corrupt(case, &tree, &tree_path(&twin, &index_dir));

        let answer = search(&open(&root, &index_dir), "alpha-01");
        assert!(
            answer.tree.starts_with("discarded: "),
            "{case}: {}",
            answer.tree
        );
        assert_eq!(answer.paths, all, "{case}");
        assert_eq!(answer.hashed, 60, "{case}: trusted part of a bad tree");
        // Product Lead ruling on 9dcaab8: a missing tree is "no saved index
        // yet"; damaged, other-version and other-root trees were rebuilt.
        let why = if case == "missing" {
            "no saved index yet"
        } else {
            "the saved index was rebuilt"
        };
        assert_eq!(
            answer.freshness,
            json!({
                "state": "rebuilt",
                "checked_at": T_UTC,
                "folders_not_rechecked": 0,
                "label": format!("search: 60 matches, read every folder at {T} ({why})"),
                "next": null,
            }),
            "{case}"
        );
        let reloaded = search(&open(&root, &index_dir), "alpha-01");
        assert_eq!(reloaded.tree, "loaded", "{case}: tree not rewritten");
        assert_eq!(reloaded.freshness["state"], "fresh", "{case}");
        assert_eq!(
            (reloaded.hashed, reloaded.paths),
            (0, all.clone()),
            "{case}"
        );
    }
}

#[test]
fn a_kill_mid_write_leaves_the_old_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    let all = fixture(&root, &["roll"], 40, "alpha-01");
    settle();
    search(&open(&root, &index_dir), "alpha-01");
    let tree = tree_path(&root, &index_dir);
    let old = fs::read(&tree).unwrap();
    put(&root, &all[3], "bravo-0123");

    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"))
        .arg("--library")
        .arg(&root)
        .arg("--index-dir")
        .arg(&index_dir)
        .arg("--thumbnail-dir")
        .arg(tmp.path().join("thumbnails"))
        .env("ALBUMEN_SEARCH_TREE_CRASH", "before-rename")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for msg in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"keyword":"alpha-01"}}}"#,
    ] {
        stdin.write_all(msg.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    stdin.flush().unwrap();
    drop(stdin);
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success(), "the crash hook did not fire");
    assert!(
        !String::from_utf8_lossy(&out).contains(r#""id":2"#),
        "search answered before the crash"
    );

    assert_eq!(
        fs::read(&tree).unwrap(),
        old,
        "a kill mid-write changed the tree"
    );
    let library = open(&root, &index_dir);
    let answer = search(&library, "bravo-0123");
    assert_eq!(answer.tree, "loaded");
    assert_eq!((answer.hashed, answer.paths), (1, vec![all[3].clone()]));
    assert_ne!(
        fs::read(&tree).unwrap(),
        old,
        "the next search did not rewrite it"
    );
    assert_eq!(search(&open(&root, &index_dir), "alpha-01").paths.len(), 39);
}

/// Words the freshness strings must never use (spec v1 section 4).
const BANNED: &[&str] = &[
    "safe",
    "up to date",
    "synced",
    "all your photos",
    "nothing changed",
    "warning",
    "error",
];

/// Spec v1 check 6 and 7 for one freshness object: consistent fields, UTC
/// checked_at, no banned word.
fn assert_consistent(freshness: &Value, folders: u64) {
    let state = freshness["state"].as_str().unwrap();
    assert_eq!(freshness["folders_not_rechecked"], folders, "{freshness}");
    if state == "unreachable" {
        assert_eq!(freshness["checked_at"], Value::Null);
    } else {
        assert_eq!(freshness["checked_at"], T_UTC, "{freshness}");
    }
    assert_eq!(
        freshness["next"].is_null(),
        matches!(state, "fresh" | "rebuilt")
    );
    for field in ["label", "next"] {
        let text = freshness[field].as_str().unwrap_or("").to_lowercase();
        for word in BANNED {
            assert!(
                !text.contains(word),
                "{state} {field} uses {word:?}: {text}"
            );
        }
    }
}

/// Freshness label copy spec v1, section 6, checks 1 to 7, word for word on
/// the MCP search output (the canonical surface).
#[test]
fn freshness_spec_v1_acceptance_checks_word_for_word() {
    let tmp = tempfile::tempdir().unwrap();
    let index_dir = tmp.path().join("index");

    // 1. Warm run, nothing changed, 3 matches.
    let one = tmp.path().join("one");
    fixture(&one, &["roll"], 3, "alpha-01");
    settle();
    search(&open(&one, &index_dir), "alpha-01");
    let library = open(&one, &index_dir);
    let answer = search(&library, "alpha-01").freshness;
    assert_eq!(
        answer,
        json!({
            "state": "fresh",
            "checked_at": T_UTC,
            "folders_not_rechecked": 0,
            "label": format!("search: 3 matches, checked against disk at {T}"),
            "next": null,
        })
    );
    assert_consistent(&answer, 0);

    // 2. The recheck budget hook skips 1 folder, 3 matches.
    let library = open(&one, &index_dir).with_recheck_budget(Duration::ZERO);
    let answer = search(&library, "alpha-01").freshness;
    assert_eq!(
        answer["label"],
        format!("search: 3 matches, checked against disk at {T}; 1 folder not rechecked")
    );
    assert_eq!(
        answer["next"],
        "next: run the same search again to recheck that folder"
    );
    assert_eq!(answer["state"], "partial");
    assert_consistent(&answer, 1);

    // 3. The hook skips 4 folders, 1 match.
    let four = tmp.path().join("four");
    put(&four, "a/IMG_0001.jpg", "solo-01");
    for dir in ["b", "c", "d"] {
        put(&four, &format!("{dir}/IMG_0001.jpg"), "other-01");
    }
    settle();
    search(&open(&four, &index_dir), "solo-01");
    let library = open(&four, &index_dir).with_recheck_budget(Duration::ZERO);
    let answer = search(&library, "solo-01").freshness;
    assert_eq!(
        answer["label"],
        format!("search: 1 match, checked against disk at {T}; 4 folders not rechecked")
    );
    assert_eq!(
        answer["next"],
        "next: run the same search again to recheck those folders"
    );
    assert_consistent(&answer, 4);

    // 4. Index truncated (corrupt, wrong version and other root are in
    // a_bad_tree_is_discarded_whole_and_rewritten), 0 matches.
    let tree = tree_path(&one, &index_dir);
    let bytes = fs::read(&tree).unwrap();
    fs::write(&tree, &bytes[..bytes.len() / 2]).unwrap();
    let answer = search(&open(&one, &index_dir), "no-such-keyword").freshness;
    assert_eq!(
        answer,
        json!({
            "state": "rebuilt",
            "checked_at": T_UTC,
            "folders_not_rechecked": 0,
            "label": format!("search: 0 matches, read every folder at {T} (the saved index was rebuilt)"),
            "next": null,
        })
    );
    assert_consistent(&answer, 0);

    // 5. Root path removed (as an unmounted drive): a tool error, state D.
    let lib = tmp.path().join("lib");
    fixture(&lib, &["roll"], 2, "alpha-01");
    let library = open(&lib, &index_dir);
    search(&library, "alpha-01");
    fs::rename(&lib, tmp.path().join("lib-unplugged")).unwrap();
    let err = library
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap_err();
    let shown = lib.display();
    assert_eq!(
        err,
        format!(
            "search: could not read {shown}; no results shown\nnext: connect the drive or check the path, then search again"
        )
    );
    let answer = library.error_freshness("search", &err).unwrap();
    assert_eq!(answer["state"], "unreachable");
    assert_eq!(
        format!(
            "{}\n{}",
            answer["label"].as_str().unwrap(),
            answer["next"].as_str().unwrap()
        ),
        err
    );
    assert_consistent(&answer, 0);
    assert_eq!(library.error_freshness("browse", &err), None);

    // 6. Over the wire: isError true, text is line 1 + next:, and the
    // freshness object rides along. A query-only search is labeled too (7).
    let wire = tmp.path().join("wire");
    fixture(&wire, &["roll"], 2, "alpha-01");
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"))
        .arg("--library")
        .arg(&wire)
        .arg("--index-dir")
        .arg(&index_dir)
        .arg("--thumbnail-dir")
        .arg(tmp.path().join("thumbnails"))
        .env("ALBUMEN_SEARCH_CLOCK_NS", PINNED_NS)
        .env("TZ", "America/Los_Angeles")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: u32, body: &str| -> Value {
        use std::io::BufRead;
        stdin.write_all(body.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "server closed");
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    };
    ask(
        1,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
    );
    let reply = ask(
        2,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"IMG"}}}"#,
    );
    assert_eq!(reply["result"]["isError"], false);
    let page: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        page["freshness"]["label"],
        format!("search: 2 matches, checked against disk at {T}")
    );
    assert_consistent(&page["freshness"], 0);
    fs::rename(&wire, tmp.path().join("wire-unplugged")).unwrap();
    let reply = ask(
        3,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"keyword":"alpha-01"}}}"#,
    );
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let result = &reply["result"];
    assert_eq!(result["isError"], true, "{reply}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        format!(
            "search: could not read {}; no results shown\nnext: connect the drive or check the path, then search again",
            wire.display()
        )
    );
    let freshness = &result["structuredContent"]["freshness"];
    assert_eq!(freshness["state"], "unreachable");
    assert_eq!(
        format!(
            "{}\n{}",
            freshness["label"].as_str().unwrap(),
            freshness["next"].as_str().unwrap()
        ),
        text
    );
}

fn page(library: &Library, keyword: &str, limit: u64, cursor: Option<&str>) -> Value {
    let mut args = json!({"keyword": keyword, "limit": limit});
    if let Some(cursor) = cursor {
        args["cursor"] = json!(cursor);
    }
    library.call("search", &args).unwrap()
}

/// Product Lead ruling on 9dcaab8, item 2: the total is not counted (search
/// stops at `limit`), so a paginated answer says "{K} matches on this page"
/// in every state, with the same plurals. Unpaginated wording is unchanged.
#[test]
fn a_paginated_answer_counts_only_this_page_in_every_state() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let index_dir = tmp.path().join("index");
    fixture(&root, &["a", "b"], 2, "alpha-01");
    settle();

    // C, first search: no saved index yet.
    let first = page(&open(&root, &index_dir), "alpha-01", 3, None);
    assert_eq!(first["truncated"], true);
    assert_eq!(
        first["freshness"]["label"],
        format!("search: 3 matches on this page, read every folder at {T} (no saved index yet)")
    );
    assert_consistent(&first["freshness"], 0);

    // A, both pages; the last page is still a page.
    let library = open(&root, &index_dir);
    let one = page(&library, "alpha-01", 3, None);
    assert_eq!(
        one["freshness"]["label"],
        format!("search: 3 matches on this page, checked against disk at {T}")
    );
    let cursor = one["next_cursor"].as_str().unwrap().to_string();
    let two = page(&library, "alpha-01", 3, Some(&cursor));
    assert_eq!(two["truncated"], false);
    assert_eq!(
        two["freshness"]["label"],
        format!("search: 1 match on this page, checked against disk at {T}")
    );
    let none = page(&library, "no-such-keyword", 3, Some(&cursor));
    assert_eq!(
        none["freshness"]["label"],
        format!("search: 0 matches on this page, checked against disk at {T}")
    );
    // Unpaginated stays as it was.
    let whole = page(&library, "alpha-01", 10, None);
    assert_eq!(
        whole["freshness"]["label"],
        format!("search: 4 matches, checked against disk at {T}")
    );

    // B: nothing rechecked, both folders reported.
    let stale = open(&root, &index_dir).with_recheck_budget(Duration::ZERO);
    let answer = page(&stale, "alpha-01", 3, None)["freshness"].clone();
    assert_eq!(
        answer["label"],
        format!(
            "search: 3 matches on this page, checked against disk at {T}; 2 folders not rechecked"
        )
    );
    assert_consistent(&answer, 2);

    // C, rebuilt: a damaged tree.
    let tree = tree_path(&root, &index_dir);
    let bytes = fs::read(&tree).unwrap();
    fs::write(&tree, &bytes[..bytes.len() / 2]).unwrap();
    let answer = page(&open(&root, &index_dir), "alpha-01", 3, None)["freshness"].clone();
    assert_eq!(
        answer["label"],
        format!(
            "search: 3 matches on this page, read every folder at {T} (the saved index was rebuilt)"
        )
    );
}

/// Product Lead ruling on 9dcaab8, item 4. Finding: a memory-only index
/// (no index dir) runs the same recheck as a saved one. Every file is
/// stat'ed against the disk until the recheck budget runs out; a file past
/// the budget that the in-memory cache already holds is served without a
/// recheck and is labeled partial with its folder count. So memory-only is
/// not always fresh, and the label already says which it is.
#[test]
fn a_memory_only_index_is_fresh_only_when_every_file_was_rechecked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    fixture(&root, &["a", "b", "c"], 2, "alpha-01");
    settle();
    open(&root, tmp.path())
        .with_index_dir(None)
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap();
    // Within the budget every search rechecks disk: fresh, and never
    // "rebuilt" because there is no saved tree to miss.
    let library = open(&root, tmp.path()).with_index_dir(None);
    let first = library
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap();
    assert_eq!(first["stats"]["tree"], "none");
    assert_eq!(
        first["freshness"]["label"],
        format!("search: 6 matches, checked against disk at {T}")
    );
    let again = library
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap();
    assert_eq!(again["freshness"]["state"], "fresh");

    // Out of budget: the first search reads every file (nothing cached),
    // the second serves the cache unchecked and says so.
    let library = open(&root, tmp.path())
        .with_index_dir(None)
        .with_recheck_budget(Duration::ZERO);
    let first = library
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap();
    assert_eq!(first["stats"]["unchecked_files"], 0);
    assert_eq!(first["freshness"]["state"], "fresh");
    let second = library
        .call("search", &json!({"keyword": "alpha-01"}))
        .unwrap();
    assert_eq!(second["stats"]["unchecked_files"], 6);
    assert_eq!(
        second["freshness"]["label"],
        format!("search: 6 matches, checked against disk at {T}; 3 folders not rechecked")
    );
    assert_eq!(
        second["freshness"]["next"],
        "next: run the same search again to recheck those folders"
    );
    assert_consistent(&second["freshness"], 3);
}
