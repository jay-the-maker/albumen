// SPDX-License-Identifier: Apache-2.0

//! End to end "switch from Google": import a Takeout, check its fidelity,
//! export the library, verify the export with a proof, then tamper with one
//! exported file and see the drift. Every step's exit code is asserted.
//! Synthetic files only: each JPEG is the public fixture with its metadata
//! stripped and rewritten. No network is used.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn albumen(home: &Path, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .env("ALBUMEN_HOME", home)
        .output()
        .unwrap()
}

fn code(output: &Output, want: i32, step: &str) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(want), "{step}: {text}");
    text
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

fn sidecar(title: &str, taken: &str) -> Vec<u8> {
    format!(
        r#"{{"title":"{title}","photoTakenTime":{{"timestamp":"{taken}","formatted":"synthetic"}},"geoData":{{"latitude":37.5,"longitude":-122.25,"altitude":0.0,"latitudeSpan":0.0,"longitudeSpan":0.0}}}}"#
    )
    .into_bytes()
}

fn walk(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            let bytes = fs::read(&path).unwrap();
            out.push((path, bytes));
        }
    }
    out.sort();
    out
}

#[test]
fn switch_from_google_end_to_end() {
    let started = Instant::now();
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let home = root.join("home");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();

    // 1. A synthetic two-part Takeout.
    let tags = [
        "-EXIF:DateTimeOriginal=2020:05:20 11:40:00",
        "-EXIF:OffsetTimeOriginal=-07:00",
        "-GPSLatitude=37.5",
        "-GPSLatitudeRef=N",
        "-GPSLongitude=122.25",
        "-GPSLongitudeRef=W",
    ];
    let takeout = root.join("takeout");
    fs::create_dir(&takeout).unwrap();
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0).unwrap());
    for (part, names) in [
        (1, ["BEACH.jpg", "PARK.jpg"]),
        (2, ["HIKE.jpg", "LAKE.jpg"]),
    ] {
        let file = File::create(takeout.join(format!("takeout-00{part}.zip"))).unwrap();
        let mut zip = ZipWriter::new(file);
        for name in names {
            let jpeg = synthetic_jpeg(&work, name, &tags);
            zip.start_file(format!("Takeout/Google Photos/{name}"), options)
                .unwrap();
            zip.write_all(&jpeg).unwrap();
            zip.start_file(format!("Takeout/Google Photos/{name}.json"), options)
                .unwrap();
            zip.write_all(&sidecar(name, "1590000000")).unwrap();
        }
        zip.finish().unwrap();
    }
    let takeout_before = walk(&takeout);

    // 2. Import the Takeout.
    let library = root.join("library");
    let report = root.join("report.jsonl");
    let out = albumen(
        &home,
        &[
            "import-takeout".as_ref(),
            "--apply".as_ref(),
            "--from".as_ref(),
            takeout.as_os_str(),
            "--to".as_ref(),
            library.as_os_str(),
            "--report".as_ref(),
            report.as_os_str(),
        ],
    );
    let text = code(&out, 0, "import-takeout");
    assert!(text.contains("next: albumen takeout-fidelity"), "{text}");
    let library_before = walk(&library);
    assert!(!library_before.is_empty());

    // 3. Fidelity: one line per photo, and the library is untouched.
    let fidelity = root.join("fidelity.jsonl");
    let out = albumen(
        &home,
        &[
            "takeout-fidelity".as_ref(),
            "--report".as_ref(),
            report.as_os_str(),
            "--library".as_ref(),
            library.as_os_str(),
            "--out".as_ref(),
            fidelity.as_os_str(),
        ],
    );
    code(&out, 0, "takeout-fidelity");
    let lines = fs::read_to_string(&fidelity).unwrap();
    assert!(lines.lines().count() >= 4, "{lines}");
    assert_eq!(
        walk(&library),
        library_before,
        "fidelity wrote to the library"
    );

    // 4. Export the library, then re-run: nothing new.
    let export = root.join("export");
    let manifest = root.join("manifest.txt");
    let export_args = |manifest: &Path| -> Vec<std::ffi::OsString> {
        vec![
            "export".into(),
            "--apply".into(),
            "--from".into(),
            library.clone().into(),
            "--to".into(),
            export.clone().into(),
            "--manifest".into(),
            manifest.into(),
        ]
    };
    let args = export_args(&manifest);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let text = code(&albumen(&home, &args), 0, "export");
    let files = library_before.len();
    assert!(
        text.contains(&format!(
            "exported {files} new, 0 already present and verified"
        )),
        "{text}"
    );
    let again_manifest = root.join("manifest2.txt");
    let args = export_args(&again_manifest);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let text = code(&albumen(&home, &args), 0, "export re-run");
    assert!(
        text.contains(&format!(
            "exported 0 new, {files} already present and verified"
        )),
        "{text}"
    );

    // 5. Verify with a proof.
    let verify = |proof: &Path| {
        albumen(
            &home,
            &[
                "verify-manifest".as_ref(),
                "--manifest".as_ref(),
                manifest.as_os_str(),
                "--against".as_ref(),
                export.as_os_str(),
                "--proof".as_ref(),
                proof.as_os_str(),
            ],
        )
    };
    let proof = root.join("proof");
    code(&verify(&proof), 0, "verify --proof");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("proof.json")).unwrap()).unwrap();
    assert_eq!(json["result"], "verified");
    assert_eq!(json["files"], files);
    // An existing proof is never overwritten.
    code(&verify(&proof), 3, "verify --proof again");

    // 6. Tamper with one exported file: drift, exit 1.
    let (target, _) = walk(&export)
        .into_iter()
        .find(|(p, _)| p.extension().is_some_and(|e| e == "jpg"))
        .unwrap();
    let mut perms = fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&target, perms).unwrap();
    fs::write(&target, b"tampered").unwrap();
    let tampered = root.join("tampered");
    code(&verify(&tampered), 1, "verify after tamper");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("tampered.json")).unwrap()).unwrap();
    assert_eq!(json["result"], "drift");
    assert_eq!(json["drifted"], 1);

    // Sources stayed bit-exact the whole way.
    assert_eq!(walk(&takeout), takeout_before, "the Takeout changed");
    assert_eq!(walk(&library), library_before, "the library changed");
    let secs = started.elapsed().as_secs_f64();
    eprintln!("switch_from_google_end_to_end: {secs:.2}s");
    assert!(secs < 120.0, "took {secs:.1}s, budget is 2 minutes");
}
