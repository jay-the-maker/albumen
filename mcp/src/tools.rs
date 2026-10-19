// SPDX-License-Identifier: Apache-2.0

//! Library tools. Search and browse only read. Export and upload call
//! `copy_verify` and refuse to overwrite. Nothing here deletes a source or
//! renames one into quarantine.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use albumen_ingest::{
    copy_verify, discover, export_tree, walk_sorted, write_manifest, DirectoryTree, ExportDest,
    IngestError, LocalStore, S3Config, S3Store, SourceRoot, WalkControl,
};
use serde_json::{json, Value};

use crate::metadata::{self, Filters, GeoBox};
use crate::page::{Hit, Page};

pub struct Library {
    root: PathBuf,
    pub enable_export: bool,
    pub enable_upload: bool,
    pub apply: bool,
    metadata: Mutex<metadata::Cache>,
}

impl Library {
    pub fn new(
        root: PathBuf,
        enable_export: bool,
        enable_upload: bool,
        apply: bool,
    ) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("library is not a directory: {}", root.display()));
        }
        Ok(Self {
            root,
            enable_export,
            enable_upload,
            apply,
            metadata: Mutex::new(metadata::Cache::default()),
        })
    }

    pub fn tool_defs(&self) -> Vec<Value> {
        let mut tools = vec![search_def(), browse_def()];
        if self.enable_export {
            tools.push(export_def());
        }
        if self.enable_upload {
            tools.push(upload_def());
        }
        tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        tools
    }

    pub fn call(&self, name: &str, arguments: &Value) -> Result<Value, String> {
        match name {
            "search" => self.search(arguments),
            "browse" => self.browse(arguments),
            "export" => {
                if !self.enable_export {
                    return Err(
                        "export is disabled; start albumen-mcp with --enable-export".to_string()
                    );
                }
                self.export(arguments)
            }
            "upload" => {
                if !self.enable_upload {
                    return Err(
                        "upload is disabled; start albumen-mcp with --enable-upload".to_string()
                    );
                }
                self.upload(arguments)
            }
            "delete" | "remove" | "unlink" | "move" | "move_verified" => {
                Err("albumen has no delete tool".to_string())
            }
            "quarantine" | "copy_verify_quarantine" => {
                Err("copy_verify_quarantine is not a default path".to_string())
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    fn search(&self, arguments: &Value) -> Result<Value, String> {
        reject_unknown_search_keys(arguments)?;
        let query = optional_string(arguments, "query")?;
        if query.is_some_and(|query| query.is_empty()) {
            return Err("query must not be empty".to_string());
        }
        let filters = metadata_filters(arguments)?;
        if query.is_none() && filters.is_none() {
            return Err("search needs a query or a metadata filter".to_string());
        }
        let query = query.map(str::to_lowercase);
        let limit = optional_limit(arguments)?.unwrap_or(default_page("ALBUMEN_SEARCH_PAGE", 1000));
        let after = optional_cursor(arguments)?;
        let mut page = Page::default();
        let mut pending: Vec<(String, u64)> = Vec::new();
        let batch = metadata::exiftool_batch();
        let mut cache = self.metadata.lock().unwrap_or_else(|err| err.into_inner());
        let mut failed: Option<String> = None;
        let walked = walk_sorted(&self.root, after.as_deref(), &mut |relative| {
            if query
                .as_ref()
                .is_some_and(|query| !relative.to_lowercase().contains(query))
            {
                return WalkControl::Continue;
            }
            let bytes = match file_len(&self.root, relative) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failed = Some(err);
                    return WalkControl::Stop;
                }
            };
            if filters.is_none() {
                let stop = page.consider(
                    limit,
                    Hit {
                        path: relative.to_string(),
                        bytes,
                        record: None,
                    },
                );
                return if stop {
                    WalkControl::Stop
                } else {
                    WalkControl::Continue
                };
            }
            pending.push((relative.to_string(), bytes));
            if pending.len() >= batch {
                if let Err(err) = flush_metadata(
                    &self.root,
                    &mut cache,
                    filters.as_ref().unwrap(),
                    limit,
                    &mut pending,
                    &mut page,
                ) {
                    failed = Some(err);
                    return WalkControl::Stop;
                }
                if page.truncated {
                    return WalkControl::Stop;
                }
            }
            WalkControl::Continue
        });
        walked.map_err(show)?;
        if let Some(err) = failed {
            return Err(err);
        }
        if !pending.is_empty() && !page.truncated {
            if let Some(filters) = filters.as_ref() {
                flush_metadata(
                    &self.root,
                    &mut cache,
                    filters,
                    limit,
                    &mut pending,
                    &mut page,
                )?;
            }
        }
        Ok(page_value(&page))
    }

    fn browse(&self, arguments: &Value) -> Result<Value, String> {
        let prefix = optional_string(arguments, "prefix")?
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        let limit = optional_limit(arguments)?.unwrap_or(default_page("ALBUMEN_BROWSE_PAGE", 200));
        let after = optional_cursor(arguments)?;
        let mut page = Page::default();
        let mut failed: Option<String> = None;
        walk_sorted(&self.root, after.as_deref(), &mut |relative| {
            if !under_prefix(relative, &prefix) {
                return WalkControl::Continue;
            }
            let bytes = match file_len(&self.root, relative) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failed = Some(err);
                    return WalkControl::Stop;
                }
            };
            let stop = page.consider(
                limit,
                Hit {
                    path: relative.to_string(),
                    bytes,
                    record: None,
                },
            );
            if stop {
                WalkControl::Stop
            } else {
                WalkControl::Continue
            }
        })
        .map_err(show)?;
        if let Some(err) = failed {
            return Err(err);
        }
        Ok(page_value(&page))
    }

    fn export(&self, arguments: &Value) -> Result<Value, String> {
        let to = required_string(arguments, "to")?;
        let manifest = match optional_string(arguments, "manifest")? {
            Some(path) => PathBuf::from(path),
            None if to.starts_with("s3://") => {
                return Err("pass manifest when exporting to S3".to_string());
            }
            None => Path::new(to).join("ALBUMEN-MANIFEST.txt"),
        };
        let source = SourceRoot::open(&self.root).map_err(show)?;
        if !to.starts_with("s3://") {
            source.refuse_destination(Path::new(to)).map_err(show)?;
            albumen_ingest::family::refuse_shared_write_root(Path::new(to)).map_err(show)?;
        }
        source.refuse_destination(&manifest).map_err(show)?;
        if !self.apply {
            return export_plan(&source, to, &manifest);
        }
        let dest = open_target(to)?;
        let entries = export_tree(&source, dest.as_ref()).map_err(show)?;
        if !to.starts_with("s3://") {
            if let Ok(relative) = manifest.strip_prefix(to) {
                let relative = relative_text(relative);
                if entries.iter().any(|entry| entry.relative_path == relative) {
                    return Err(format!(
                        "refusing to overwrite exported file {relative} with the manifest"
                    ));
                }
            }
        }
        write_manifest(&manifest, &entries).map_err(show)?;
        Ok(json!({
            "files": entries.len(),
            "manifest": manifest.display().to_string(),
            "destination": to,
        }))
    }

    fn upload(&self, arguments: &Value) -> Result<Value, String> {
        albumen_ingest::family::refuse_shared_write_root(&self.root).map_err(show)?;
        let path = PathBuf::from(required_string(arguments, "path")?);
        let asset = discover(&path).map_err(show)?;
        if !self.apply {
            let files = asset
                .files()
                .iter()
                .map(|file| {
                    file.path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>();
            return Ok(json!({
                "dry_run": true,
                "into": self.root.display().to_string(),
                "files": files,
                "network": "none",
            }));
        }
        let report = copy_verify(&asset, &LocalStore::new(&self.root)).map_err(show)?;
        let files = report
            .files
            .iter()
            .map(|file| {
                json!({
                    "filename": file.filename,
                    "sha256": file.sha256,
                    "bytes": file.byte_len,
                    "role": match file.role {
                        albumen_ingest::Role::Primary => "primary",
                        albumen_ingest::Role::Sidecar => "sidecar",
                        albumen_ingest::Role::Pair => "pair",
                    },
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"files": files}))
    }
}

fn export_plan(root: &SourceRoot, to: &str, manifest: &Path) -> Result<Value, String> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    walk_sorted(root.canonical(), None, &mut |relative| {
        let full = root.canonical().join(relative);
        bytes += root
            .metadata_nofollow(&full)
            .map(|meta| meta.len())
            .unwrap_or(0);
        files += 1;
        WalkControl::Continue
    })
    .map_err(show)?;
    let network = if to.starts_with("s3://") {
        "on --apply, connect to ALBUMEN_S3_ENDPOINT and write objects"
    } else {
        "none"
    };
    Ok(json!({
        "dry_run": true,
        "destination": to,
        "manifest": manifest.display().to_string(),
        "files": files,
        "bytes": bytes,
        "network": network,
    }))
}

fn show(err: IngestError) -> String {
    err.to_string()
}

fn file_len(root: &Path, relative: &str) -> Result<u64, String> {
    let full = root.join(PathBuf::from_iter(relative.split('/')));
    fs::metadata(&full)
        .map(|meta| meta.len())
        .map_err(|err| format!("{relative}: {err}"))
}

fn under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn flush_metadata(
    root: &Path,
    cache: &mut metadata::Cache,
    filters: &Filters,
    limit: u64,
    pending: &mut Vec<(String, u64)>,
    page: &mut Page,
) -> Result<(), String> {
    let mut load_paths: Vec<String> = pending.iter().map(|(path, _)| path.clone()).collect();
    for (path, _) in pending.iter() {
        for sidecar in metadata::existing_sidecars(root, path) {
            if !load_paths.iter().any(|item| item == &sidecar) {
                load_paths.push(sidecar);
            }
        }
    }
    let read = cache.load(root, &load_paths)?;
    for (path, bytes) in pending.drain(..) {
        let Some(own) = read.get(&path) else {
            continue;
        };
        let merged = metadata::with_sidecars(&path, own, &read);
        if !filters.matches(&merged) {
            continue;
        }
        if page.consider(
            limit,
            Hit {
                path,
                bytes,
                record: Some(merged),
            },
        ) {
            return Ok(());
        }
    }
    Ok(())
}

fn page_value(page: &Page) -> Value {
    let files = page
        .hits
        .iter()
        .map(|hit| {
            let mut file = json!({"path": hit.path, "bytes": hit.bytes});
            if let Some(record) = &hit.record {
                attach_metadata(&mut file, record);
            }
            file
        })
        .collect::<Vec<_>>();
    let mut value = json!({"files": files, "truncated": page.truncated});
    if let Some(cursor) = page.next_cursor() {
        value["next_cursor"] = Value::String(cursor.to_string());
    }
    value
}

fn default_page(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn open_target(spec: &str) -> Result<Box<dyn ExportDest>, String> {
    if let Some(rest) = spec.strip_prefix("s3://") {
        let rest = rest.trim_end_matches('/');
        let (bucket, prefix) = match rest.split_once('/') {
            Some((bucket, prefix)) => (bucket.to_string(), prefix.to_string()),
            None => (rest.to_string(), String::new()),
        };
        if bucket.is_empty()
            || (!prefix.is_empty()
                && prefix
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == ".."))
        {
            return Err(format!("bad s3 target {spec}"));
        }
        let mut config = S3Config::from_env().map_err(show)?;
        config.bucket = bucket;
        config.prefix = prefix;
        let store = S3Store::connect(config).map_err(show)?;
        Ok(Box::new(store))
    } else {
        Ok(Box::new(DirectoryTree::new(spec)))
    }
}

fn relative_text(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn arguments_object(arguments: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    match arguments {
        Value::Object(map) => Ok(map),
        Value::Null => Err("arguments must be an object".to_string()),
        _ => Err("arguments must be an object".to_string()),
    }
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    match arguments_object(arguments)?.get(key) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(format!("{key} must be a string")),
        None => Err(format!("missing {key}")),
    }
}

fn optional_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments_object(arguments)?.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn optional_cursor(arguments: &Value) -> Result<Option<String>, String> {
    match optional_string(arguments, "cursor")? {
        None => Ok(None),
        Some("") => Err("cursor must not be empty".to_string()),
        Some(text) => Ok(Some(text.to_string())),
    }
}

fn optional_limit(arguments: &Value) -> Result<Option<u64>, String> {
    match arguments_object(arguments)?.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| "limit must be a non-negative integer".to_string()),
        Some(_) => Err("limit must be a non-negative integer".to_string()),
    }
}

