// SPDX-License-Identifier: Apache-2.0

//! Launch the pinned ExifTool.
//!
//! `scripts/install-exiftool.sh` puts Image-ExifTool 13.59 on `PATH`. On
//! Windows that directory holds the Perl script plus `exiftool.cmd`, which
//! runs the script with `perl`. Rust resolves a bare program name only as
//! `name.exe`, so `Command::new("exiftool")` does not see `exiftool.cmd` and
//! returns "program not found". Asking for `exiftool.cmd` is the lookup that
//! finds the launcher. Rust then runs it through `cmd.exe`.

use std::process::Command;

/// A `Command` that starts the pinned ExifTool and has no arguments yet.
pub fn exiftool_command() -> Command {
    #[cfg(windows)]
    {
        Command::new("exiftool.cmd")
    }
    #[cfg(not(windows))]
    {
        Command::new("exiftool")
    }
}

#[cfg(test)]
mod tests {
    use super::exiftool_command;

    #[test]
    fn exiftool_command_reports_a_version() {
        let output = exiftool_command()
            .arg("-ver")
            .output()
            .expect("spawn exiftool");
        assert!(
            output.status.success(),
            "exiftool -ver failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let version = String::from_utf8_lossy(&output.stdout);
        assert!(
            !version.trim().is_empty(),
            "exiftool -ver printed no version"
        );
    }
}
