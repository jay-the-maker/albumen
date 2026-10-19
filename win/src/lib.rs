// SPDX-License-Identifier: Apache-2.0

//! List NTFS alternate data streams.
//!
//! `albumen-ingest` forbids unsafe code. The Win32 stream enumeration lives
//! here so a copy can report every named stream and write it to the
//! destination. The default `::$DATA` stream is the file itself and is not
//! returned. A volume without stream support returns an empty list.

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
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
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
