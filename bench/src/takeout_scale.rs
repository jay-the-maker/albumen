// SPDX-License-Identifier: Apache-2.0

//! Synthetic Google Takeout import at a chosen item count.
//!
//! The fixture is generated: tiny JPEG bytes and tiny JSON sidecars, no real
//! photos. The timed work is `import_takeout` only. Generation is not included.
//! Reconciliation is 100 percent when `count_in` equals `count_out`, nothing
//! mismatched, and both equal the requested item count.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use albumen_ingest::import_takeout;
use serde_json::{json, Value};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Archive members in the local switching-time run.
pub const DEFAULT_TAKEOUT_ITEMS: u64 = 100_000;
/// Local budget for [`DEFAULT_TAKEOUT_ITEMS`] members.
pub const TAKEOUT_BUDGET_SECONDS: f64 = 4.0 * 60.0 * 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct TakeoutScale {
    pub items: u64,
    pub count_in: u64,
    pub count_out: u64,
    pub mismatched: u64,
    pub seconds: f64,
}

impl TakeoutScale {
    pub fn reconciled(&self) -> bool {
        self.count_in == self.items && self.count_out == self.items && self.mismatched == 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "items": self.items,
            "count_in": self.count_in,
            "count_out": self.count_out,
            "mismatched": self.mismatched,
            "reconciled": self.reconciled(),
            "seconds": self.seconds,
            "budget_seconds": TAKEOUT_BUDGET_SECONDS,
        })
    }
}

/// Build a stored zip of `items` tiny members and time one import.
///
/// Even counts are photo/JSON pairs. An odd count adds one photo. The zip is
/// deleted when this function returns.
pub fn measure_takeout(items: u64) -> Result<TakeoutScale, String> {
    if items == 0 {
        return Err("takeout item count must be greater than zero".to_string());
    }
    let root = TempDir::new()?;
    let incoming = root.path.join("takeout");
    let library = root.path.join("library");
    let report = root.path.join("report.jsonl");
    fs::create_dir(&incoming).map_err(|err| format!("create takeout dir: {err}"))?;
    let archive = incoming.join("takeout-scale.zip");
    write_fixture(&archive, items)?;

    let started = Instant::now();
    let outcome = import_takeout(&incoming, &library, &report).map_err(|err| err.to_string())?;
    let seconds = started.elapsed().as_secs_f64();
    Ok(TakeoutScale {
        items,
        count_in: outcome.count_in,
        count_out: outcome.count_out,
        mismatched: outcome.mismatched,
        seconds,
    })
}

fn write_fixture(path: &std::path::Path, items: u64) -> Result<(), String> {
    let file = File::create(path).map_err(|err| format!("create zip: {err}"))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(
            zip::DateTime::from_date_and_time(2020, 5, 20, 16, 0, 0)
                .map_err(|err| format!("zip timestamp: {err}"))?,
        );
    let pairs = items / 2;
    let width = decimal_width(pairs.max(1));
    let photo = b"\xff\xd8\xff\xd9";
    for index in 0..pairs {
        let name = format!("Takeout/Photos/p{index:0width$}.jpg");
        let json_name = format!("{name}.json");
        let json = format!(
            "{{\"title\":\"p{index:0width$}.jpg\",\"photoTakenTime\":{{\"timestamp\":\"1590000000\",\"formatted\":\"synthetic\"}}}}"
        );
        write_stored(&mut zip, &options, &name, photo)?;
        write_stored(&mut zip, &options, &json_name, json.as_bytes())?;
    }
    if items % 2 == 1 {
        let name = format!("Takeout/Photos/p{pairs:0width$}.jpg");
        write_stored(&mut zip, &options, &name, photo)?;
    }
    zip.finish().map_err(|err| format!("finish zip: {err}"))?;
    Ok(())
}

fn write_stored(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    zip.start_file(name, *options)
        .map_err(|err| format!("start {name}: {err}"))?;
    zip.write_all(bytes)
        .map_err(|err| format!("write {name}: {err}"))?;
    Ok(())
}

fn decimal_width(value: u64) -> usize {
    value
        .checked_ilog10()
        .map(|digits| digits as usize + 1)
        .unwrap_or(1)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| err.to_string())?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "albumen-takeout-scale-{nanos}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).map_err(|err| format!("create temp dir: {err}"))?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_takeout_reconciles() {
        let scale = measure_takeout(4).unwrap();
        assert!(scale.reconciled(), "{scale:?}");
        assert!(scale.seconds >= 0.0);
        assert!(scale.seconds < TAKEOUT_BUDGET_SECONDS);
    }

    #[test]
    fn zero_items_is_refused() {
        let err = measure_takeout(0).unwrap_err();
        assert!(err.contains("greater than zero"), "{err}");
    }
}
