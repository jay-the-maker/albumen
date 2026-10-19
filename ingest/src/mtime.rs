// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::IngestError;

pub(crate) fn set_mtime(path: &Path, mtime: SystemTime) -> Result<(), IngestError> {
    crate::dest::set_mtime(path, mtime)
}

pub(crate) fn mtime_of(path: &Path) -> Result<SystemTime, IngestError> {
    std::fs::metadata(path)
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
