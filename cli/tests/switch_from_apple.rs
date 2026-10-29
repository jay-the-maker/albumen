// SPDX-License-Identifier: Apache-2.0

//! End to end "switch from Apple": import a Photos library export with
//! `import-icloud`, check the report's Live Photo pairing, export the
//! library, verify the export with a proof, then tamper with one exported
//! file and see the drift. Every step's exit code is asserted, including the
//! refusals. Synthetic files only: the Live Photo comes from
//! `scripts/synthetic-live-photo.sh`, the JPEG is the public fixture with its
//! metadata stripped and rewritten. No network is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::Value;

const CONTENT_ID: &str = "A1B2C3D4-E5F6-4071-8A9B-C0D1E2F30405";

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

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

fn bash_program() -> PathBuf {
    if cfg!(windows) {
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
        ] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return path;
            }
        }
    }
    PathBuf::from("bash")
}

fn exiftool() -> Command {
    if cfg!(windows) {
        Command::new("exiftool.cmd")
    } else {
        Command::new("exiftool")
    }
}

fn stamp(path: &Path) {
    let when = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn copy_stamped(source: &Path, dest: &Path) {
    fs::copy(source, dest).unwrap();
    stamp(dest);
}

fn synthetic_jpeg(path: &Path, tags: &[&str]) {
    fs::copy(repo_path("fixtures/jpeg-gps/IMG_0001.jpg"), path).unwrap();
    let status = exiftool()
        .args(["-q", "-overwrite_original", "-all="])
        .args(tags)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    stamp(path);
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

fn report_lines(report: &Path) -> Vec<Value> {
    fs::read_to_string(report)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn switch_from_apple_end_to_end() {
    let started = Instant::now();
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let home = root.join("home");

    // 1. A synthetic Photos library: one Live Photo pair with a shared
    //    ContentIdentifier, one JPEG with an XMP sidecar and an edited render.
    let generated = root.join("generated");
    let output = Command::new(bash_program())
        .arg(repo_path("scripts/synthetic-live-photo.sh"))
        .arg(&generated)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "synthetic-live-photo.sh: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let from = root.join("Photos Library.photoslibrary");
    let live = from.join("originals/0");
    let still = from.join("originals/1");
    let renders = from.join("resources/renders/1");
    for dir in [&live, &still, &renders] {
        fs::create_dir_all(dir).unwrap();
    }
    copy_stamped(
        &generated.join("IMG_LIVE.heic"),
        &live.join("IMG_LIVE.heic"),
    );
    copy_stamped(&generated.join("IMG_LIVE.mov"), &live.join("IMG_LIVE.mov"));
    synthetic_jpeg(
        &still.join("IMG_0002.jpg"),
        &[
            "-EXIF:DateTimeOriginal=2020:05:20 11:40:00",
            "-EXIF:OffsetTimeOriginal=-07:00",
        ],
    );
    let xmp =
        br#"<x:xmpmeta><rdf:Description exif:DateTimeOriginal="2020-05-20T11:40:00"/></x:xmpmeta>"#;
    fs::write(still.join("IMG_0002.jpg.xmp"), xmp).unwrap();
    stamp(&still.join("IMG_0002.jpg.xmp"));
    synthetic_jpeg(
        &renders.join("IMG_0002.jpeg"),
        &[
            "-EXIF:DateTimeOriginal=2020:05:20 11:40:00",
            "-XMP:Rating=5",
        ],
    );
    let source_before = walk(&from);
    assert_eq!(source_before.len(), 5);

    let library = root.join("library");
    let report = root.join("icloud.jsonl");
    let import = |apply: bool, to: &Path, report: &Path| {
        let mut args: Vec<&std::ffi::OsStr> = vec!["import-icloud".as_ref()];
        if apply {
            args.push("--apply".as_ref());
        }
        args.extend([
            "--from".as_ref(),
            from.as_os_str(),
            "--to".as_ref(),
            to.as_os_str(),
            "--report".as_ref(),
            report.as_os_str(),
        ]);
        albumen(&home, &args)
    };

    // 2. Usage error: no --from at all is exit 2.
    let out = albumen(&home, &["import-icloud".as_ref(), "--apply".as_ref()]);
    code(&out, 2, "import-icloud without --from");

    // 3. Unsafe layout: a library inside the source is refused with exit 3.
    let inside = from.join("library");
    let out = import(true, &inside, &root.join("inside.jsonl"));
    code(&out, 3, "import-icloud into the source");
    assert!(!inside.exists(), "a refused import created its library");

    // 4. Dry run: the plan names the Live Photo, and nothing is written.
    let text = code(
        &import(false, &library, &report),
        0,
        "import-icloud dry run",
    );
    assert!(text.contains("dry-run: no files written"), "{text}");
    assert!(text.contains("  files: 5"), "{text}");
    assert!(text.contains("live-photo:"), "{text}");
    assert!(!library.exists(), "the dry run created the library");
    assert!(!report.exists(), "the dry run wrote a report");

    // 5. Import. The pair rests on the shared ContentIdentifier.
    let text = code(&import(true, &library, &report), 0, "import-icloud");
    assert!(text.contains("imported 5 files (5 in)"), "{text}");
    let lines = report_lines(&report);
    let heic = lines
        .iter()
        .find(|line| line["member"] == "originals/0/IMG_LIVE.heic")
        .unwrap();
    assert_eq!(heic["kind"], "original", "{heic}");
    assert_eq!(heic["pair_basis"], "content-id", "{heic}");
    assert_eq!(heic["content_identifier"], CONTENT_ID, "{heic}");
    let render = lines
        .iter()
        .find(|line| line["member"] == "resources/renders/1/IMG_0002.jpeg")
        .unwrap();
    assert_eq!(render["kind"], "edited", "{render}");
    let summary = lines.last().unwrap();
    assert_eq!(summary["pair_basis"]["content_id"], 2, "{summary}");
    assert_eq!(summary["pair_basis"]["stem_only"], 0, "{summary}");
    for (path, bytes) in &source_before {
        let rel = path.strip_prefix(&from).unwrap();
        assert_eq!(&fs::read(library.join(rel)).unwrap(), bytes, "{rel:?}");
    }
    let library_before = walk(&library);

    // The report is never overwritten: a second import is refused with exit 3.
    let again = root.join("library2");
    code(
        &import(true, &again, &report),
        3,
        "import-icloud with an existing report",
    );
    assert!(!again.exists(), "a refused import created its library");

    // 6. Export the library, then re-run: nothing new.
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

    // 7. Verify with a proof. An existing proof is never overwritten.
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
    let json: Value =
        serde_json::from_str(&fs::read_to_string(root.join("proof.json")).unwrap()).unwrap();
    assert_eq!(json["result"], "verified");
    assert_eq!(json["files"], files);
    code(&verify(&proof), 3, "verify --proof again");

    // 8. Tamper with the exported Live Photo movie: drift, exit 1.
    let (target, _) = walk(&export)
        .into_iter()
        .find(|(p, _)| p.file_name().is_some_and(|n| n == "IMG_LIVE.mov"))
        .unwrap();
    let mut perms = fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&target, perms).unwrap();
    fs::write(&target, b"tampered").unwrap();
    let tampered = root.join("tampered");
    code(&verify(&tampered), 1, "verify after tamper");
    let json: Value =
        serde_json::from_str(&fs::read_to_string(root.join("tampered.json")).unwrap()).unwrap();
    assert_eq!(json["result"], "drift");
    assert_eq!(json["drifted"], 1);

    // Sources stayed bit-exact the whole way.
    assert_eq!(walk(&from), source_before, "the Photos library changed");
    assert_eq!(walk(&library), library_before, "the library changed");
    let secs = started.elapsed().as_secs_f64();
    eprintln!("switch_from_apple_end_to_end: {secs:.2}s");
    assert!(secs < 120.0, "took {secs:.1}s, budget is 2 minutes");
}
