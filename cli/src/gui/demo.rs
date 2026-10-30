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
/// R12, R19: pier has a small picture of its own (96x64 baseline JPEG: sky,
/// sun, sea and a wooden pier). It carries no EXIF, so it has no camera date
/// and is dated by its file name (Oct 3, 2026).
pub const PIER_JPEG: &[u8] = &[
    255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67, 0, 6,
    4, 5, 6, 5, 4, 6, 6, 5, 6, 7, 7, 6, 8, 10, 16, 10, 10, 9, 9, 10, 20, 14, 15, 12, 16, 23, 20,
    24, 24, 23, 20, 22, 22, 26, 29, 37, 31, 26, 27, 35, 28, 22, 22, 32, 44, 32, 35, 38, 39, 41, 42,
    41, 25, 31, 45, 48, 45, 40, 48, 37, 40, 41, 40, 255, 219, 0, 67, 1, 7, 7, 7, 10, 8, 10, 19, 10,
    10, 19, 40, 26, 22, 26, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,
    40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,
    40, 40, 40, 40, 40, 40, 40, 40, 255, 192, 0, 17, 8, 0, 64, 0, 96, 3, 1, 34, 0, 2, 17, 1, 3, 17,
    1, 255, 196, 0, 31, 0, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7,
    8, 9, 10, 11, 255, 196, 0, 181, 16, 0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125, 1, 2, 3,
    0, 4, 17, 5, 18, 33, 49, 65, 6, 19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8, 35, 66, 177,
    193, 21, 82, 209, 240, 36, 51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37, 38, 39, 40, 41, 42,
    52, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99,
    100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 131, 132, 133, 134,
    135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165, 166, 167,
    168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198, 199, 200,
    201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 225, 226, 227, 228, 229, 230, 231, 232,
    233, 234, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 255, 196, 0, 31, 1, 0, 3, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 255, 196, 0, 181, 17, 0,
    2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119, 0, 1, 2, 3, 17, 4, 5, 33, 49, 6, 18, 65, 81, 7,
    97, 113, 19, 34, 50, 129, 8, 20, 66, 145, 161, 177, 193, 9, 35, 51, 82, 240, 21, 98, 114, 209,
    10, 22, 36, 52, 225, 37, 241, 23, 24, 25, 26, 38, 39, 40, 41, 42, 53, 54, 55, 56, 57, 58, 67,
    68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99, 100, 101, 102, 103, 104, 105,
    106, 115, 116, 117, 118, 119, 120, 121, 122, 130, 131, 132, 133, 134, 135, 136, 137, 138, 146,
    147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165, 166, 167, 168, 169, 170, 178, 179,
    180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198, 199, 200, 201, 202, 210, 211, 212,
    213, 214, 215, 216, 217, 218, 226, 227, 228, 229, 230, 231, 232, 233, 234, 242, 243, 244, 245,
    246, 247, 248, 249, 250, 255, 218, 0, 12, 3, 1, 0, 2, 17, 3, 17, 0, 63, 0, 237, 190, 207, 237,
    71, 217, 253, 171, 91, 200, 246, 163, 200, 246, 175, 127, 219, 30, 31, 33, 147, 246, 127, 106,
    62, 207, 237, 90, 222, 71, 181, 30, 71, 181, 30, 216, 57, 12, 159, 179, 251, 81, 246, 127, 106,
    214, 242, 61, 168, 242, 61, 168, 246, 193, 200, 100, 253, 159, 218, 143, 179, 251, 86, 183,
    145, 237, 71, 145, 237, 71, 182, 14, 67, 39, 236, 254, 213, 126, 45, 2, 250, 91, 113, 60, 118,
    204, 99, 35, 112, 57, 25, 35, 233, 156, 214, 158, 145, 109, 19, 234, 80, 44, 234, 166, 50, 78,
    67, 116, 39, 7, 31, 174, 43, 181, 175, 35, 50, 205, 231, 133, 146, 133, 53, 118, 245, 212, 239,
    194, 96, 99, 89, 57, 73, 158, 81, 246, 127, 106, 62, 207, 237, 93, 87, 136, 237, 162, 26, 145,
    49, 42, 130, 202, 25, 241, 253, 236, 159, 254, 181, 101, 249, 30, 213, 233, 80, 197, 251, 106,
    113, 169, 181, 206, 58, 148, 125, 156, 220, 123, 26, 255, 0, 103, 246, 163, 236, 254, 213, 137,
    255, 0, 11, 15, 194, 95, 244, 21, 255, 0, 201, 105, 191, 248, 138, 63, 225, 97, 248, 75, 254,
    130, 191, 249, 45, 55, 255, 0, 17, 92, 150, 171, 252, 175, 238, 53, 188, 59, 163, 111, 236,
    254, 212, 125, 159, 218, 177, 63, 225, 97, 248, 75, 254, 130, 191, 249, 45, 55, 255, 0, 17, 71,
    252, 44, 63, 9, 127, 208, 87, 255, 0, 37, 166, 255, 0, 226, 40, 181, 95, 229, 127, 112, 94, 29,
    209, 183, 246, 127, 106, 62, 207, 237, 88, 127, 240, 177, 124, 34, 95, 104, 213, 134, 236, 103,
    31, 103, 151, 63, 250, 5, 73, 255, 0, 9, 239, 133, 191, 232, 41, 255, 0, 146, 242, 255, 0, 241,
    53, 148, 235, 123, 63, 143, 79, 83, 88, 82, 117, 62, 5, 127, 67, 99, 236, 254, 212, 125, 159,
    218, 177, 37, 248, 135, 225, 40, 134, 100, 213, 130, 143, 83, 111, 46, 63, 244, 26, 63, 225,
    97, 248, 75, 254, 130, 191, 249, 45, 55, 255, 0, 17, 85, 9, 74, 106, 241, 87, 38, 81, 80, 118,
    150, 134, 223, 217, 253, 170, 250, 95, 220, 172, 65, 62, 82, 64, 198, 226, 50, 107, 149, 255,
    0, 133, 135, 225, 47, 250, 10, 255, 0, 228, 180, 223, 252, 69, 31, 240, 176, 252, 37, 255, 0,
    65, 95, 252, 150, 155, 255, 0, 136, 169, 169, 65, 213, 178, 169, 11, 219, 200, 33, 85, 67, 225,
    149, 190, 102, 219, 66, 89, 139, 54, 73, 39, 36, 158, 244, 125, 159, 218, 177, 63, 225, 97,
    248, 75, 254, 130, 191, 249, 45, 55, 255, 0, 17, 71, 252, 44, 63, 9, 127, 208, 87, 255, 0, 37,
    166, 255, 0, 226, 43, 75, 84, 254, 87, 247, 19, 205, 14, 235, 239, 62, 124, 242, 232, 242, 234,
    215, 151, 71, 151, 94, 209, 229, 149, 124, 186, 100, 165, 33, 140, 188, 135, 10, 58, 156, 85,
    223, 46, 169, 93, 168, 186, 142, 230, 214, 63, 150, 68, 219, 146, 221, 57, 231, 250, 86, 56,
    138, 170, 149, 55, 46, 182, 118, 251, 174, 109, 66, 147, 171, 81, 71, 205, 126, 118, 25, 246,
    86, 55, 126, 110, 216, 221, 54, 108, 218, 199, 28, 231, 62, 149, 47, 147, 255, 0, 78, 214, 255,
    0, 159, 255, 0, 99, 74, 98, 36, 146, 109, 224, 36, 247, 39, 255, 0, 177, 164, 242, 127, 233,
    218, 223, 243, 255, 0, 236, 107, 227, 113, 53, 189, 189, 87, 83, 185, 246, 56, 106, 62, 194,
    154, 135, 98, 59, 171, 103, 154, 217, 226, 142, 40, 99, 45, 142, 67, 122, 31, 165, 57, 246,
    164, 137, 27, 28, 59, 231, 104, 245, 197, 59, 201, 255, 0, 167, 107, 127, 207, 255, 0, 177,
    166, 253, 153, 141, 204, 18, 108, 142, 52, 143, 118, 66, 158, 185, 24, 244, 174, 172, 191, 26,
    240, 243, 229, 123, 59, 124, 181, 255, 0, 135, 57, 115, 12, 18, 196, 67, 153, 110, 175, 243,
    211, 254, 24, 127, 151, 71, 151, 82, 198, 235, 36, 242, 196, 160, 238, 139, 25, 61, 185, 25,
    226, 165, 242, 235, 235, 35, 37, 37, 116, 124, 164, 162, 226, 236, 202, 190, 93, 30, 93, 90,
    242, 232, 242, 234, 137, 45, 249, 116, 121, 117, 111, 203, 163, 203, 160, 12, 203, 201, 126,
    205, 228, 124, 155, 188, 217, 86, 62, 184, 198, 123, 213, 25, 45, 243, 117, 52, 190, 84, 111,
    188, 143, 188, 222, 131, 31, 221, 173, 13, 94, 53, 117, 142, 51, 18, 73, 134, 223, 243, 49, 24,
    35, 167, 110, 123, 214, 127, 147, 255, 0, 78, 214, 255, 0, 159, 255, 0, 99, 95, 51, 155, 226,
    156, 167, 236, 83, 209, 126, 103, 210, 229, 24, 101, 24, 123, 102, 181, 127, 144, 121, 63, 244,
    237, 111, 249, 255, 0, 246, 52, 121, 63, 244, 237, 111, 249, 255, 0, 246, 52, 121, 63, 244,
    237, 111, 249, 255, 0, 246, 52, 121, 63, 244, 237, 111, 249, 255, 0, 246, 53, 227, 30, 200,
    121, 63, 244, 237, 111, 249, 255, 0, 246, 52, 121, 63, 244, 237, 111, 249, 255, 0, 246, 52,
    121, 63, 244, 237, 111, 249, 255, 0, 246, 52, 121, 63, 244, 237, 111, 249, 255, 0, 246, 52, 1,
    45, 140, 94, 85, 196, 174, 202, 171, 230, 5, 28, 54, 64, 192, 192, 227, 2, 172, 233, 242, 253,
    174, 210, 57, 246, 108, 223, 158, 51, 156, 96, 145, 253, 42, 143, 147, 255, 0, 78, 214, 255, 0,
    159, 255, 0, 99, 90, 58, 72, 88, 225, 22, 251, 21, 2, 231, 104, 12, 79, 191, 122, 247, 50, 172,
    107, 82, 84, 38, 244, 214, 222, 191, 213, 207, 15, 53, 193, 39, 23, 94, 11, 93, 47, 233, 253,
    88, 151, 203, 163, 203, 171, 126, 93, 30, 93, 125, 25, 243, 167, 255, 217,
];

