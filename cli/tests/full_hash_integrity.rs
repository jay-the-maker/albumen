// SPDX-License-Identifier: Apache-2.0

//! M3 guard: the warm-search stat shortcut is search-only. verify-manifest,
//! status and backup must still SHA-256 every file even when a file's size and
//! mtime are unchanged. Each case flips one byte in place (same size), puts
//! the old mtime back, and waits past the racy window so a stat-trusting
//! check would see nothing to do.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn albumen(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Flip one byte of `path` without changing its size, then restore its mtime.
fn flip_keep_mtime(path: &Path) {
    let mtime = fs::metadata(path).unwrap().modified().unwrap();
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 0x01;
    fs::write(path, &bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), mtime);
}

#[test]
fn verify_status_and_backup_rehash_files_with_unchanged_size_and_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let dest = tmp.path().join("dest");
    let state = tmp.path().join("state");
    fs::create_dir_all(library.join("roll")).unwrap();
    for i in 0..20 {
        fs::write(
            library.join("roll").join(format!("IMG_{i:04}.jpg")),
            format!("synthetic integrity {i:04}"),
        )
        .unwrap();
    }
    let backup = || {
        albumen(&[
            "backup",
            "--from",
            p(&library),
            "--to",
            p(&dest),
            "--state",
            p(&state),
        ])
    };
    let (code, text) = backup();
    assert_eq!(code, 0, "{text}");
    std::thread::sleep(Duration::from_millis(2_100));

    // Destination copy changed in place: verify-manifest and status catch it.
    let target = dest.join("roll").join("IMG_0007.jpg");
    let manifest = dest.join("ALBUMEN-MANIFEST.txt");
    flip_keep_mtime(&target);
    let (code, text) = albumen(&[
        "verify-manifest",
        "--manifest",
        p(&manifest),
        "--against",
        p(&dest),
    ]);
    assert_ne!(code, 0, "verify-manifest missed a same-size flip: {text}");
    assert!(text.contains("IMG_0007.jpg"), "{text}");
    let (code, text) = albumen(&["status", p(&dest)]);
    assert_eq!(code, 1, "status missed a same-size flip: {text}");
    flip_keep_mtime(&target);
    let (code, text) = albumen(&[
        "verify-manifest",
        "--manifest",
        p(&manifest),
        "--against",
        p(&dest),
    ]);
    assert_eq!(code, 0, "{text}");

    // Source changed in place: backup re-hashes it and refuses to pass it off
    // as the copy already in DEST.
    let before = fs::read(&target).unwrap();
    let source = library.join("roll").join("IMG_0007.jpg");
    flip_keep_mtime(&source);
    std::thread::sleep(Duration::from_millis(2_100));
    let (code, text) = backup();
    assert_ne!(code, 0, "backup trusted stat for a changed source: {text}");
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "DEST copy was overwritten"
    );
}
