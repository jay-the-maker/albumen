// SPDX-License-Identifier: Apache-2.0

//! Sync is not backup, live family libraries: copy A and copy B are both
//! full family libraries. Signed records made on one (a hide, a
//! tombstone, a revoke, a new photo) reach the other by adding the new
//! signed files only. The other copy's merged manifest then shows them,
//! and nothing it already held is removed or changed. Synthetic bytes.
//!
//! Files travel with `albumen family sync`, which only adds: the test
//! snapshots every file on both copies around each sync, and checks that a
//! conflicting file is refused with nothing written.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Snapshot = BTreeMap<String, (Vec<u8>, SystemTime)>;

fn ok(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "albumen {}: {text}",
        args.join(" ")
    );
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

fn set_mtime(path: &Path, mtime: SystemTime) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn stamp(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    set_mtime(path, UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700));
}

fn snapshot(root: &Path) -> Snapshot {
    let mut out = BTreeMap::new();
    if !root.exists() {
        return out;
    }
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

fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// `albumen family sync --from from --to to --apply`; returns the added
/// relative paths. A dry run first must write nothing to either copy.
fn sync(from: &Path, to: &Path) -> Vec<String> {
    let (from_before, to_before) = (snapshot(from), snapshot(to));
    let plan = ok(&["family", "sync", "--from", p(from), "--to", p(to)]);
    assert!(plan.contains("dry-run"), "{plan}");
    assert_eq!(snapshot(from), from_before, "dry run wrote the source");
    assert_eq!(snapshot(to), to_before, "dry run wrote the destination");
    let text = ok(&[
        "family",
        "sync",
        "--from",
        p(from),
        "--to",
        p(to),
        "--apply",
    ]);
    assert!(
        text.contains("removed: 0") && text.contains("replaced: 0"),
        "{text}"
    );
    assert_eq!(snapshot(from), from_before, "sync wrote the source");
    let added: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("  + "))
        .map(str::to_string)
        .collect();
    // Exactly the listed files are new; everything else is untouched.
    let now = snapshot(to);
    for (name, entry) in &to_before {
        assert_eq!(now.get(name), Some(entry), "sync changed or removed {name}");
    }
    let mut new: Vec<_> = now
        .keys()
        .filter(|k| !to_before.contains_key(*k))
        .cloned()
        .collect();
    new.sort();
    let mut listed = added.clone();
    listed.sort();
    assert_eq!(new, listed, "sync added files it did not report");
    for name in &added {
        assert_eq!(
            now[name], from_before[name],
            "{name} not copied bit-exact with mtime"
        );
    }
    added
}

fn kept_everything(label: &str, before: &Snapshot, root: &Path) {
    let now = snapshot(root);
    for (name, (bytes, modified)) in before {
        let (now_bytes, now_modified) = now
            .get(name)
            .unwrap_or_else(|| panic!("{label}: {name} was removed"));
        assert!(now_bytes == bytes, "{label}: {name} bytes changed");
        assert_eq!(now_modified, modified, "{label}: {name} mtime changed");
    }
}

