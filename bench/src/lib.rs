// SPDX-License-Identifier: Apache-2.0

//! Repeated-sample gate for the paths that exist today.
//!
//! This is a criterion-style harness: several repeats, a median, and a stored
//! baseline. A noisy repeat is dropped before the median. The criterion crate
//! is not a dependency.
//!
//! GitHub-hosted runners differ by more than 10 percent from job to job, so
//! the gate does not compare raw MB/s or milliseconds with a laptop. Each
//! repeat also times a same-run reference that does the same kind of work:
//! hash while copying, fsync, and hash the destination; one ExifTool read;
//! and the same directory walk search and browse use. Throughput is Albumen's MB/s
//! divided by the reference copy. Latency is the reference time divided by
//! Albumen's time. Higher is better, and the score fails when it drops by
//! more than 10 percent. Raw MB/s and milliseconds are still reported.

#![forbid(unsafe_code)]

mod takeout_scale;

use std::fs::{self, File};
use std::hint::black_box;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use albumen_ingest::{
    copy_verify, exiftool_command, sha256_hex, Asset, AssetFile, DirectoryTree, ExportDest,
    LocalStore, Role,
};
use albumen_mcp::Library;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Repeats of a whole measurement. The stored score is the median.
pub const REPEATS: usize = 5;
/// Timed calls inside one warm block. p50 and p95 come from these.
pub const WARM_SAMPLES: usize = 20;
/// Drop the worst sample when `(max - min) / median` is above this.
pub const NOISE_SPREAD: f64 = 0.25;
/// Fail when the median moves by more than this fraction.
pub const REGRESSION: f64 = 0.10;

pub use takeout_scale::{
    measure_takeout, TakeoutScale, DEFAULT_TAKEOUT_ITEMS, TAKEOUT_BUDGET_SECONDS,
};

pub const DEFAULT_SEARCH_ITEMS: u64 = 128;
pub const DEFAULT_COPY_FILES: u64 = 8;
pub const DEFAULT_COPY_BYTES: u64 = 4 * 1024 * 1024;

const CAPTION: &str = "bench-caption";
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub search_items: u64,
    pub copy_files: u64,
    pub copy_bytes_each: u64,
    pub copy_verify_mb_s: f64,
    pub copy_verify_items_s: f64,
    pub search_cold_ms: f64,
    pub search_warm_p50_ms: f64,
    pub search_warm_p95_ms: f64,
    pub mcp_browse_p50_ms: f64,
    /// `copy_verify` MB/s divided by a plain copy of the same bytes.
    pub copy_verify_vs_plain: f64,
    /// ExifTool's own read time divided by cold search time.
    pub search_cold_vs_exif: f64,
    /// Stat-and-hash pass divided by warm search p50.
    pub search_warm_p50_vs_hash: f64,
    /// Stat-and-hash pass divided by warm search p95.
    pub search_warm_p95_vs_hash: f64,
    /// Directory walk divided by browse p50.
    pub browse_vs_walk: f64,
}

/// More than 10 percent worse. The boundary is exact at thousandths.
pub fn regressed(higher_is_better: bool, baseline: f64, observed: f64) -> bool {
    if !(baseline.is_finite() && baseline > 0.0 && observed.is_finite() && observed >= 0.0) {
        return true;
    }
    let base = (baseline * 1000.0).round() as i128;
    let seen = (observed * 1000.0).round() as i128;
    if higher_is_better {
        seen * 100 < base * 90
    } else {
        seen * 100 > base * 110
    }
}

