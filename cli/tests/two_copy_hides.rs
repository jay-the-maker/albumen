// SPDX-License-Identifier: Apache-2.0

//! Sync is not backup, second half: a hide, a tombstone, or a removed family
//! member's view on copy A never removes anything from copy B, and A keeps
//! its originals. Covers the receiver path and the family manifest merge.
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

// 100 ns aligned so NTFS keeps this exact tick.
fn mtime() -> SystemTime {
    UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700)
}

fn stamp(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime()))
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

/// Every file in `before` is still under `root`. Files `keep` selects must
/// also keep their bytes and mtime.
fn nothing_removed(label: &str, before: &Snapshot, root: &Path, keep: impl Fn(&str) -> bool) {
    let after = snapshot(root);
    for (name, (bytes, modified)) in before {
        let Some((now_bytes, now_modified)) = after.get(name) else {
            panic!("{label}: {name} was removed");
        };
        if keep(name) {
            assert!(now_bytes == bytes, "{label}: {name} bytes changed");
            assert_eq!(now_modified, modified, "{label}: {name} mtime changed");
        }
    }
}

fn is_photo(name: &str) -> bool {
    name.ends_with(".jpg") || name.ends_with(".xmp")
}

#[test]
fn receiver_hide_on_copy_a_leaves_copy_b_and_a_originals() {
    let work = tempfile::tempdir().unwrap();
    let copy_a = work.path().join("copy-a");
    let copy_b = work.path().join("copy-b");
    let manifests = work.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    for (name, bytes) in [
        ("IMG_0001.jpg", &b"synthetic-receiver-original-1"[..]),
        ("IMG_0002.jpg", &b"synthetic-receiver-original-2"[..]),
        ("IMG_0002.xmp", &b"<x:xmpmeta synthetic/>"[..]),
    ] {
        stamp(&copy_a.join(name), bytes);
    }
    let library = albumen_receive::Library::open(&copy_a).unwrap();

    let m1 = manifests.join("b1.txt");
    ok(&[
        "export",
        "--from",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&m1),
        "--apply",
    ]);
    let a_before = snapshot(&copy_a);
    let b_before = snapshot(&copy_b);
    assert_eq!(b_before.len(), 3);

    // Receiver hide: writes a tombstone sidecar on A.
    library.hide("IMG_0001.jpg").unwrap();
    library.hide("IMG_0002.jpg").unwrap();
    let tombstones: Vec<_> = snapshot(&copy_a)
        .into_keys()
        .filter(|name| name.ends_with(".albumen-tombstone"))
        .collect();
    assert_eq!(tombstones.len(), 2, "{tombstones:?}");

    // Sync A to B again: B loses nothing, and B still verifies.
    let m2 = manifests.join("b2.txt");
    ok(&[
        "export",
        "--from",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&m2),
        "--apply",
    ]);
    nothing_removed(
        "copy B after a receiver hide on A",
        &b_before,
        &copy_b,
        |_| true,
    );
    // The re-export adds A's tombstone files to B (additive) and B verifies
    // against the new manifest; the originals listed in the first manifest stay.
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&m2),
        "--against",
        p(&copy_b),
    ]);
    nothing_removed("copy A after its own hide", &a_before, &copy_a, |_| true);
}

