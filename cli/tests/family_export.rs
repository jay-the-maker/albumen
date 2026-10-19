// SPDX-License-Identifier: Apache-2.0

//! `albumen family export` copies every member's originals and sidecars.
//! `verify-manifest` reconciles that export. The library files stay.

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

fn summary_field(text: &str, key: &str) -> u64 {
    let line = text
        .lines()
        .rev()
        .find(|line| line.contains("\"count_in\""))
        .unwrap_or_else(|| panic!("missing reconciliation summary in:\n{text}"));
    let needle = format!("\"{key}\":");
    let rest = line
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("missing {key} in {line}"));
    rest.chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("bad {key} in {line}"))
}

#[test]
fn family_export_reconciles_and_leaves_member_objects_unchanged() {
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

    // 100 ns aligned so NTFS stores this exact tick.
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700);
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
    let sha_a = line(&text, "sha256 ").to_string();
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
    let sha_b = line(&text, "sha256 ").to_string();

    let object_a = library
        .join("members")
        .join(&member_a)
        .join("objects")
        .join("sha256")
        .join(&sha_a[..2])
        .join(&sha_a);
    let side_a = library
        .join("members")
        .join(&member_a)
        .join("objects")
        .join("sha256")
        .join(&sha_a[..2])
        .join(format!("{sha_a}.sidecar"));
    let object_b = library
        .join("members")
        .join(&member_b)
        .join("objects")
        .join("sha256")
        .join(&sha_b[..2])
        .join(&sha_b);
    let before_a = kept(&object_a);
    let before_side = kept(&side_a);
    let before_b = kept(&object_b);

    let dest = tmp.path().join("export");
    let (ok, text) = run(&[
        "family",
        "export",
        "--library",
        library.to_str().unwrap(),
        "--to",
        dest.to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("dry-run"));
    assert!(!dest.exists());

    let (ok, text) = run(&[
        "family",
        "export",
        "--library",
        library.to_str().unwrap(),
        "--to",
        library.join("inside").to_str().unwrap(),
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("inside") || text.contains("family library") || text.contains("read-only"),
        "{text}"
    );
    assert_eq!(kept(&object_a), before_a);
    assert_eq!(kept(&side_a), before_side);
    assert_eq!(kept(&object_b), before_b);

    let (ok, text) = run(&[
        "family",
        "export",
        "--library",
        library.to_str().unwrap(),
        "--to",
        dest.to_str().unwrap(),
        "--apply",
    ]);
    assert!(ok, "{text}");
    let count_in = summary_field(&text, "count_in");
    let count_out = summary_field(&text, "count_out");
    let mismatched = summary_field(&text, "mismatched");
    assert_eq!(count_in, count_out, "{text}");
    assert_eq!(mismatched, 0, "{text}");
    assert_eq!(count_in, 3, "{text}");

    let manifest = dest.join("ALBUMEN-MANIFEST.txt");
    let (ok, verified) = run(&[
        "verify-manifest",
        "--manifest",
        manifest.to_str().unwrap(),
        "--against",
        dest.to_str().unwrap(),
    ]);
    assert!(ok, "{verified}");
    let verified_in = summary_field(&verified, "count_in");
    let verified_out = summary_field(&verified, "count_out");
    let verified_bad = summary_field(&verified, "mismatched");
    assert_eq!(verified_in, verified_out, "{verified}");
    assert_eq!(verified_bad, 0, "{verified}");
    assert!(verified_in >= count_in, "{verified}");

    let copied_a = dest
        .join("members")
        .join(&member_a)
        .join(&sha_a)
        .join("note.txt");
    let copied_side = dest
        .join("members")
        .join(&member_a)
        .join(&sha_a)
        .join("note.txt.sidecar");
    let copied_b = dest
        .join("members")
        .join(&member_b)
        .join(&sha_b)
        .join("other.txt");
    assert_eq!(sha256_hex(&copied_a).unwrap(), before_a.sha256);
    assert_eq!(sha256_hex(&copied_side).unwrap(), before_side.sha256);
    assert_eq!(sha256_hex(&copied_b).unwrap(), before_b.sha256);
    assert_eq!(fs::read(&copied_a).unwrap(), before_a.bytes);
    assert_eq!(fs::read(&copied_side).unwrap(), before_side.bytes);
    assert_eq!(fs::read(&copied_b).unwrap(), before_b.bytes);

    for member in [&member_a, &member_b] {
        let provenance =
            fs::read_to_string(dest.join("members").join(member).join("provenance.json")).unwrap();
        assert!(provenance.contains(member), "{provenance}");
        assert!(provenance.contains("signed_manifest"), "{provenance}");
        assert!(provenance.contains("signature"), "{provenance}");
    }

    assert_eq!(kept(&object_a), before_a);
    assert_eq!(kept(&side_a), before_side);
    assert_eq!(kept(&object_b), before_b);

    let mut tampered = fs::read(&copied_a).unwrap();
    tampered[0] ^= 0xff;
    fs::write(&copied_a, &tampered).unwrap();
    let (ok, mismatch) = run(&[
        "verify-manifest",
        "--manifest",
        manifest.to_str().unwrap(),
        "--against",
        dest.to_str().unwrap(),
    ]);
    assert!(!ok, "{mismatch}");
    assert!(
        mismatch.contains("mismatch") || mismatch.contains(&before_a.sha256),
        "{mismatch}"
    );
    assert_eq!(kept(&object_a), before_a);
    assert_eq!(kept(&side_a), before_side);
    assert_eq!(kept(&object_b), before_b);
}
