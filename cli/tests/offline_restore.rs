// SPDX-License-Identifier: Apache-2.0

//! A library on disk can be read and exported with no account and no network.
//! On Linux the process is started with `connect` forced to fail, so a call
//! out to a service cannot succeed quietly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn albumen() -> &'static str {
    env!("CARGO_BIN_EXE_albumen")
}

fn run(root: &Path, preload: Option<&Path>, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(albumen());
    cmd.env_clear();
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    cmd.env("PATH", path);
    cmd.env("TMPDIR", root);
    for (key, value) in [
        ("http_proxy", "http://127.0.0.1:9"),
        ("https_proxy", "http://127.0.0.1:9"),
        ("all_proxy", "http://127.0.0.1:9"),
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("ALL_PROXY", "http://127.0.0.1:9"),
    ] {
        cmd.env(key, value);
    }
    if let Some(preload) = preload {
        cmd.env("LD_PRELOAD", preload);
    }
    let output = cmd.args(args).output().expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn preload_library() -> Option<PathBuf> {
    static READY: OnceLock<Option<PathBuf>> = OnceLock::new();
    READY
        .get_or_init(|| {
            if !cfg!(target_os = "linux") {
                return None;
            }
            let dir = std::env::temp_dir()
                .join(format!("albumen-offline-preload-{}", std::process::id()));
            fs::create_dir_all(&dir).ok()?;
            let source = dir.join("block_connect.c");
            let library = dir.join("block_connect.so");
            let checker = dir.join("check_connect");
            fs::write(
                &source,
                r#"
                #define _GNU_SOURCE
                #include <errno.h>
                #include <netinet/in.h>
                #include <sys/socket.h>
                int connect(int fd, const struct sockaddr *addr, socklen_t len) {
                    (void)fd; (void)addr; (void)len;
                    errno = ENETUNREACH;
                    return -1;
                }
                "#,
            )
            .ok()?;
            let built = Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(&library)
                .arg(&source)
                .status()
                .ok()?
                .success();
            if !built {
                return None;
            }
            let check_src = dir.join("check_connect.c");
            fs::write(
                &check_src,
                r#"
                #include <netinet/in.h>
                #include <arpa/inet.h>
                #include <sys/socket.h>
                int main(void) {
                    int fd = socket(AF_INET, SOCK_STREAM, 0);
                    struct sockaddr_in addr;
                    addr.sin_family = AF_INET;
                    addr.sin_port = htons(80);
                    addr.sin_addr.s_addr = htonl(0x01010101);
                    return connect(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0 ? 0 : 1;
                }
                "#,
            )
            .ok()?;
            let compiled = Command::new("cc")
                .arg("-o")
                .arg(&checker)
                .arg(&check_src)
                .status()
                .ok()?
                .success();
            if !compiled {
                return None;
            }
            let blocked = Command::new(&checker)
                .env_clear()
                .env("LD_PRELOAD", &library)
                .status()
                .ok()?
                .code()
                == Some(1);
            blocked.then_some(library)
        })
        .clone()
}

#[test]
fn library_is_readable_and_exportable_with_no_network_or_account() {
    let preload = preload_library();
    if cfg!(target_os = "linux") {
        assert!(
            preload.is_some(),
            "Linux export must run with connect() blocked"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    let jpeg = b"\xff\xd8\xff\xdb synthetic-offline-restore-jpeg";
    let xmp = br#"<x:xmpmeta><rdf:Description albumen:note="synthetic"/></x:xmpmeta>"#;
    fs::write(library.join("IMG_0001.jpg"), jpeg).unwrap();
    fs::write(library.join("IMG_0001.jpg.xmp"), xmp).unwrap();
    let source_jpeg = fs::read(library.join("IMG_0001.jpg")).unwrap();

    let dest = root.path().join("export");
    let (ok, text) = run(
        root.path(),
        preload.as_deref(),
        &[
            "export",
            "--from",
            library.to_str().unwrap(),
            "--to",
            dest.to_str().unwrap(),
        ],
    );
    assert!(ok, "{text}");
    assert_eq!(fs::read(dest.join("IMG_0001.jpg")).unwrap(), jpeg);
    assert_eq!(fs::read(dest.join("IMG_0001.jpg.xmp")).unwrap(), xmp);
    assert_eq!(fs::read(library.join("IMG_0001.jpg")).unwrap(), source_jpeg);

    let manifest = dest.join("ALBUMEN-MANIFEST.txt");
    let (ok, text) = run(
        root.path(),
        preload.as_deref(),
        &[
            "verify-manifest",
            "--manifest",
            manifest.to_str().unwrap(),
            "--against",
            dest.to_str().unwrap(),
        ],
    );
    assert!(ok, "{text}");
    let listed = fs::read_to_string(&manifest).unwrap();
    assert!(listed.contains("IMG_0001.jpg"), "{listed}");
    assert!(listed.contains("IMG_0001.jpg.xmp"), "{listed}");
    assert_eq!(fs::read(dest.join("IMG_0001.jpg")).unwrap(), jpeg);
}
