// SPDX-License-Identifier: Apache-2.0

//! Metadata read from the files in front of us.
//!
//! There is no catalog on disk. A process-local cache remembers a file only
//! while its SHA-256 and length still match, and a search that misses
//! re-reads that file with ExifTool. Sidecar metadata is joined in memory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use albumen_ingest::sha256_hex;
use serde_json::{Map, Value};

/// Capture time as written in the file, without converting time zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WallTime {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanos: u32,
}

impl WallTime {
    pub fn to_text(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Inclusive GPS bounds. When `west` is greater than `east`, the box crosses
/// the antimeridian.
#[derive(Debug, Clone, Copy)]
pub struct GeoBox {
    pub south: f64,
    pub north: f64,
    pub west: f64,
    pub east: f64,
}

impl GeoBox {
    pub fn contains(self, latitude: f64, longitude: f64) -> bool {
        if !latitude.is_finite() || !longitude.is_finite() {
            return false;
        }
        if latitude < self.south || latitude > self.north {
            return false;
        }
        if self.west <= self.east {
            longitude >= self.west && longitude <= self.east
        } else {
            longitude >= self.west || longitude <= self.east
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub captured: Option<WallTime>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub keywords: Vec<String>,
    pub captions: Vec<String>,
}

impl Record {
    fn empty() -> Self {
        Self {
            captured: None,
            camera: None,
            lens: None,
            latitude: None,
            longitude: None,
            keywords: Vec::new(),
            captions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub captured_after: Option<WallTime>,
    pub captured_before: Option<WallTime>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub keyword: Option<String>,
    pub caption: Option<String>,
    pub gps: Option<GeoBox>,
}

impl Filters {
    pub fn matches(&self, record: &Record) -> bool {
        if let Some(after) = self.captured_after {
            match record.captured {
                Some(captured) if captured >= after => {}
                _ => return false,
            }
        }
        if let Some(before) = self.captured_before {
            match record.captured {
                Some(captured) if captured <= before => {}
                _ => return false,
            }
        }
        if let Some(camera) = &self.camera {
            if !contains_ignore_case(record.camera.as_deref(), camera) {
                return false;
            }
        }
        if let Some(lens) = &self.lens {
            if !contains_ignore_case(record.lens.as_deref(), lens) {
                return false;
            }
        }
        if let Some(keyword) = &self.keyword {
            if !record
                .keywords
                .iter()
                .any(|item| contains_ignore_case(Some(item), keyword))
            {
                return false;
            }
        }
        if let Some(caption) = &self.caption {
            if !record
                .captions
                .iter()
                .any(|item| contains_ignore_case(Some(item), caption))
            {
                return false;
            }
        }
        if let Some(gps) = self.gps {
            match (record.latitude, record.longitude) {
                (Some(latitude), Some(longitude)) if gps.contains(latitude, longitude) => {}
                _ => return false,
            }
        }
        true
    }
}

/// SHA-256 keyed memory of ExifTool reads. Dropping it drops the only copy.
#[derive(Debug, Default)]
pub struct Cache {
    entries: HashMap<String, (String, u64, Record)>,
}

impl Cache {
    pub fn load(
        &mut self,
        root: &Path,
        relative_paths: &[String],
    ) -> Result<HashMap<String, Record>, String> {
        let mut digests = HashMap::with_capacity(relative_paths.len());
        let mut missing = Vec::new();
        for relative in relative_paths {
            let full = root.join(PathBuf::from_iter(relative.split('/')));
            let len = std::fs::metadata(&full)
                .map_err(|err| format!("{relative}: {err}"))?
                .len();
            let sha = sha256_hex(&full).map_err(|err| err.to_string())?;
            if let Some((cached_sha, cached_len, record)) = self.entries.get(relative) {
                if cached_sha == &sha && *cached_len == len {
                    digests.insert(relative.clone(), record.clone());
                    continue;
                }
            }
            missing.push((relative.clone(), sha, len));
        }
        if !missing.is_empty() {
            let fresh = read_exiftool(
                root,
                &missing
                    .iter()
                    .map(|(path, _, _)| path.clone())
                    .collect::<Vec<_>>(),
            )?;
            for (relative, sha, len) in missing {
                let record = fresh.get(&relative).cloned().unwrap_or_else(Record::empty);
                self.entries
                    .insert(relative.clone(), (sha, len, record.clone()));
                digests.insert(relative, record);
            }
        }
        Ok(digests)
    }
}

pub fn with_sidecars(relative: &str, own: &Record, all: &HashMap<String, Record>) -> Record {
    if is_sidecar(relative) {
        return own.clone();
    }
    let (parent, name) = split_relative(relative);
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let mut merged = own.clone();
    for candidate in [
        format!("{stem}.xmp"),
        format!("{name}.xmp"),
        format!("{stem}.aae"),
        format!("{name}.aae"),
    ] {
        let Some(sidecar) = find_sibling(all, parent, &candidate) else {
            continue;
        };
        if merged.captured.is_none() {
            merged.captured = sidecar.captured;
        }
        if merged.camera.is_none() {
            merged.camera.clone_from(&sidecar.camera);
        }
        if merged.lens.is_none() {
            merged.lens.clone_from(&sidecar.lens);
        }
        if merged.latitude.is_none() {
            merged.latitude = sidecar.latitude;
        }
        if merged.longitude.is_none() {
            merged.longitude = sidecar.longitude;
        }
        push_unique(&mut merged.keywords, &sidecar.keywords);
        push_unique(&mut merged.captions, &sidecar.captions);
    }
    merged
}

pub fn parse_user_time(text: &str, end_of_day: bool) -> Result<WallTime, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("a capture time must not be empty".to_string());
    }
    let (wall, date_only) = parse_wall(text)?;
    if date_only && end_of_day {
        Ok(WallTime {
            hour: 23,
            minute: 59,
            second: 59,
            nanos: 999_999_999,
            ..wall
        })
    } else {
        Ok(wall)
    }
}

pub fn parse_geo_box(south: f64, north: f64, west: f64, east: f64) -> Result<GeoBox, String> {
    if ![south, north, west, east]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err("gps bounds must be finite numbers".to_string());
    }
    if !(-90.0..=90.0).contains(&south) || !(-90.0..=90.0).contains(&north) || south > north {
        return Err(
            "gps south/north must be within -90..90 and south must not exceed north".to_string(),
        );
    }
    if !(-180.0..=180.0).contains(&west) || !(-180.0..=180.0).contains(&east) {
        return Err("gps west/east must be within -180..180".to_string());
    }
    Ok(GeoBox {
        south,
        north,
        west,
        east,
    })
}

fn read_exiftool(
    root: &Path,
    relative_paths: &[String],
) -> Result<HashMap<String, Record>, String> {
    let mut out = HashMap::new();
    for chunk in relative_paths.chunks(64) {
        let output = Command::new("exiftool")
            .current_dir(root)
            .arg("-json")
            .arg("-n")
            .arg("-G1")
            .arg("-s")
            .arg("-api")
            .arg("largefilesupport=1")
            .args(chunk)
            .output()
            .map_err(|err| format!("exiftool is required to search metadata: {err}"))?;
        if !output.status.success() {
            return Err(format!(
                "exiftool failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let items: Vec<Map<String, Value>> = serde_json::from_slice(&output.stdout)
            .map_err(|err| format!("exiftool json: {err}"))?;
        if items.len() != chunk.len() {
            return Err(format!(
                "exiftool returned {} records for {} files",
                items.len(),
                chunk.len()
            ));
        }
        for (relative, item) in chunk.iter().zip(items) {
            let source = item
                .get("SourceFile")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim_start_matches("./");
            if source != relative {
                return Err(format!("exiftool reported {source} for {relative}"));
            }
            out.insert(relative.clone(), record_from_exif(&item));
        }
    }
    Ok(out)
}

fn record_from_exif(item: &Map<String, Value>) -> Record {
    let mut record = Record::empty();
    record.captured = capture_time(item);
    record.camera = join_tags(
        item,
        &["Make", "Model", "UniqueCameraModel", "CameraModelName"],
    );
    record.lens = join_tags(item, &["LensModel", "LensMake", "Lens", "LensType"]);
    record.latitude = coordinate(item, "Latitude");
    record.longitude = coordinate(item, "Longitude");
    record.keywords = string_tags(item, &["Keywords", "Subject", "HierarchicalSubject"]);
    record.captions = string_tags(
        item,
        &[
            "Caption-Abstract",
            "Description",
            "ImageDescription",
            "Title",
        ],
    );
    record
}

fn capture_time(item: &Map<String, Value>) -> Option<WallTime> {
    const TAGS: &[&str] = &[
        "DateTimeOriginal",
        "CreateDate",
        "DateCreated",
        "MediaCreateDate",
        "TrackCreateDate",
        "CreationDate",
        "DateTimeDigitized",
    ];
    for tag in TAGS {
        for value in tag_values(item, tag) {
            if let Some(text) = value.as_str() {
                if let Ok((wall, _)) = parse_wall(text) {
                    return Some(wall);
                }
            }
        }
    }
    None
}

fn coordinate(item: &Map<String, Value>, axis: &str) -> Option<f64> {
    let suffix = format!("GPS{axis}");
    let composite = format!("Composite:{suffix}");
    if let Some(value) = item.get(&composite).and_then(as_f64) {
        return Some(value);
    }
    if let Some(position) = item.get("Composite:GPSPosition").and_then(Value::as_str) {
        let mut parts = position.split_whitespace();
        let latitude: f64 = parts.next()?.parse().ok()?;
        let longitude: f64 = parts.next()?.parse().ok()?;
        return Some(if axis == "Latitude" {
            latitude
        } else {
            longitude
        });
    }
    for (key, value) in item {
        if key == "SourceFile" || key.ends_with("Ref") || !key_matches(key, &suffix) {
            continue;
        }
        let Some(number) = as_f64(value) else {
            continue;
        };
        let reference = ref_value(item, axis);
        return Some(match reference {
            Some('N') | Some('E') => number.abs(),
            Some('S') | Some('W') => -number.abs(),
            _ => number,
        });
    }
    None
}

fn ref_value(item: &Map<String, Value>, axis: &str) -> Option<char> {
    let suffix = format!("GPS{axis}Ref");
    for (key, value) in item {
        if key_matches(key, &suffix) {
            let text = value.as_str()?.trim();
            return text.chars().next().map(|ch| ch.to_ascii_uppercase());
        }
    }
    None
}

fn join_tags(item: &Map<String, Value>, suffixes: &[&str]) -> Option<String> {
    let mut parts = Vec::new();
    for suffix in suffixes {
        for value in string_values(&tag_values(item, suffix)) {
            if parts
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
            {
                continue;
            }
            parts.push(value);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn string_tags(item: &Map<String, Value>, suffixes: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for suffix in suffixes {
        push_unique(&mut out, &string_values(&tag_values(item, suffix)));
    }
    out
}

fn tag_values<'a>(item: &'a Map<String, Value>, suffix: &str) -> Vec<&'a Value> {
    item.iter()
        .filter(|(key, _)| key_matches(key, suffix))
        .map(|(_, value)| value)
        .collect()
}

fn key_matches(key: &str, suffix: &str) -> bool {
    key == suffix || key.rsplit_once(':').is_some_and(|(_, tail)| tail == suffix)
}

fn string_values(values: &[&Value]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        collect_strings(value, &mut out);
    }
    out
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                out.push(text.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn parse_wall(text: &str) -> Result<(WallTime, bool), String> {
    let mut text = text.trim();
    if let Some(stripped) = text.strip_suffix('Z').or_else(|| text.strip_suffix('z')) {
        text = stripped.trim();
    }
    if let Some(index) = timezone_index(text) {
        text = text[..index].trim();
    }
    let (date, time) = if let Some((date, time)) = text.split_once(['T', ' ']) {
        (date, Some(time.trim()))
    } else {
        (text, None)
    };
    let mut date_parts = date.split(['-', ':']);
    let year = take_number(&mut date_parts, "year")?;
    let month = take_number(&mut date_parts, "month")?;
    let day = take_number(&mut date_parts, "day")?;
    if date_parts.next().is_some() {
        return Err(format!("bad capture date {text}"));
    }
    let (hour, minute, second, nanos) = match time {
        None => (0, 0, 0, 0),
        Some(time) => {
            let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
            let mut clock_parts = clock.split(':');
            let hour = take_number(&mut clock_parts, "hour")?;
            let minute = take_number(&mut clock_parts, "minute")?;
            let second = take_number(&mut clock_parts, "second")?;
            if clock_parts.next().is_some() {
                return Err(format!("bad capture time {text}"));
            }
            (hour, minute, second, fraction_nanos(fraction)?)
        }
    };
    let wall = WallTime {
        year: i32::try_from(year).map_err(|_| format!("bad year in {text}"))?,
        month: u8::try_from(month).map_err(|_| format!("bad month in {text}"))?,
        day: u8::try_from(day).map_err(|_| format!("bad day in {text}"))?,
        hour: u8::try_from(hour).map_err(|_| format!("bad hour in {text}"))?,
        minute: u8::try_from(minute).map_err(|_| format!("bad minute in {text}"))?,
        second: u8::try_from(second).map_err(|_| format!("bad second in {text}"))?,
        nanos,
    };
    validate_wall(wall, text)?;
    Ok((wall, time.is_none()))
}

fn timezone_index(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 11.min(bytes.len());
    while index < bytes.len() {
        if bytes[index] == b'+' || bytes[index] == b'-' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn take_number<'a>(parts: &mut impl Iterator<Item = &'a str>, label: &str) -> Result<u32, String> {
    let text = parts
        .next()
        .ok_or_else(|| format!("capture time is missing {label}"))?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("bad {label} in capture time"));
    }
    text.parse()
        .map_err(|_| format!("bad {label} in capture time"))
}

fn fraction_nanos(text: &str) -> Result<u32, String> {
    if text.is_empty() {
        return Ok(0);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) || text.len() > 9 {
        return Err(format!("bad fractional seconds {text}"));
    }
    let mut digits = text.to_string();
    digits.extend(std::iter::repeat_n('0', 9 - text.len()));
    digits
        .parse()
        .map_err(|_| format!("bad fractional seconds {text}"))
}

fn validate_wall(wall: WallTime, text: &str) -> Result<(), String> {
    let bad = || format!("bad capture time {text}");
    if !(1..=12).contains(&wall.month) || wall.hour > 23 || wall.minute > 59 || wall.second > 60 {
        return Err(bad());
    }
    let max_day = match wall.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(wall.year) {
                29
            } else {
                28
            }
        }
        _ => return Err(bad()),
    };
    if wall.day == 0 || wall.day > max_day {
        return Err(bad());
    }
    Ok(())
}

fn is_leap(year: i32) -> bool {
    let year = year as u32;
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn contains_ignore_case(haystack: Option<&str>, needle: &str) -> bool {
    haystack
        .map(|text| text.to_lowercase().contains(&needle.to_lowercase()))
        .unwrap_or(false)
}

fn is_sidecar(relative: &str) -> bool {
    let name = split_relative(relative).1;
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    ext.eq_ignore_ascii_case("xmp") || ext.eq_ignore_ascii_case("aae")
}

fn split_relative(relative: &str) -> (&str, &str) {
    match relative.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", relative),
    }
}

fn find_sibling<'a>(
    all: &'a HashMap<String, Record>,
    parent: &str,
    name: &str,
) -> Option<&'a Record> {
    let exact = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    };
    if let Some(record) = all.get(&exact) {
        return Some(record);
    }
    all.iter().find_map(|(path, record)| {
        let (other_parent, other_name) = split_relative(path);
        (other_parent == parent && other_name.eq_ignore_ascii_case(name)).then_some(record)
    })
}

fn push_unique(dest: &mut Vec<String>, extra: &[String]) {
    let mut seen: HashSet<String> = dest.iter().map(|item| item.to_lowercase()).collect();
    for item in extra {
        if seen.insert(item.to_lowercase()) {
            dest.push(item.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_bounds_and_antimeridian() {
        let day = parse_user_time("2016-03-21", false).unwrap();
        let end = parse_user_time("2016-03-21", true).unwrap();
        let captured = parse_wall("2016:03:21 09:15:00").unwrap().0;
        assert!(captured >= day && captured <= end);
        assert!(parse_user_time("2016-02-31", false).is_err());

        let box_ = parse_geo_box(10.0, 20.0, 170.0, -170.0).unwrap();
        assert!(box_.contains(15.0, 175.0));
        assert!(box_.contains(15.0, -175.0));
        assert!(!box_.contains(15.0, 0.0));
        assert!(parse_geo_box(20.0, 10.0, 0.0, 1.0).is_err());
    }

    #[test]
    fn fixture_files_and_sidecar_join_round_trip_without_a_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let before = list_files(&root);
        let mut cache = Cache::default();
        let paths = [
            "jpeg-gps/IMG_0001.jpg".to_string(),
            "xmp/IMG_0004.jpg".to_string(),
            "xmp/IMG_0004.xmp".to_string(),
            "raw-jpeg/IMG_0003.jpg".to_string(),
        ];
        let first = cache.load(&root, &paths).unwrap();
        let again = cache.load(&root, &paths).unwrap();
        assert_eq!(first, again);
        assert_eq!(list_files(&root), before);

        let jpeg = first.get("jpeg-gps/IMG_0001.jpg").unwrap();
        assert_eq!(
            jpeg.captured,
            Some(parse_wall("2012:06:15 10:30:00").unwrap().0)
        );
        assert_eq!(jpeg.latitude, Some(37.8199));
        assert_eq!(jpeg.longitude, Some(-122.4783));
        assert!(jpeg.keywords.iter().any(|item| item == "cc0"));
        assert!(jpeg
            .captions
            .iter()
            .any(|item| item.contains("Not a photograph")));

        let vegas = first.get("raw-jpeg/IMG_0003.jpg").unwrap();
        assert_eq!(vegas.longitude, Some(-115.1398));

        let joined = with_sidecars(
            "xmp/IMG_0004.jpg",
            first.get("xmp/IMG_0004.jpg").unwrap(),
            &first,
        );
        assert!(joined.latitude.unwrap() > 48.0);
        assert!(joined.longitude.unwrap() > 2.0);
        assert!(joined
            .captions
            .iter()
            .any(|item| item.contains("sidecar fixture")));
        assert!(joined.camera.as_deref().unwrap().contains("Fixture Camera"));
    }

    #[test]
    fn a_changed_file_is_read_again() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.xmp");
        std::fs::write(&path, xmp("alpha caption", "alpha-keyword", "Lens A")).unwrap();
        let mut cache = Cache::default();
        let relative = vec!["note.xmp".to_string()];
        let first = cache.load(root.path(), &relative).unwrap();
        assert!(first["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("alpha caption")));
        std::fs::write(&path, xmp("beta caption", "beta-keyword", "Lens B")).unwrap();
        let second = cache.load(root.path(), &relative).unwrap();
        assert!(second["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("beta caption")));
        assert!(!second["note.xmp"]
            .captions
            .iter()
            .any(|item| item.contains("alpha caption")));
        assert_eq!(second["note.xmp"].lens.as_deref(), Some("Lens B"));
        let names = list_files(root.path());
        assert_eq!(names, vec!["note.xmp".to_string()]);
    }

    fn list_files(root: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    names.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        names.sort();
        names
    }

    fn xmp(caption: &str, keyword: &str, lens: &str) -> String {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:aux="http://ns.adobe.com/exif/1.0/aux/">
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">{caption}</rdf:li></rdf:Alt></dc:description>
   <dc:subject><rdf:Bag><rdf:li>{keyword}</rdf:li></rdf:Bag></dc:subject>
   <aux:Lens>{lens}</aux:Lens>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#
        )
    }
}
