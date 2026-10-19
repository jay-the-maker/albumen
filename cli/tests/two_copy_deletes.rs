// SPDX-License-Identifier: Apache-2.0

//! Sync is not backup: two copies of one synthetic library. A delete at the
//! source, or in one copy, never removes an original from the other copy,
//! and never touches the source library. Fixtures only; no network.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn run(args: &[&OsStr]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn export(from: &Path, to: &Path, manifest: &Path) {
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        from.as_os_str(),
        OsStr::new("--to"),
        to.as_os_str(),
        OsStr::new("--manifest"),
        manifest.as_os_str(),
        OsStr::new("--apply"),
    ]);
    assert!(ok, "export {} -> {}: {text}", from.display(), to.display());
}

fn verify(manifest: &Path, against: &Path) {
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.as_os_str(),
        OsStr::new("--against"),
        against.as_os_str(),
    ]);
    assert!(ok, "verify-manifest {}: {text}", against.display());
}

/// Relative path -> (bytes, mtime) for every file under root.
fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
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

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn assert_same(label: &str, before: &BTreeMap<String, (Vec<u8>, SystemTime)>, root: &Path) {
    let after = snapshot(root);
    for (name, (bytes, modified)) in before {
        let Some((now_bytes, now_modified)) = after.get(name) else {
            panic!("{label}: {name} was deleted");
        };
        assert!(now_bytes == bytes, "{label}: {name} bytes changed");
        assert_eq!(now_modified, modified, "{label}: {name} mtime changed");
    }
}

#[test]
fn a_delete_at_the_source_or_in_one_copy_never_reaches_the_other_copy() {
    let repo_fixtures = snapshot(&fixtures());
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    let copy_a = work.path().join("copy-a");
    let copy_b = work.path().join("copy-b");
    let manifests = work.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();

    // A disposable source built from synthetic fixtures; the repo fixtures stay read-only.
    copy_tree(&fixtures(), &source);
    export(&source, &copy_a, &manifests.join("a1.txt"));
    export(&copy_a, &copy_b, &manifests.join("b1.txt"));
    verify(&manifests.join("b1.txt"), &copy_b);
    let a_before = snapshot(&copy_a);
    let b_before = snapshot(&copy_b);
    assert!(a_before.len() >= 8, "fixture library is too small");
    assert_eq!(
        a_before.keys().collect::<Vec<_>>(),
        b_before.keys().collect::<Vec<_>>()
    );

    // Delete a photo at the source and back up again: copy A keeps it.
    let gone_at_source = "jpeg-gps/IMG_0001.jpg";
    fs::remove_file(source.join(gone_at_source)).unwrap();
    export(&source, &copy_a, &manifests.join("a2.txt"));
    assert_same("copy A after a source delete", &a_before, &copy_a);

    // Delete a photo in copy A and sync A to B again: copy B keeps it.
    let gone_in_a = a_before
        .keys()
        .find(|name| name.as_str() != gone_at_source)
        .unwrap()
        .clone();
    fs::remove_file(copy_a.join(&gone_in_a)).unwrap();
    export(&copy_a, &copy_b, &manifests.join("b2.txt"));
    assert_same("copy B after a delete in copy A", &b_before, &copy_b);
    verify(&manifests.join("b1.txt"), &copy_b);

    // Every other original in copy A is still on disk.
    let mut a_rest = a_before.clone();
    a_rest.remove(&gone_in_a);
    assert_same("copy A's other originals", &a_rest, &copy_a);

    // The repo's synthetic fixtures were never written.
    assert_same("repo fixtures", &repo_fixtures, &fixtures());
}
