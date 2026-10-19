// SPDX-License-Identifier: Apache-2.0

//! Read album, favorite, and caption rows from a copied Photos library.
//!
//! The database is opened with SQLite's `mode=ro&immutable=1` URI. That mode
//! does not create a journal, a WAL, or a shared-memory file, and it does not
//! write the database. Rows are visited one asset at a time.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::IngestError;

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

pub(crate) enum MetaRow {
    Ready {
        relative: String,
        favorite: bool,
        caption: Option<String>,
        albums: Vec<String>,
        /// UTC `YYYY-MM-DDTHH:MM:SSZ` from `ZDATECREATED`, when that column exists.
        capture_time: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
    },
    Missing {
        filename: String,
    },
}

pub(crate) fn for_each_metadata(
    root: &Path,
    mut emit: impl FnMut(MetaRow) -> Result<(), IngestError>,
) -> Result<(), IngestError> {
    let Some(database) = find_database(root) else {
        return Ok(());
    };
    if !is_sqlite(&database)? {
        return Ok(());
    }
    let conn = open_immutable(&database)?;
    let lookup = open_immutable(&database)?;
    let Some(asset) = asset_table(&conn)? else {
        return Ok(());
    };
    let caption_sql = caption_query(&conn)?;
    let album_sql = album_queries(&conn)?;
    let mut assets = conn
        .prepare(&asset.select)
        .map_err(|err| sqlite_err(&database, err))?;
    let mut rows = assets.query([]).map_err(|err| sqlite_err(&database, err))?;
    while let Some(row) = rows.next().map_err(|err| sqlite_err(&database, err))? {
        let pk: i64 = row.get(0).map_err(|err| sqlite_err(&database, err))?;
        let filename: Option<String> = row.get(1).map_err(|err| sqlite_err(&database, err))?;
        let directory: Option<String> = row.get(2).map_err(|err| sqlite_err(&database, err))?;
        let favorite_raw: Option<i64> = row.get(3).map_err(|err| sqlite_err(&database, err))?;
        let created_raw: Option<f64> = if asset.has_date {
            row.get(4).map_err(|err| sqlite_err(&database, err))?
        } else {
            None
        };
        let latitude: Option<f64> = if asset.has_gps {
            row.get(if asset.has_date { 5 } else { 4 })
                .map_err(|err| sqlite_err(&database, err))?
        } else {
            None
        };
        let longitude: Option<f64> = if asset.has_gps {
            row.get(if asset.has_date { 6 } else { 5 })
                .map_err(|err| sqlite_err(&database, err))?
        } else {
            None
        };
        let Some(filename) = filename.filter(|name| !name.is_empty()) else {
            continue;
        };
        let favorite = favorite_raw == Some(1);
        let caption = caption_for(&lookup, caption_sql.as_deref(), pk, &database)?;
        let albums = albums_for(&lookup, &album_sql, pk, &database)?;
        let caption_present = caption.as_ref().is_some_and(|text| !text.is_empty());
        let capture_time = created_raw.and_then(apple_timestamp_utc);
        let gps = match (latitude, longitude) {
            (Some(lat), Some(lon)) if !(lat == 0.0 && lon == 0.0) => Some((lat, lon)),
            _ => None,
        };
        if !favorite
            && !caption_present
            && albums.is_empty()
            && capture_time.is_none()
            && gps.is_none()
        {
            continue;
        }
        let caption = caption.filter(|text| !text.is_empty());
        match locate(root, directory.as_deref().unwrap_or(""), &filename) {
            Some(relative) => emit(MetaRow::Ready {
                relative,
                favorite,
                caption,
                albums,
                capture_time,
                latitude: gps.map(|(lat, _)| lat),
                longitude: gps.map(|(_, lon)| lon),
            })?,
            None => emit(MetaRow::Missing { filename })?,
        }
    }
    Ok(())
}

struct AssetSql {
    select: String,
    has_date: bool,
    has_gps: bool,
}

/// Photos stores `ZDATECREATED` as seconds since 2001-01-01 00:00:00 UTC.
fn apple_timestamp_utc(seconds: f64) -> Option<String> {
    if !seconds.is_finite() {
        return None;
    }
    let unix = seconds + 978_307_200.0;
    if unix < 0.0 {
        return None;
    }
    let secs = unix.floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days)?;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn civil_from_days(mut z: i64) -> Option<(i32, u32, u32)> {
    z = z.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let year = i32::try_from(year).ok()?;
    Some((year, u32::try_from(month).ok()?, u32::try_from(day).ok()?))
}

