// SPDX-License-Identifier: Apache-2.0

//! Path, time, and stream behavior shared by Linux, macOS, and Windows.
//!
//! Windows paths longer than 260 characters, reserved device names, and
//! trailing dots or spaces are opened with the `\\?\` prefix. Case folding
//! is reported when a destination already has the same name in another case.
//! NTFS alternate data streams are listed and copied with the file. NTFS
//! keeps modification time in 100 ns ticks.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::IngestError;
#[cfg(windows)]
use crate::mtime::to_unix_nanos;

/// A named NTFS stream other than the file's default `::$DATA` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedStream {
    pub name: String,
}

pub fn list_named_streams(path: &Path) -> Result<Vec<NamedStream>, IngestError> {
    let opened = io_path(path);
    let raw = albumen_win::raw_stream_names(&opened).map_err(|err| IngestError::io(path, err))?;
    let mut named = Vec::new();
    for (raw_name, _) in raw {
        if let Some(name) = parse_stream_name(&raw_name) {
            named.push(NamedStream { name });
        }
    }
    Ok(named)
}

/// `:secret:$DATA` is the named stream `secret`. `::$DATA` is the file.
pub fn parse_stream_name(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix(':')?;
    let name = rest.strip_suffix(":$DATA").unwrap_or(rest);
    if name.is_empty() || name.contains(':') || name.contains('/') || name.contains('\\') {
        return None;
    }
    Some(name.to_string())
}

/// Path passed to the operating system.
///
/// On Linux and macOS this is `path` unchanged. On Windows a short ordinary
/// path is unchanged, and a long path, a reserved device name, or a trailing
/// dot or space is the absolute path with the `\\?\` prefix.
pub fn io_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        windows_io_path(path)
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(windows)]
fn windows_io_path(path: &Path) -> PathBuf {
    let absolute = match std::path::absolute(path) {
        Ok(absolute) => absolute,
        Err(_) => return path.to_path_buf(),
    };
    let Some(text) = absolute.to_str() else {
        return path.to_path_buf();
    };
    if !needs_windows_extended(text) {
        return path.to_path_buf();
    }
    PathBuf::from(windows_extended_form(text))
}

/// `\\?\` form of an absolute Windows path. Other strings are returned as given.
#[cfg(any(windows, test))]
#[allow(dead_code)]
pub fn windows_extended_form(absolute: &str) -> String {
    if absolute.starts_with(r"\\?\") || !needs_windows_extended(absolute) {
        return absolute.to_string();
    }
    if let Some(rest) = absolute.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{absolute}")
    }
}

#[cfg(any(windows, test))]
#[allow(dead_code)]
pub fn needs_windows_extended(absolute: &str) -> bool {
    if absolute.starts_with(r"\\?\") {
        return false;
    }
    if absolute.len() >= 260 {
        return true;
    }
    for component in absolute.split(['\\', '/']) {
        if component.is_empty() || component.ends_with(':') {
            continue;
        }
        if component != "."
            && component != ".."
            && (component.ends_with(' ') || component.ends_with('.'))
        {
            return true;
        }
        if is_windows_reserved_component(component) {
            return true;
        }
    }
    false
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, and `LPT1`–`LPT9`, with or without an extension.
#[cfg(any(windows, test))]
#[allow(dead_code)]
pub fn is_windows_reserved_component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.trim_end_matches(' ');
    if stem.is_empty() {
        return false;
    }
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let rest = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"));
    matches!(rest, Some(rest) if rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit() && rest != "0")
}

/// Another file in `dir` whose name matches `name` under case folding.
pub fn case_collision(dir: &Path, name: &str) -> Result<Option<String>, IngestError> {
    let read = match fs::read_dir(io_path(dir)) {
        Ok(read) => read,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(IngestError::io(dir, err)),
    };
    let folded = name.to_lowercase();
    for entry in read {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        let existing_name = entry.file_name();
        let Some(existing) = existing_name.to_str() else {
            continue;
        };
        if existing != name && existing.to_lowercase() == folded {
            return Ok(Some(existing.to_string()));
        }
    }
    Ok(None)
}

/// Config and data directories for this operating system.
///
/// Linux uses XDG (`~/.config/albumen`, `~/.local/share/albumen`). macOS uses
/// `~/Library/Application Support/albumen`. Windows uses the roaming Known
/// Folder. Nothing is created.
pub fn platform_dirs() -> Result<(PathBuf, PathBuf), IngestError> {
    let dirs = directories::ProjectDirs::from("org", "Albumen", "albumen").ok_or_else(|| {
        IngestError::store(
            "platform-dirs",
            "home",
            "no home directory for platform paths",
        )
    })?;
    Ok((
        dirs.config_dir().to_path_buf(),
        dirs.data_dir().to_path_buf(),
    ))
}

