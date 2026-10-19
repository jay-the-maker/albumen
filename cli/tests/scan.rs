// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

struct ModeGuard {
    #[cfg(unix)]
    saved: Vec<(PathBuf, u32)>,
    #[cfg(not(unix))]
    saved: Vec<PathBuf>,
}

impl ModeGuard {
    fn lock(paths: &[PathBuf]) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let saved = paths
                .iter()
                .map(|path| {
                    let mode = fs::symlink_metadata(path).unwrap().mode();
                    let mut perms = fs::symlink_metadata(path).unwrap().permissions();
                    perms.set_mode(mode & !0o222);
                    fs::set_permissions(path, perms).unwrap();
                    (path.clone(), mode)
                })
                .collect();
            Self { saved }
        }
        #[cfg(not(unix))]
        {
            for path in paths {
                let meta = fs::symlink_metadata(path).unwrap();
                if meta.is_file() {
                    let mut perms = meta.permissions();
                    perms.set_readonly(true);
                    fs::set_permissions(path, perms).unwrap();
                }
            }
            Self {
                saved: paths.to_vec(),
            }
        }
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for (path, mode) in self.saved.iter().rev() {
                let Ok(meta) = fs::symlink_metadata(path) else {
                    continue;
                };
                let mut perms = meta.permissions();
                perms.set_mode(*mode);
                let _ = fs::set_permissions(path, perms);
            }
        }
        #[cfg(not(unix))]
        {
            for path in self.saved.iter().rev() {
                let Ok(meta) = fs::symlink_metadata(path) else {
                    continue;
                };
                if meta.is_file() {
                    let mut perms = meta.permissions();
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(path, perms);
                }
            }
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o222, 0);
    }
    #[cfg(not(unix))]
    assert!(fs::metadata(&file).unwrap().permissions().readonly());
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
