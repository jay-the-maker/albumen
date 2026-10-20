// SPDX-License-Identifier: Apache-2.0

//! #147: `albumen family tombstone` records a removal as a signed record
//! merged through the family manifest. The stored original and sidecar stay
//! on copy A, copy B is untouched, and a fresh copy still carries them.
//! Synthetic bytes only; no network.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Snapshot = BTreeMap<String, (Vec<u8>, SystemTime)>;

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
    let (ok, text) = run(args);
    assert!(ok, "albumen {}: {text}", args.join(" "));
    text
}

fn line(text: &str, prefix: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in:\n{text}"))
        .to_string()
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stamp(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    // 100 ns aligned so NTFS keeps this exact tick.
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn snapshot(root: &Path) -> Snapshot {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            out.insert(relative, (fs::read(&path).unwrap(), modified));
        }
    }
    out
}

fn objects_of(snapshot: &Snapshot) -> Snapshot {
    snapshot
        .iter()
        .filter(|(name, _)| name.contains("/objects/"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

#[test]
fn a_family_tombstone_is_a_record_and_every_original_stays() {
    let work = tempfile::tempdir().unwrap();
    let key_a = work.path().join("a.key");
    let key_b = work.path().join("b.key");
    ok(&["family", "keygen", "--key", p(&key_a), "--apply"]);
    let member_b = line(
        &ok(&["family", "keygen", "--key", p(&key_b), "--apply"]),
        "member ",
    );
    let parent = work.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let copy_a = PathBuf::from(line(
        &ok(&[
            "family",
            "init",
            "--to",
            p(&parent),
            "--key",
            p(&key_a),
            "--name",
            "ada",
            "--apply",
        ]),
        "library ",
    ));
    let token = line(
        &ok(&[
            "family",
            "invite",
            "--library",
            p(&copy_a),
            "--key",
            p(&key_a),
            "--member",
            &member_b,
            "--apply",
        ]),
        "token ",
    );
    ok(&[
        "family",
        "join",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_b),
        "--token",
        &token,
        "--name",
        "bea",
        "--apply",
    ]);
    ok(&[
        "family",
        "admit",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--member",
        &member_b,
        "--token",
        &token,
        "--apply",
    ]);

    let inputs = work.path().join("inputs");
    let photo = inputs.join("IMG_0001.jpg");
    let sidecar = inputs.join("IMG_0001.xmp");
    stamp(&photo, b"synthetic-tombstone-original");
    stamp(&sidecar, b"<x:xmpmeta synthetic/>");
    let sha = line(
        &ok(&[
            "family",
            "add",
            "--library",
            p(&copy_a),
            "--key",
            p(&key_a),
            "--from",
            p(&photo),
            "--sidecar",
            p(&sidecar),
            "--apply",
        ]),
        "sha256 ",
    );
    let inputs_before = snapshot(&inputs);

    let copy_b = work.path().join("copy-b");
    let manifests = work.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    ok(&[
        "family",
        "export",
        "--library",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&manifests.join("b1.txt")),
        "--apply",
    ]);
    let b_before = snapshot(&copy_b);
    let a_objects = objects_of(&snapshot(&copy_a));
    assert_eq!(a_objects.len(), 2, "{:?}", a_objects.keys());

    // Dry run writes nothing.
    let a_before = snapshot(&copy_a);
    let text = ok(&[
        "family",
        "tombstone",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--sha256",
        &sha,
    ]);
    assert!(text.contains("dry-run"), "{text}");
    assert_eq!(snapshot(&copy_a), a_before);

    // Bea cannot tombstone Ada's photo.
    let (refused, text) = run(&[
        "family",
        "tombstone",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_b),
        "--sha256",
        &sha,
        "--apply",
    ]);
    assert!(!refused, "{text}");
    assert!(text.contains("only an object they added"), "{text}");

    // Ada tombstones her own photo: a record, merged into the manifest.
    let text = ok(&[
        "family",
        "tombstone",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--sha256",
        &sha,
        "--apply",
    ]);
    assert!(text.contains("original kept"), "{text}");
    let manifest = ok(&["family", "manifest", "--library", p(&copy_a)]);
    assert!(manifest.contains("\"tombstones\""), "{manifest}");
    assert!(manifest.contains(&sha), "{manifest}");
    ok(&[
        "family",
        "manifest",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--apply",
    ]);

    // Copy A keeps the original and sidecar bit-exact; copy B is untouched.
    assert_eq!(objects_of(&snapshot(&copy_a)), a_objects);
    assert_eq!(snapshot(&copy_b), b_before);

    // A fresh copy made after the tombstone still carries both files, and
    // its provenance carries the tombstone record.
    let copy_c = work.path().join("copy-c");
    ok(&[
        "family",
        "export",
        "--library",
        p(&copy_a),
        "--to",
        p(&copy_c),
        "--manifest",
        p(&manifests.join("c1.txt")),
        "--apply",
    ]);
    let c = snapshot(&copy_c);
    for (name, (bytes, _)) in b_before
        .iter()
        .filter(|(name, _)| !name.ends_with("provenance.json"))
    {
        assert!(
            c.get(name).map(|(now, _)| now == bytes).unwrap_or(false),
            "fresh copy lost {name}"
        );
    }
    let provenance: String = c
        .iter()
        .filter(|(name, _)| name.ends_with("provenance.json"))
        .map(|(_, (bytes, _))| String::from_utf8_lossy(bytes).into_owned())
        .collect();
    assert!(provenance.contains("tombstone"), "{provenance}");
    assert_eq!(snapshot(&inputs), inputs_before);
}
