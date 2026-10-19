// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::IngestError;

/// The modification time this filesystem can store.
///
/// NTFS ticks are 100 nanoseconds. `set_mtime` writes that value and the
/// copy check requires it back. Linux and macOS keep the full `SystemTime`.
pub(crate) fn representable_mtime(mtime: SystemTime) -> SystemTime {
    #[cfg(not(windows))]
    {
        mtime
    }
    #[cfg(windows)]
    {
        const TICK: i128 = 100;
        let nanos = to_unix_nanos(mtime);
        let coarse = if nanos >= 0 {
            nanos / TICK * TICK
        } else {
            -(((-nanos) + (TICK - 1)) / TICK * TICK)
        };
        system_time_from_unix_nanos(coarse).unwrap_or(mtime)
    }
}

pub(crate) fn set_mtime(path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    crate::dest::set_mtime(path, representable_mtime(mtime))
}

pub(crate) use crate::platform::mtime_matches;

pub(crate) fn mtime_of(path: &Path) -> Result<SystemTime, IngestError> {
    std::fs::metadata(crate::platform::io_path(path))
        .and_then(|meta| meta.modified())
        .map_err(|err| IngestError::io(path, err))
}

pub(crate) fn to_unix_nanos(mtime: SystemTime) -> i128 {
    match mtime.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
        }
        Err(err) => {
            let duration = err.duration();
            let nanos = i128::from(duration.as_secs()) * 1_000_000_000
                + i128::from(duration.subsec_nanos());
            -nanos
        }
    }
}

pub(crate) fn to_unix_nanos_string(mtime: SystemTime) -> String {
    to_unix_nanos(mtime).to_string()
}

#[cfg(windows)]
fn system_time_from_unix_nanos(nanos: i128) -> Option<SystemTime> {
    let negative = nanos < 0;
    let nanos = nanos.unsigned_abs();
    let secs = u64::try_from(nanos / 1_000_000_000).ok()?;
    let sub = u32::try_from(nanos % 1_000_000_000).ok()?;
    let duration = Duration::new(secs, sub);
    if negative {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    }
}

pub(crate) fn from_unix_nanos_string(
    value: &str,
    filename: &str,
) -> Result<SystemTime, IngestError> {
    let nanos: i128 = value.parse().map_err(|_| IngestError::Store {
        operation: "parse mtime",
        filename: filename.to_string(),
        message: format!("invalid albumen-mtime-ns value {value:?}"),
    })?;
    let negative = nanos < 0;
    let nanos = nanos.unsigned_abs();
    let secs = u64::try_from(nanos / 1_000_000_000).map_err(|_| IngestError::Store {
        operation: "parse mtime",
        filename: filename.to_string(),
        message: format!("mtime {value:?} is out of range"),
    })?;
    let sub = u32::try_from(nanos % 1_000_000_000).expect("nanos modulo 1e9 fits u32");
    let duration = Duration::new(secs, sub);
    if negative {
        UNIX_EPOCH
            .checked_sub(duration)
            .ok_or_else(|| IngestError::Store {
                operation: "parse mtime",
                filename: filename.to_string(),
                message: format!("mtime {value:?} is out of range"),
            })
    } else {
        Ok(UNIX_EPOCH + duration)
    }
}

pub(crate) fn sync_file(file: &File, path: &Path) -> Result<(), IngestError> {
    file.sync_all().map_err(|err| IngestError::io(path, err))
}