fn attach_metadata(file: &mut Value, record: &metadata::Record) {
    file["captured"] = record
        .captured
        .map(|time| Value::String(time.to_text()))
        .unwrap_or(Value::Null);
    file["camera"] = optional_text(record.camera.as_deref());
    file["lens"] = optional_text(record.lens.as_deref());
    file["latitude"] = optional_number(record.latitude);
    file["longitude"] = optional_number(record.longitude);
    file["keywords"] = Value::Array(record.keywords.iter().cloned().map(Value::String).collect());
    file["captions"] = Value::Array(record.captions.iter().cloned().map(Value::String).collect());
}

fn optional_text(text: Option<&str>) -> Value {
    match text {
        Some(text) => Value::String(text.to_string()),
        None => Value::Null,
    }
}

fn optional_number(number: Option<f64>) -> Value {
    match number {
        Some(number) => json!(number),
        None => Value::Null,
    }
}

fn reject_unknown_search_keys(arguments: &Value) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "query",
        "limit",
        "cursor",
        "captured_after",
        "captured_before",
        "camera",
        "lens",
        "keyword",
        "caption",
        "gps",
    ];
    for key in arguments_object(arguments)?.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(format!("unknown search field {key}"));
        }
    }
    Ok(())
}

fn metadata_filters(arguments: &Value) -> Result<Option<Filters>, String> {
    let captured_after = optional_string(arguments, "captured_after")?
        .map(|text| metadata::parse_user_time(text, false))
        .transpose()?;
    let captured_before = optional_string(arguments, "captured_before")?
        .map(|text| metadata::parse_user_time(text, true))
        .transpose()?;
    let camera = optional_nonempty(arguments, "camera")?;
    let lens = optional_nonempty(arguments, "lens")?;
    let keyword = optional_nonempty(arguments, "keyword")?;
    let caption = optional_nonempty(arguments, "caption")?;
    let gps = optional_gps(arguments)?;
    if captured_after.is_none()
        && captured_before.is_none()
        && camera.is_none()
        && lens.is_none()
        && keyword.is_none()
        && caption.is_none()
        && gps.is_none()
    {
        return Ok(None);
    }
    Ok(Some(Filters {
        captured_after,
        captured_before,
        camera,
        lens,
        keyword,
        caption,
        gps,
    }))
}

