// SPDX-License-Identifier: Apache-2.0

//! When a photo was taken, for Events. Read-only and dependency-free.
//!
//! Order: the capture time stored in the file (JPEG or PNG Exif
//! DateTimeOriginal, or a PNG `Creation Time` text chunk), then a
//! `YYYY-MM-DD` at the start of the file name. File times are never used:
//! they say when a copy was made, not when the photo was taken.

use std::io::Read;
use std::path::Path;

/// Bytes read from the start of a file when looking for its capture time.
const HEAD: u64 = 256 * 1024;

/// A capture time and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// `YYYY-MM-DDTHH:MM:SS`, or `YYYY-MM-DD` from a file name.
    pub when: String,
    /// "photo metadata" or "file name".
    pub source: &'static str,
}

impl Taken {
    /// The `YYYY-MM-DD` day this photo belongs to.
    pub fn day(&self) -> &str {
        &self.when[..10]
    }
}

pub fn taken(library: &Path, rel: &str) -> Option<Taken> {
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(library.join(rel)) {
        let _ = file.take(HEAD).read_to_end(&mut head);
    }
    if let Some(when) = from_bytes(&head) {
        return Some(Taken {
            when,
            source: "photo metadata",
        });
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let day = name.get(..10)?;
    valid_day(day).then(|| Taken {
        when: day.to_string(),
        source: "file name",
    })
}

/// Capture time stored in JPEG or PNG bytes.
pub fn from_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return from_png(bytes);
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return from_jpeg(bytes);
    }
    None
}

fn from_png(bytes: &[u8]) -> Option<String> {
    let mut at = 8;
    let mut text = None;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        let data = bytes.get(at + 8..at + 8 + len)?;
        match kind {
            b"eXIf" => {
                if let Some(when) = from_tiff(data) {
                    return Some(when);
                }
            }
            b"tEXt" if text.is_none() => {
                if let Some(value) = data.strip_prefix(b"Creation Time\0") {
                    text = normalise(&String::from_utf8_lossy(value));
                }
            }
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    text
}

fn from_jpeg(bytes: &[u8]) -> Option<String> {
    let mut at = 2;
    while at + 4 <= bytes.len() && bytes[at] == 0xFF {
        let marker = bytes[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        let data = bytes.get(at + 4..at + 2 + len)?;
        if marker == 0xE1 {
            if let Some(tiff) = data.strip_prefix(b"Exif\0\0") {
                if let Some(when) = from_tiff(tiff) {
                    return Some(when);
                }
            }
        }
        at += 2 + len;
    }
    None
}

/// DateTimeOriginal (0x9003) from the Exif IFD of a TIFF block.
fn from_tiff(tiff: &[u8]) -> Option<String> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let find = |ifd: usize, tag: u16| -> Option<usize> {
        let count = u16_at(ifd)? as usize;
        (0..count.min(512))
            .map(|i| ifd + 2 + 12 * i)
            .find(|&entry| u16_at(entry) == Some(tag))
    };
    let ifd0 = u32_at(4)? as usize;
    let exif = u32_at(find(ifd0, 0x8769)? + 8)? as usize;
    let entry = find(exif, 0x9003)?;
    let count = u32_at(entry + 4)? as usize;
    let offset = if count <= 4 {
        entry + 8
    } else {
        u32_at(entry + 8)? as usize
    };
    let value = tiff.get(offset..offset + count.min(32))?;
    normalise(&String::from_utf8_lossy(value))
}

/// "2026:10:03 09:12:00" or "2026-10-03T09:12:00Z" as `2026-10-03T09:12:00`.
fn normalise(text: &str) -> Option<String> {
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    let b = text.as_bytes();
    if b.len() < 19 || !text.is_ascii() {
        return None;
    }
    let day = format!("{}-{}-{}", &text[..4], &text[5..7], &text[8..10]);
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let ok = valid_day(&day)
        && matches!(b[10], b' ' | b'T')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    ok.then(|| format!("{day}T{}", &text[11..19]))
}

fn valid_day(day: &str) -> bool {
    let b = day.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !digits(0..4) || !digits(5..7) {
        return false;
    }
    if !digits(8..10) {
        return false;
    }
    let month: u32 = day[5..7].parse().unwrap_or(0);
    let date: u32 = day[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&date) && &day[..4] != "0000"
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Oct 3, 2026" for "2026-10-03".
pub fn day_title(day: &str) -> String {
    let month: usize = day[5..7].parse().unwrap_or(1);
    let date: u32 = day[8..10].parse().unwrap_or(1);
    format!("{} {date}, {}", MONTHS[month.clamp(1, 12) - 1], &day[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiff(little: bool, when: &str) -> Vec<u8> {
        let w16 = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let w32 = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let mut t = Vec::new();
        t.extend_from_slice(if little { b"II" } else { b"MM" });
        t.extend_from_slice(&w16(42));
        t.extend_from_slice(&w32(8));
        // IFD0 at 8: one entry, ExifIFD pointer to 26.
        t.extend_from_slice(&w16(1));
        t.extend_from_slice(&w16(0x8769));
        t.extend_from_slice(&w16(4));
        t.extend_from_slice(&w32(1));
        t.extend_from_slice(&w32(26));
        t.extend_from_slice(&w32(0));
        // Exif IFD at 26: DateTimeOriginal, 20 ASCII bytes at 44.
        t.extend_from_slice(&w16(1));
        t.extend_from_slice(&w16(0x9003));
        t.extend_from_slice(&w16(2));
        t.extend_from_slice(&w32(20));
        t.extend_from_slice(&w32(44));
        t.extend_from_slice(&w32(0));
        t.extend_from_slice(when.as_bytes());
        t.push(0);
        t
    }

    #[test]
    fn reads_exif_date_time_original_both_byte_orders() {
        for little in [true, false] {
            let t = tiff(little, "2009:07:14 18:03:21");
            assert_eq!(from_tiff(&t).as_deref(), Some("2009-07-14T18:03:21"));
            let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
            jpeg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
            jpeg.extend_from_slice(b"Exif\0\0");
            jpeg.extend_from_slice(&t);
            jpeg.extend_from_slice(&[0xFF, 0xD9]);
            assert_eq!(from_bytes(&jpeg).as_deref(), Some("2009-07-14T18:03:21"));
        }
    }

    #[test]
    fn reads_png_creation_time_and_ignores_junk() {
        let png = crate::gui::demo::synthetic_png_taken(8, 8, 1, "2026-10-03T09:12:00");
        assert_eq!(from_bytes(&png).as_deref(), Some("2026-10-03T09:12:00"));
        assert_eq!(from_bytes(&crate::gui::demo::synthetic_png(8, 8, 1)), None);
        assert_eq!(normalise("2026:13:01 00:00:00"), None);
        assert_eq!(normalise("0000:00:00 00:00:00"), None);
        assert_eq!(from_bytes(b"\xFF\xD8\xFF"), None);
        assert_eq!(day_title("2026-10-03"), "Oct 3, 2026");
    }
}
