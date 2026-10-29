// SPDX-License-Identifier: Apache-2.0

//! `albumen import-card` (issue #28). Synthetic fixtures only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn make_card(root: &Path) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let card = root.join("card");
    for (dir, fixture) in [
        ("100TEST", "xmp/IMG_0004.jpg"),
        ("100TEST", "xmp/IMG_0004.xmp"),
        ("100TEST", "raw-jpeg/IMG_0003.dng"),
        ("100TEST", "raw-jpeg/IMG_0003.jpg"),
        // A second folder reusing a camera name with other bytes, as
        // cameras do after a counter reset.
        ("101TEST", "aae/IMG_0008.jpg"),
    ] {
        let to = card.join("DCIM").join(dir);
        fs::create_dir_all(&to).unwrap();
        let src = fixtures.join(fixture);
        let name = if dir == "101TEST" {
            "IMG_0004.jpg".into()
        } else {
            src.file_name().unwrap().to_owned()
        };
        fs::copy(&src, to.join(name)).unwrap();
    }
    card
}

fn tree(dir: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let mtime = fs::metadata(&path).unwrap().modified().unwrap();
                out.push((rel, fs::read(&path).unwrap(), mtime));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    let before = tree(&card);

    let out = albumen()
        .args(["import-card", "--from"])
        .arg(&card)
        .arg("--to")
        .arg(&library)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dry-run: no files written"), "{stdout}");
    assert!(stdout.contains("assets: 3"), "{stdout}");
    assert!(stdout.contains("files: 5"), "{stdout}");
    assert!(!library.exists(), "dry run must not create the library");
    assert_eq!(tree(&card), before);
}

#[test]
fn apply_imports_renames_the_reused_name_and_leaves_the_card() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let library = tmp.path().join("library");
    let before = tree(&card);

    let out = albumen()
        .args(["import-card", "--apply", "--from"])
        .arg(&card)
        .arg("--to")
        .arg(&library)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("imported 3 assets (5 files, 0 already present)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("renamed: IMG_0004.jpg -> IMG_0004~"),
        "{stdout}"
    );
    assert_eq!(tree(&card), before, "card unchanged");

    // Both IMG_0004.jpg originals are in the library, byte-exact.
    let first = fs::read(card.join("DCIM/100TEST/IMG_0004.jpg")).unwrap();
    let second = fs::read(card.join("DCIM/101TEST/IMG_0004.jpg")).unwrap();
    assert_eq!(fs::read(library.join("IMG_0004.jpg")).unwrap(), first);
    let renamed: Vec<_> = fs::read_dir(&library)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("IMG_0004~") && n.ends_with(".jpg"))
        .collect();
    assert_eq!(renamed.len(), 1, "{renamed:?}");
    assert_eq!(fs::read(library.join(&renamed[0])).unwrap(), second);
    assert!(library
        .join(format!("{}.albumen-card.json", renamed[0]))
        .is_file());

    // Running it again copies nothing.
    let lib_before = tree(&library);
    let again = albumen()
        .args(["import-card", "--execute", "--from"])
        .arg(&card)
        .arg("--to")
        .arg(&library)
        .output()
        .unwrap();
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout)
        .contains("imported 0 assets (0 files, 3 already present)"));
    assert_eq!(tree(&library), lib_before);
}

#[test]
fn a_library_on_the_card_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let card = make_card(tmp.path());
    let before = tree(&card);

    let out = albumen()
        .args(["import-card", "--apply", "--from"])
        .arg(&card)
        .arg("--to")
        .arg(card.join("library"))
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert_eq!(tree(&card), before, "nothing created on the card");
}
