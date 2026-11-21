// SPDX-License-Identifier: Apache-2.0

//! Small JPEG previews for the photo grid.
//!
//! The original is opened read-only through [`SourceRoot`], decoded in
//! memory, turned upright from its EXIF orientation, scaled to fit a square
//! box, and encoded as a new JPEG. The original is never changed. The
//! preview is cached only in the GUI's own `--work` folder (`thumbs/`), and a
//! cache folder inside the library is refused before anything is read. A
//! cache that cannot be written still returns the bytes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use albumen_ingest::{dest, IngestError, SourceRoot};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use sha2::{Digest, Sha256};

/// Longest edge of a grid preview, in pixels. Sharp on a 2x display at the
/// grid's tile size.
pub const EDGE: u32 = 480;
const QUALITY: u8 = 80;

/// What `/thumb` sends back.
pub enum Preview {
    /// A new (or cached) JPEG preview.
    Jpeg(Vec<u8>),
    /// Not a JPEG: the caller serves the original instead.
    Original,
}

/// Check that `cache` is outside `library`. Called at start, and again
/// before each preview is made.
pub fn refuse_cache_in_library(library: &Path, cache: &Path) -> Result<(), IngestError> {
    SourceRoot::open(library)?.refuse_inside(cache)
}

/// The preview for `relative` (already checked by the caller to be a safe
/// relative image path inside the library).
pub fn preview(library: &Path, cache: &Path, relative: &str) -> Result<Preview, String> {
    let root = SourceRoot::open(library).map_err(|e| e.to_string())?;
    root.refuse_inside(cache).map_err(|e| e.to_string())?;
    let mut path = root.canonical().to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    let meta = root.metadata_nofollow(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("not a file: {relative}"));
    }
    let ext = relative
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext != "jpg" && ext != "jpeg" {
        return Ok(Preview::Original);
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let key: String = Sha256::digest(
        format!(
            "{}\0{relative}\0{}\0{mtime_ns}\0{EDGE}",
            root.canonical().display(),
            meta.len()
        )
        .as_bytes(),
    )
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
    let file = cache.join(&key[..2]).join(format!("{key}.jpg"));
    if let Ok(bytes) = std::fs::read(&file) {
        if bytes.starts_with(&[0xFF, 0xD8]) {
            return Ok(Preview::Jpeg(bytes));
        }
    }
    let mut original = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    root.open_read(&path)
        .map_err(|e| e.to_string())?
        .read_to_end(&mut original)
        .map_err(|err| format!("read {relative}: {err}"))?;
    let mut decoder = ImageReader::with_format(std::io::Cursor::new(&original), ImageFormat::Jpeg)
        .into_decoder()
        .map_err(|err| format!("no preview for {relative}: {err}"))?;
    let orientation = decoder.orientation().ok();
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|err| format!("no preview for {relative}: {err}"))?;
    drop(original);
    if let Some(o) = orientation {
        decoded.apply_orientation(o);
    }
    let scaled = decoded.thumbnail(EDGE, EDGE).to_rgb8();
    drop(decoded);
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, QUALITY)
        .encode_image(&scaled)
        .map_err(|err| format!("encode preview for {relative}: {err}"))?;
    if let Err(err) = save(&file, &bytes) {
        eprintln!("albumen gui: preview not cached: {err}");
    }
    Ok(Preview::Jpeg(bytes))
}

fn save(file: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    if let Some(parent) = file.parent() {
        dest::create_dir_all(parent)?;
    }
    let partial: PathBuf = file.with_extension(
        format!(
            "jpg.partial-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], ""),
    );
    let mut handle = dest::create_file(&partial)?;
    handle.write_all(bytes).map_err(|source| IngestError::Io {
        path: partial.clone(),
        source,
    })?;
    drop(handle);
    dest::rename(&partial, file)
}