pub fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Nearest-rank percentile. `samples` must be sorted ascending and non-empty.
pub fn percentile(sorted: &[f64], pct: f64) -> f64 {
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Median after dropping the worst sample when the spread is too wide.
pub fn settle(samples: &[f64], higher_is_better: bool) -> Result<f64, String> {
    if samples.is_empty() {
        return Err("a benchmark repeat produced no samples".to_string());
    }
    if samples
        .iter()
        .any(|sample| !sample.is_finite() || *sample < 0.0)
    {
        return Err("a benchmark sample was not a finite non-negative number".to_string());
    }
    let mut values = samples.to_vec();
    let med = median(&values);
    if values.len() >= 3 && med > 0.0 {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if (max - min) / med > NOISE_SPREAD {
            let index = if higher_is_better {
                values
                    .iter()
                    .enumerate()
                    .min_by(|(_, left), (_, right)| left.total_cmp(right))
                    .map(|(index, _)| index)
            } else {
                values
                    .iter()
                    .enumerate()
                    .max_by(|(_, left), (_, right)| left.total_cmp(right))
                    .map(|(index, _)| index)
            };
            if let Some(index) = index {
                values.remove(index);
            }
        }
    }
    Ok(median(&values))
}

pub fn scale_problems(baseline: &Report, observed: &Report) -> Vec<String> {
    let mut problems = Vec::new();
    if baseline.search_items != observed.search_items {
        problems.push(format!(
            "search_items is {} but the baseline was recorded at {}. Refusing to compare a different scale.",
            observed.search_items, baseline.search_items
        ));
    }
    if baseline.copy_files != observed.copy_files {
        problems.push(format!(
            "copy_files is {} but the baseline was recorded at {}. Refusing to compare a different scale.",
            observed.copy_files, baseline.copy_files
        ));
    }
    if baseline.copy_bytes_each != observed.copy_bytes_each {
        problems.push(format!(
            "copy_bytes_each is {} but the baseline was recorded at {}. Refusing to compare a different scale.",
            observed.copy_bytes_each, baseline.copy_bytes_each
        ));
    }
    problems
}

pub fn regression_problems(baseline: &Report, observed: &Report) -> Vec<String> {
    let mut problems = scale_problems(baseline, observed);
    for (name, higher_is_better, base, seen) in metric_rows(baseline, observed) {
        if regressed(higher_is_better, base, seen) {
            let direction = if higher_is_better {
                "higher is better"
            } else {
                "lower is better"
            };
            problems.push(format!(
                "{name} regressed by more than 10 percent ({direction}): baseline {base}, observed {seen}"
            ));
        }
    }
    problems
}

pub fn to_json(report: &Report) -> Value {
    json!({
        "search_items": report.search_items,
        "copy_files": report.copy_files,
        "copy_bytes_each": report.copy_bytes_each,
        "copy_verify_mb_s": report.copy_verify_mb_s,
        "copy_verify_items_s": report.copy_verify_items_s,
        "search_cold_ms": report.search_cold_ms,
        "search_warm_p50_ms": report.search_warm_p50_ms,
        "search_warm_p95_ms": report.search_warm_p95_ms,
        "mcp_browse_p50_ms": report.mcp_browse_p50_ms,
        "copy_verify_vs_plain": report.copy_verify_vs_plain,
        "search_cold_vs_exif": report.search_cold_vs_exif,
        "search_warm_p50_vs_hash": report.search_warm_p50_vs_hash,
        "search_warm_p95_vs_hash": report.search_warm_p95_vs_hash,
        "browse_vs_walk": report.browse_vs_walk,
    })
}

pub fn from_json(value: &Value) -> Result<Report, String> {
    Ok(Report {
        search_items: required_u64(value, "search_items")?,
        copy_files: required_u64(value, "copy_files")?,
        copy_bytes_each: required_u64(value, "copy_bytes_each")?,
        copy_verify_mb_s: required_f64(value, "copy_verify_mb_s")?,
        copy_verify_items_s: required_f64(value, "copy_verify_items_s")?,
        search_cold_ms: required_f64(value, "search_cold_ms")?,
        search_warm_p50_ms: required_f64(value, "search_warm_p50_ms")?,
        search_warm_p95_ms: required_f64(value, "search_warm_p95_ms")?,
        mcp_browse_p50_ms: required_f64(value, "mcp_browse_p50_ms")?,
        copy_verify_vs_plain: required_f64(value, "copy_verify_vs_plain")?,
        search_cold_vs_exif: required_f64(value, "search_cold_vs_exif")?,
        search_warm_p50_vs_hash: required_f64(value, "search_warm_p50_vs_hash")?,
        search_warm_p95_vs_hash: required_f64(value, "search_warm_p95_vs_hash")?,
        browse_vs_walk: required_f64(value, "browse_vs_walk")?,
    })
}

pub fn load_baseline(path: &Path) -> Result<Report, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|err| format!("parse {}: {err}", path.display()))?;
    from_json(&value)
}

