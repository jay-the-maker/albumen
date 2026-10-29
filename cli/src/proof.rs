// SPDX-License-Identifier: Apache-2.0

//! `verify-manifest --proof`: a Markdown + JSON pair the user can keep.
//!
//! Both files are created new; an existing file is never overwritten. The
//! combined hash is SHA-256 over the sorted `"<sha256>  <path>\n"` lines, so
//! anyone can recompute it from the manifest with standard tools.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use albumen_ingest::{dest, sha256_hex, IngestError};
use serde_json::json;
use sha2::{Digest, Sha256};

pub struct ProofInput<'a> {
    pub manifest: &'a Path,
    pub against: &'a str,
    pub started: SystemTime,
    pub finished: SystemTime,
    /// (path, sha256, bytes) from the manifest.
    pub files: Vec<(String, String, u64)>,
    /// (kind, path, detail).
    pub drift: Vec<(String, String, String)>,
}

/// Paths for the pair: `<path>.md` and `<path>.json`, with any `.md` or
/// `.json` extension on `path` replaced.
pub fn proof_paths(path: &Path) -> (PathBuf, PathBuf) {
    let stem = match path.extension().and_then(|ext| ext.to_str()) {
        Some("md") | Some("json") => path.with_extension(""),
        _ => path.to_path_buf(),
    };
    let with = |ext: &str| {
        let mut name = stem.as_os_str().to_owned();
        name.push(ext);
        PathBuf::from(name)
    };
    (with(".md"), with(".json"))
}

pub fn write_proof(path: &Path, input: &ProofInput<'_>) -> Result<(PathBuf, PathBuf), IngestError> {
    let (md_path, json_path) = proof_paths(path);
    for target in [&md_path, &json_path] {
        if target.exists() {
            return Err(IngestError::AlreadyExists {
                filename: target.display().to_string(),
                destination: target.display().to_string(),
            });
        }
    }
    let manifest_sha = sha256_hex(input.manifest)?;
    let mut sorted: Vec<String> = input
        .files
        .iter()
        .map(|(path, sha, _)| format!("{sha}  {path}\n"))
        .collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for line in &sorted {
        hasher.update(line.as_bytes());
    }
    let combined = hex(&hasher.finalize());
    let files = input.files.len() as u64;
    let bytes: u64 = input.files.iter().map(|(_, _, len)| len).sum();
    let drifted = input.drift.len() as u64;
    let drifted_paths: std::collections::BTreeSet<&str> =
        input.drift.iter().map(|(_, p, _)| p.as_str()).collect();
    let verified = input
        .files
        .iter()
        .filter(|(p, _, _)| !drifted_paths.contains(p.as_str()))
        .count() as u64;
    let counts_line = format!("{verified} present and verified, {drifted} drifted");
    let rerun = format!(
        "albumen verify-manifest --manifest {} --against {}",
        shell_quote(&input.manifest.display().to_string()),
        shell_quote(input.against)
    );
    let version = env!("CARGO_PKG_VERSION");
    let commit = env!("ALBUMEN_GIT_COMMIT");
    let started = utc(input.started);
    let finished = utc(input.finished);
    let result = if drifted == 0 { "verified" } else { "drift" };

    let value = json!({
        "albumen_proof": 1,
        "result": result,
        "version": version,
        "commit": commit,
        "started_utc": started,
        "finished_utc": finished,
        "manifest": input.manifest.display().to_string(),
        "manifest_sha256": manifest_sha,
        "against": input.against,
        "files": files,
        "bytes": bytes,
        "verified": verified,
        "drifted": drifted,
        "counts": counts_line,
        "sorted_hashes_sha256": combined,
        "sorted_hashes_rule": "sha256 over sorted lines '<sha256>  <path>\\n'",
        "drift": input.drift.iter().map(|(kind, path, detail)| json!({"kind": kind, "path": path, "detail": detail})).collect::<Vec<_>>(),
        "rerun": rerun,
    });
    let mut md = String::new();
    md.push_str("# Albumen integrity proof\n\n");
    md.push_str(&format!("Result: **{result}**. {counts_line}.\n\n"));
    md.push_str("| Field | Value |\n|---|---|\n");
    for (field, val) in [
        ("Albumen version", version.to_string()),
        ("Commit", commit.to_string()),
        ("Started (UTC)", started.clone()),
        ("Finished (UTC)", finished.clone()),
        ("Manifest", input.manifest.display().to_string()),
        ("Manifest SHA-256", manifest_sha.clone()),
        ("Checked against", input.against.to_string()),
        ("Files", files.to_string()),
        ("Bytes", bytes.to_string()),
        ("SHA-256 of sorted per-file hashes", combined.clone()),
    ] {
        md.push_str(&format!("| {field} | `{}` |\n", val.replace('|', "\\|")));
    }
    md.push_str("\n## Drift\n\n");
    if input.drift.is_empty() {
        md.push_str("None.\n");
    } else {
        for (kind, path, detail) in &input.drift {
            md.push_str(&format!("- {kind} `{path}`: {detail}\n"));
        }
    }
    md.push_str("\n## Re-run\n\n```sh\n");
    md.push_str(&rerun);
    md.push_str("\n```\n\nThe sorted-hash value is SHA-256 over the manifest's `<sha256>  <path>` lines, sorted, each ending in a newline.\n");

    let mut json_text = serde_json::to_string_pretty(&value).unwrap_or_default();
    json_text.push('\n');
    write_new(&json_path, json_text.as_bytes())?;
    write_new(&md_path, md.as_bytes())?;
    Ok((md_path, json_path))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    let mut file = dest::open_new(path)?;
    file.write_all(bytes).map_err(|err| IngestError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    file.sync_all().map_err(|err| IngestError::Io {
        path: path.to_path_buf(),
        source: err,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:=+@".contains(c))
    {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// RFC 3339 UTC with seconds.
pub fn utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_formats_a_known_instant() {
        assert_eq!(
            utc(UNIX_EPOCH + Duration::from_secs(1_590_000_000)),
            "2020-05-20T18:40:00Z"
        );
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn proof_paths_pair() {
        let (md, js) = proof_paths(Path::new("/x/proof.json"));
        assert_eq!(md, Path::new("/x/proof.md"));
        assert_eq!(js, Path::new("/x/proof.json"));
    }
}
