// SPDX-License-Identifier: Apache-2.0

//! `albumen backup` on a synthetic library: a first run copies and proves,
//! a second run copies nothing and proves the same set, a run killed partway
//! is finished by the next, and `albumen status` reads the proof. The
//! library is only read.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn cmd(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_albumen"));
    command.env("ALBUMEN_HOME", home);
    command
}

fn albumen(home: &Path, args: &[&str]) -> (i32, String) {
    let out = cmd(home).args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn library(tmp: &Path, n: usize) -> PathBuf {
    let library = tmp.join("library");
    fs::create_dir_all(library.join("roll")).unwrap();
    for i in 0..n {
        fs::write(
            library.join("roll").join(format!("IMG_{i:04}.jpg")),
            format!("synthetic backup photo {i} {}", "x".repeat(i % 97)),
        )
        .unwrap();
    }
    library
}

fn proofs(dest: &Path) -> Vec<serde_json::Value> {
    let mut names: Vec<PathBuf> = fs::read_dir(dest.join("ALBUMEN-PROOFS"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    // `backup-<t>-2` is later than `backup-<t>`: order by length, then name.
    names.sort_by_key(|path| (path.as_os_str().len(), path.clone()));
    names
        .into_iter()
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .collect()
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in fs::read_dir(&dir).unwrap().flatten() {
            let path = item.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_second_backup_copies_nothing_and_proves_the_same_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let lib = library(tmp.path(), 40);
    let before = snapshot(&lib);
    let dest = tmp.path().join("dest");
    let args = ["backup", "--from", p(&lib), "--to", p(&dest)];

    let (code, first) = albumen(&home, &args);
    assert_eq!(code, 0, "{first}");
    assert!(first.contains("export: 40 copied, 0 already"), "{first}");
    assert!(first.contains("backup ok:"), "{first}");

    let (code, second) = albumen(&home, &args);
    assert_eq!(code, 0, "{second}");
    assert!(second.contains("export: 0 copied, 40 already"), "{second}");
    assert!(second.contains("0 hashed, 40 unchanged"), "{second}");

    let proofs = proofs(&dest);
    assert_eq!(proofs.len(), 2);
    for proof in &proofs {
        assert_eq!(proof["result"], "verified");
        assert_eq!(proof["files"], 40);
    }
    assert_eq!(
        proofs[0]["sorted_hashes_sha256"],
        proofs[1]["sorted_hashes_sha256"]
    );
    assert_eq!(proofs[0]["manifest_sha256"], proofs[1]["manifest_sha256"]);
    assert_eq!(snapshot(&lib), before, "the library was written to");
    assert!(!lib.join("ALBUMEN-PROOFS").exists());

    // status reads the proof and ignores the proof folder.
    let out = cmd(&home).args(["status", p(&dest)]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("40 items, all match"), "{text}");
    assert!(text.contains("proof: ALBUMEN-PROOFS/backup-"), "{text}");
}

#[test]
fn a_changed_photo_is_copied_by_the_next_backup_under_a_new_name_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let lib = library(tmp.path(), 10);
    let dest = tmp.path().join("dest");
    let args = ["backup", "--from", p(&lib), "--to", p(&dest)];
    assert_eq!(albumen(&home, &args).0, 0);
    fs::write(
        lib.join("roll").join("NEW_0001.jpg"),
        "a new synthetic photo",
    )
    .unwrap();
    let (code, text) = albumen(&home, &args);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("export: 1 copied, 10 already"), "{text}");
    assert!(text.contains("1 hashed, 10 unchanged"), "{text}");
    assert_eq!(proofs(&dest).last().unwrap()["files"], 11);
}

#[test]
fn a_killed_backup_is_finished_by_the_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let lib = library(tmp.path(), 400);
    let dest = tmp.path().join("dest");
    let args = ["backup", "--from", p(&lib), "--to", p(&dest)];
    for delay_ms in [30u64, 120, 300] {
        let mut child = cmd(&home)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(delay_ms));
        let _ = child.kill(); // SIGKILL on unix
        let _ = child.wait();
    }
    let (code, text) = albumen(&home, &args);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("backup ok:"), "{text}");
    let last = proofs(&dest).pop().unwrap();
    assert_eq!(last["result"], "verified");
    assert_eq!(last["files"], 400);
    let (code, again) = albumen(&home, &args);
    assert_eq!(code, 0, "{again}");
    assert!(again.contains("export: 0 copied, 400 already"), "{again}");
}

#[test]
fn backup_refuses_a_changed_file_at_the_destination_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let lib = library(tmp.path(), 6);
    let dest = tmp.path().join("dest");
    let args = ["backup", "--from", p(&lib), "--to", p(&dest)];
    assert_eq!(albumen(&home, &args).0, 0);

    // The copy at DEST changes (bit rot, an editor saving over it).
    let changed = dest.join("roll/IMG_0003.jpg");
    let mut bytes = fs::read(&changed).unwrap();
    bytes[0] ^= 1;
    fs::write(&changed, &bytes).unwrap();
    fs::write(
        lib.join("roll").join("NEW_0001.jpg"),
        "new after the change",
    )
    .unwrap();
    let lib_before = snapshot(&lib);
    let dest_before = snapshot(&dest);

    let (code, text) = albumen(&home, &args);
    assert_eq!(code, 3, "{text}");
    assert!(text.contains("roll/IMG_0003.jpg"), "{text}");
    assert!(text.contains("nothing written"), "{text}");
    assert_eq!(
        snapshot(&dest),
        dest_before,
        "backup wrote to DEST after refusing"
    );
    assert_eq!(
        fs::read(&changed).unwrap(),
        bytes,
        "the changed copy was overwritten"
    );
    assert!(!dest.join("roll/NEW_0001.jpg").exists());
    assert_eq!(
        proofs(&dest).len(),
        1,
        "a refused run must not write a proof"
    );
    assert_eq!(snapshot(&lib), lib_before, "the library was written to");

    // status says to move the changed copy aside, not to rerun the backup.
    let out = cmd(&home).args(["status", p(&dest)]).output().unwrap();
    let status = String::from_utf8_lossy(&out.stdout);
    let last = status.lines().last().unwrap();
    assert!(
        last.starts_with("next: move the changed copy aside"),
        "{status}"
    );
    assert!(!last.contains("albumen backup"), "{status}");
}

#[test]
fn backup_refuses_a_remote_destination_with_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let lib = library(tmp.path(), 1);
    let (code, text) = albumen(
        &home,
        &["backup", "--from", p(&lib), "--to", "s3://bucket/x"],
    );
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("albumen export"), "{text}");
}