/// Generate a synthetic library, copy through `copy_verify`, and time search
/// and browse. Sources are removed only when this temporary directory drops.
/// Destinations are directories this run just created.
pub fn measure(search_items: u64, copy_files: u64, copy_bytes_each: u64) -> Result<Report, String> {
    if search_items == 0 || copy_files == 0 || copy_bytes_each == 0 {
        return Err("search items, copy files, and copy size must be at least 1".to_string());
    }
    require_exiftool()?;
    let root = TempDir::new()?;
    let library = root.path.join("library");
    let sources = root.path.join("sources");
    fs::create_dir_all(&library).map_err(|err| format!("create library: {err}"))?;
    fs::create_dir_all(&sources).map_err(|err| format!("create sources: {err}"))?;
    let relative = write_library(&library, search_items)?;
    let copies = write_copies(&sources, copy_files, copy_bytes_each)?;

    let mut mb_s = Vec::with_capacity(REPEATS);
    let mut items_s = Vec::with_capacity(REPEATS);
    let mut cold = Vec::with_capacity(REPEATS);
    let mut warm_p50 = Vec::with_capacity(REPEATS);
    let mut warm_p95 = Vec::with_capacity(REPEATS);
    let mut browse_p50 = Vec::with_capacity(REPEATS);
    let mut copy_ratio = Vec::with_capacity(REPEATS);
    let mut cold_ratio = Vec::with_capacity(REPEATS);
    let mut warm_p50_ratio = Vec::with_capacity(REPEATS);
    let mut warm_p95_ratio = Vec::with_capacity(REPEATS);
    let mut browse_ratio = Vec::with_capacity(REPEATS);
    // Ask for the whole synthetic library. A page of one would stop at the
    // first match, and this job measures a full read against a full-read reference.
    let search_args = json!({"caption": CAPTION, "limit": search_items.saturating_mul(2)});
    let browse_args = json!({"limit": search_items.saturating_mul(2)});

    for repeat in 0..REPEATS {
        let plain = plain_copy_mb_s(&copies, &root.path.join(format!("plain-{repeat}")))?;
        let (mb, items) = copy_once(&copies, &root.path.join(format!("dest-{repeat}")))?;
        mb_s.push(mb);
        items_s.push(items);
        copy_ratio.push(ratio(mb, plain)?);

        let library_handle = Library::new(library.clone(), false, false)?;
        let (value, ms) = timed(|| library_handle.call("search", &search_args));
        assert_hit(&value?, "cold search")?;
        cold.push(ms);
        let exif_ms = exif_pass_ms(&library, &relative)?;
        cold_ratio.push(ratio(exif_ms, ms)?);

        let (p50, p95) = warm_search(&library_handle, &search_args)?;
        warm_p50.push(p50);
        warm_p95.push(p95);
        let hash_ms = search_reference_ms(&library, &relative)?;
        warm_p50_ratio.push(ratio(hash_ms, p50)?);
        warm_p95_ratio.push(ratio(hash_ms, p95)?);

        let browse_ms = warm_browse(&library_handle, &browse_args)?;
        browse_p50.push(browse_ms);
        let walk_ms = browse_reference_ms(&library)?;
        browse_ratio.push(ratio(walk_ms, browse_ms)?);
    }

    Ok(Report {
        search_items,
        copy_files,
        copy_bytes_each,
        copy_verify_mb_s: settle(&mb_s, true)?,
        copy_verify_items_s: settle(&items_s, true)?,
        search_cold_ms: settle(&cold, false)?,
        search_warm_p50_ms: settle(&warm_p50, false)?,
        search_warm_p95_ms: settle(&warm_p95, false)?,
        mcp_browse_p50_ms: settle(&browse_p50, false)?,
        copy_verify_vs_plain: settle(&copy_ratio, true)?,
        search_cold_vs_exif: settle(&cold_ratio, true)?,
        search_warm_p50_vs_hash: settle(&warm_p50_ratio, true)?,
        search_warm_p95_vs_hash: settle(&warm_p95_ratio, true)?,
        browse_vs_walk: settle(&browse_ratio, true)?,
    })
}

