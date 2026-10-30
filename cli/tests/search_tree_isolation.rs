// SPDX-License-Identifier: Apache-2.0

//! M3 v2 guard (XO): the persisted search tree is search-only.
//! verify-manifest, backup, status and export must never open it. The tree path for
//! the library and for the backup is made a FIFO: any open for read or write
//! blocks until the other end opens, so a command that touched it would hang
//! and fail this test's deadline. The source of the CLI and of the ingest
//! integrity code must not mention the tree at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Run `albumen args` with `ALBUMEN_INDEX_DIR=index_dir`; fail if it is
/// still running after 60 s (it would be, blocked on a FIFO it opened).
fn albumen(index_dir: &Path, args: &[&str]) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .env("ALBUMEN_INDEX_DIR", index_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run albumen");
    let began = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if began.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("albumen {args:?} hung: it opened the search tree");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// Where albumen-mcp keeps the search tree for `library`: `<key>.search`
/// beside the `<key>.idx` index.
fn tree_path(library: &Path, index_dir: &Path) -> PathBuf {
    let index = albumen_ingest::index::LibraryIndex::open(library, Some(index_dir)).unwrap();
    index.file().unwrap().with_extension("search")
}

#[cfg(unix)]
fn plant_fifo(path: &Path) {
    let _ = fs::remove_file(path);
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success(), "mkfifo {}", path.display());
}

#[cfg(unix)]
fn is_fifo(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_fifo())
        .unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn verify_manifest_backup_status_and_export_never_open_the_search_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let dest = tmp.path().join("dest");
    let exported = tmp.path().join("exported");
    let state = tmp.path().join("state");
    let index_dir = tmp.path().join("index");
    fs::create_dir_all(library.join("roll")).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(&exported).unwrap();
    fs::create_dir_all(&index_dir).unwrap();
    for i in 0..20 {
        fs::write(
            library.join("roll").join(format!("IMG_{i:04}.jpg")),
            format!("synthetic isolation {i:04}"),
        )
        .unwrap();
    }
    let trees = [
        tree_path(&library, &index_dir),
        tree_path(&dest, &index_dir),
        tree_path(&exported, &index_dir),
    ];
    for tree in &trees {
        plant_fifo(tree);
    }
    let backup = [
        "backup",
        "--from",
        p(&library),
        "--to",
        p(&dest),
        "--state",
        p(&state),
    ];
    let (code, text) = albumen(&index_dir, &backup);
    assert_eq!(code, 0, "{text}");
    let manifest = dest.join("ALBUMEN-MANIFEST.txt");
    let (code, text) = albumen(
        &index_dir,
        &[
            "verify-manifest",
            "--manifest",
            p(&manifest),
            "--against",
            p(&dest),
        ],
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = albumen(&index_dir, &["status", p(&dest)]);
    assert_eq!(code, 0, "{text}");
    // Export: the plan, then the copy, then its manifest is verified.
    let (code, text) = albumen(
        &index_dir,
        &["export", "--from", p(&library), "--to", p(&exported)],
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = albumen(
        &index_dir,
        &[
            "export",
            "--from",
            p(&library),
            "--to",
            p(&exported),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = albumen(
        &index_dir,
        &[
            "verify-manifest",
            "--manifest",
            p(&exported.join("ALBUMEN-MANIFEST.txt")),
            "--against",
            p(&exported),
        ],
    );
    assert_eq!(code, 0, "{text}");
    // A second backup of an unchanged library takes the incremental path.
    let (code, text) = albumen(&index_dir, &backup);
    assert_eq!(code, 0, "{text}");
    for tree in &trees {
        assert!(is_fifo(tree), "{} was replaced", tree.display());
    }
}

#[test]
fn integrity_code_does_not_mention_the_search_tree() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut stack = vec![workspace.join("cli/src"), workspace.join("ingest/src")];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for needle in [
                "search_tree",
                "albumen-search-tree",
                ".search\"",
                "albumen_mcp",
            ] {
                assert!(
                    !text.contains(needle),
                    "{} mentions {needle}",
                    path.display()
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 10, "only {checked} source files checked");
    let manifest = fs::read_to_string(workspace.join("cli/Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("albumen-mcp"),
        "cli depends on albumen-mcp"
    );
}
