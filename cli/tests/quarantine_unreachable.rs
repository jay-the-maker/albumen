// SPDX-License-Identifier: Apache-2.0

//! `copy_verify_quarantine` is a library opt-in. The default CLI cannot name
//! it, and export / verify-manifest leave the source path where it was.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

fn run(args: &[impl AsRef<OsStr>]) -> (bool, String) {
    let output = albumen().args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn default_cli_and_mcp_sources_do_not_name_quarantine() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    for crate_src in ["cli/src", "mcp/src"] {
        let sources = rust_sources(&workspace.join(crate_src));
        assert!(!sources.is_empty(), "no sources in {crate_src}");
        for path in sources {
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("copy_verify_quarantine(") && !text.contains("Quarantine"),
                "{} calls the quarantine path",
                path.display()
            );
        }
    }
}

#[test]
fn every_default_cli_path_leaves_the_source_in_place() {
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    let source = library.join("IMG_0001.jpg");
    fs::write(&source, b"jpeg-bytes").unwrap();
    let inode = fs::metadata(&source).unwrap().ino();
    let bytes = fs::read(&source).unwrap();

    let (ok, text) = run(&[] as &[&str]);
    assert!(!ok, "{text}");
    for args in [
        &["--help"][..],
        &["help"],
        &["export", "--help"],
        &["verify-manifest", "--help"],
        &["help", "export"],
        &["help", "verify-manifest"],
    ] {
        let (ok, text) = run(args);
        assert!(ok, "{args:?}: {text}");
        assert!(
            !text.to_lowercase().contains("quarantine"),
            "{args:?} offers quarantine:\n{text}"
        );
    }

    for args in [&["quarantine"][..], &["copy_verify_quarantine"]] {
        let (ok, text) = run(args);
        assert!(!ok, "{args:?} was accepted: {text}");
    }

    let dest = root.path().join("dest");
    let (ok, text) = run(&[
        OsStr::new("export"),
        OsStr::new("--from"),
        library.as_os_str(),
        OsStr::new("--to"),
        dest.as_os_str(),
        OsStr::new("--apply"),
    ]);
    assert!(ok, "{text}");
    let manifest = dest.join("ALBUMEN-MANIFEST.txt");
    let (ok, text) = run(&[
        OsStr::new("verify-manifest"),
        OsStr::new("--manifest"),
        manifest.as_os_str(),
        OsStr::new("--against"),
        dest.as_os_str(),
    ]);
    assert!(ok, "{text}");

    assert!(source.is_file(), "source path was removed");
    assert_eq!(fs::metadata(&source).unwrap().ino(), inode);
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert!(dest.join("IMG_0001.jpg").is_file());
    let names = fs::read_dir(&library)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["IMG_0001.jpg".to_string()]);
}
