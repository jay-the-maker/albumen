// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

fn albumen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_albumen"))
}

#[test]
fn platform_dirs_prints_paths_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let output = albumen()
        .arg("platform-dirs")
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("platform-dirs"), "{text}");
    assert!(text.contains("config:"), "{text}");
    assert!(text.contains("data:"), "{text}");
    assert!(text.contains("creates: nothing"), "{text}");
    #[cfg(target_os = "linux")]
    {
        assert!(
            text.contains(tmp.path().join("config").join("albumen").to_str().unwrap()),
            "{text}"
        );
        assert!(
            text.contains(tmp.path().join("data").join("albumen").to_str().unwrap()),
            "{text}"
        );
        assert!(!tmp.path().join("config").join("albumen").exists());
        assert!(!tmp.path().join("data").join("albumen").exists());
    }
}