fn ratio(reference: f64, observed: f64) -> Result<f64, String> {
    if !(reference.is_finite() && reference > 0.0 && observed.is_finite() && observed > 0.0) {
        return Err("a reference or a measurement was not a positive number".to_string());
    }
    Ok(reference / observed)
}

fn metric_rows(baseline: &Report, observed: &Report) -> [(&'static str, bool, f64, f64); 3] {
    // Warm p50/p95 ratios are printed and stored. They are not a failure
    // line: on ubuntu-24.04 that ratio has been both about 0.13 and about
    // 0.61 for the same benchmark. Copy, cold search, and browse stay inside
    // 10 percent across those runners.
    [
        (
            "copy_verify_vs_plain",
            true,
            baseline.copy_verify_vs_plain,
            observed.copy_verify_vs_plain,
        ),
        (
            "search_cold_vs_exif",
            true,
            baseline.search_cold_vs_exif,
            observed.search_cold_vs_exif,
        ),
        (
            "browse_vs_walk",
            true,
            baseline.browse_vs_walk,
            observed.browse_vs_walk,
        ),
    ]
}

fn warm_search(library: &Library, args: &Value) -> Result<(f64, f64), String> {
    let mut samples = Vec::with_capacity(WARM_SAMPLES);
    for _ in 0..WARM_SAMPLES {
        let (value, ms) = timed(|| library.call("search", args));
        assert_hit(&value?, "warm search")?;
        samples.push(ms);
    }
    samples.sort_by(f64::total_cmp);
    Ok((percentile(&samples, 50.0), percentile(&samples, 95.0)))
}

fn warm_browse(library: &Library, args: &Value) -> Result<f64, String> {
    let mut samples = Vec::with_capacity(WARM_SAMPLES);
    for _ in 0..WARM_SAMPLES {
        let (value, ms) = timed(|| library.call("browse", args));
        assert_hit(&value?, "browse")?;
        samples.push(ms);
    }
    samples.sort_by(f64::total_cmp);
    Ok(percentile(&samples, 50.0))
}

fn assert_hit(value: &Value, tool: &str) -> Result<(), String> {
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{tool} returned no file list"))?;
    if files.is_empty() {
        return Err(format!(
            "{tool} returned no files; the synthetic library was not read"
        ));
    }
    Ok(())
}

fn plain_copy_mb_s(sources: &[PathBuf], dest: &Path) -> Result<f64, String> {
    fs::create_dir_all(dest).map_err(|err| format!("create plain dest: {err}"))?;
    let started = Instant::now();
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| format!("missing file name {}", source.display()))?;
        let target = dest.join(name);
        let mut input =
            File::open(source).map_err(|err| format!("open {}: {err}", source.display()))?;
        let mut output = File::create(&target)
            .map_err(|err| format!("create plain copy of {}: {err}", source.display()))?;
        let mut hasher = Sha256::new();
        let copied = hash_while_copying(&mut input, &mut output, &mut hasher, &mut buffer)
            .map_err(|err| format!("plain copy {}: {err}", source.display()))?;
        output
            .sync_all()
            .map_err(|err| format!("sync plain copy of {}: {err}", source.display()))?;
        black_box(hasher.finalize());
        let written = sha256_hex(&target).map_err(|err| err.to_string())?;
        black_box(written);
        bytes += copied;
    }
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    Ok((bytes as f64) / 1_000_000.0 / seconds)
}

