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

    // 100 ns aligned so NTFS stores this exact tick. The equality below stays exact.
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

/// Every file under `root`, by path relative to it, with bytes, mtime and hash.
fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Kept> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), kept(&path));
            }
        }
    }
    out
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &dest);
        } else {
            fs::copy(&path, &dest).unwrap();
        }
    }
}

fn family_ok(args: &[&str]) -> String {
    let mut all = vec!["family"];
    all.extend_from_slice(args);
    let (ok, text) = run(&all);
    assert!(ok, "albumen {all:?}\n{text}");
    text
}

fn manifest(library: &Path) -> serde_json::Value {
    let text = family_ok(&["manifest", "--library", library.to_str().unwrap()]);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}\n{text}"))
}

fn entries<'a>(value: &'a serde_json::Value, key: &str) -> &'a Vec<serde_json::Value> {
    value[key].as_array().unwrap_or_else(|| panic!("no {key}"))
}

fn hidden_for(value: &serde_json::Value, viewer: &str, sha: &str) -> bool {
    entries(value, "hides").iter().any(|h| {
        h["viewer"] == viewer && h["sha256"] == sha && h["hidden"] == serde_json::Value::Bool(true)
    })
}

/// M4 AC1-AC5: twelve synthetic members on one computer, no network.
#[test]
fn family_twelve_members() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (1..=12).map(|n| format!("member-{n:02}")).collect();
    let mut keys = Vec::new();
    let mut ids = Vec::new();
    for name in &names {
        let key = tmp.path().join(format!("{name}.key"));
        let text = family_ok(&["keygen", "--key", key.to_str().unwrap(), "--apply"]);
        ids.push(line(&text, "member ").to_string());
        keys.push(key.to_str().unwrap().to_string());
    }
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let text = family_ok(&[
        "init",
        "--to",
        parent.to_str().unwrap(),
        "--key",
        &keys[0],
        "--name",
        &names[0],
        "--apply",
    ]);
    let library = PathBuf::from(line(&text, "library "));
    let lib = library.to_str().unwrap().to_string();

    // AC1: eleven invite, join, admit rounds. Nothing caps the count.
    for n in 1..12 {
        let text = family_ok(&[
            "invite",
            "--library",
            &lib,
            "--key",
            &keys[0],
            "--member",
            &ids[n],
            "--apply",
        ]);
        let token = line(&text, "token ").to_string();
        family_ok(&[
            "join",
            "--library",
            &lib,
            "--key",
            &keys[n],
            "--token",
            &token,
            "--name",
            &names[n],
            "--apply",
        ]);
        family_ok(&[
            "admit",
            "--library",
            &lib,
            "--key",
            &keys[0],
            "--member",
            &ids[n],
            "--token",
            &token,
            "--apply",
        ]);
    }
    let value = manifest(&library);
    let members = entries(&value, "members");
    assert_eq!(members.len(), 12, "{value:#}");
    for (n, id) in ids.iter().enumerate() {
        let m = members.iter().find(|m| m["id"] == id.as_str()).unwrap();
        assert_eq!(m["admitted"], true, "{m}");
        assert_eq!(m["revoked"], false, "{m}");
        assert_eq!(m["name"], names[n].as_str(), "{m}");
    }

    // AC2: two synthetic photos with sidecars per member, attributed to the author.
    let sources = tmp.path().join("sources");
    let mut added = Vec::new(); // (member index, sha, source photo, source sidecar)
    let mut sources_before = Vec::new();
    for n in 0..12 {
        for p in 0..2 {
            // 100 ns aligned so NTFS stores this exact tick.
            let mtime = UNIX_EPOCH
                + Duration::new(1_500_000_000 + (n * 10 + p) as u64, 700 * (n as u32 + 1));
            let photo = sources.join(format!("{}-{p}.jpg", names[n]));
            let side = sources.join(format!("{}-{p}.xmp", names[n]));
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
            bytes.extend_from_slice(format!("synthetic {} photo {p}", names[n]).as_bytes());
            bytes.extend_from_slice(&[0xff, 0xd9]);
            stamp(&photo, &bytes, mtime);
            stamp(
                &side,
                format!("<x:xmpmeta synthetic='{}-{p}'/>", names[n]).as_bytes(),
                mtime,
            );
            sources_before.push((photo.clone(), kept(&photo)));
            sources_before.push((side.clone(), kept(&side)));
            let text = family_ok(&[
                "add",
                "--library",
                &lib,
                "--key",
                &keys[n],
                "--from",
                photo.to_str().unwrap(),
                "--sidecar",
                side.to_str().unwrap(),
                "--apply",
            ]);
            added.push((n, line(&text, "sha256 ").to_string(), photo, side));
        }
    }
    let value = manifest(&library);
    let objects = entries(&value, "objects");
    assert_eq!(objects.len(), 24, "{value:#}");
    for (n, sha, photo, side) in &added {
        let o = objects
            .iter()
            .find(|o| o["sha256"] == sha.as_str())
            .unwrap();
        let src = kept(photo);
        assert_eq!(o["member"], ids[*n].as_str(), "{o}");
        assert_eq!(&src.sha256, sha);
        assert_eq!(o["bytes"], src.bytes.len() as u64, "{o}");
        let ns = src
            .mtime
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        assert_eq!(o["mtime_ns"], ns.as_str(), "{o}");
        assert_eq!(
            o["sidecar_sha256"],
            sha256_hex(side).unwrap().as_str(),
            "{o}"
        );
        let stored = library
            .join("members")
            .join(&ids[*n])
            .join("objects/sha256")
            .join(&sha[..2])
            .join(sha);
        assert_eq!(kept(&stored), src);
    }

    // AC3: revoke member-12. Nothing of theirs is moved, changed or deleted.
    let m12 = library.join("members").join(&ids[11]);
    let genesis_before = tree(&library)
        .into_iter()
        .filter(|(p, _)| !p.starts_with("members"))
        .collect::<Vec<_>>();
    let m12_before = tree(&m12);
    // A second copy where member-12 is not yet revoked: their late record is
    // signed there, then arrives in the real library after the revoke.
    let fork_parent = tmp.path().join("fork");
    let fork = fork_parent.join(library.file_name().unwrap());
    copy_tree(&library, &fork);
    let sha01 = added[0].1.clone();
    family_ok(&[
        "hide",
        "--library",
        fork.to_str().unwrap(),
        "--key",
        &keys[11],
        "--sha256",
        &sha01,
        "--apply",
    ]);
    let late: Vec<PathBuf> = tree(&fork.join("members").join(&ids[11]))
        .into_keys()
        .filter(|p| !m12_before.contains_key(p))
        .collect();
    assert_eq!(late.len(), 1, "{late:?}");

    let text = family_ok(&[
        "revoke",
        "--library",
        &lib,
        "--key",
        &keys[0],
        "--member",
        &ids[11],
        "--apply",
    ]);
    assert!(text.contains("originals kept"), "{text}");
    assert_eq!(tree(&m12), m12_before, "member-12's tree changed on revoke");
    let genesis_after = tree(&library)
        .into_iter()
        .filter(|(p, _)| !p.starts_with("members"))
        .collect::<Vec<_>>();
    assert_eq!(genesis_after, genesis_before);
    let value = manifest(&library);
    let members = entries(&value, "members");
    assert_eq!(members.len(), 12);
    let revoked: Vec<_> = members.iter().filter(|m| m["revoked"] == true).collect();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0]["id"], ids[11].as_str());
    assert_eq!(
        entries(&value, "objects").len(),
        24,
        "revoked photos stay listed"
    );

    // The revoked member's CLI write is refused; their late signed record is ignored.
    let (ok, text) = run(&[
        "family",
        "hide",
        "--library",
        &lib,
        "--key",
        &keys[11],
        "--sha256",
        &sha01,
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert_eq!(tree(&m12), m12_before);
    for rel in &late {
        let dest = m12.join(rel);
        fs::copy(fork.join("members").join(&ids[11]).join(rel), &dest).unwrap();
    }
    let value = manifest(&library);
    assert!(
        !hidden_for(&value, &ids[11], &sha01),
        "late record applied: {value:#}"
    );
    let ignored = entries(&value, "ignored_after_revoke");
    assert!(
        ignored
            .iter()
            .any(|i| i["member"] == ids[11].as_str() && i["kind"] == "hide"),
        "{value:#}"
    );

    // AC4: member-02 hides one of member-01's photos, only for member-02.
    let m01 = library.join("members").join(&ids[0]);
    let m01_before = tree(&m01);
    let m02 = library.join("members").join(&ids[1]);
    let m02_before = tree(&m02);
    family_ok(&[
        "hide",
        "--library",
        &lib,
        "--key",
        &keys[1],
        "--sha256",
        &sha01,
        "--apply",
    ]);
    let value = manifest(&library);
    assert!(hidden_for(&value, &ids[1], &sha01), "{value:#}");
    assert!(!hidden_for(&value, &ids[0], &sha01), "{value:#}");
    let s02 = family_ok(&[
        "manifest",
        "--library",
        &lib,
        "--summary",
        "--viewer",
        &ids[1],
    ]);
    assert!(s02.contains(": 23 of 24 photos shown, 1 hidden"), "{s02}");
    let s01 = family_ok(&[
        "manifest",
        "--library",
        &lib,
        "--summary",
        "--viewer",
        &ids[0],
    ]);
    assert!(s01.contains(": 24 of 24 photos shown, 0 hidden"), "{s01}");
    let after_hide = tree(&m02);
    family_ok(&[
        "hide",
        "--library",
        &lib,
        "--key",
        &keys[1],
        "--sha256",
        &sha01,
        "--show",
        "--apply",
    ]);
    let value = manifest(&library);
    assert!(!hidden_for(&value, &ids[1], &sha01), "{value:#}");
    let after_show = tree(&m02);
    // Both the hide and the unhide records remain, unchanged.
    assert_eq!(after_hide.len(), m02_before.len() + 1);
    assert_eq!(after_show.len(), m02_before.len() + 2);
    for (p, k) in &after_hide {
        assert_eq!(after_show.get(p), Some(k), "{p:?} changed");
    }
    assert_eq!(tree(&m01), m01_before, "member-01's tree changed");

    // Summary words: twelve members, the revoked one keeps its photos.
    let summary = family_ok(&["manifest", "--library", &lib, "--summary"]);
    assert!(
        summary.contains("12 members: 11 active, 1 revoked\n"),
        "{summary}"
    );
    assert!(summary.contains("member-12 ("), "{summary}");
    assert!(
        summary.contains(": 2 photos, revoked; their photos stay\n"),
        "{summary}"
    );
    assert!(
        !summary.contains("removed") && !summary.contains("deleted"),
        "{summary}"
    );

    // AC5: export copies all twelve namespaces, the revoked one included.
    let library_before = tree(&library);
    let dest = tmp.path().join("export");
    let text = family_ok(&[
        "export",
        "--library",
        &lib,
        "--to",
        dest.to_str().unwrap(),
        "--apply",
    ]);
    let manifest_file = PathBuf::from(line(&text, "manifest "));
    for (n, sha, photo, _) in &added {
        let copy = dest
            .join("members")
            .join(&ids[*n])
            .join(sha)
            .join(photo.file_name().unwrap());
        assert_eq!(kept(&copy).bytes, kept(photo).bytes, "{copy:?}");
    }
    let (ok, text) = run(&[
        "verify-manifest",
        "--manifest",
        manifest_file.to_str().unwrap(),
        "--against",
        dest.to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("\"mismatched\":0"), "{text}");
    let inside = library.join("members").join("export-here");
    let (ok, text) = run(&[
        "family",
        "export",
        "--library",
        &lib,
        "--to",
        inside.to_str().unwrap(),
        "--apply",
    ]);
    assert!(!ok, "{text}");
    assert!(!inside.exists());
    assert_eq!(tree(&library), library_before, "export changed the library");
    for (path, before) in &sources_before {
        assert_eq!(&kept(path), before, "source {path:?} changed");
    }
}
