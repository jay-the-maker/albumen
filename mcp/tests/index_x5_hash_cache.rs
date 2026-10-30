// SPDX-License-Identifier: Apache-2.0

//! M6 X5, the search tool's half: deleting the saved index, or damaging it,
//! loses none of the search tool's hash cache (the `.search` tree saved next
//! to the index: every file's SHA-256 and metadata record). The tree's bytes
//! are taken before and after each step and diffed, and the search after
//! each step must load the tree and hash nothing again. Needs exiftool
//! (metadata filters read it). Synthetic files only.
//!
//! One test in its own binary: it sets ALBUMEN_SEARCH_STATS for itself.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use albumen_mcp::Library;
use serde_json::{json, Value};

const FILES: usize = 300;

fn make_library(root: &Path) {
    let past = SystemTime::now() - Duration::from_secs(3600);
    for i in 0..FILES {
        let path = root.join(format!("d{:02}/img_{i:04}.jpg", i % 10));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Distinct bytes so every SHA-256 differs.
        fs::write(&path, format!("\u{ff}synthetic {i}").as_bytes()).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(past))
            .unwrap();
    }
}

fn open(root: &Path, index_dir: &Path) -> Library {
    Library::new(root.to_path_buf(), false, false, false, None)
        .unwrap()
        .with_index_dir(Some(index_dir.to_path_buf()))
}

/// One filtered search over everything; returns (paths, stats).
fn filtered_search(library: &Library) -> (Vec<String>, Value) {
    let page = library
        .call(
            "search",
            &json!({"camera": "no-such-camera", "limit": 1000}),
        )
        .unwrap();
    let paths = page["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    (paths, page["stats"].clone())
}

fn files_in(dir: &Path) -> (Vec<PathBuf>, PathBuf) {
    let mut slots = Vec::new();
    let mut tree = None;
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "search") {
            assert!(tree.replace(path).is_none(), "two search trees");
        } else {
            slots.push(path);
        }
    }
    slots.sort();
    (
        slots,
        tree.expect("no .search tree saved next to the index"),
    )
}

#[test]
fn deleting_or_damaging_the_index_keeps_the_search_hash_cache() {
    std::env::set_var("ALBUMEN_SEARCH_STATS", "1");
    let library_dir = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    make_library(library_dir.path());
    // A hash is kept as settled only once the file's newer of mtime and
    // ctime (creation time on Windows) is RACY_NANOS old; wait that out so
    // the hashes are reusable at all.
    std::thread::sleep(
        Duration::from_nanos(albumen_ingest::index::RACY_NANOS) + Duration::from_millis(500),
    );

    let library = open(library_dir.path(), index_dir.path());
    let (first_paths, first) = filtered_search(&library);
    drop(library);
    assert_eq!(first["hashed"].as_u64(), Some(FILES as u64), "{first}");
    let (slots, tree) = files_in(index_dir.path());
    assert!(!slots.is_empty(), "no index slot saved");

    // 0. Control: reopen with the index untouched. Settles every entry
    // (rewrites the tree once); the bytes after this are the "before".
    let library = open(library_dir.path(), index_dir.path());
    let (control_paths, control) = filtered_search(&library);
    drop(library);
    let tree_before = fs::read(&tree).unwrap();
    let library = open(library_dir.path(), index_dir.path());
    let (_, control2) = filtered_search(&library);
    drop(library);
    let control_stable = fs::read(&tree).unwrap() == tree_before;

    // 1. Delete every index slot, keep the tree, search again.
    for slot in &slots {
        fs::remove_file(slot).unwrap();
    }
    let library = open(library_dir.path(), index_dir.path());
    let (paths_delete, after_delete) = filtered_search(&library);
    drop(library);
    let tree_after_delete = fs::read(&tree).unwrap();

    // 2. Damage every index slot (flip a middle byte), search again.
    // Empty files there (the writer lock) are not slots.
    let (slots, _) = files_in(index_dir.path());
    let slots: Vec<PathBuf> = slots
        .into_iter()
        .filter(|p| fs::metadata(p).unwrap().len() > 0)
        .collect();
    assert!(!slots.is_empty(), "no index slot rebuilt after the delete");
    for slot in &slots {
        let mut bytes = fs::read(slot).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x55;
        fs::write(slot, bytes).unwrap();
    }
    let library = open(library_dir.path(), index_dir.path());
    let (paths_damage, after_damage) = filtered_search(&library);
    drop(library);
    let tree_after_damage = fs::read(&tree).unwrap();

    println!(
        "M6 X5HC RESULT files={FILES} slots={} tree_bytes={} first_hashed={} \
         control: tree={} hashed={} then hashed={} tree_bytes_stable={control_stable} \
         delete: tree={} hashed={} tree_bytes_equal={} \
         damage: tree={} hashed={} tree_bytes_equal={} answers_equal={}",
        slots.len(),
        tree_before.len(),
        first["hashed"],
        control["tree"],
        control["hashed"],
        control2["hashed"],
        after_delete["tree"],
        after_delete["hashed"],
        tree_after_delete == tree_before,
        after_damage["tree"],
        after_damage["hashed"],
        tree_after_damage == tree_before,
        paths_delete == first_paths && paths_damage == first_paths
    );
    assert_eq!(control_paths, first_paths);
    assert_eq!(control2["hashed"].as_u64(), Some(0), "{control2}");
    assert!(
        control_stable,
        "control: tree rewritten with nothing changed"
    );
    assert_eq!(after_delete["tree"], "loaded", "{after_delete}");
    assert_eq!(after_delete["hashed"].as_u64(), Some(0), "{after_delete}");
    assert!(
        tree_after_delete == tree_before,
        "hash cache changed after index delete"
    );
    assert_eq!(after_damage["tree"], "loaded", "{after_damage}");
    assert_eq!(after_damage["hashed"].as_u64(), Some(0), "{after_damage}");
    assert!(
        tree_after_damage == tree_before,
        "hash cache changed after index damage"
    );
    assert_eq!(paths_delete, first_paths);
    assert_eq!(paths_damage, first_paths);
}