#[test]
fn signed_records_merge_between_two_live_libraries_without_removing_anything() {
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
    let lib_a = PathBuf::from(line(
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
            p(&lib_a),
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
        p(&lib_a),
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
        p(&lib_a),
        "--key",
        p(&key_a),
        "--member",
        &member_b,
        "--token",
        &token,
        "--apply",
    ]);

    let inputs = work.path().join("inputs");
    let mut shas = Vec::new();
    for (index, bytes) in [&b"synthetic-live-1"[..], &b"synthetic-live-2"[..]]
        .into_iter()
        .enumerate()
    {
        let photo = inputs.join(format!("IMG_{index}.jpg"));
        let sidecar = inputs.join(format!("IMG_{index}.xmp"));
        stamp(&photo, bytes);
        stamp(&sidecar, b"<x:xmpmeta synthetic/>");
        shas.push(line(
            &ok(&[
                "family",
                "add",
                "--library",
                p(&lib_a),
                "--key",
                p(&key_a),
                "--from",
                p(&photo),
                "--sidecar",
                p(&sidecar),
                "--apply",
            ]),
            "sha256 ",
        ));
    }

    // Copy B: a second live library, seeded from A.
    let lib_b = work.path().join("copy-b").join("library");
    assert!(sync(&lib_a, &lib_b).len() > 4);
    let b_seeded = snapshot(&lib_b);

    // On A: Bea hides one photo, Ada tombstones the other, Ada revokes Bea.
    ok(&[
        "family",
        "hide",
        "--library",
        p(&lib_a),
        "--key",
        p(&key_b),
        "--sha256",
        &shas[0],
        "--apply",
    ]);
    ok(&[
        "family",
        "tombstone",
        "--library",
        p(&lib_a),
        "--key",
        p(&key_a),
        "--sha256",
        &shas[1],
        "--apply",
    ]);
    ok(&[
        "family",
        "revoke",
        "--library",
        p(&lib_a),
        "--key",
        p(&key_a),
        "--member",
        &member_b,
        "--apply",
    ]);
    let a_settled = snapshot(&lib_a);

    // Merge A into B: only new signed files arrive.
    let added = sync(&lib_a, &lib_b);
    assert!(!added.is_empty());
    assert!(
        added.iter().all(|name| !name.contains("/objects/")),
        "{added:?}"
    );
    kept_everything("copy B after merging A", &b_seeded, &lib_b);
    assert_eq!(snapshot(&lib_a), a_settled, "merging read copy A only");

    // B's merged manifest now carries A's hide, tombstone and revoke.
    let manifest_b = ok(&["family", "manifest", "--library", p(&lib_b)]);
    assert!(manifest_b.contains("\"hidden\": true"), "{manifest_b}");
    assert!(manifest_b.contains("\"tombstones\""), "{manifest_b}");
    assert!(manifest_b.contains(&member_b), "{manifest_b}");
    for sha in &shas {
        assert!(manifest_b.contains(sha.as_str()), "copy B lost {sha}");
    }
    assert_eq!(
        ok(&["family", "manifest", "--library", p(&lib_a)]),
        manifest_b,
        "both copies merge to the same manifest"
    );

    // The other way: Ada adds a photo on B; merging B into A only adds.
    let photo = inputs.join("IMG_2.jpg");
    stamp(&photo, b"synthetic-live-3");
    let sha_new = line(
        &ok(&[
            "family",
            "add",
            "--library",
            p(&lib_b),
            "--key",
            p(&key_a),
            "--from",
            p(&photo),
            "--apply",
        ]),
        "sha256 ",
    );
    let b_settled = snapshot(&lib_b);
    let added = sync(&lib_b, &lib_a);
    assert!(
        added.iter().any(|name| name.contains(&sha_new)),
        "{added:?}"
    );
    kept_everything("copy A after merging B", &a_settled, &lib_a);
    assert_eq!(snapshot(&lib_b), b_settled, "merging read copy B only");
    let manifest_a = ok(&["family", "manifest", "--library", p(&lib_a)]);
    assert!(manifest_a.contains(&sha_new), "{manifest_a}");
    assert!(manifest_a.contains("\"tombstones\""), "{manifest_a}");

    // Syncing again adds nothing.
    assert!(sync(&lib_a, &lib_b).is_empty());
    assert!(sync(&lib_b, &lib_a).is_empty());

    // A copy-local receiver tombstone on A is not carried to B.
    let local = lib_a.join("note.jpg.albumen-tombstone");
    fs::write(&local, "{\"hidden\":true}\n").unwrap();
    assert!(sync(&lib_a, &lib_b).is_empty());
    assert!(!lib_b.join("note.jpg.albumen-tombstone").exists());
    fs::remove_file(&local).unwrap();

    // Conflicting bytes: a shared file that differs refuses the whole sync,
    // with nothing written to either copy.
    let lib_c = work.path().join("copy-c").join("library");
    sync(&lib_a, &lib_c);
    let shared = snapshot(&lib_c)
        .keys()
        .find(|name| name.contains("/objects/"))
        .cloned()
        .expect("an object on copy C");
    fs::write(lib_c.join(&shared), b"synthetic-different-bytes").unwrap();
    photo_extra(&inputs, &lib_a, &key_a);
    let (a_before, c_before) = (snapshot(&lib_a), snapshot(&lib_c));
    let (passed, text) = run(&[
        "family",
        "sync",
        "--from",
        p(&lib_a),
        "--to",
        p(&lib_c),
        "--apply",
    ]);
    assert!(!passed, "a conflicting sync must fail: {text}");
    assert!(text.contains("different bytes"), "{text}");
    assert!(text.contains("nothing was written"), "{text}");
    assert_eq!(snapshot(&lib_a), a_before, "refused sync wrote the source");
    assert_eq!(
        snapshot(&lib_c),
        c_before,
        "refused sync wrote the destination"
    );
}

/// Add one more synthetic photo on A so a refused sync has something to add.
fn photo_extra(inputs: &Path, lib: &Path, key: &Path) {
    let photo = inputs.join("IMG_9.jpg");
    stamp(&photo, b"synthetic-live-9");
    ok(&[
        "family",
        "add",
        "--library",
        p(lib),
        "--key",
        p(key),
        "--from",
        p(&photo),
        "--apply",
    ]);
}
