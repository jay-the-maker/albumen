// SPDX-License-Identifier: Apache-2.0

//! One member cannot change another member's originals through `albumen family`
//! or `albumen export`.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::sha256_hex;

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn run(args: &[impl AsRef<std::ffi::OsStr>]) -> (bool, String) {
    let output = albumen().args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn line<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in:\n{text}"))
}

fn stamp(path: &Path, bytes: &[u8], mtime: SystemTime) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    bytes: Vec<u8>,
    mtime: SystemTime,
    sha256: String,
}

fn kept(path: &Path) -> Kept {
    Kept {
        bytes: fs::read(path).unwrap(),
        mtime: fs::metadata(path).unwrap().modified().unwrap(),
        sha256: sha256_hex(path).unwrap(),
    }
}

#[test]
fn member_cannot_alter_another_members_originals_through_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let key_a = tmp.path().join("a.key");
    let key_b = tmp.path().join("b.key");
    let (ok, text) = run(&[
        "family",
        "keygen",
        "--key",
        key_a.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    let member_a = line(&text, "member ").to_string();
    let (ok, text) = run(&[
        "family",
        "keygen",
        "--key",
        key_b.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    let member_b = line(&text, "member ").to_string();

    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let (ok, text) = run(&[
        "family",
        "init",
        "--to",
        parent.to_str().unwrap(),
        "--key",
        key_a.to_str().unwrap(),
        "--name",
        "ada",
        "--apply",
    ]);
    assert!(ok, "{text}");
    let library = PathBuf::from(line(&text, "library "));

    let (ok, text) = run(&[
        "family",
        "invite",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_a.to_str().unwrap(),
        "--member",
        &member_b,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let token = line(&text, "token ").to_string();
    let (ok, text) = run(&[
        "family",
        "join",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--token",
        &token,
        "--name",
        "bea",
        "--apply",
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        "family",
        "admit",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_a.to_str().unwrap(),
        "--member",
        &member_b,
        "--token",
        &token,
        "--apply",
    ]);
    assert!(ok, "{text}");

    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = tmp.path().join("note.txt");
    let sidecar = tmp.path().join("note.xmp");
    stamp(&photo, b"synthetic-family-original", mtime);
    stamp(&sidecar, b"<x:xmpmeta synthetic/>", mtime);
    let (ok, text) = run(&[
        "family",
        "add",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_a.to_str().unwrap(),
        "--from",
        photo.to_str().unwrap(),
        "--sidecar",
        sidecar.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    let sha = line(&text, "sha256 ").to_string();
    let object = library
        .join("members")
        .join(&member_a)
        .join("objects")
        .join("sha256")
        .join(&sha[..2])
        .join(&sha);
    let sidecar_stored = library
        .join("members")
        .join(&member_a)
        .join("objects")
        .join("sha256")
        .join(&sha[..2])
        .join(format!("{sha}.sidecar"));
    let original = kept(&object);
    let side = kept(&sidecar_stored);
    assert_eq!(original.bytes, b"synthetic-family-original");
    assert_eq!(original.sha256, sha);
    assert_eq!(original.mtime, mtime);
    assert_eq!(side.bytes, b"<x:xmpmeta synthetic/>");
    assert_eq!(side.mtime, mtime);

    let (ok, text) = run(&[
        "family",
        "hide",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--sha256",
        &sha,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        "family",
        "album-add",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--album",
        "trip",
        "--sha256",
        &sha,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let add_id = line(&text, "add ").to_string();
    let (ok, text) = run(&[
        "family",
        "album-omit",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--add",
        &add_id,
        "--apply",
    ]);
    assert!(ok, "{text}");
    let (ok, text) = run(&[
        "family",
        "revoke",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--member",
        &member_a,
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("cannot revoke"));

    let other = tmp.path().join("other.txt");
    stamp(&other, b"bea-bytes", mtime);
    let (ok, text) = run(&[
        "family",
        "add",
        "--library",
        library.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--from",
        other.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");

    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    stamp(&outside.join("extra.txt"), b"not-a-member-write", mtime);
    let (ok, text) = run(&[
        "export",
        "--from",
        outside.to_str().unwrap(),
        "--to",
        library.to_str().unwrap(),
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(text.contains("family library"));

    assert_eq!(kept(&object), original);
    assert_eq!(kept(&sidecar_stored), side);
    assert!(object.is_file());
    assert!(sidecar_stored.is_file());

    let (ok, manifest) = run(&["family", "manifest", "--library", library.to_str().unwrap()]);
    assert!(ok, "{manifest}");
    assert!(manifest.contains(&sha));
    assert!(manifest.contains(&member_a));
    assert!(manifest.contains("\"hidden\": true"));
}

#[test]
fn family_writes_require_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("a.key");
    let (ok, text) = run(&["family", "keygen", "--key", key.to_str().unwrap()]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"));
    assert!(!key.exists());
}
