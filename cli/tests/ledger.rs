// SPDX-License-Identifier: Apache-2.0

//! `albumen ledger` accounts for every file in a synthetic library.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use albumen_ingest::{ledger::Ledger, sha256_hex};

#[test]
fn ledger_accounts_for_every_stored_object() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(library.join("members/ab")).unwrap();
    fs::create_dir_all(library.join("roll")).unwrap();
    let note = library.join("note.txt");
    let photo = library.join("members/ab/photo.jpg");
    let frame = library.join("roll/frame.dng");
    write_stamped(&note, b"note");
    write_stamped(&photo, b"jpeg");
    write_stamped(&frame, b"raw-bytes");
    let before = fs::read(&note).unwrap();
    let before_mtime = fs::metadata(&note).unwrap().modified().unwrap();

    let log = tmp.path().join("connections.jsonl");
    fs::write(
        &log,
        "{\"via\":\"export\",\"kind\":\"connect\",\"host\":\"127.0.0.1\",\"port\":19000,\"at\":\"2026-10-19T09:00:01Z\"}\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&[
        "ledger",
        "--library",
        library.to_str().unwrap(),
        "--connection-log",
        log.to_str().unwrap(),
        "--json",
    ]);
    assert!(ok, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let parsed = Ledger::from_json(stdout.trim()).unwrap();
    let expected = vec![
        ("note.txt", &note, None),
        ("members/ab/photo.jpg", &photo, Some("ab")),
        ("roll/frame.dng", &frame, None),
    ];
    assert_eq!(parsed.object_count, expected.len() as u64);
    assert_eq!(parsed.objects.len(), expected.len());
    for (path, full, member) in &expected {
        let object = parsed
            .objects
            .iter()
            .find(|object| object.path == *path)
            .unwrap_or_else(|| panic!("missing {path} in {stdout}"));
        assert_eq!(object.bytes, fs::metadata(full).unwrap().len(), "{path}");
        assert_eq!(object.sha256, sha256_hex(full).unwrap(), "{path}");
        assert_eq!(object.member.as_deref(), *member, "{path}");
    }
    assert!(parsed.destinations.iter().any(|destination| {
        destination.via == "export"
            && destination.kind == "connect"
            && destination.host == "127.0.0.1"
            && destination.port == 19000
    }));

    let (ok, text, stderr) = run(&["ledger", "--library", library.to_str().unwrap()]);
    assert!(ok, "{stderr}");
    assert!(text.starts_with("ledger\n"), "{text}");
    assert!(text.contains("objects: 3"), "{text}");
    assert!(text.contains("member=ab"), "{text}");
    assert!(text.contains("roll/frame.dng"), "{text}");
    assert_eq!(fs::read(&note).unwrap(), before);
    assert_eq!(
        fs::metadata(&note).unwrap().modified().unwrap(),
        before_mtime
    );
    assert!(!library.join("connections.jsonl").exists());
    assert!(!library.join("analyses.jsonl").exists());
}

fn write_stamped(path: &Path, bytes: &[u8]) {
    let mut file = File::create(path).unwrap();
    file.write_all(bytes).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .expect("spawn albumen");
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}
