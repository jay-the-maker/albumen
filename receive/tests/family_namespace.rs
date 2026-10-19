// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` refuses a family library and leaves originals in place.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::family;
use albumen_ingest::sha256_hex;

fn stamp(path: &Path, bytes: &[u8], mtime: SystemTime) {
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

#[test]
fn receive_refuses_a_family_library_and_keeps_originals() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let founder = family::create_key(&tmp.path().join("founder.key")).unwrap();
    let library = family::init_library(&parent, &founder, "ada").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = tmp.path().join("note.txt");
    let sidecar = tmp.path().join("note.xmp");
    stamp(&photo, b"synthetic-family-original", mtime);
    stamp(&sidecar, b"<x:xmpmeta synthetic/>", mtime);
    let added = family::add_object(&library, &founder, &photo, Some(&sidecar)).unwrap();
    let before_object = fs::read(&added.object_path).unwrap();
    let before_side = fs::read(added.sidecar_path.as_ref().unwrap()).unwrap();
    let before_object_mtime = fs::metadata(&added.object_path)
        .unwrap()
        .modified()
        .unwrap();
    let before_side_mtime = fs::metadata(added.sidecar_path.as_ref().unwrap())
        .unwrap()
        .modified()
        .unwrap();
    let before_object_sha = sha256_hex(&added.object_path).unwrap();
    let before_side_sha = sha256_hex(added.sidecar_path.as_ref().unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args([
            "--to",
            library.to_str().unwrap(),
            "--user",
            "cam",
            "--password",
            "secret",
            "--webdav",
            "127.0.0.1:9",
            "--apply",
        ])
        .output()
        .expect("run albumen-receive");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("family library"), "{text}");

    let open_err = albumen_receive::Library::open(&library).unwrap_err();
    assert!(open_err.to_string().contains("family library"));

    assert_eq!(fs::read(&added.object_path).unwrap(), before_object);
    assert_eq!(
        fs::read(added.sidecar_path.as_ref().unwrap()).unwrap(),
        before_side
    );
    assert_eq!(
        fs::metadata(&added.object_path)
            .unwrap()
            .modified()
            .unwrap(),
        before_object_mtime
    );
    assert_eq!(
        fs::metadata(added.sidecar_path.as_ref().unwrap())
            .unwrap()
            .modified()
            .unwrap(),
        before_side_mtime
    );
    assert_eq!(sha256_hex(&added.object_path).unwrap(), before_object_sha);
    assert_eq!(
        sha256_hex(added.sidecar_path.as_ref().unwrap()).unwrap(),
        before_side_sha
    );
}
