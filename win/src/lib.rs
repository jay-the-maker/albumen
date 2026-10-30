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

/// User plus kernel CPU time this process has used, from `GetProcessTimes`.
#[cfg(windows)]
pub fn process_cpu_time() -> std::io::Result<std::time::Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    // FILETIME counts 100-nanosecond intervals.
    Ok(std::time::Duration::from_nanos(
        (ticks(kernel) + ticks(user)).saturating_mul(100),
    ))
}

/// The 8.3 short form of an existing path, from `GetShortPathNameW`. A
/// volume with 8.3 names turned off returns the long path unchanged.
#[cfg(windows)]
pub fn short_path(path: &Path) -> std::io::Result<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut out = vec![0u16; 1024];
    loop {
        let len = unsafe { GetShortPathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let len = len as usize;
        if len < out.len() {
            out.truncate(len);
            return Ok(OsString::from_wide(&out).into());
        }
        out.resize(len + 1, 0);
    }
}

/// User plus kernel CPU time the calling thread has used, from `GetThreadTimes`.
#[cfg(windows)]
pub fn thread_cpu_time() -> std::io::Result<std::time::Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    // FILETIME counts 100-nanosecond intervals.
    Ok(std::time::Duration::from_nanos(
        (ticks(kernel) + ticks(user)).saturating_mul(100),
    ))
}

/// User plus system CPU time the calling thread has used,
/// from `CLOCK_THREAD_CPUTIME_ID`.
#[cfg(unix)]
pub fn thread_cpu_time() -> std::io::Result<std::time::Duration> {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Ok(std::time::Duration::new(
        u64::try_from(time.tv_sec).unwrap_or(0),
        u32::try_from(time.tv_nsec).unwrap_or(0),
    ))
}

#[cfg(not(windows))]
pub fn raw_stream_names(path: &Path) -> std::io::Result<Vec<(String, u64)>> {
    let _ = path;
    Ok(Vec::new())
}

/// Ask macOS not to keep this file's written pages in the buffer cache
/// (`F_NOCACHE`). Writes then go to the device without the extra copy into
/// cached pages. It is only a cache hint: the bytes, the fsync, and a later
/// read are unchanged. Elsewhere it does nothing.
pub fn uncached_writes(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        rustix::fs::fcntl_nocache(file, true)?;
    }
    let _ = file;
    Ok(())
}

/// Reserve disk blocks for `len` bytes at the end of a new, empty file
/// (macOS `F_PREALLOCATE`, contiguous first, then any blocks). It allocates
/// only and never changes the file's logical size. Elsewhere it does
/// nothing. An error means nothing was reserved; the caller writes as usual.
static RESERVE_CONTIG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESERVE_ANY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESERVE_FAILED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESERVE_LAST_ERRNO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// What [`reserve_blocks`] did so far in this process: calls that got
/// contiguous blocks, calls that got any blocks, failed calls, and the last
/// failure's errno (0 for none).
pub fn reserve_stats() -> (u64, u64, u64, i32) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        RESERVE_CONTIG.load(Relaxed),
        RESERVE_ANY.load(Relaxed),
        RESERVE_FAILED.load(Relaxed),
        RESERVE_LAST_ERRNO.load(Relaxed),
    )
}

pub fn reserve_blocks(file: &std::fs::File, len: u64) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        #[repr(C)]
        struct Fstore {
            flags: u32,
            posmode: std::ffi::c_int,
            offset: i64,
            length: i64,
            bytesalloc: i64,
        }
        extern "C" {
            fn fcntl(fd: std::ffi::c_int, cmd: std::ffi::c_int, ...) -> std::ffi::c_int;
        }
        const F_PREALLOCATE: std::ffi::c_int = 42;
        const F_ALLOCATECONTIG: u32 = 0x2;
        const F_ALLOCATEALL: u32 = 0x4;
        const F_PEOFPOSMODE: std::ffi::c_int = 3;
        let length = i64::try_from(len).map_err(|_| std::io::Error::other("length too large"))?;
        let fd = file.as_raw_fd();
        for flags in [F_ALLOCATECONTIG | F_ALLOCATEALL, F_ALLOCATEALL] {
            let mut store = Fstore {
                flags,
                posmode: F_PEOFPOSMODE,
                offset: 0,
                length,
                bytesalloc: 0,
            };
            // SAFETY: `fd` is an open descriptor owned by `file`, and
            // `store` is a valid fstore_t for the duration of the call.
            let rc = unsafe { fcntl(fd, F_PREALLOCATE, &mut store as *mut Fstore) };
            if rc != -1 {
                let counter = if flags & F_ALLOCATECONTIG != 0 {
                    &RESERVE_CONTIG
                } else {
                    &RESERVE_ANY
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
        }
        let err = std::io::Error::last_os_error();
        RESERVE_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RESERVE_LAST_ERRNO.store(
            err.raw_os_error().unwrap_or(-1),
            std::sync::atomic::Ordering::Relaxed,
        );
        Err(err)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = (file, len);
        Ok(())
    }
}

/// Minutes east of UTC that this computer's zone applies at `unix` seconds,
/// or `None` if the OS cannot say. Linux and macOS: `tzset` then
/// `localtime_r`'s `tm_gmtoff`, so `TZ` is honoured. Windows: the OS zone and
/// that year's DST rules via `SystemTimeToTzSpecificLocalTimeEx`; `TZ` has
/// no effect there.
#[cfg(unix)]
pub fn utc_offset_minutes(unix: i64) -> Option<i64> {
    extern "C" {
        fn tzset();
    }
    let t: libc::time_t = unix.try_into().ok()?;
    // SAFETY: tzset takes no arguments; localtime_r writes only into `tm`,
    // which we own, and reports failure with a null return.
    let tm = unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    let minutes = i64::from(tm.tm_gmtoff) / 60;
    (minutes.abs() <= 14 * 60).then_some(minutes)
}

/// See the unix version.
#[cfg(windows)]
pub fn utc_offset_minutes(unix: i64) -> Option<i64> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{
        GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx,
        DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
    };
    let (year, month, day, secs) = civil(unix);
    let utc = SYSTEMTIME {
        wYear: u16::try_from(year).ok()?,
        wMonth: month as u16,
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: (secs / 3_600) as u16,
        wMinute: ((secs % 3_600) / 60) as u16,
        wSecond: (secs % 60) as u16,
        wMilliseconds: 0,
    };
    // SAFETY: both structs are plain data we own; the calls only write into
    // them and report failure through their return values.
    let local = unsafe {
        let mut zone: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut zone) == TIME_ZONE_ID_INVALID {
            return None;
        }
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTimeEx(&zone, &utc, &mut local) == 0 {
            return None;
        }
        local
    };
    let local_unix = days_from_civil(
        i64::from(local.wYear),
        i64::from(local.wMonth),
        i64::from(local.wDay),
    ) * 86_400
        + i64::from(local.wHour) * 3_600
        + i64::from(local.wMinute) * 60
        + i64::from(local.wSecond);
    let minutes = (local_unix - unix).div_euclid(60);
    (minutes.abs() <= 14 * 60).then_some(minutes)
}

#[cfg(not(any(unix, windows)))]
pub fn utc_offset_minutes(unix: i64) -> Option<i64> {
    let _ = unix;
    None
}

/// (year, month, day, seconds into the day) for Unix seconds (Hinnant).
#[cfg(windows)]
fn civil(unix: i64) -> (i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day, secs)
}

/// Days since 1970-01-01 for a civil date (Hinnant).
#[cfg(windows)]
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
