// SPDX-License-Identifier: Apache-2.0

//! `verify-manifest --proof`: a clean export writes a Markdown + JSON proof
//! and exits 0; a tampered file shows as drift and exits non-zero; an
//! existing proof is never overwritten. Synthetic files only.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn albumen(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .output()
        .unwrap()
}

fn export(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let src = root.join("src");
    fs::create_dir_all(src.join("2020")).unwrap();
    fs::write(src.join("2020/a.jpg"), b"synthetic-a").unwrap();
    fs::write(src.join("2020/b.jpg"), b"synthetic-bb").unwrap();
    fs::write(src.join("c.mov"), b"synthetic-ccc").unwrap();
    let out = root.join("out");
    let manifest = root.join("manifest.txt");
    let output = albumen(&[
        "export".as_ref(),
        "--apply".as_ref(),
        "--from".as_ref(),
        src.as_os_str(),
        "--to".as_ref(),
        out.as_os_str(),
        "--manifest".as_ref(),
        manifest.as_os_str(),
    ]);
    assert!(output.status.success(), "{output:?}");
    (out, manifest)
}

fn verify(manifest: &Path, out: &Path, proof: &Path) -> Output {
    albumen(&[
        "verify-manifest".as_ref(),
        "--manifest".as_ref(),
        manifest.as_os_str(),
        "--against".as_ref(),
        out.as_os_str(),
        "--proof".as_ref(),
        proof.as_os_str(),
    ])
}

#[test]
fn clean_export_writes_a_keepable_proof() {
    let root = tempfile::tempdir().unwrap();
    let (out, manifest) = export(root.path());
    let proof = root.path().join("proof");
    let output = verify(&manifest, &out, &proof);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.path().join("proof.json")).unwrap()).unwrap();
    let md = fs::read_to_string(root.path().join("proof.md")).unwrap();
    assert_eq!(json["result"], "verified");
    assert_eq!(json["files"], 3);
    assert_eq!(json["bytes"], 11 + 12 + 13);
    assert_eq!(json["counts"], "3 present and verified, 0 drifted");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert!(json["commit"].as_str().is_some_and(|c| !c.is_empty()));
    for key in ["started_utc", "finished_utc"] {
        let t = json[key].as_str().unwrap();
        assert!(t.ends_with('Z') && t.len() == 20, "{t}");
    }
    let manifest_sha = json["manifest_sha256"].as_str().unwrap();
    assert_eq!(manifest_sha.len(), 64);
    assert_eq!(json["sorted_hashes_sha256"].as_str().unwrap().len(), 64);
    assert!(json["drift"].as_array().unwrap().is_empty());
    assert!(json["rerun"]
        .as_str()
        .unwrap()
        .starts_with("albumen verify-manifest --manifest "));
    assert!(md.contains(manifest_sha), "{md}");
    assert!(md.contains("3 present and verified, 0 drifted"));
    assert!(md.contains("## Re-run"));

    // Never overwrites: a second run refuses and leaves both files as they were.
    let before = fs::read(root.path().join("proof.json")).unwrap();
    let again = verify(&manifest, &out, &proof);
    assert!(!again.status.success());
    assert_eq!(fs::read(root.path().join("proof.json")).unwrap(), before);
}

#[test]
fn tampered_file_is_drift_and_exits_non_zero() {
    let root = tempfile::tempdir().unwrap();
    let (out, manifest) = export(root.path());
    let target = out.join("2020/a.jpg");
    let mut perms = fs::metadata(&target).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&target, perms).unwrap();
    fs::write(&target, b"tampered!!!").unwrap();
    let proof = root.path().join("proof.md");
    let output = verify(&manifest, &out, &proof);
    assert!(!output.status.success(), "{output:?}");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.path().join("proof.json")).unwrap()).unwrap();
    assert_eq!(json["result"], "drift");
    assert_eq!(json["drifted"], 1);
    assert_eq!(json["counts"], "2 present and verified, 1 drifted");
    assert_eq!(json["drift"][0]["kind"], "changed");
    assert_eq!(json["drift"][0]["path"], "2020/a.jpg");
    let md = fs::read_to_string(root.path().join("proof.md")).unwrap();
    assert!(md.contains("- changed `2020/a.jpg`"), "{md}");
}