fn optional_nonempty(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match optional_string(arguments, key)? {
        None => Ok(None),
        Some("") => Err(format!("{key} must not be empty")),
        Some(text) => Ok(Some(text.to_string())),
    }
}

fn optional_gps(arguments: &Value) -> Result<Option<GeoBox>, String> {
    match arguments_object(arguments)?.get("gps") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => {
            for key in map.keys() {
                if !["south", "north", "west", "east"].contains(&key.as_str()) {
                    return Err(format!("unknown gps field {key}"));
                }
            }
            let south = required_f64(map, "south")?;
            let north = required_f64(map, "north")?;
            let west = required_f64(map, "west")?;
            let east = required_f64(map, "east")?;
            metadata::parse_geo_box(south, north, west, east).map(Some)
        }
        Some(_) => Err("gps must be an object".to_string()),
    }
}

fn required_f64(map: &serde_json::Map<String, Value>, key: &str) -> Result<f64, String> {
    match map.get(key) {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("gps {key} must be a finite number")),
        Some(_) => Err(format!("gps {key} must be a number")),
        None => Err(format!("gps is missing {key}")),
    }
}

fn search_def() -> Value {
    json!({
        "name": "search",
        "description": "Find library files by path, capture date, camera, lens, GPS box, keyword, or caption. Metadata is read from the files and their XMP/AAE sidecars. Results are one page. Pass next_cursor to read the next page. Nothing is written.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Case-insensitive substring of the relative path"},
                "captured_after": {"type": "string", "description": "Inclusive capture date or time, YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS"},
                "captured_before": {"type": "string", "description": "Inclusive capture date or time. A date alone includes that whole day."},
                "camera": {"type": "string", "description": "Case-insensitive substring of make and model"},
                "lens": {"type": "string", "description": "Case-insensitive substring of the lens name"},
                "keyword": {"type": "string", "description": "Case-insensitive substring of an XMP subject or IPTC keyword"},
                "caption": {"type": "string", "description": "Case-insensitive substring of an XMP/IPTC caption or description"},
                "gps": {
                    "type": "object",
                    "description": "Inclusive latitude/longitude box. If west is greater than east, the box crosses the antimeridian.",
                    "properties": {
                        "south": {"type": "number"},
                        "north": {"type": "number"},
                        "west": {"type": "number"},
                        "east": {"type": "number"}
                    },
                    "required": ["south", "north", "west", "east"],
                    "additionalProperties": false
                },
                "limit": {"type": "integer", "minimum": 0, "description": "Page size. Soft default is 1000, or ALBUMEN_SEARCH_PAGE. There is no maximum."},
                "cursor": {"type": "string", "description": "Return matches strictly after this relative path."}
            },
            "additionalProperties": false
        }
    })
}

