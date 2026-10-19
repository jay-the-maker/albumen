// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

#[test]
fn completions_bash_prints_script_mentioning_verify_manifest() {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "completions bash failed with {:?}: {stderr}",
        output.status
    );
    assert!(
        stdout.contains("verify-manifest"),
        "completions bash output does not mention verify-manifest"
    );
}
