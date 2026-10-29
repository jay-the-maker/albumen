// SPDX-License-Identifier: Apache-2.0

//! Exit codes: 0 ok, 1 mismatch or drift, 2 usage, 3 refused, 4 I/O.
//! Every failure ends with a `next:` line on stderr.

use std::fs;
use std::process::{Command, Output};

fn albumen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .env(
            "ALBUMEN_HOME",
            std::env::temp_dir().join("albumen-exit-codes-home"),
        )
        .output()
        .unwrap()
}

fn assert_code(output: &Output, code: i32) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(code), "stderr: {stderr}");
    if code != 0 && (code != 2 || stderr.starts_with("albumen:")) {
        let last = stderr.lines().last().unwrap_or("");
        assert!(last.starts_with("next: "), "stderr: {stderr}");
    }
}

#[test]
fn ok_is_zero_and_drift_is_one() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    fs::write(library.join("IMG_0001.jpg"), b"synthetic-exit-code-bytes").unwrap();
    let export = tmp.path().join("export");
    let from = library.to_str().unwrap();
    let to = export.to_str().unwrap();
    let out = albumen(&["export", "--from", from, "--to", to, "--apply"]);
    assert_code(&out, 0);
    let manifest = export.join("ALBUMEN-MANIFEST.txt");
    let manifest = manifest.to_str().unwrap();
    let out = albumen(&["verify-manifest", "--manifest", manifest, "--against", to]);
    assert_code(&out, 0);
    fs::write(export.join("IMG_0001.jpg"), b"changed").unwrap();
    let out = albumen(&["verify-manifest", "--manifest", manifest, "--against", to]);
    assert_code(&out, 1);
}

#[test]
fn bad_arguments_are_two() {
    assert_code(&albumen(&["export", "--no-such-flag"]), 2);
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let out = albumen(&[
        "export",
        "--from",
        library.to_str().unwrap(),
        "--to",
        "s3://",
        "--manifest",
        tmp.path().join("m.txt").to_str().unwrap(),
        "--apply",
    ]);
    assert_code(&out, 2);
}

#[test]
fn export_inside_the_source_is_refused_with_three() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    fs::write(library.join("IMG_0001.jpg"), b"synthetic-exit-code-bytes").unwrap();
    let inside = library.join("export");
    let out = albumen(&[
        "export",
        "--from",
        library.to_str().unwrap(),
        "--to",
        inside.to_str().unwrap(),
        "--apply",
    ]);
    assert_code(&out, 3);
    assert!(!inside.exists());
}

#[test]
fn a_missing_manifest_is_four() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing.txt");
    let out = albumen(&[
        "verify-manifest",
        "--manifest",
        missing.to_str().unwrap(),
        "--against",
        tmp.path().to_str().unwrap(),
    ]);
    assert_code(&out, 4);
}

#[test]
fn help_lists_the_exit_codes() {
    let out = albumen(&["--help"]);
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(help.contains("Exit codes: 0 ok, 1 verification mismatch or drift, 2 usage, 3 refused unsafe layout, 4 I/O."));
    let out = albumen(&["verify-manifest", "--help"]);
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(
        help.contains("Example:\n  albumen verify-manifest"),
        "{help}"
    );
}
