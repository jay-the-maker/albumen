// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

fn receive() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
}

#[test]
fn print_service_omits_the_password_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let output = receive()
        .args([
            "--to",
            tmp.path().join("library").to_str().unwrap(),
            "--user",
            "camera",
            "--password",
            "s3cret-value",
            "--webdav",
            "127.0.0.1:8080",
            "--print-service",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("--apply"), "{text}");
    assert!(text.contains("ALBUMEN_RECEIVE_PASSWORD"), "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(!text.contains("s3cret-value"), "{text}");
    assert!(!tmp.path().join("library").exists());
}

#[test]
fn service_dir_without_apply_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("unit");
    let output = receive()
        .args([
            "--to",
            tmp.path().join("library").to_str().unwrap(),
            "--user",
            "camera",
            "--webdav",
            "127.0.0.1:8080",
            "--service-dir",
            dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("dry-run"), "{text}");
    assert!(!dir.exists());
}

#[test]
fn service_dir_apply_writes_the_file_and_does_not_listen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("unit");
    let output = receive()
        .args([
            "--to",
            tmp.path().join("library").to_str().unwrap(),
            "--user",
            "camera",
            "--password",
            "s3cret-value",
            "--webdav",
            "127.0.0.1:8080",
            "--service-dir",
            dir.to_str().unwrap(),
            "--apply",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("service not started"), "{text}");
    assert!(!text.contains("s3cret-value"), "{text}");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let body = std::fs::read_to_string(dir.join(&files[0])).unwrap();
    assert!(body.contains("--apply"), "{body}");
    assert!(!body.contains("s3cret-value"), "{body}");
}

#[test]
fn keyring_dry_run_does_not_print_a_password() {
    let tmp = tempfile::tempdir().unwrap();
    let output = receive()
        .args([
            "--to",
            tmp.path().join("library").to_str().unwrap(),
            "--user",
            "camera",
            "--webdav",
            "127.0.0.1:8080",
            "--keyring",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("password: keyring"), "{text}");
    assert!(text.contains("dry-run"), "{text}");
}
