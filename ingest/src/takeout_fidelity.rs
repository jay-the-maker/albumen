// SPDX-License-Identifier: Apache-2.0

//! Takeout fidelity: compare what the Google JSON sidecar says against what
//! the original's own EXIF says, for capture time and GPS.
//!
//! This pass reads a finished `import-takeout` report and the library it
//! wrote. It opens every file read-only and writes nothing except the
//! fidelity report, which must not already exist. It never infers a value:
//! a field that neither source carries is reported as missing.
//!
//! Which source wins: the original's EXIF, when it has the field, because it
//! travels inside the original's bytes. The JSON value wins only when EXIF
//! lacks the field. Neither value is written into any original.
//!
//! Thresholds: capture times disagree when they differ by more than
//! [`TIME_THRESHOLD_SECS`] seconds; GPS positions disagree when they are more
//! than [`GPS_THRESHOLD_METERS`] metres apart (haversine). An EXIF time with
//! no `OffsetTimeOriginal` has no time zone, so it cannot be compared with
//! the JSON's UTC time; that is flagged `time_unzoned`, not guessed.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::{dest, exiftool_command, IngestError};

/// Capture times more than this many seconds apart disagree.
pub const TIME_THRESHOLD_SECS: i64 = 60;
/// GPS positions more than this many metres apart disagree.
pub const GPS_THRESHOLD_METERS: f64 = 100.0;

/// Every flag the fidelity pass can raise, in report order.
pub const FLAGS: [&str; 8] = [
    "time_missing_json",
    "time_missing_exif",
    "time_disagree",
    "time_unzoned",
    "gps_missing_json",
    "gps_missing_exif",
    "gps_disagree",
    "no_sidecar",
];

/// Totals from one fidelity pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FidelitySummary {
    pub originals: u64,
    pub flags: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq)]
struct Sides {
    json_time: Option<i64>,
    exif_time: Option<ExifTime>,
    json_gps: Option<(f64, f64)>,
    exif_gps: Option<(f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
struct ExifTime {
    text: String,
    utc: Option<i64>,
}

/// Write one fidelity line per original in `report` to `out`, plus a final
/// summary line. Refuses to overwrite `out`.
pub fn takeout_fidelity(
    report: &Path,
    library: &Path,
    out: &Path,
) -> Result<FidelitySummary, IngestError> {
    let text = std::fs::read_to_string(report).map_err(|err| IngestError::io(report, err))?;
    let mut originals = Vec::new();
    let mut sidecars: HashMap<String, String> = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|err| IngestError::Store {
                operation: "takeout-fidelity",
                filename: format!("{}:{}", report.display(), index + 1),
                message: err.to_string(),
            })?;
        let destination = value["destination"].as_str().unwrap_or("").to_string();
        match value["kind"].as_str() {
            Some("original") if !destination.is_empty() => originals.push(destination),
            Some("sidecar") => {
                // `partner` names the original in the sidecar's own folder.
                if let Some(partner) = value["partner"].as_str() {
                    let folder = match destination.rfind('/') {
                        Some(cut) => &destination[..=cut],
                        None => "",
                    };
                    sidecars.insert(format!("{folder}{partner}"), destination.clone());
                }
            }
            _ => {}
        }
    }
    let exif = read_exif(library, &originals)?;
    let mut file = dest::open_new(out)?;
    let mut summary = FidelitySummary {
        originals: 0,
        flags: FLAGS.iter().map(|flag| (flag.to_string(), 0)).collect(),
    };
    for original in &originals {
        let sidecar = sidecars.get(original);
        let (json_time, json_gps) = match sidecar {
            Some(rel) => read_json(&library.join(rel))?,
            None => (None, None),
        };
        let (exif_time, exif_gps) = exif.get(original).cloned().unwrap_or((None, None));
        let sides = Sides {
            json_time,
            exif_time,
            json_gps,
            exif_gps,
        };
        let mut line = describe(&sides);
        let mut flags = flags_for(&sides);
        if sidecar.is_none() {
            flags.insert(0, "no_sidecar");
        }
        for flag in &flags {
            *summary.flags.entry(flag.to_string()).or_default() += 1;
        }
        summary.originals += 1;
        line["kind"] = json!("fidelity");
        line["destination"] = json!(original);
        line["sidecar"] = json!(sidecar);
        line["flags"] = json!(flags);
        writeln!(file, "{line}").map_err(|err| IngestError::io(out, err))?;
    }
    let mut total = json!({
        "kind": "fidelity-summary",
        "originals": summary.originals,
        "flags": summary.flags,
        "time_threshold_secs": TIME_THRESHOLD_SECS,
        "gps_threshold_meters": GPS_THRESHOLD_METERS,
    });
    total["winner_rule"] = json!("exif when present, else json; nothing is written into originals");
    writeln!(file, "{total}").map_err(|err| IngestError::io(out, err))?;
    file.sync_all().map_err(|err| IngestError::io(out, err))?;
    Ok(summary)
}

