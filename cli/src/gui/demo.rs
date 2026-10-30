// SPDX-License-Identifier: Apache-2.0

//! `albumen gui --demo`: a synthetic photo set and pre-staged backup folders.
//!
//! Every PNG is generated here (colour gradients and shapes); the RAW,
//! HEIC and JPEG extras are the repo's own synthetic fixtures. No real photo
//! is used. This is the one library every GUI screen and test is shot from,
//! so counts agree across screens. Each start builds a new dated folder under the work
//! directory, so the click-path always begins at "exported 12 new". Nothing
//! is removed. All writes go through `albumen_ingest::dest`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use albumen_ingest::{dest, IngestError};

use super::io_err;

/// Generated PNG photos in the demo set.
pub const DEMO_PHOTOS: u32 = 12;

/// The rest of the shared fixture: one of each case the screens show.
/// Oct 3 gets a JPEG and a lone RAW (dated by file name); IMG_0003 is a
/// RAW + JPEG pair dated by the RAW's metadata; IMG_0002 is HEIC only; the
/// CR3 and NEF carry no date and land in "No date".
pub const EXTRAS: [(&str, &[u8]); 7] = [
    (
        "2026-10-03 pier.jpg",
        include_bytes!("../../../fixtures/raw-jpeg/IMG_0003.jpg"),
    ),
    ("2026-10-03 reef.dng", b"demo dng\n"),
    (
        "IMG_0003.dng",
        include_bytes!("../../../fixtures/raw-jpeg/IMG_0003.dng"),
    ),
    (
        "IMG_0003.jpg",
        include_bytes!("../../../fixtures/raw-jpeg/IMG_0003.jpg"),
    ),
    (
        "IMG_0002.heic",
        include_bytes!("../../../fixtures/heic/IMG_0002.heic"),
    ),
    ("IMG_0101.cr3", b"synthetic not a real raw"),
    ("IMG_0102.nef", b"demo nef\n"),
];

/// Photos the library shows: the PNGs plus the extras, with the
/// IMG_0003 pair counted once.
#[cfg(test)]
pub const DEMO_LIBRARY_PHOTOS: u32 = DEMO_PHOTOS + EXTRAS.len() as u32 - 1;

pub struct Demo {
    pub library: PathBuf,
    pub card: PathBuf,
    pub backups: Vec<String>,
    pub state: PathBuf,
}

/// CRC-32 (IEEE), for PNG chunks.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A synthetic RGB PNG: a sky-to-ground gradient tinted by `seed`, a sun,
/// and hills. Deterministic for a given size and seed.
pub fn synthetic_png(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let hue = (seed * 47) % 360;
    let tint = |deg: u32| -> [f32; 3] {
        let x = (deg % 360) as f32 / 60.0;
        let f = |n: f32| {
            let k = (n + x) % 6.0;
            1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))
        };
        [f(5.0), f(3.0), f(1.0)]
    };
    let sky = tint(hue);
    let land = tint(hue + 140);
    let sun = (
        (seed * 37 % 60 + 20) as f32 / 100.0 * w as f32,
        (seed * 13 % 25 + 15) as f32 / 100.0 * h as f32,
    );
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0); // filter: none
        let fy = y as f32 / h as f32;
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let ridge = 0.62 + 0.08 * ((fx * 6.3 + seed as f32).sin()) + 0.04 * ((fx * 17.0).cos());
            let (dx, dy) = (x as f32 - sun.0, y as f32 - sun.1);
            let px = if fy > ridge {
                let d = 0.55 + 0.45 * (1.0 - (fy - ridge) / (1.0 - ridge));
                land.map(|c| (0.25 + 0.6 * c) * d)
            } else if dx * dx + dy * dy < (h as f32 * 0.09).powi(2) {
                [1.0, 0.95, 0.75]
            } else {
                sky.map(|c| 0.45 + 0.5 * c * (1.0 - fy) + 0.1 * fx)
            };
            raw.extend(px.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8));
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).expect("in-memory zlib");
    let idat = z.finish().expect("in-memory zlib");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// [`synthetic_png`] with a capture time, stored the standard PNG way: a
