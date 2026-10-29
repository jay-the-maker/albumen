// SPDX-License-Identifier: Apache-2.0

//! An export re-run reports what it did: new copies and files already present
//! and verified, never the whole library as "exported". The dry-run uses the
//! same wording. Synthetic bytes only.

use std::fs;
use std::path::Path;
use std::process::Command;

fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn ok(args: &[&str]) -> String {
    let (success, text) = run(args);
    assert!(success, "albumen {}: {text}", args.join(" "));
    text
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn export(from: &Path, to: &Path, manifest: &Path, apply: bool) -> String {
    let mut args = vec![
        "export",
        "--from",
        p(from),
        "--to",
        p(to),
        "--manifest",
        p(manifest),
    ];
    if apply {
        args.push("--apply");
    }
    ok(&args)
}

#[test]
fn rerun_reports_zero_new_and_every_file_already_present() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let roll = library.join("roll");
    fs::create_dir_all(&roll).unwrap();
    for i in 0..15 {
        fs::write(
            roll.join(format!("IMG_{i:04}.jpg")),
            format!("synthetic {i}"),
        )
        .unwrap();
    }
    let target = tmp.path().join("export");
    let manifests = tmp.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();

    let plan = export(&library, &target, &manifests.join("m0.txt"), false);
    assert!(
        plan.contains("would export 15 new, 0 already present and verified (15 files)"),
        "{plan}"
    );
    assert!(!target.exists(), "dry-run wrote the target");

    let first = export(&library, &target, &manifests.join("m1.txt"), true);
    assert!(
        first.contains("exported 15 new, 0 already present and verified (15 files)"),
        "{first}"
    );

    let plan = export(&library, &target, &manifests.join("m2.txt"), false);
    assert!(
        plan.contains("would export 0 new, 15 already present and verified (15 files)"),
        "{plan}"
    );

    let again = export(&library, &target, &manifests.join("m3.txt"), true);
    assert!(
        again.contains("exported 0 new, 15 already present and verified (15 files)"),
        "{again}"
    );
    assert!(!again.contains("exported 15"), "{again}");

    // One removed from the target, one added to the library.
    fs::remove_file(target.join("roll/IMG_0003.jpg")).unwrap();
    fs::write(roll.join("IMG_0100.jpg"), b"synthetic new").unwrap();
    let plan = export(&library, &target, &manifests.join("m4.txt"), false);
    assert!(
        plan.contains("would export 2 new, 14 already present and verified (16 files)"),
        "{plan}"
    );
    let partial = export(&library, &target, &manifests.join("m5.txt"), true);
    assert!(
        partial.contains("exported 2 new, 14 already present and verified (16 files)"),
        "{partial}"
    );
}

#[test]
fn dry_run_names_a_conflict_that_apply_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    fs::write(library.join("IMG_0001.jpg"), b"library bytes").unwrap();
    let target = tmp.path().join("export");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("IMG_0001.jpg"), b"different bytes").unwrap();
    let manifests = tmp.path().join("m");
    fs::create_dir_all(&manifests).unwrap();

    let plan = export(&library, &target, &manifests.join("a.txt"), false);
    assert!(
        plan.contains("would export 0 new, 0 already present and verified (1 files)"),
        "{plan}"
    );
    assert!(plan.contains("conflicts: 1"), "{plan}");

    let m = manifests.join("b.txt");
    let (success, text) = run(&[
        "export",
        "--from",
        p(&library),
        "--to",
        p(&target),
        "--manifest",
        p(&m),
        "--apply",
    ]);
    assert!(!success, "{text}");
    assert_eq!(
        fs::read(target.join("IMG_0001.jpg")).unwrap(),
        b"different bytes"
    );
}