fn hash_while_copying(
    input: &mut File,
    output: &mut File,
    hasher: &mut Sha256,
    buffer: &mut [u8],
) -> Result<u64, std::io::Error> {
    let mut copied = 0u64;
    loop {
        let read = input.read(buffer)?;
        if read == 0 {
            return Ok(copied);
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        copied += read as u64;
    }
}

fn search_reference_ms(root: &Path, _relative: &[String]) -> Result<f64, String> {
    let started = Instant::now();
    let listed = DirectoryTree::new(root)
        .list()
        .map_err(|err| err.to_string())?;
    let mut lines = Vec::with_capacity(listed.len());
    for name in &listed {
        let path = root.join(name);
        let len = fs::metadata(&path)
            .map_err(|err| format!("stat {name}: {err}"))?
            .len();
        let digest = sha256_hex(&path).map_err(|err| err.to_string())?;
        lines.push(json!({
            "path": name,
            "bytes": len,
            "sha256": digest,
            "captions": [CAPTION],
        }));
    }
    black_box(lines);
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn browse_reference_ms(root: &Path) -> Result<f64, String> {
    let started = Instant::now();
    let listed = DirectoryTree::new(root)
        .list()
        .map_err(|err| err.to_string())?;
    let mut lines = Vec::with_capacity(listed.len());
    for name in listed {
        let len = fs::metadata(root.join(&name))
            .map_err(|err| format!("stat {name}: {err}"))?
            .len();
        lines.push(json!({
            "path": name,
            "bytes": len,
        }));
    }
    black_box(lines);
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn exif_pass_ms(root: &Path, relative: &[String]) -> Result<f64, String> {
    let started = Instant::now();
    for chunk in relative.chunks(albumen_mcp::metadata::exiftool_batch()) {
        let output = exiftool_command()
            .current_dir(root)
            .arg("-json")
            .arg("-n")
            .arg("-G1")
            .arg("-s")
            .arg("-api")
            .arg("largefilesupport=1")
            .args(chunk)
            .output()
            .map_err(|err| format!("exiftool reference: {err}"))?;
        if !output.status.success() {
            return Err(format!(
                "exiftool reference failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        black_box(output.stdout.len());
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

fn copy_once(sources: &[PathBuf], dest: &Path) -> Result<(f64, f64), String> {
    fs::create_dir_all(dest).map_err(|err| format!("create dest: {err}"))?;
    let started = Instant::now();
    let mut bytes = 0u64;
    let mut count = 0u64;
    for source in sources {
        let asset = Asset::from_files(vec![AssetFile {
            path: source.clone(),
            role: Role::Primary,
        }])
        .map_err(|err| err.to_string())?;
        let report = copy_verify(&asset, &LocalStore::new(dest)).map_err(|err| err.to_string())?;
        black_box(&report);
        for file in &report.files {
            bytes += file.byte_len;
            count += 1;
        }
    }
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    let mb_s = (bytes as f64) / 1_000_000.0 / seconds;
    let items_s = (count as f64) / seconds;
    Ok((mb_s, items_s))
}

fn write_library(dir: &Path, items: u64) -> Result<Vec<String>, String> {
    let sidecar = xmp(CAPTION);
    let mut relative = Vec::with_capacity(usize::try_from(items).unwrap_or(0) * 2);
    for index in 0..items {
        let stem = format!("b{index:06}");
        let jpeg = format!("{stem}.jpg");
        let xmp_name = format!("{stem}.xmp");
        fs::write(dir.join(&jpeg), JPEG).map_err(|err| format!("write {jpeg}: {err}"))?;
        fs::write(dir.join(&xmp_name), sidecar.as_bytes())
            .map_err(|err| format!("write {xmp_name}: {err}"))?;
        relative.push(jpeg);
        relative.push(xmp_name);
    }
    Ok(relative)
}

fn write_copies(dir: &Path, files: u64, bytes_each: u64) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::with_capacity(usize::try_from(files).unwrap_or(0));
    let mut chunk = vec![0u8; 1024 * 1024];
    for index in 0..files {
        let path = dir.join(format!("c{index:04}.bin"));
        let mut file =
            File::create(&path).map_err(|err| format!("create {}: {err}", path.display()))?;
        let mut remaining = bytes_each;
        let mut offset = 0u64;
        while remaining > 0 {
            let n = usize::try_from(remaining.min(chunk.len() as u64)).unwrap_or(chunk.len());
            for (index, byte) in chunk[..n].iter_mut().enumerate() {
                *byte = ((offset + index as u64) % 251) as u8;
            }
            file.write_all(&chunk[..n])
                .map_err(|err| format!("write {}: {err}", path.display()))?;
            remaining -= n as u64;
            offset += n as u64;
        }
        file.sync_all()
            .map_err(|err| format!("sync {}: {err}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

fn xmp(caption: &str) -> String {
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">{caption}</rdf:li></rdf:Alt></dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#
    )
}

fn require_exiftool() -> Result<(), String> {
    let output = exiftool_command().arg("-ver").output().map_err(|err| {
        format!(
            "exiftool is required for the search benchmark ({err}). Run scripts/install-exiftool.sh and put it on PATH."
        )
    })?;
    if !output.status.success() {
        return Err("exiftool -ver failed".to_string());
    }
    Ok(())
}

fn timed<T>(body: impl FnOnce() -> T) -> (T, f64) {
    let started = Instant::now();
    let value = body();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    (black_box(value), ms)
}

fn required_u64(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("baseline {key} must be an integer"))
}

fn required_f64(value: &Value, key: &str) -> Result<f64, String> {
    let number = value
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("baseline {key} must be a number"))?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!(
            "baseline {key} must be a finite non-negative number"
        ));
    }
    Ok(number)
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
        let path =
            std::env::temp_dir().join(format!("albumen-bench-{nanos}-{}", std::process::id()));
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
    fn ten_percent_is_the_failure_line() {
        assert!(!regressed(true, 100.0, 90.0));
        assert!(regressed(true, 100.0, 89.9));
        assert!(!regressed(false, 100.0, 110.0));
        assert!(regressed(false, 100.0, 110.1));
        assert!(regressed(true, 0.0, 1.0));
    }

    #[test]
    fn a_noisy_repeat_drops_the_worst_sample() {
        let throughput = [80.0, 80.0, 80.0, 100.0, 50.0];
        assert_eq!(settle(&throughput, true).unwrap(), 80.0);
        let latency = [10.0, 10.0, 10.0, 12.0, 200.0];
        assert_eq!(settle(&latency, false).unwrap(), 10.0);
    }

    #[test]
    fn a_tight_spread_keeps_every_sample() {
        let samples = [100.0, 102.0, 101.0, 103.0, 104.0];
        assert_eq!(settle(&samples, true).unwrap(), 102.0);
    }

    #[test]
    fn nearest_rank_picks_the_upper_sample() {
        let samples: Vec<f64> = (1..=20).map(|n| n as f64).collect();
        assert_eq!(percentile(&samples, 50.0), 10.0);
        assert_eq!(percentile(&samples, 95.0), 19.0);
    }

    #[test]
    fn a_different_item_count_is_not_compared() {
        let baseline = sample_report(128);
        let mut observed = sample_report(128);
        observed.search_items = 380_000;
        let problems = scale_problems(&baseline, &observed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("380000"), "{problems:?}");
    }

    #[test]
    fn json_round_trip_keeps_the_scale() {
        let report = sample_report(128);
        let parsed = from_json(&to_json(&report)).unwrap();
        assert_eq!(parsed, report);
    }

    #[test]
    fn committed_baseline_uses_the_ci_scale() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json");
        let baseline = load_baseline(&path).unwrap();
        assert_eq!(baseline.search_items, DEFAULT_SEARCH_ITEMS);
        assert_eq!(baseline.copy_files, DEFAULT_COPY_FILES);
        assert_eq!(baseline.copy_bytes_each, DEFAULT_COPY_BYTES);
        assert!(regression_problems(&baseline, &baseline).is_empty());
    }

    fn sample_report(items: u64) -> Report {
        Report {
            search_items: items,
            copy_files: DEFAULT_COPY_FILES,
            copy_bytes_each: DEFAULT_COPY_BYTES,
            copy_verify_mb_s: 100.0,
            copy_verify_items_s: 20.0,
            search_cold_ms: 40.0,
            search_warm_p50_ms: 5.0,
            search_warm_p95_ms: 8.0,
            mcp_browse_p50_ms: 1.0,
            copy_verify_vs_plain: 0.5,
            search_cold_vs_exif: 0.8,
            search_warm_p50_vs_hash: 0.9,
            search_warm_p95_vs_hash: 0.7,
            browse_vs_walk: 0.4,
        }
    }
}
