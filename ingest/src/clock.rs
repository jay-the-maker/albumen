// SPDX-License-Identifier: Apache-2.0

//! The one time label for people: "2026-10-29 17:42 UTC-7". Backup health
//! (`albumen status` and the GUI) and MCP search use it on every OS. The
//! offset is numeric, "UTC+5:30" for part-hour zones and "UTC" at zero.
//! Zone abbreviations such as PDT are never printed.

/// "UTC", "UTC-7", "UTC+5:30" from minutes east of UTC.
pub fn offset_label(minutes: i64) -> String {
    if minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// Unix seconds shown at a given offset: "2026-10-29 17:42 UTC-7".
pub fn stamp_at(unix: i64, offset_minutes: i64) -> String {
    let local = unix + offset_minutes * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
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
        "{year:04}-{month:02}-{day:02} {:02}:{:02} {}",
        secs / 3_600,
        (secs % 3_600) / 60,
        offset_label(offset_minutes)
    )
}

/// Unix seconds in this computer's zone. The offset comes from the OS
/// (`TZ` and `localtime_r` on Linux and macOS, the Windows zone on
/// Windows). If the OS cannot say, the UTC time is shown and named "UTC".
pub fn local_stamp(unix: i64) -> String {
    stamp_at(unix, albumen_win::utc_offset_minutes(unix).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-30T00:42:00Z.
    const T: i64 = 1_793_320_920;

    #[test]
    fn pacific_daylight_is_minus_7() {
        assert_eq!(offset_label(-420), "UTC-7");
        assert_eq!(stamp_at(T, -420), "2026-10-29 17:42 UTC-7");
    }

    #[test]
    fn india_is_plus_5_30() {
        assert_eq!(offset_label(330), "UTC+5:30");
        assert_eq!(stamp_at(T, 330), "2026-10-30 06:12 UTC+5:30");
    }

    #[test]
    fn zero_offset_is_plain_utc() {
        assert_eq!(offset_label(0), "UTC");
        assert_eq!(stamp_at(T, 0), "2026-10-30 00:42 UTC");
        assert_eq!(stamp_at(0, 0), "1970-01-01 00:00 UTC");
        assert_eq!(stamp_at(951_782_400, 0), "2000-02-29 00:00 UTC");
        assert_eq!(stamp_at(-60, 0), "1969-12-31 23:59 UTC");
    }

    #[test]
    fn other_part_hour_offsets() {
        assert_eq!(offset_label(-570), "UTC-9:30");
        assert_eq!(offset_label(345), "UTC+5:45");
    }

    #[test]
    fn local_stamp_never_prints_an_abbreviation() {
        let s = local_stamp(T);
        let zone = &s[17..];
        assert!(zone.starts_with("UTC"), "{s}");
        assert!(
            zone[3..]
                .chars()
                .all(|c| c.is_ascii_digit() || "+-:".contains(c)),
            "{s}"
        );
    }
}
