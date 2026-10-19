// SPDX-License-Identifier: Apache-2.0

//! On-demand JPEG thumbnails, cached outside the library.
//!
//! The original is opened read-only through [`SourceRoot`], decoded in memory,
//! scaled to fit a square box, and encoded as a new JPEG. The original is not
//! changed. The encoded thumbnail is cached under `--thumbnail-dir`,
//! `ALBUMEN_THUMBNAIL_DIR`, or `thumbnails` in the platform data directory, and
//! a directory inside the library is refused. The cache key includes the
//! original's size and modification time, so an edited original gets a new
//! thumbnail. A cache that cannot be written still returns the bytes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use albumen_ingest::{dest, IngestError, SourceRoot};
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::ImageFormat;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Default longest edge, in pixels.
pub const DEFAULT_EDGE: u32 = 256;
/// Largest longest edge a caller may ask for. A soft default the caller can
/// raise with `ALBUMEN_THUMBNAIL_MAX_EDGE`.
pub const DEFAULT_MAX_EDGE: u32 = 2048;
/// Environment override for the cache directory.
pub const THUMBNAIL_DIR_ENV: &str = "ALBUMEN_THUMBNAIL_DIR";
const QUALITY: u8 = 82;

/// The default cache directory.
pub fn default_thumbnail_dir() -> Option<PathBuf> {
    if let Ok(value) = std::env::var(THUMBNAIL_DIR_ENV) {
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    albumen_ingest::platform_dirs()
        .ok()
        .map(|(_, data)| data.join("thumbnails"))
}

fn max_edge() -> u32 {
    std::env::var("ALBUMEN_THUMBNAIL_MAX_EDGE")
        .ok()
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|edge| *edge >= 1)
        .unwrap_or(DEFAULT_MAX_EDGE)
}

/// Make or read the thumbnail of `relative` and return it as a tool result.
pub fn thumbnail(
    library: &Path,
    cache_dir: Option<&Path>,
    relative: &str,
    edge: Option<u64>,
) -> Result<Value, String> {
    let edge = match edge {
        None => DEFAULT_EDGE,
        Some(edge) if edge >= 1 && edge <= u64::from(max_edge()) => edge as u32,
        Some(edge) => {
            return Err(format!(
                "size must be between 1 and {} (got {edge}); ALBUMEN_THUMBNAIL_MAX_EDGE raises the limit",
                max_edge()
            ))
        }
    };
    if relative.is_empty()
        || relative.starts_with('/')
        || relative
            .split(['/', '\\'])
            .any(|part| part == ".." || part.is_empty())
    {
        return Err(format!("path must be relative to the library: {relative}"));
    }
    let root = SourceRoot::open(library).map_err(show)?;
    let mut path = root.canonical().to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    let meta = root.metadata_nofollow(&path).map_err(show)?;
    if !meta.is_file() {
        return Err(format!("not a file: {relative}"));
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let cache_file = match cache_dir {
        Some(dir) => {
            root.refuse_inside(dir).map_err(|_| {
                format!(
                    "the thumbnail directory is inside the library; use --thumbnail-dir outside {}",
                    library.display()
                )
            })?;
            let key = hex_key(&format!(
                "{}\0{relative}\0{}\0{mtime_ns}\0{edge}",
                root.canonical().display(),
                meta.len()
            ));
            Some(dir.join(&key[..2]).join(format!("{key}.jpg")))
        }
        None => None,
    };
    if let Some(file) = cache_file.as_ref() {
        if let Ok(bytes) = std::fs::read(file) {
            if let Ok((width, height)) =
                image::ImageReader::with_format(std::io::Cursor::new(&bytes), ImageFormat::Jpeg)
                    .into_dimensions()
            {
                return Ok(result(relative, &bytes, width, height, true));
            }
        }
    }
    let mut original = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    root.open_read(&path)
        .map_err(show)?
        .read_to_end(&mut original)
        .map_err(|err| format!("read {relative}: {err}"))?;
    let format = image::guess_format(&original)
        .map_err(|_| format!("no thumbnail for {relative}: not a JPEG"))?;
    if format != ImageFormat::Jpeg {
        return Err(format!(
            "no thumbnail for {relative}: only JPEG is supported"
        ));
    }
    let decoded = image::load_from_memory_with_format(&original, ImageFormat::Jpeg)
        .map_err(|err| format!("no thumbnail for {relative}: {err}"))?;
    let scaled = decoded.thumbnail(edge, edge).to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, QUALITY)
        .encode_image(&scaled)
        .map_err(|err| format!("encode thumbnail for {relative}: {err}"))?;
    if let Some(file) = cache_file.as_ref() {
        if let Err(err) = save(file, &bytes) {
            eprintln!("albumen-mcp: thumbnail not cached: {err}");
        }
    }
    Ok(result(
        relative,
        &bytes,
        scaled.width(),
        scaled.height(),
        false,
    ))
}

fn save(file: &Path, bytes: &[u8]) -> Result<(), String> {
    save_inner(file, bytes).map_err(show)
}

fn save_inner(file: &Path, bytes: &[u8]) -> Result<(), Box<IngestError>> {
    if let Some(parent) = file.parent() {
        dest::create_dir_all(parent).map_err(Box::new)?;
    }
    let partial = file.with_extension(format!("jpg.partial-{}", std::process::id()));
    let mut handle = dest::create_file(&partial).map_err(Box::new)?;
    handle.write_all(bytes).map_err(|source| IngestError::Io {
        path: partial.clone(),
        source,
    })?;
    drop(handle);
    dest::rename(&partial, file).map_err(Box::new)
}

fn result(relative: &str, bytes: &[u8], width: u32, height: u32, cached: bool) -> Value {
    json!({
        "path": relative,
        "mime_type": "image/jpeg",
        "width": width,
        "height": height,
        "bytes": bytes.len(),
        "cached": cached,
        "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn show(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn hex_key(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