pub const EXTRAS: [(&str, &[u8]); 7] = [
    ("2026-10-03 pier.jpg", PIER_JPEG),
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
    /// A synthetic twelve-member family library; member-12 is revoked.
    pub family: PathBuf,
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
    // Plain folders standing in for backup targets. They are not drives, so
    // they get folder names, never invented drive or volume names.
    let volumes = root.join("backup-folders");
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
    let demo = volumes.join("demo-backup");
    let damaged = volumes.join("damaged-backup");
    let stale = volumes.join("stale-backup");
    let never = volumes.join("new-backup");
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
    let family = build_family(&root.join("family"))?;
    Ok(Demo {
        library,
        card,
        backups,
        state,
        family,
    })
}

/// Demo only: twelve synthetic members, two synthetic photos each, then the
/// founder revokes member-12. Keys and sources sit beside the library.
fn build_family(root: &Path) -> Result<PathBuf, IngestError> {
    use albumen_ingest::family;
    let keys = root.join("keys");
    let sources = root.join("sources");
    let parent = root.join("library");
    for dir in [&keys, &sources, &parent] {
        dest::create_dir_all(dir)?;
    }
    let mut members = Vec::new();
    for n in 1..=12 {
        members.push(family::create_key(
            &keys.join(format!("member-{n:02}.key")),
        )?);
    }
    let library = family::init_library(&parent, &members[0], "member-01")?;
    for (i, key) in members.iter().enumerate().skip(1) {
        let token = family::invite(&library, &members[0], key.member_id())?;
        family::join(&library, key, &token, &format!("member-{:02}", i + 1))?;
        family::admit(&library, &members[0], key.member_id(), &token)?;
    }
    for (i, key) in members.iter().enumerate() {
        for p in 0..2u32 {
            let photo = sources.join(format!("member-{:02}-{p}.png", i + 1));
            write_new(&photo, &synthetic_png(160, 120, 60 + 2 * i as u32 + p))?;
            family::add_object(&library, key, &photo, None)?;
        }
    }
    family::revoke(&library, &members[0], members[11].member_id())?;
    Ok(library)
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
        assert_eq!(oct3["count"], 5, "{oct3}");
    }
}