pub fn mtime_matches(expected: SystemTime, actual: SystemTime) -> bool {
    if expected == actual {
        return true;
    }
    #[cfg(windows)]
    {
        return to_unix_nanos(expected).abs_diff(to_unix_nanos(actual)) < 100;
    }
    #[cfg(not(windows))]
    {
        let _ = (expected, actual);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_names_keep_the_named_stream_and_skip_the_file() {
        assert_eq!(parse_stream_name("::$DATA"), None);
        assert_eq!(
            parse_stream_name(":secret:$DATA").as_deref(),
            Some("secret")
        );
        assert_eq!(
            parse_stream_name(":Zone.Identifier:$DATA").as_deref(),
            Some("Zone.Identifier")
        );
        assert_eq!(parse_stream_name(":has:colon:$DATA"), None);
        assert_eq!(parse_stream_name("plain"), None);
    }

    #[test]
    fn reserved_windows_names_and_long_paths_take_the_extended_prefix() {
        assert!(is_windows_reserved_component("CON"));
        assert!(is_windows_reserved_component("con.txt"));
        assert!(is_windows_reserved_component("COM1"));
        assert!(is_windows_reserved_component("lpt9.jpg"));
        assert!(!is_windows_reserved_component("COM"));
        assert!(!is_windows_reserved_component("COM0"));
        assert!(!is_windows_reserved_component("IMG_0001.JPG"));
        assert!(!needs_windows_extended(r"C:\photos\IMG_0001.JPG"));
        assert!(needs_windows_extended(r"C:\photos\CON.jpg"));
        assert!(needs_windows_extended(r"C:\photos\trail."));
        let long = format!(r"C:\{}", "a".repeat(260));
        assert!(needs_windows_extended(&long));
        assert!(windows_extended_form(&long).starts_with(r"\\?\"));
        assert_eq!(
            windows_extended_form(r"C:\photos\IMG_0001.JPG"),
            r"C:\photos\IMG_0001.JPG"
        );
        let unc = format!(r"\\server\share\{}", "b".repeat(260));
        assert!(windows_extended_form(&unc).starts_with(r"\\?\UNC\"));
    }

    #[test]
    fn case_fold_reports_a_different_spelling() {
        let tmp = crate::test_support::Tmp::new();
        let dir = tmp.path().join("dest");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("IMG.JPG"), b"one").unwrap();
        assert_eq!(
            case_collision(&dir, "img.jpg").unwrap().as_deref(),
            Some("IMG.JPG")
        );
        assert_eq!(case_collision(&dir, "IMG.JPG").unwrap(), None);
        assert_eq!(case_collision(&dir, "other.jpg").unwrap(), None);
    }

    #[cfg(windows)]
    #[test]
    fn reserved_and_long_paths_round_trip_bytes() {
        let tmp = crate::test_support::Tmp::new();
        let reserved = tmp.path().join("NUL.txt");
        {
            use std::io::Write;
            let mut file = crate::dest::create_file(&reserved).unwrap();
            file.write_all(b"hello-reserved").unwrap();
        }
        assert_eq!(fs::read(&reserved).unwrap(), b"hello-reserved");

        let mut deep = tmp.path().to_path_buf();
        for index in 0..30 {
            deep.push(format!("dir{index:02}{}", "n".repeat(12)));
        }
        crate::dest::create_dir_all(&deep).unwrap();
        let photo = deep.join("IMG_0001.JPG");
        assert!(photo.as_os_str().len() > 260, "{}", photo.display());
        {
            use std::io::Write;
            let mut file = crate::dest::create_file(&photo).unwrap();
            file.write_all(b"long-path").unwrap();
        }
        assert_eq!(fs::read(io_path(&photo)).unwrap(), b"long-path");
    }

    #[cfg(windows)]
    #[test]
    fn alternate_streams_are_listed() {
        let tmp = crate::test_support::Tmp::new();
        let file = tmp.path().join("IMG_0001.JPG");
        fs::write(&file, b"jpeg").unwrap();
        let mut stream = file.as_os_str().to_os_string();
        stream.push(":note");
        fs::write(stream, b"kept").unwrap();
        let names: Vec<_> = list_named_streams(&file)
            .unwrap()
            .into_iter()
            .map(|stream| stream.name)
            .collect();
        assert!(names.iter().any(|name| name == "note"), "{names:?}");
    }
}
