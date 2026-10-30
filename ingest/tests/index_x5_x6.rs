// SPDX-License-Identifier: Apache-2.0

//! M6 X5 and X6 on a synthetic library.
//!
//! X5: deleting the saved index, or damaging it, and rebuilding loses no
//! metadata. The manifest is the path set, each file's SHA-256 (the hash
//! search keeps), and the bytes of every sidecar; it is taken before and
//! after, and the two are diffed.
//!
//! X6: every original's SHA-256, name and modification time are unchanged
//! after the index is saved, deleted, rebuilt and refreshed many times, and
//! an index directory inside the library is refused.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use albumen_ingest::index::{read_saved_file, slot_files, LibraryIndex};
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq, Clone)]
struct Entry {
    sha256: String,
    len: u64,
    mtime_ns: u128,
    sidecar: Option<Vec<u8>>,
}

fn make_library(root: &Path, files: usize) {
    for i in 0..files {
        let dir = root.join(format!("{}/{:02}", 2000 + i % 20, i % 12 + 1));
        std::fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..(64 + i % 300)).map(|b| (b * 31 + i) as u8).collect();
        std::fs::write(dir.join(format!("IMG_{i:05}.jpg")), &body).unwrap();
        if i % 3 == 0 {
            let xmp = format!(
                "<x:xmpmeta><rdf:Description dc:title=\"synthetic {i}\" xmp:Rating=\"{}\" \
                 dc:subject=\"tag{}\"/></x:xmpmeta>\n",
                i % 6,
                i % 7
            );
            std::fs::write(dir.join(format!("IMG_{i:05}.jpg.xmp")), xmp).unwrap();
        }
        if i % 5 == 0 {
            let json = format!(
                "{{\"title\":\"synthetic {i}\",\"favorited\":{}}}\n",
                i % 2 == 0
            );
            std::fs::write(dir.join(format!("IMG_{i:05}.jpg.json")), json).unwrap();
        }
    }
}

fn manifest(root: &Path) -> BTreeMap<String, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in std::fs::read_dir(&dir).unwrap() {
            let item = item.unwrap();
            let path = item.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let is_sidecar = rel.ends_with(".xmp") || rel.ends_with(".json");
            out.insert(
                rel,
                Entry {
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    len: meta.len(),
                    mtime_ns: meta
                        .modified()
                        .unwrap()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    sidecar: is_sidecar.then_some(bytes),
                },
            );
        }
    }
    out
}

fn diff(before: &BTreeMap<String, Entry>, after: &BTreeMap<String, Entry>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in before {
        match after.get(k) {
            None => out.push(format!("- {k}")),
            Some(w) if w != v => out.push(format!("~ {k}")),
            _ => {}
        }
    }
    for k in after.keys() {
        if !before.contains_key(k) {
            out.push(format!("+ {k}"));
        }
    }
    out
}

fn index_set(index: &LibraryIndex) -> Vec<String> {
    let mut p = index.paths().to_vec();
    p.sort();
    p
}

#[test]
fn deleting_or_damaging_the_index_loses_no_metadata() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    make_library(library.path(), 1200);
    let before = manifest(library.path());
    let walk: Vec<String> = before.keys().cloned().collect();

    let mut index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    index.refresh().unwrap();
    let file = index.file().unwrap().to_path_buf();
    assert_eq!(index_set(&index), walk, "first index is not the library");
    let first_gen = index.generation().unwrap();
    drop(index);

    // 1. Delete both slots, then reopen: rebuilt from the library.
    for slot in slot_files(&file) {
        let _ = std::fs::remove_file(slot);
    }
    let rebuilt = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let after_delete = index_set(&rebuilt);
    let (_, saved) = read_saved_file(&file).unwrap();
    let mut saved = saved;
    saved.sort();
    drop(rebuilt);

    // 2. Damage every slot (flip a byte in the middle), then reopen.
    for slot in slot_files(&file) {
        if let Ok(mut bytes) = std::fs::read(&slot) {
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0x55;
            std::fs::write(&slot, bytes).unwrap();
        }
    }
    let repaired = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
    let after_damage = index_set(&repaired);
    drop(repaired);

    let after = manifest(library.path());
    let changes = diff(&before, &after);
    let sidecars = before.values().filter(|e| e.sidecar.is_some()).count();
    println!(
        "M6 X5 RESULT entries={} originals={} sidecars={} first_gen={first_gen} \
         manifest_diff_lines={} index_after_delete_equals_walk={} saved_after_delete_equals_walk={} \
         index_after_damage_equals_walk={}",
        before.len(),
        before.len() - sidecars,
        sidecars,
        changes.len(),
        after_delete == walk,
        saved == walk,
        after_damage == walk
    );
    for line in changes.iter().take(20) {
        println!("M6 X5 DIFF {line}");
    }
    assert!(changes.is_empty());
    assert_eq!(after_delete, walk);
    assert_eq!(saved, walk);
    assert_eq!(after_damage, walk);
}

#[test]
fn originals_are_untouched_and_an_index_inside_the_library_is_refused() {
    let library = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    make_library(library.path(), 800);
    let before = manifest(library.path());

    let mut cycles = 0;
    for round in 0..50 {
        let mut index = LibraryIndex::open(library.path(), Some(index_dir.path())).unwrap();
        index.refresh().unwrap();
        if round % 10 == 9 {
            for slot in slot_files(index.file().unwrap()) {
                let _ = std::fs::remove_file(slot);
            }
        }
        cycles += 1;
    }
    let after = manifest(library.path());
    let changes = diff(&before, &after);

    let inside = library.path().join("2001/02/.albumen-index");
    let refused_inside = LibraryIndex::open(library.path(), Some(&inside)).is_err();
    let refused_root = LibraryIndex::open(library.path(), Some(library.path())).is_err();
    let created = inside.exists();
    #[cfg(unix)]
    let refused_link = {
        let link = index_dir.path().join("link-into-library");
        std::os::unix::fs::symlink(library.path().join("2003"), &link).unwrap();
        LibraryIndex::open(library.path(), Some(&link)).is_err()
    };
    #[cfg(not(unix))]
    let refused_link = true;
    let after_refusals = diff(&before, &manifest(library.path()));

    println!(
        "M6 X6 RESULT originals_and_sidecars={} cycles={cycles} sha_name_mtime_diff_lines={} \
         refused_inside={refused_inside} refused_root={refused_root} refused_symlink_into={refused_link} \
         inside_dir_created={created} diff_after_refusals={}",
        before.len(),
        changes.len(),
        after_refusals.len()
    );
    for line in changes.iter().take(20) {
        println!("M6 X6 DIFF {line}");
    }
    assert!(changes.is_empty());
    assert!(refused_inside && refused_root && refused_link);
    assert!(
        !created,
        "a refused index dir was created inside the library"
    );
    assert!(after_refusals.is_empty());
}
