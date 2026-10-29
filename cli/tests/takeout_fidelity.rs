// SPDX-License-Identifier: Apache-2.0

//! Synthetic Takeout fidelity cases: agree, disagree, JSON-only, EXIF-only,
//! neither. Every JPEG is generated here from the public fixture with its
//! metadata stripped and rewritten; no real photo is used.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn exiftool() -> Command {
    if cfg!(windows) {
        Command::new("exiftool.cmd")
    } else {
        Command::new("exiftool")
    }
}

fn synthetic_jpeg(dir: &Path, name: &str, tags: &[&str]) -> Vec<u8> {
    let path = dir.join(name);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/jpeg-gps/IMG_0001.jpg");
    fs::copy(fixture, &path).unwrap();
    let status = exiftool()
        .args(["-q", "-overwrite_original", "-all="])
        .args(tags)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(&path).unwrap()
}

fn sidecar(title: &str, taken: Option<&str>, gps: (f64, f64)) -> Vec<u8> {
    let taken = taken
        .map(|t| format!(r#""photoTakenTime":{{"timestamp":"{t}","formatted":"synthetic"}},"#))
        .unwrap_or_default();
    format!(
        r#"{{"title":"{title}",{taken}"geoData":{{"latitude":{},"longitude":{},"altitude":0.0,"latitudeSpan":0.0,"longitudeSpan":0.0}}}}"#,
        gps.0, gps.1
    )
    .into_bytes()
}

const EXIF_TIME: [&str; 2] = [
    "-EXIF:DateTimeOriginal=2020:05:20 11:40:00",
    "-EXIF:OffsetTimeOriginal=-07:00",
];
const EXIF_GPS: [&str; 4] = [
    "-GPSLatitude=37.5",
    "-GPSLatitudeRef=N",
    "-GPSLongitude=122.25",
    "-GPSLongitudeRef=W",
];

fn run_import(root: &Path) -> (PathBuf, PathBuf) {
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    let both: Vec<&str> = EXIF_TIME.iter().chain(EXIF_GPS.iter()).copied().collect();
    let late = [
        "-EXIF:DateTimeOriginal=2020:05:20 13:40:00",
        "-EXIF:OffsetTimeOriginal=-07:00",
        "-GPSLatitude=37.51",
        "-GPSLatitudeRef=N",
        "-GPSLongitude=122.25",
        "-GPSLongitudeRef=W",
    ];
    let files: Vec<(String, Vec<u8>)> = vec![
        (
            "AGREE.jpg".into(),
            synthetic_jpeg(&work, "AGREE.jpg", &both),
        ),
        (
            "AGREE.jpg.json".into(),
            sidecar("AGREE.jpg", Some("1590000030"), (37.5, -122.2502)),
        ),
        (
            "DISAGREE.jpg".into(),
            synthetic_jpeg(&work, "DISAGREE.jpg", &late),
        ),
        (
            "DISAGREE.jpg.json".into(),
            sidecar("DISAGREE.jpg", Some("1590000000"), (37.5, -122.25)),
        ),
        (
            "JSONONLY.jpg".into(),
            synthetic_jpeg(&work, "JSONONLY.jpg", &[]),
        ),
        (
            "JSONONLY.jpg.json".into(),
            sidecar("JSONONLY.jpg", Some("1590000000"), (37.5, -122.25)),
        ),
        (
            "EXIFONLY.jpg".into(),
            synthetic_jpeg(&work, "EXIFONLY.jpg", &both),
        ),
        (
            "EXIFONLY.jpg.json".into(),
            sidecar("EXIFONLY.jpg", None, (0.0, 0.0)),
        ),
        (
            "NEITHER.jpg".into(),
            synthetic_jpeg(&work, "NEITHER.jpg", &[]),
        ),
        (
            "NEITHER.jpg.json".into(),
            sidecar("NEITHER.jpg", None, (0.0, 0.0)),
        ),
    ];
    let from = root.join("takeout");
    fs::create_dir(&from).unwrap();
    let file = File::create(from.join("takeout-001.zip")).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
    for (name, bytes) in &files {
        zip.start_file(format!("Takeout/Google Photos/{name}"), options)
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let library = root.join("library");
    let report = root.join("report.jsonl");
    let output = albumen()
        .args(["import-takeout", "--apply", "--from"])
        .arg(&from)
        .arg("--to")
        .arg(&library)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let last = stdout.lines().last().unwrap();
    assert_eq!(
        last,
        format!(
            "next: albumen takeout-fidelity --report {} --library {} --out <file>",
            report.display(),
            library.display()
        )
    );
    (library, report)
}

fn hashes(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in walk(dir) {
        out.push((entry.clone(), fs::read(&entry).unwrap()));
    }
    out.sort();
    out
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn takeout_fidelity_reports_all_five_cases_and_never_fills_a_value() {
    let root = tempfile::tempdir().unwrap();
    let (library, report) = run_import(root.path());
    let before = hashes(&library);
    let out = root.path().join("fidelity.jsonl");
    let output = albumen()
        .arg("takeout-fidelity")
        .arg("--report")
        .arg(&report)
        .arg("--library")
        .arg(&library)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(hashes(&library), before, "library must be untouched");

    let lines: Vec<serde_json::Value> = fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let find = |name: &str| {
        lines
            .iter()
            .find(|line| {
                line["destination"]
                    .as_str()
                    .is_some_and(|d| d.ends_with(name))
            })
            .unwrap_or_else(|| panic!("{name} missing from {lines:?}"))
            .clone()
    };
    let flags = |line: &serde_json::Value| -> Vec<String> {
        line["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_string())
            .collect()
    };

    let agree = find("AGREE.jpg");
    assert_eq!(flags(&agree), Vec::<String>::new(), "{agree}");
    assert_eq!(agree["time"]["json_utc"], "2020-05-20T18:40:30Z");
    assert_eq!(agree["time"]["exif_utc"], "2020-05-20T18:40:00Z");
    assert_eq!(agree["time"]["difference_secs"], 30);
    assert_eq!(agree["time"]["winner"], "exif");
    assert_eq!(agree["gps"]["winner"], "exif");

    let disagree = find("DISAGREE.jpg");
    assert_eq!(flags(&disagree), vec!["time_disagree", "gps_disagree"]);
    assert_eq!(disagree["time"]["difference_secs"], 7200);
    assert!(disagree["gps"]["distance_meters"].as_f64().unwrap() > 1000.0);

    let json_only = find("JSONONLY.jpg");
    assert_eq!(
        flags(&json_only),
        vec!["time_missing_exif", "gps_missing_exif"]
    );
    assert_eq!(json_only["time"]["winner"], "json");
    assert!(json_only["time"]["exif"].is_null());
    assert!(json_only["gps"]["exif"].is_null());

    let exif_only = find("EXIFONLY.jpg");
    assert_eq!(
        flags(&exif_only),
        vec!["time_missing_json", "gps_missing_json"]
    );
    assert!(exif_only["time"]["json_utc"].is_null());
    assert!(
        exif_only["gps"]["json"].is_null(),
        "0,0 is missing, not a place"
    );

    let neither = find("NEITHER.jpg");
    assert_eq!(
        flags(&neither),
        vec![
            "time_missing_json",
            "time_missing_exif",
            "gps_missing_json",
            "gps_missing_exif"
        ]
    );
    assert_eq!(neither["time"]["winner"], "none");
    assert_eq!(neither["gps"]["winner"], "none");
    assert!(neither["time"]["json_utc"].is_null() && neither["time"]["exif"].is_null());

    let summary = lines.last().unwrap();
    assert_eq!(summary["kind"], "fidelity-summary");
    assert_eq!(summary["originals"], 5);
    assert_eq!(summary["flags"]["time_disagree"], 1);
    assert_eq!(summary["flags"]["gps_disagree"], 1);
    assert_eq!(summary["flags"]["time_missing_exif"], 2);
    assert_eq!(summary["flags"]["time_missing_json"], 2);
    assert_eq!(summary["flags"]["no_sidecar"], 0);

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("takeout-fidelity: 5 originals compared"),
        "{stdout}"
    );

    // Refuses to overwrite its own report.
    let again = albumen()
        .arg("takeout-fidelity")
        .arg("--report")
        .arg(&report)
        .arg("--library")
        .arg(&library)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!again.status.success());
}

#[test]
fn import_takeout_help_mentions_takeout_fidelity() {
    let output = albumen()
        .args(["import-takeout", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        help.contains("albumen takeout-fidelity --report <report>"),
        "{help}"
    );
}
