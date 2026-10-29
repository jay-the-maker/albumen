// SPDX-License-Identifier: Apache-2.0

//! #146: a receiver hide on copy A must not reach copy B. Re-exporting A to
//! B leaves A's `.albumen-tombstone` files on A, B's originals bit-exact,
//! and B still passes its original manifest check. Synthetic bytes only.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

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

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stamp(path: &Path, bytes: &[u8]) {
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

fn files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn receiver_tombstones_on_copy_a_stay_on_copy_a() {
    let work = tempfile::tempdir().unwrap();
    let copy_a = work.path().join("copy-a");
    let copy_b = work.path().join("copy-b");
    let manifests = work.path().join("manifests");
    fs::create_dir_all(&copy_a).unwrap();
    fs::create_dir_all(&manifests).unwrap();
    stamp(
        &copy_a.join("IMG_0001.jpg"),
        b"synthetic-receiver-original-1",
    );
    stamp(
        &copy_a.join("IMG_0002.jpg"),
        b"synthetic-receiver-original-2",
    );

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
    let original = |root: &Path, name: &str| {
        let path = root.join(name);
        (
            fs::read(&path).unwrap(),
            fs::metadata(&path).unwrap().modified().unwrap(),
        )
    };
    let b_before: Vec<_> = ["IMG_0001.jpg", "IMG_0002.jpg"]
        .iter()
        .map(|name| original(&copy_b, name))
        .collect();

    // The receiver's hide record, in the format `Library::hide` writes.
    let tombstone = copy_a.join("IMG_0001.jpg.albumen-tombstone");
    fs::write(
        &tombstone,
        "{\"hidden\":true,\"reason\":\"hide\",\"stored_name\":\"IMG_0001.jpg\"}\n",
    )
    .unwrap();
    let a_before = files(&copy_a);

    let m2 = manifests.join("b2.txt");
    let text = ok(&[
        "export",
        "--from",
        p(&copy_a),
        "--to",
        p(&copy_b),
        "--manifest",
        p(&m2),
        "--apply",
    ]);
    assert!(
        text.lines()
            .any(|line| line == "exported 0 new, 2 already present and verified"),
        "{text}"
    );

    // B: no tombstone, originals bit-exact, original manifest still passes.
    assert_eq!(files(&copy_b), ["IMG_0001.jpg", "IMG_0002.jpg"]);
    for (index, name) in ["IMG_0001.jpg", "IMG_0002.jpg"].iter().enumerate() {
        assert_eq!(
            original(&copy_b, name),
            b_before[index],
            "{name} changed in copy B"
        );
    }
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&m1),
        "--against",
        p(&copy_b),
    ]);
    ok(&[
        "verify-manifest",
        "--manifest",
        p(&m2),
        "--against",
        p(&copy_b),
    ]);
    assert_eq!(
        fs::read_to_string(&m1).unwrap(),
        fs::read_to_string(&m2).unwrap()
    );

    // A: the tombstone and both originals stay where they were.
    assert_eq!(files(&copy_a), a_before);
    assert!(tombstone.is_file());
}