fn describe(sides: &Sides) -> serde_json::Value {
    let time_winner = match (&sides.exif_time, sides.json_time) {
        (Some(_), _) => "exif",
        (None, Some(_)) => "json",
        (None, None) => "none",
    };
    let gps_winner = match (sides.exif_gps, sides.json_gps) {
        (Some(_), _) => "exif",
        (None, Some(_)) => "json",
        (None, None) => "none",
    };
    json!({
        "time": {
            "json_utc": sides.json_time.and_then(utc_text),
            "exif": sides.exif_time.as_ref().map(|time| time.text.clone()),
            "exif_utc": sides.exif_time.as_ref().and_then(|time| time.utc).and_then(utc_text),
            "difference_secs": time_difference(sides),
            "winner": time_winner,
        },
        "gps": {
            "json": sides.json_gps.map(|(lat, lon)| json!([lat, lon])),
            "exif": sides.exif_gps.map(|(lat, lon)| json!([lat, lon])),
            "distance_meters": gps_distance(sides).map(|metres| (metres * 10.0).round() / 10.0),
            "winner": gps_winner,
        },
    })
}

fn time_difference(sides: &Sides) -> Option<i64> {
    let exif = sides.exif_time.as_ref()?.utc?;
    Some((exif - sides.json_time?).abs())
}

fn gps_distance(sides: &Sides) -> Option<f64> {
    Some(haversine_meters(sides.json_gps?, sides.exif_gps?))
}

fn flags_for(sides: &Sides) -> Vec<&'static str> {
    let mut flags = Vec::new();
    match (sides.json_time, &sides.exif_time) {
        (None, None) => flags.extend(["time_missing_json", "time_missing_exif"]),
        (None, Some(_)) => flags.push("time_missing_json"),
        (Some(_), None) => flags.push("time_missing_exif"),
        (Some(_), Some(exif)) if exif.utc.is_none() => flags.push("time_unzoned"),
        _ => {}
    }
    if time_difference(sides).is_some_and(|diff| diff > TIME_THRESHOLD_SECS) {
        flags.push("time_disagree");
    }
    match (sides.json_gps, sides.exif_gps) {
        (None, None) => flags.extend(["gps_missing_json", "gps_missing_exif"]),
        (None, Some(_)) => flags.push("gps_missing_json"),
        (Some(_), None) => flags.push("gps_missing_exif"),
        _ => {}
    }
    if gps_distance(sides).is_some_and(|metres| metres > GPS_THRESHOLD_METERS) {
        flags.push("gps_disagree");
    }
    flags
}

/// Great-circle distance in metres between two (lat, lon) points.
pub fn haversine_meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let radius = 6_371_008.8_f64;
    let (lat1, lat2) = (a.0.to_radians(), b.0.to_radians());
    let dlat = (b.0 - a.0).to_radians();
    let dlon = (b.1 - a.1).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * radius * h.sqrt().asin()
}

type JsonSides = (Option<i64>, Option<(f64, f64)>);

