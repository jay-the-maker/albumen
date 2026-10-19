// SPDX-License-Identifier: Apache-2.0

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
    for (name, bytes) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

fn write_tgz(path: &Path, name: &str, bytes: &[u8]) {
    let file = File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_path(name).unwrap();
    header.set_size(bytes.len() as u64);
    header.set_mtime(1_590_000_000);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, bytes).unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

#[test]
fn import_takeout_reconciles_a_synthetic_split_download() {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("takeout");
    fs::create_dir(&from).unwrap();
    let jpeg = b"\xff\xd8\xff\xdb synthetic-cli-takeout-jpeg";
    let json = br#"{"title":"IMG_0001.jpg","creationTime":{"timestamp":"1600000000","formatted":"synthetic"},"photoTakenTime":{"timestamp":"1590000000","formatted":"synthetic"},"geoData":{"latitude":0.0,"longitude":0.0,"altitude":0.0,"latitudeSpan":0.0,"longitudeSpan":0.0}}"#;
    let mp4 = b"synthetic-cli-takeout-mp4";
    let zip_path = from.join("takeout-001.zip");
    let tgz_path = from.join("takeout-002.tgz");
    write_zip(
        &zip_path,
        &[
            ("Takeout/Google Photos/IMG_0001.jpg", jpeg.as_slice()),
            ("Takeout/Google Photos/IMG_0001.jpg.json", json.as_slice()),
        ],
    );
    write_tgz(&tgz_path, "Takeout/Google Photos/Album/CLIP.mp4", mp4);
    let zip_before = fs::read(&zip_path).unwrap();
    let tgz_before = fs::read(&tgz_path).unwrap();

    let library = root.path().join("library");
    let report = root.path().join("report.jsonl");
    let output = albumen()
        .args(["import-takeout", "--apply"])
        .arg("--from")
        .arg(&from)
        .arg("--to")
        .arg(&library)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert_eq!(
        fs::read(library.join("Takeout/Google Photos/IMG_0001.jpg")).unwrap(),
        jpeg
    );
    assert_eq!(
        fs::read(library.join("Takeout/Google Photos/IMG_0001.jpg.json")).unwrap(),
        json
    );
    assert_eq!(
        fs::read(library.join("Takeout/Google Photos/Album/CLIP.mp4")).unwrap(),
        mp4
    );
    assert!(!jpeg.windows(10).any(|window| window == b"1590000000"));
    assert_eq!(fs::read(&zip_path).unwrap(), zip_before);
    assert_eq!(fs::read(&tgz_path).unwrap(), tgz_before);
    assert!(!from.join("Takeout").exists());

    let report_text = fs::read_to_string(&report).unwrap();
    let summary = report_text.lines().last().unwrap();
    assert!(summary.contains("\"count_in\":3"), "{summary}");
    assert!(summary.contains("\"count_out\":3"), "{summary}");
    assert!(summary.contains("\"mismatched\":0"), "{summary}");
    assert!(summary.contains("\"archives_unchanged\":true"), "{summary}");
    assert!(report_text.contains("\"photo_taken_time\":\"1590000000\""));
    assert!(report_text.contains("\"partner\":\"IMG_0001.jpg\""));
    let original = report_text
        .lines()
        .find(|line| line.contains("IMG_0001.jpg") && line.contains("\"kind\":\"original\""))
        .unwrap();
    assert!(!original.contains("photo_taken_time"), "{original}");
}

#[test]
fn import_takeout_derive_xmp_leaves_the_original() {
    let root = tempfile::tempdir().unwrap();
    let incoming = root.path().join("incoming");
    fs::create_dir(&incoming).unwrap();
    let archive = incoming.join("takeout.zip");
    let jpeg = b"\xff\xd8\xff\xdb synthetic-cli-xmp-jpeg";
    let json = br#"{"title":"IMG_0001.jpg","description":"synthetic fixture","creationTime":{"timestamp":"1600000000","formatted":"synthetic"},"photoTakenTime":{"timestamp":"1590000000","formatted":"synthetic"},"geoData":{"latitude":0.0,"longitude":0.0,"altitude":0.0,"latitudeSpan":0.0,"longitudeSpan":0.0}}"#;
    write_zip(
        &archive,
        &[
            ("Takeout/IMG_0001.jpg", jpeg.as_slice()),
            ("Takeout/IMG_0001.jpg.json", json.as_slice()),
        ],
    );
    let library = root.path().join("library");
    let report = root.path().join("report.jsonl");
    let output = albumen()
        .args(["import-takeout", "--apply", "--derive-xmp", "--from"])
        .arg(&archive)
        .arg("--to")
        .arg(&library)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    let photo = library.join("Takeout/IMG_0001.jpg");
    assert_eq!(fs::read(&photo).unwrap(), jpeg);
    assert_eq!(
        fs::read(library.join("Takeout/IMG_0001.jpg.json")).unwrap(),
        json
    );
    let xmp = fs::read_to_string(library.join("Takeout/IMG_0001.jpg.xmp")).unwrap();
    assert!(xmp.contains("2020-05-20T18:40:00Z"), "{xmp}");
    assert!(!xmp.contains("2020-09-13T12:26:40Z"), "{xmp}");
    let summary = fs::read_to_string(&report).unwrap();
    assert!(summary.contains("\"count_in\":2"), "{summary}");
    assert!(summary.contains("\"count_out\":2"), "{summary}");
    assert!(summary.contains("\"kind\":\"derived-xmp\""));
}

#[test]
fn import_takeout_exits_nonzero_on_a_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let incoming = root.path().join("incoming");
    fs::create_dir(&incoming).unwrap();
    let archive = incoming.join("takeout.zip");
    write_zip(&archive, &[("Takeout/IMG_0001.jpg", b"synthetic-original")]);
    let library = root.path().join("library");
    let dest = library.join("Takeout/IMG_0001.jpg");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"already-there").unwrap();
    let report = root.path().join("report.jsonl");
    let output = albumen()
        .args(["import-takeout", "--apply"])
        .arg("--from")
        .arg(&archive)
        .arg("--to")
        .arg(&library)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&dest).unwrap(), b"already-there");
    assert!(!fs::read(&archive).unwrap().is_empty());
    let summary = fs::read_to_string(&report)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_string();
    assert!(summary.contains("\"count_in\":1"), "{summary}");
    assert!(summary.contains("\"count_out\":0"), "{summary}");
}

#[test]
fn import_takeout_dry_run_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let incoming = root.path().join("incoming");
    fs::create_dir(&incoming).unwrap();
    let archive = incoming.join("takeout.zip");
    write_zip(&archive, &[("Takeout/IMG_0001.jpg", b"synthetic-dry-run")]);
    let before = fs::read(&archive).unwrap();
    let library = root.path().join("library");
    let report = root.path().join("report.jsonl");
    let output = albumen()
        .args(["import-takeout", "--from"])
        .arg(&archive)
        .arg("--to")
        .arg(&library)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(text.contains("unmatched-file:"), "{text}");
    assert!(!library.exists(), "{text}");
    assert!(!report.exists(), "{text}");
    assert_eq!(fs::read(&archive).unwrap(), before);
}
