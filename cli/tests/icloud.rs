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
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
    for (name, bytes) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn import_icloud_reconciles_a_synthetic_library() {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("Photos Library.photoslibrary");
    let heic = b"synthetic-cli-icloud-heic";
    let mov = b"synthetic-cli-icloud-mov";
    let edited = b"synthetic-cli-icloud-edited";
    let xmp =
        br#"<x:xmpmeta><rdf:Description exif:DateTimeOriginal="2020-05-20T16:00:00"/></x:xmpmeta>"#;
    fs::create_dir_all(from.join("originals/0")).unwrap();
    fs::create_dir_all(from.join("resources/renders/0")).unwrap();
    fs::write(from.join("originals/0/IMG_0001.HEIC"), heic).unwrap();
    fs::write(from.join("originals/0/IMG_0001.MOV"), mov).unwrap();
    fs::write(from.join("originals/0/IMG_0001.HEIC.xmp"), xmp).unwrap();
    fs::write(from.join("resources/renders/0/IMG_0001.jpeg"), edited).unwrap();
    let before = fs::read(from.join("originals/0/IMG_0001.HEIC")).unwrap();

    let library = root.path().join("library");
    let report = root.path().join("report.jsonl");
    let output = albumen()
        .arg("import-icloud")
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
        fs::read(library.join("originals/0/IMG_0001.HEIC")).unwrap(),
        heic
    );
    assert_eq!(
        fs::read(library.join("originals/0/IMG_0001.MOV")).unwrap(),
        mov
    );
    assert_eq!(
        fs::read(library.join("originals/0/IMG_0001.HEIC.xmp")).unwrap(),
        xmp
    );
    assert_eq!(
        fs::read(library.join("resources/renders/0/IMG_0001.jpeg")).unwrap(),
        edited
    );
    assert!(!heic.windows(4).any(|window| window == b"2020"));
    assert_eq!(
        fs::read(from.join("originals/0/IMG_0001.HEIC")).unwrap(),
        before
    );

    let report_text = fs::read_to_string(&report).unwrap();
    let summary = report_text.lines().last().unwrap();
    assert!(summary.contains("\"count_in\":4"), "{summary}");
    assert!(summary.contains("\"count_out\":4"), "{summary}");
    assert!(summary.contains("\"mismatched\":0"), "{summary}");
    assert!(summary.contains("\"sources_unchanged\":true"), "{summary}");
    assert!(report_text.contains("\"capture_time\":\"2020-05-20T16:00:00\""));
    assert!(report_text.contains("\"partner_stem\":\"IMG_0001\""));
    assert!(report_text.contains("\"live_photo_stem\":\"IMG_0001\""));
    let original = report_text
        .lines()
        .find(|line| line.contains("IMG_0001.HEIC") && line.contains("\"kind\":\"original\""))
        .unwrap();
    assert!(!original.contains("capture_time"), "{original}");
    let edited_line = report_text
        .lines()
        .find(|line| line.contains("renders/0/IMG_0001.jpeg"))
        .unwrap();
    assert!(edited_line.contains("\"kind\":\"edited\""), "{edited_line}");
}

#[test]
fn import_icloud_exits_nonzero_on_a_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let incoming = root.path().join("incoming");
    fs::create_dir(&incoming).unwrap();
    let archive = incoming.join("iCloud Photos.zip");
    write_zip(
        &archive,
        &[("iCloud Photos/IMG_0001.HEIC", b"synthetic-original")],
    );
    let library = root.path().join("library");
    let dest = library.join("iCloud Photos/IMG_0001.HEIC");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"already-there").unwrap();
    let report = root.path().join("report.jsonl");
    let before = fs::read(&archive).unwrap();
    let output = albumen()
        .arg("import-icloud")
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
    assert_eq!(fs::read(&archive).unwrap(), before);
    assert!(!incoming.join("iCloud Photos").exists());
    let summary = fs::read_to_string(&report)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_string();
    assert!(summary.contains("\"count_in\":1"), "{summary}");
    assert!(summary.contains("\"count_out\":0"), "{summary}");
    assert!(summary.contains("\"sources_unchanged\":true"), "{summary}");
}
