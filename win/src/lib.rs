// SPDX-License-Identifier: Apache-2.0

//! Windows calls that `albumen-ingest` cannot make itself.
//!
//! `albumen-ingest` forbids unsafe code. Stream enumeration and
//! `MoveFileExW` live here. The default `::$DATA` stream is the file itself
//! and is not returned. A volume without stream support returns an empty list.
//!
//! `move_file_ex` is `MoveFileExW`. Callers pass
//! `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` for a durable rename,
//! or `MOVEFILE_WRITE_THROUGH` alone to flush a directory without replacing it.

use std::path::Path;

#[cfg(windows)]
mod sys {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard, MoveFileExW,
        WIN32_FIND_STREAM_DATA,
    };

    pub fn raw_stream_names(path: &Path) -> std::io::Result<Vec<(String, u64)>> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut data = WIN32_FIND_STREAM_DATA {
            StreamSize: 0,
            cStreamName: [0; 296],
        };
        let handle = unsafe {
            FindFirstStreamW(
                wide.as_ptr(),
                FindStreamInfoStandard,
                &mut data as *mut WIN32_FIND_STREAM_DATA as *mut _,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_INVALID_FUNCTION as i32) {
                return Ok(Vec::new());
            }
            return Err(err);
        }
        let mut out = Vec::new();
        loop {
            if let Some(parsed) = decode(&data) {
                out.push(parsed);
            }
            let more = unsafe {
                FindNextStreamW(handle, &mut data as *mut WIN32_FIND_STREAM_DATA as *mut _)
            };
            if more == 0 {
                let code = unsafe { GetLastError() };
                unsafe { FindClose(handle) };
                if code != 0 && code != ERROR_HANDLE_EOF {
                    return Err(std::io::Error::from_raw_os_error(code as i32));
                }
                return Ok(out);
            }
        }
    }

    fn decode(data: &WIN32_FIND_STREAM_DATA) -> Option<(String, u64)> {
        let end = data
            .cStreamName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(data.cStreamName.len());
        let raw = OsString::from_wide(&data.cStreamName[..end]);
        let raw = raw.to_string_lossy().into_owned();
        let byte_len = u64::try_from(data.StreamSize.max(0)).unwrap_or(0);
        Some((raw, byte_len))
    }

    pub fn move_file_ex(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
        let mut from_wide: Vec<u16> = from.as_os_str().encode_wide().collect();
        from_wide.push(0);
        let mut to_wide: Vec<u16> = to.as_os_str().encode_wide().collect();
        to_wide.push(0);
        // SAFETY: both buffers are valid, null-terminated UTF-16, and live for the call.
        let moved = unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) };
        if moved == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// `MOVEFILE_REPLACE_EXISTING`.
#[cfg(windows)]
pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
/// `MOVEFILE_WRITE_THROUGH`.
#[cfg(windows)]
pub const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

/// `MoveFileExW`. `flags` is a combination of [`MOVEFILE_REPLACE_EXISTING`] and
/// [`MOVEFILE_WRITE_THROUGH`].
#[cfg(windows)]
pub fn move_file_ex(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    sys::move_file_ex(from, to, flags)
}

#[cfg(windows)]
pub fn raw_stream_names(path: &Path) -> std::io::Result<Vec<(String, u64)>> {
    sys::raw_stream_names(path)
}

#[cfg(not(windows))]
pub fn raw_stream_names(path: &Path) -> std::io::Result<Vec<(String, u64)>> {
    let _ = path;
    Ok(Vec::new())
}