fn browse_def() -> Value {
    json!({
        "name": "browse",
        "description": "List library files under an optional relative prefix. Results are one page. Pass next_cursor to read the next page. Reads only. Does not modify the library.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prefix": {"type": "string"},
                "limit": {"type": "integer", "minimum": 0, "description": "Page size. Soft default is 200, or ALBUMEN_BROWSE_PAGE. There is no maximum."},
                "cursor": {"type": "string", "description": "Return files strictly after this relative path."}
            },
            "additionalProperties": false
        }
    })
}

fn export_def() -> Value {
    json!({
        "name": "export",
        "description": "Copy the library, sidecars, and pairs to a directory or s3://bucket/prefix with copy_verify. Skips identical files. Never overwrites. Does not modify the library. Returns a plan and writes nothing unless albumen-mcp was started with --apply or --execute.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "manifest": {"type": "string", "description": "Required for s3:// destinations"}
            },
            "required": ["to"],
            "additionalProperties": false
        }
    })
}

fn upload_def() -> Value {
    json!({
        "name": "upload",
        "description": "Copy a file and its discovered sidecars and pairs into the library with copy_verify. Never overwrites. Does not modify or remove the source. Returns a plan and writes nothing unless albumen-mcp was started with --apply or --execute.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Primary file to copy into the library"}
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_and_search_pages_cover_every_file_once() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..25 {
            fs::write(root.path().join(format!("f{index:02}.jpg")), b"x").unwrap();
        }
        let library = Library::new(root.path().to_path_buf(), false, false, false).unwrap();
        let seen = collect_pages(&library, "browse", 10);
        assert_eq!(seen.len(), 25, "{seen:?}");
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique, seen);
        let found = collect_pages(&library, "search", 10);
        assert_eq!(found, seen);
    }

    fn collect_pages(library: &Library, tool: &str, limit: u64) -> Vec<String> {
        let mut cursor = None;
        let mut seen = Vec::new();
        for _ in 0..10 {
            let mut args = json!({"limit": limit});
            if tool == "search" {
                args["query"] = json!("f");
            }
            if let Some(cursor) = &cursor {
                args["cursor"] = json!(cursor);
            }
            let page = library.call(tool, &args).unwrap();
            let files = page["files"].as_array().unwrap();
            assert!(files.len() as u64 <= limit);
            for file in files {
                seen.push(file["path"].as_str().unwrap().to_string());
            }
            if !page["truncated"].as_bool().unwrap() {
                assert!(page.get("next_cursor").is_none());
                return seen;
            }
            cursor = Some(page["next_cursor"].as_str().unwrap().to_string());
        }
        panic!("pagination did not finish: {seen:?}");
    }
}
