// SPDX-License-Identifier: Apache-2.0

//! `albumen receive hide|unhide` (#196) on synthetic files: a dry run
//! writes nothing, --apply writes only the tombstone, unhide rewrites it,
//! and the original is never touched.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn albumen(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
    let mut out: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let meta = fs::metadata(&path).unwrap();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(&path).unwrap(),
                meta.modified().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn receive_hide_is_dry_run_by_default_and_unhide_keeps_the_tombstone() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("library");
    fs::create_dir(&lib).unwrap();
    let photo = lib.join("IMG_0100.JPG");
    fs::write(&photo, b"synthetic-receiver-original").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&photo)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000))
        .unwrap();
    let lib_arg = lib.to_str().unwrap();
    let tombstone = lib.join("IMG_0100.JPG.albumen-tombstone");
    let original = (
        fs::read(&photo).unwrap(),
        fs::metadata(&photo).unwrap().modified().unwrap(),
    );
    let kept = || {
        assert_eq!(
            (
                fs::read(&photo).unwrap(),
                fs::metadata(&photo).unwrap().modified().unwrap()
            ),
            original
        );
    };

    let before = snapshot(&lib);
    let (ok, out) = albumen(&[
        "receive",
        "hide",
        "--library",
        lib_arg,
        "--name",
        "IMG_0100.JPG",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("dry-run: no tombstone written"), "{out}");
    assert!(out.contains("change: true"), "{out}");
    assert_eq!(snapshot(&lib), before, "dry run wrote");

    let (ok, out) = albumen(&[
        "receive",
        "hide",
        "--library",
        lib_arg,
        "--name",
        "IMG_0100.JPG",
        "--apply",
    ]);
    assert!(ok, "{out}");
    assert!(
        out.contains("hidden IMG_0100.JPG") && out.contains("original kept"),
        "{out}"
    );
    let names: Vec<_> = snapshot(&lib).into_iter().map(|entry| entry.0).collect();
    assert_eq!(
        names,
        vec!["IMG_0100.JPG", "IMG_0100.JPG.albumen-tombstone"]
    );
    assert!(fs::read_to_string(&tombstone)
        .unwrap()
        .contains("\"hidden\":true"));
    kept();

    let before = snapshot(&lib);
    let (ok, out) = albumen(&[
        "receive",
        "unhide",
        "--library",
        lib_arg,
        "--name",
        "IMG_0100.JPG",
    ]);
    assert!(ok && out.contains("dry-run"), "{out}");
    assert_eq!(snapshot(&lib), before, "dry-run unhide wrote");

    let (ok, out) = albumen(&[
        "receive",
        "unhide",
        "--library",
        lib_arg,
        "--name",
        "IMG_0100.JPG",
        "--execute",
    ]);
    assert!(ok && out.contains("visible IMG_0100.JPG"), "{out}");
    assert!(fs::read_to_string(&tombstone)
        .unwrap()
        .contains("\"hidden\":false"));
    kept();

    let (ok, out) = albumen(&[
        "receive",
        "unhide",
        "--library",
        lib_arg,
        "--name",
        "IMG_0100.JPG",
        "--apply",
    ]);
    assert!(ok && out.contains("already visible"), "{out}");

    for name in [
        "../IMG_0100.JPG",
        "missing.jpg",
        "IMG_0100.JPG.albumen-tombstone",
    ] {
        let before = snapshot(&lib);
        let (ok, out) = albumen(&[
            "receive",
            "hide",
            "--library",
            lib_arg,
            "--name",
            name,
            "--apply",
        ]);
        assert!(!ok, "{name}: {out}");
        assert_eq!(snapshot(&lib), before);
    }
    kept();
}

#[test]
fn receive_hide_dry_run_does_not_create_a_missing_library() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-library");
    let (ok, out) = albumen(&[
        "receive",
        "hide",
        "--library",
        missing.to_str().unwrap(),
        "--name",
        "IMG_0001.JPG",
    ]);
    assert!(!ok, "{out}");
    assert!(!missing.exists(), "a dry run created the library");
}
