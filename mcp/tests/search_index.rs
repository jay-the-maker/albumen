// SPDX-License-Identifier: Apache-2.0

//! Search reads a saved index outside the library. These tests check that the
//! index answers exactly what a full walk answers, including after files are
//! added, renamed, and removed between runs, and that nothing is written
//! inside the library.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use albumen_ingest::index::LibraryIndex;
use albumen_ingest::{walk_sorted, WalkControl};
use albumen_mcp::Library;
use serde_json::{json, Value};

const QUERIES: &[&str] = &[
    "jpg",
    "img_",
    "2020/",
    "2021/03",
    "dsc",
    "renamed",
    "no-such-item",
];

fn write(root: &Path, relative: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"\xff\xd8\xff\xd9").unwrap();
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

fn expected(root: &Path, query: &str) -> Vec<String> {
    walk(root)
        .into_iter()
        .filter(|path| path.to_lowercase().contains(query))
        .collect()
}

fn search_all(library: &Library, query: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut arguments = json!({"query": query, "limit": 3});
        if let Some(cursor) = cursor.as_ref() {
            arguments["cursor"] = Value::String(cursor.clone());
        }
        let page = library.call("search", &arguments).unwrap();
        for file in page["files"].as_array().unwrap() {
            seen.push(file["path"].as_str().unwrap().to_string());
        }
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => return seen,
        }
    }
}

fn open(root: &Path, index_dir: &Path) -> Library {
    Library::new(root.to_path_buf(), false, false, false, None)
        .unwrap()
        .with_index_dir(Some(index_dir.to_path_buf()))
}

fn assert_matches_walk(library: &Library, root: &Path) {
    for query in QUERIES {
        assert_eq!(
            search_all(library, query),
            expected(root, query),
            "query {query}"
        );
    }
}

/// Move every directory's time an hour back, so a later change gives the
/// directory a new time the index can see.
fn backdate_dirs(root: &Path) {
    let past = SystemTime::now() - Duration::from_secs(3600);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
        }
        set_dir_mtime(&dir, past);
    }
}

#[cfg(unix)]
fn set_dir_mtime(dir: &Path, time: SystemTime) {
    let handle = fs::File::open(dir).unwrap();
    handle
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

#[cfg(windows)]
fn set_dir_mtime(dir: &Path, time: SystemTime) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let handle = fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .unwrap();
    handle
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

fn library_fixture(root: &Path) {
    for year in [2020, 2021] {
        for month in 1..=4 {
            for n in 0..5 {
                let prefix = if n % 2 == 0 { "IMG_" } else { "DSC_" };
                write(
                    root,
                    &format!("{year}/{month:02}/{prefix}{year}{month:02}{n:02}.jpg"),
                );
            }
        }
    }
    write(root, "loose.JPG");
    write(root, "a.b/IMG_9999.heic");
    write(root, "a/IMG_9998.jpg");
}

#[test]
fn index_matches_a_full_walk_across_add_rename_and_remove() {
    let root_dir = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    library_fixture(root);
    backdate_dirs(root);

    // First run builds and saves the index.
    let first = open(root, index_dir.path());
    assert!(!first.warm_index().unwrap().1);
    assert_matches_walk(&first, root);
    drop(first);

    // Between runs: add, rename, remove, a new directory, a renamed directory.
    write(root, "2020/02/IMG_new_added.jpg");
    fs::rename(
        root.join("2020/03/IMG_20200300.jpg"),
        root.join("2020/03/IMG_renamed.jpg"),
    )
    .unwrap();
    fs::remove_file(root.join("2021/01/DSC_20210101.jpg")).unwrap();
    write(root, "2022/01/IMG_20220100.jpg");
    fs::rename(root.join("2021/04"), root.join("2021/04-renamed")).unwrap();

    // Second run loads the saved index and brings it current.
    let second = open(root, index_dir.path());
    assert!(second.warm_index().unwrap().1);
    assert_matches_walk(&second, root);

    // A change while the server runs is seen by the next query.
    write(root, "2021/02/IMG_live.jpg");
    fs::rename(
        root.join("2020/01/IMG_20200100.jpg"),
        root.join("2020/01/renamed_live.jpg"),
    )
    .unwrap();
    fs::remove_file(root.join("a/IMG_9998.jpg")).unwrap();
    assert_matches_walk(&second, root);
    let live = search_all(&second, "live");
    assert_eq!(
        live,
        vec!["2020/01/renamed_live.jpg", "2021/02/IMG_live.jpg"]
    );
}

#[test]
fn unchanged_directories_are_not_listed_again() {
    let root_dir = tempfile::tempdir().unwrap();
    let index_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    library_fixture(root);
    backdate_dirs(root);
    let built = LibraryIndex::open(root, Some(index_dir.path())).unwrap();
    assert!(!built.loaded);
    assert_eq!(built.paths(), walk(root).as_slice());
    let dirs = {
        let mut index = LibraryIndex::open(root, Some(index_dir.path())).unwrap();
        assert!(index.loaded);
        let stats = index.refresh().unwrap();
        assert_eq!(stats.dirs_listed, 0, "{stats:?}");
        assert!(!stats.changed);
        stats.dirs_checked
    };
    assert!(dirs > 10);
    write(root, "2020/02/IMG_one_more.jpg");
    let mut index = LibraryIndex::open(root, Some(index_dir.path())).unwrap();
    assert_eq!(index.paths(), walk(root).as_slice());
    let stats = index.refresh().unwrap();
    assert!(stats.dirs_listed <= 1, "{stats:?}");
}

#[test]
fn the_index_lives_outside_the_library() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    library_fixture(root);
    let before = walk(root);
    let index_dir = tempfile::tempdir().unwrap();
    let library = open(root, index_dir.path());
    assert_matches_walk(&library, root);
    assert_eq!(walk(root), before, "the library gained or lost a file");
    let saved: Vec<_> = fs::read_dir(index_dir.path()).unwrap().collect();
    assert_eq!(saved.len(), 1, "one saved index file");

    let inside = root.join("2020").join("index");
    let refused = open(root, &inside);
    let err = refused
        .call("search", &json!({"query": "jpg"}))
        .unwrap_err();
    assert!(err.contains("inside the library"), "{err}");
    assert!(!inside.exists(), "nothing is created inside the library");
    assert_eq!(walk(root), before);
}

#[test]
fn an_index_dir_that_cannot_be_written_still_answers() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    library_fixture(root);
    let blocker = tempfile::NamedTempFile::new().unwrap();
    // A regular file where the index directory should be: the save fails.
    let library = open(root, &blocker.path().join("index"));
    assert_matches_walk(&library, root);
}