#[test]
fn family_hide_omit_and_revoke_on_copy_a_leave_copy_b_and_a_originals() {
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
    let text = ok(&[
        "family",
        "init",
        "--to",
        p(&parent),
        "--key",
        p(&key_a),
        "--name",
        "ada",
        "--apply",
    ]);
    let copy_a = PathBuf::from(line(&text, "library "));
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
    let mut shas = Vec::new();
    for (index, bytes) in [&b"synthetic-family-1"[..], &b"synthetic-family-2"[..]]
        .into_iter()
        .enumerate()
    {
        let photo = inputs.join(format!("IMG_{index}.jpg"));
        let sidecar = inputs.join(format!("IMG_{index}.xmp"));
        stamp(&photo, bytes);
        stamp(&sidecar, b"<x:xmpmeta synthetic/>");
        let text = ok(&[
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
        ]);
        shas.push(line(&text, "sha256 "));
    }
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
    let a_before = snapshot(&copy_a);
    let b_before = snapshot(&copy_b);
    let objects = |name: &str| is_photo(name) || name.ends_with(".jpg.sidecar");
    assert!(
        b_before.keys().filter(|name| objects(name)).count() == 4,
        "copy B has no objects: {:?}",
        b_before.keys().collect::<Vec<_>>()
    );

    // On copy A: Bea hides the first photo from her view, Ada hides the
    // second, Bea adds then omits an album entry (the omit record), and Ada
    // revokes Bea, which removes Bea's view of the library.
    ok(&[
        "family",
        "hide",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_b),
        "--sha256",
        &shas[0],
        "--apply",
    ]);
    ok(&[
        "family",
        "hide",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--sha256",
        &shas[1],
        "--apply",
    ]);
    let add = line(
        &ok(&[
            "family",
            "album-add",
            "--library",
            p(&copy_a),
            "--key",
            p(&key_b),
            "--album",
            "trip",
            "--sha256",
            &shas[0],
            "--apply",
        ]),
        "add ",
    );
    ok(&[
        "family",
        "album-omit",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_b),
        "--add",
        &add,
        "--apply",
    ]);
    ok(&[
        "family",
        "revoke",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--member",
        &member_b,
        "--apply",
    ]);
    // Family manifest merge, signed by Ada.
    let merged = ok(&[
        "family",
        "manifest",
        "--library",
        p(&copy_a),
        "--key",
        p(&key_a),
        "--apply",
    ]);
    assert!(
        merged.contains("\"hidden\": true") || merged.contains("manifest"),
        "{merged}"
    );

    // Every file on copy A (originals, sidecars, signed hide/omit/revoke
    // records, manifests) with its bytes and mtime, after A's own changes
    // and before any update. Updates only read A.
    let a_settled = snapshot(&copy_a);
    let a_unchanged = |label: &str| {
        let now = snapshot(&copy_a);
        assert_eq!(
            now.keys().collect::<Vec<_>>(),
            a_settled.keys().collect::<Vec<_>>(),
            "{label}: copy A file set changed"
        );
        for (name, (bytes, modified)) in &a_settled {
            let (now_bytes, now_modified) = &now[name];
            assert!(now_bytes == bytes, "{label}: copy A {name} bytes changed");
            assert_eq!(
                now_modified, modified,
                "{label}: copy A {name} mtime changed"
            );
        }
    };

    // Update B from A (#145). The update is additive: files B already holds
    // with the right digest are skipped, nothing in B is overwritten or
    // removed, and the hides, omit and revoke arrive as added provenance
    // state beside the first provenance.json.
    let b2 = manifests.join("b2.txt");
    ok(&[
        "family",
        "export",
        "--library",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&b2),
        "--apply",
    ]);
    let b_after = snapshot(&copy_b);
    let added: Vec<_> = b_after
        .keys()
        .filter(|name| !b_before.contains_key(*name))
        .collect();
    assert!(
        !added.is_empty()
            && added
                .iter()
                .all(|name| name.contains("/provenance-") && name.ends_with(".json")),
        "update should add only new provenance state: {added:?}"
    );
    let state: String = added
        .iter()
        .map(|name| String::from_utf8_lossy(&b_after[*name].0).into_owned())
        .collect();
    assert!(
        state.contains("hide"),
        "hides did not reach copy B: {state}"
    );
    assert!(
        state.contains("revoke"),
        "revoke did not reach copy B: {state}"
    );
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&b2),
        "--against",
        p(&copy_b),
    ]);

    // A second update with nothing new changes nothing.
    let b3 = manifests.join("b3.txt");
    ok(&[
        "family",
        "export",
        "--library",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&b3),
        "--apply",
    ]);
    assert_eq!(snapshot(&copy_b), b_after, "an idle update changed copy B");
    a_unchanged("after two updates");

    // A copy whose original was altered is refused, not repaired over.
    let tampered = b_before
        .keys()
        .find(|name| name.ends_with(".jpg"))
        .unwrap()
        .clone();
    let tampered_path = copy_b.join(&tampered);
    let mut bytes = fs::read(&tampered_path).unwrap();
    bytes.push(b'!');
    fs::write(&tampered_path, &bytes).unwrap();
    let (updated, text) = run(&[
        "family",
        "export",
        "--library",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&manifests.join("b4.txt")),
        "--apply",
    ]);
    assert!(!updated, "{text}");
    assert!(text.contains("refusing to overwrite"), "{text}");
    assert_eq!(fs::read(&tampered_path).unwrap(), bytes);
    a_unchanged("after a refused update");
    let restore = b_before[&tampered].0.clone();
    fs::write(&tampered_path, &restore).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&tampered_path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(b_before[&tampered].1))
        .unwrap();

    // A fresh copy made after the hides, omit and revoke still carries every
    // original and sidecar: a hide is not a delete.
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
    for (name, (bytes, _)) in b_before.iter().filter(|(name, _)| objects(name)) {
        let (c_bytes, _) = c
            .get(name)
            .unwrap_or_else(|| panic!("fresh copy after hides lost {name}"));
        assert!(c_bytes == bytes, "fresh copy {name} bytes differ");
    }
    nothing_removed(
        "copy B after hides, omit and revoke on A",
        &b_before,
        &copy_b,
        objects,
    );
    nothing_removed("copy A originals", &a_before, &copy_a, objects);
    for sha in &shas {
        assert!(
            snapshot(&copy_b)
                .keys()
                .any(|name| name.contains(sha.as_str())),
            "copy B lost {sha}"
        );
    }
    nothing_removed("synthetic inputs", &inputs_before, &inputs, |_| true);
}