fn read_json(path: &Path) -> Result<JsonSides, IngestError> {
    let text = std::fs::read_to_string(path).map_err(|err| IngestError::io(path, err))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| IngestError::Store {
            operation: "takeout-fidelity",
            filename: path.display().to_string(),
            message: err.to_string(),
        })?;
    let time = value
        .pointer("/photoTakenTime/timestamp")
        .and_then(|item| item.as_str())
        .and_then(|text| text.parse::<i64>().ok());
    // Google writes 0,0 when it has no location; that is missing, not a place.
    let gps = value.get("geoData").and_then(|geo| {
        let lat = geo.get("latitude")?.as_f64()?;
        let lon = geo.get("longitude")?.as_f64()?;
        (lat != 0.0 || lon != 0.0).then_some((lat, lon))
    });
    Ok((time, gps))
}

type ExifSides = (Option<ExifTime>, Option<(f64, f64)>);

fn read_exif(
    library: &Path,
    originals: &[String],
) -> Result<HashMap<String, ExifSides>, IngestError> {
    let mut map = HashMap::new();
    for chunk in originals.chunks(200) {
        let mut command = exiftool_command();
        command.args([
            "-j",
            "-n",
            "-EXIF:DateTimeOriginal",
            "-EXIF:OffsetTimeOriginal",
            "-Composite:GPSLatitude",
            "-Composite:GPSLongitude",
        ]);
        for rel in chunk {
            command.arg(library.join(rel));
        }
        let output = command
            .output()
            .map_err(|err| IngestError::io(Path::new("exiftool"), err))?;
        let parsed: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).unwrap_or_default();
        for (rel, value) in chunk.iter().zip(parsed.iter()) {
            map.insert(rel.clone(), exif_sides(value));
        }
    }
    Ok(map)
}

fn exif_sides(value: &serde_json::Value) -> ExifSides {
    let time = value["DateTimeOriginal"].as_str().map(|text| {
        let offset = value["OffsetTimeOriginal"].as_str();
        ExifTime {
            text: match offset {
                Some(offset) => format!("{text}{offset}"),
                None => text.to_string(),
            },
            utc: offset.and_then(|offset| exif_utc(text, offset)),
        }
    });
    let gps = match (
        value["GPSLatitude"].as_f64(),
        value["GPSLongitude"].as_f64(),
    ) {
        (Some(lat), Some(lon)) => Some((lat, lon)),
        _ => None,
    };
    (time, gps)
}

/// `2020:05:20 11:40:00` plus `-07:00` to unix seconds.
fn exif_utc(text: &str, offset: &str) -> Option<i64> {
    let (date, clock) = text.trim().split_once(' ')?;
    let mut date = date.split(':').map(|part| part.parse::<i32>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut clock = clock
        .get(..8)?
        .split(':')
        .map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let sign = match offset.get(..1)? {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let (oh, om) = offset.get(1..)?.split_once(':')?;
    let offset_secs = sign * (oh.parse::<i64>().ok()? * 3_600 + om.parse::<i64>().ok()? * 60);
    let days = crate::takeout::days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs)
}

fn utc_text(secs: i64) -> Option<String> {
    crate::takeout::utc_text(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_time_with_offset_converts_to_utc() {
        assert_eq!(
            exif_utc("2020:05:20 11:40:00", "-07:00"),
            Some(1_590_000_000)
        );
        assert_eq!(
            exif_utc("2020:05:20 18:40:00", "+00:00"),
            Some(1_590_000_000)
        );
    }

    #[test]
    fn haversine_is_metres() {
        let d = haversine_meters((37.5, -122.25), (37.501, -122.25));
        assert!((d - 111.2).abs() < 1.0, "{d}");
    }

    #[test]
    fn missing_values_are_flagged_not_filled() {
        let sides = Sides {
            json_time: None,
            exif_time: None,
            json_gps: None,
            exif_gps: None,
        };
        let line = describe(&sides);
        assert_eq!(line["time"]["winner"], "none");
        assert!(line["time"]["json_utc"].is_null());
        assert_eq!(
            flags_for(&sides),
            vec![
                "time_missing_json",
                "time_missing_exif",
                "gps_missing_json",
                "gps_missing_exif"
            ]
        );
    }
}
