// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn meta_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
}

fn locked_mode(mode: u32) -> u32 {
    #[cfg(unix)]
    {
        mode & !0o222
    }
    #[cfg(windows)]
    {
        mode | 1
    }
}

fn apply_mode(path: &Path, mode: u32) {
    let mut perms = fs::symlink_metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(mode);
    }
    #[cfg(windows)]
    {
        perms.set_readonly(mode & 1 != 0);
    }
    let _ = fs::set_permissions(path, perms);
}

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

struct ModeGuard {
    saved: Vec<(PathBuf, u32)>,
}

impl ModeGuard {
    fn lock(paths: &[PathBuf]) -> Self {
        let saved = paths
            .iter()
            .map(|path| {
                let mode = meta_mode(&fs::symlink_metadata(path).unwrap());
                apply_mode(path, locked_mode(mode));
                (path.clone(), mode)
            })
            .collect();
        Self { saved }
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        for (path, mode) in self.saved.iter().rev() {
            if fs::symlink_metadata(path).is_err() {
                continue;
            }
            apply_mode(path, *mode);
        }
    }
}

#[test]
fn scan_refuses_a_manifest_inside_the_source_and_leaves_bytes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("library");
    fs::create_dir_all(source.join("sub")).unwrap();
    let file = source.join("sub").join("a.txt");
    fs::write(&file, b"alpha").unwrap();
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    let inside = source.join("out.jsonl");
    let refused = albumen()
        .arg("scan")
        .arg(&source)
        .arg("--manifest")
        .arg(&inside)
        .output()
        .unwrap();
    let refused_text = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{refused_text}");
    assert!(refused_text.contains("inside"), "{refused_text}");
    assert!(!inside.exists());
    assert!(!source.join("nested").exists());

    let manifest = root.path().join("scan.jsonl");
    let _guard = ModeGuard::lock(&[source.clone(), source.join("sub"), file.clone()]);
    let output = albumen()
        .arg("scan")
        .arg(&source)
        .arg("--manifest")
        .arg(&manifest)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert_eq!(fs::read(&file).unwrap(), b"alpha");
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), mtime);
    let mode = meta_mode(&fs::metadata(&file).unwrap());
    #[cfg(unix)]
    assert_eq!(mode & 0o222, 0);
    #[cfg(windows)]
    assert_eq!(mode & 1, 1);
    let manifest_text = fs::read_to_string(&manifest).unwrap();
    assert!(
        manifest_text.contains("\"path\":\"sub/a.txt\""),
        "{manifest_text}"
    );
    assert!(manifest_text.starts_with('{'));

    let verified = albumen()
        .arg("verify-manifest")
        .arg("--manifest")
        .arg(&manifest)
        .arg("--against")
        .arg(&source)
        .output()
        .unwrap();
    let verified_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(verified.status.success(), "{verified_text}");
    assert_eq!(fs::read(&file).unwrap(), b"alpha");
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), mtime);
    let _ = Path::new(&manifest);
}