/// `tEXt` chunk with the keyword `Creation Time`, after the image data.
pub fn synthetic_png_taken(w: u32, h: u32, seed: u32, taken: &str) -> Vec<u8> {
    let mut out = synthetic_png(w, h, seed);
    let iend = out.split_off(out.len() - 12);
    let mut text = b"Creation Time\0".to_vec();
    text.extend_from_slice(taken.as_bytes());
    chunk(&mut out, b"tEXt", &text);
    out.extend_from_slice(&iend);
    out
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    let mut f = dest::open_new(path)?;
    f.write_all(bytes).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

const NAMES: [&str; 12] = [
    "harbour", "meadow", "dunes", "ridge", "lake", "orchard", "canyon", "fjord", "steppe", "delta",
    "mesa", "tundra",
];

/// When each demo photo was "taken": four days, so Events shows four
/// events (3, 2, 4 and 3 photos). Written into each PNG and its name.
pub const TAKEN: [&str; 12] = [
    "2026-10-03T09:12:00",
    "2026-10-03T11:40:00",
    "2026-10-03T17:05:00",
    "2026-10-04T08:30:00",
    "2026-10-04T15:20:00",
    "2026-10-11T07:45:00",
    "2026-10-11T10:10:00",
    "2026-10-11T13:55:00",
    "2026-10-11T18:25:00",
    "2026-10-18T09:00:00",
    "2026-10-18T12:15:00",
    "2026-10-18T16:50:00",
];

/// File name of demo photo `i`: "2026-10-03 harbour.png".
fn demo_name(i: usize) -> String {
    format!("{} {}.png", &TAKEN[i][..10], NAMES[i])
}

/// Build a new demo folder under `work`.
pub fn build(work: &Path) -> Result<Demo, IngestError> {
    let stamp: String = crate::proof::utc(SystemTime::now())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let mut root = work.join(format!("demo-{stamp}"));
    let mut n = 2;
    while root.exists() {
        root = work.join(format!("demo-{stamp}-{n}"));
        n += 1;
    }
    let library = root.join("Synthetic Photos");
    let card = root.join("Camera Card");
    let volumes = root.join("Volumes");
    let state = root.join("albumen-state");
    for dir in [&library, &card.join("DCIM/100DEMO"), &volumes, &state] {
        dest::create_dir_all(dir)?;
    }
    for (i, when) in TAKEN.iter().enumerate().take(DEMO_PHOTOS as usize) {
        let png = synthetic_png_taken(480, 360, i as u32 + 1, when);
        write_new(&library.join(demo_name(i)), &png)?;
    }
    for (name, bytes) in EXTRAS {
        write_new(&library.join(name), bytes)?;
    }
    for i in 0..3u32 {
        let png = synthetic_png(480, 360, 40 + i);
        write_new(
            &card.join(format!("DCIM/100DEMO/DEMO{:04}.png", i + 1)),
            &png,
        )?;
    }
    let demo = volumes.join("Demo");
    let damaged = volumes.join("Damaged Copy");
    let stale = volumes.join("Old Drive");
    let never = volumes.join("New Drive");
    for d in [&demo, &damaged, &stale, &never] {
        dest::create_dir_all(d)?;
    }
    // Pre-staged copies for the health screen, made by the real backup.
    for d in [&damaged, &stale] {
        crate::backup::run(&crate::backup::BackupArgs {
            from: &library,
            to: &d.display().to_string(),
            state: Some(&state.join(format!(
                "prestage-{}",
                d.file_name().unwrap().to_string_lossy().replace(' ', "-")
            ))),
        })?;
    }
    flip_one_byte(&damaged.join(demo_name(4)))?;
    age_proofs(&stale, Duration::from_secs(12 * 86_400))?;
    let backups = [&demo, &stale, &damaged, &never]
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    Ok(Demo {
        library,
        card,
        backups,
        state,
    })
}

/// Demo only: change one byte of a backed-up copy (never an original).
fn flip_one_byte(path: &Path) -> Result<(), IngestError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = dest::open_read_write(path)?;
    let at = 200u64;
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(at)).map_err(|e| io_err(path, e))?;
    f.read_exact(&mut b).map_err(|e| io_err(path, e))?;
    f.seek(SeekFrom::Start(at)).map_err(|e| io_err(path, e))?;
    f.write_all(&[b[0] ^ 0xFF]).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

/// Demo only: date the pre-staged proofs `by` earlier, so status says stale.
fn age_proofs(root: &Path, by: Duration) -> Result<(), IngestError> {
    let dir = root.join(crate::backup::PROOF_DIR);
    let entries = std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
    let when = crate::proof::utc(SystemTime::now() - by);
    for item in entries.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
        let mut value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| IngestError::Usage {
                message: e.to_string(),
            })?;
        value["finished_utc"] = serde_json::Value::String(when.clone());
        let mut f = dest::create_file(&path)?;
        f.write_all(value.to_string().as_bytes())
            .map_err(|e| io_err(&path, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_has_signature_and_chunks() {
        let png = synthetic_png(16, 8, 3);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_ne!(synthetic_png(16, 8, 3), synthetic_png(16, 8, 4));
    }

    #[test]
    fn demo_names_carry_the_capture_day() {
        assert_eq!(demo_name(0), "2026-10-03 harbour.png");
        assert_eq!(demo_name(4), "2026-10-04 lake.png");
        let png = synthetic_png_taken(16, 8, 3, TAKEN[0]);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn crc_matches_known_value() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    /// R5: the shared fixture holds every case the screens show.
    #[test]
    fn shared_fixture_has_lone_raw_pair_heic_and_undated() {
        let work = tempfile::tempdir().unwrap();
        let demo = build(work.path()).unwrap();
        let v = crate::gui::library_json(&demo.library);
        let images = v["images"].as_array().unwrap();
        assert_eq!(images.len() as u32, DEMO_LIBRARY_PHOTOS, "{v}");
        let find = |p: &str| images.iter().find(|i| i["path"] == p).unwrap();
        assert_eq!(find("2026-10-03 reef.dng")["preview"], false);
        assert!(find("2026-10-03 reef.dng")["raw"].is_null());
        assert_eq!(find("IMG_0003.jpg")["raw"], "IMG_0003.dng");
        assert_eq!(find("IMG_0002.heic")["format"], "HEIC");
        assert!(find("IMG_0101.cr3")["taken"].is_null());
        assert!(find("IMG_0102.nef")["taken"].is_null());
        let oct3 = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == "Oct 3, 2026")
            .unwrap();
        // "pier.jpg" carries the 2015 camera time, so it is not on Oct 3.
        assert_eq!(oct3["count"], 4, "{oct3}");
    }
}