fn find_database(root: &Path) -> Option<PathBuf> {
    for relative in [
        "database/Photos.sqlite",
        "Database/Photos.sqlite",
        "Photos.sqlite",
    ] {
        let path = root.join(relative);
        if std::fs::symlink_metadata(&path)
            .map(|meta| meta.is_file())
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

fn is_sqlite(path: &Path) -> Result<bool, IngestError> {
    let mut file = File::open(path).map_err(|err| IngestError::io(path, err))?;
    let mut magic = [0u8; 16];
    let n = file
        .read(&mut magic)
        .map_err(|err| IngestError::io(path, err))?;
    Ok(n == magic.len() && magic == SQLITE_MAGIC)
}

fn open_immutable(path: &Path) -> Result<Connection, IngestError> {
    let uri = sqlite_uri(path)?;
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|err| sqlite_err(path, err))?;
    conn.pragma_update(None, "query_only", "ON")
        .map_err(|err| sqlite_err(path, err))?;
    Ok(conn)
}

fn sqlite_uri(path: &Path) -> Result<String, IngestError> {
    let absolute = path
        .canonicalize()
        .map_err(|err| IngestError::io(path, err))?;
    let text = absolute.to_string_lossy();
    let mut uri = String::from("file:");
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(*byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

fn asset_table(conn: &Connection) -> Result<Option<AssetSql>, IngestError> {
    let table = if table_exists(conn, "ZASSET")? {
        "ZASSET"
    } else if table_exists(conn, "ZGENERICASSET")? {
        "ZGENERICASSET"
    } else {
        return Ok(None);
    };
    if !column_exists(conn, table, "Z_PK")? || !column_exists(conn, table, "ZFILENAME")? {
        return Err(IngestError::store(
            "icloud",
            table,
            "Photos database has no asset filename column",
        ));
    }
    let directory = if column_exists(conn, table, "ZDIRECTORY")? {
        "ZDIRECTORY"
    } else {
        "NULL"
    };
    let favorite = if column_exists(conn, table, "ZFAVORITE")? {
        "ZFAVORITE"
    } else {
        "NULL"
    };
    let has_date = column_exists(conn, table, "ZDATECREATED")?;
    let has_gps =
        column_exists(conn, table, "ZLATITUDE")? && column_exists(conn, table, "ZLONGITUDE")?;
    check_ident(table)?;
    if directory != "NULL" {
        check_ident(directory)?;
    }
    if favorite != "NULL" {
        check_ident(favorite)?;
    }
    if has_date {
        check_ident("ZDATECREATED")?;
    }
    let gps_sql = if has_gps {
        ", ZLATITUDE, ZLONGITUDE"
    } else {
        ""
    };
    let date_comma = if has_date { ", ZDATECREATED" } else { "" };
    Ok(Some(AssetSql {
        select: format!(
            "SELECT Z_PK, ZFILENAME, {directory}, {favorite}{date_comma}{gps_sql} FROM {table} ORDER BY Z_PK"
        ),
        has_date,
        has_gps,
    }))
}

fn caption_query(conn: &Connection) -> Result<Option<String>, IngestError> {
    if !table_exists(conn, "ZADDITIONALASSETATTRIBUTES")? {
        return Ok(None);
    }
    if !column_exists(conn, "ZADDITIONALASSETATTRIBUTES", "ZASSET")?
        || !column_exists(conn, "ZADDITIONALASSETATTRIBUTES", "ZTITLE")?
    {
        return Ok(None);
    }
    Ok(Some(
        "SELECT ZTITLE FROM ZADDITIONALASSETATTRIBUTES WHERE ZASSET = ?1 LIMIT 1".to_string(),
    ))
}

fn album_queries(conn: &Connection) -> Result<Vec<String>, IngestError> {
    let album = if table_exists(conn, "ZGENERICALBUM")? {
        "ZGENERICALBUM"
    } else if table_exists(conn, "ZALBUM")? {
        "ZALBUM"
    } else {
        return Ok(Vec::new());
    };
    if !column_exists(conn, album, "Z_PK")? || !column_exists(conn, album, "ZTITLE")? {
        return Ok(Vec::new());
    }
    check_ident(album)?;
    let mut sql = Vec::new();
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name GLOB 'Z_*ASSETS' ORDER BY name")
        .map_err(|err| IngestError::store("icloud", "sqlite_master", err))?;
    let names = tables
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| IngestError::store("icloud", "sqlite_master", err))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| IngestError::store("icloud", "sqlite_master", err))?;
    for name in names {
        if check_ident(&name).is_err() {
            continue;
        }
        let columns = columns_of(conn, &name)?;
        let Some(album_col) = columns.iter().find(|col| col.ends_with("ALBUMS")) else {
            continue;
        };
        let Some(asset_col) = columns.iter().find(|col| col.ends_with("ASSETS")) else {
            continue;
        };
        if album_col == asset_col
            || check_ident(album_col).is_err()
            || check_ident(asset_col).is_err()
        {
            continue;
        }
        sql.push(format!(
            "SELECT a.ZTITLE FROM {name} j JOIN {album} a ON a.Z_PK = j.{album_col} WHERE j.{asset_col} = ?1 AND a.ZTITLE IS NOT NULL AND a.ZTITLE != ''"
        ));
    }
    Ok(sql)
}

fn caption_for(
    conn: &Connection,
    sql: Option<&str>,
    pk: i64,
    database: &Path,
) -> Result<Option<String>, IngestError> {
    let Some(sql) = sql else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(sql).map_err(|err| sqlite_err(database, err))?;
    let mut rows = stmt.query([pk]).map_err(|err| sqlite_err(database, err))?;
    if let Some(row) = rows.next().map_err(|err| sqlite_err(database, err))? {
        return row.get(0).map_err(|err| sqlite_err(database, err));
    }
    Ok(None)
}

fn albums_for(
    conn: &Connection,
    queries: &[String],
    pk: i64,
    database: &Path,
) -> Result<Vec<String>, IngestError> {
    let mut albums = Vec::new();
    for sql in queries {
        let mut stmt = conn.prepare(sql).map_err(|err| sqlite_err(database, err))?;
        let mut rows = stmt.query([pk]).map_err(|err| sqlite_err(database, err))?;
        while let Some(row) = rows.next().map_err(|err| sqlite_err(database, err))? {
            let title: String = row.get(0).map_err(|err| sqlite_err(database, err))?;
            if !title.is_empty() {
                albums.push(title);
            }
        }
    }
    albums.sort();
    albums.dedup();
    Ok(albums)
}

fn locate(root: &Path, directory: &str, filename: &str) -> Option<String> {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return None;
    }
    let mut candidates = Vec::new();
    if !directory.is_empty()
        && !directory.contains("..")
        && !directory.contains('\\')
        && !directory.starts_with('/')
    {
        candidates.push(format!("originals/{directory}/{filename}"));
        candidates.push(format!("Masters/{directory}/{filename}"));
        candidates.push(format!("{directory}/{filename}"));
    }
    candidates.push(format!("originals/{filename}"));
    candidates.push(format!("Masters/{filename}"));
    candidates.push(filename.to_string());
    candidates
        .into_iter()
        .find(|relative| fs_is_file(&root.join(relative)))
}

fn fs_is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, IngestError> {
    match conn.query_row(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, String>(0),
    ) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(err) => Err(IngestError::store("icloud", name, err)),
    }
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, IngestError> {
    Ok(columns_of(conn, table)?.iter().any(|name| name == column))
}

fn columns_of(conn: &Connection, table: &str) -> Result<Vec<String>, IngestError> {
    check_ident(table)?;
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| IngestError::store("icloud", table, err))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| IngestError::store("icloud", table, err))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| IngestError::store("icloud", table, err))?;
    Ok(names)
}

fn check_ident(name: &str) -> Result<(), IngestError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
    if ok {
        Ok(())
    } else {
        Err(IngestError::store(
            "icloud",
            name,
            "refusing a Photos database identifier that is not a plain name",
        ))
    }
}

fn sqlite_err(path: &Path, err: impl ToString) -> IngestError {
    IngestError::store("icloud", path.display().to_string(), err.to_string())
}
