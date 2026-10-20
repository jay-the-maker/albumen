// SPDX-License-Identifier: Apache-2.0

//! A local, read-only account of one library.
//!
//! `build` hashes the files in the library, then reads the analysis log and
//! the connection log beside it. It does not open a socket and it does not
//! write the library. Connection and analysis lines are appended by the
//! command that ran them, through [`crate::dest`]. A client records the same
//! connection line, with `via` set to `client`, in its own storage.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::checksum::sha256_hex;
use crate::dest;
use crate::error::IngestError;
use crate::source::SourceRoot;

const CONNECTION_LOG_ENV: &str = "ALBUMEN_CONNECTION_LOG";
const ANALYSIS_LOG_ENV: &str = "ALBUMEN_ANALYSIS_LOG";

thread_local! {
    static ANCHOR: RefCell<Option<(PathBuf, String)>> = const { RefCell::new(None) };
}

/// Remember which library a later network call belongs to.
///
/// The anchor is per thread. Export and the MCP server set it before they
/// contact a destination. The library directory itself is not written.
pub fn anchor(library: &Path, via: &str) {
    ANCHOR.with(|slot| {
        *slot.borrow_mut() = Some((library.to_path_buf(), via.to_string()));
    });
}

fn anchored() -> Option<(PathBuf, String)> {
    ANCHOR.with(|slot| slot.borrow().clone())
}

static LEDGER_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

const HOME_ENV: &str = "ALBUMEN_HOME";

/// Use `path` as the ledger directory for this process (`--ledger`).
///
/// Refused when `path` resolves inside `source` or inside the directory that
/// holds `source`. Nothing is created here.
pub fn set_ledger_dir(path: &Path, source: &Path) -> Result<(), IngestError> {
    refuse_near_source(path, source)?;
    *LEDGER_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(absolute(path));
    Ok(())
}

/// Directory that holds the ledger logs for `library`.
///
/// `--ledger` wins. Otherwise the logs live under `$ALBUMEN_HOME/ledgers`, or
/// the platform data directory, in a folder keyed by the canonical source
/// path. It is never inside or beside the source.
pub fn directory(library: &Path) -> PathBuf {
    if let Some(path) = LEDGER_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return path;
    }
    let canonical = library.canonicalize().unwrap_or_else(|_| absolute(library));
    let home =
        env_path(HOME_ENV).or_else(|| crate::platform::platform_dirs().ok().map(|(_, data)| data));
    let home = home.unwrap_or_else(|| std::env::temp_dir().join("albumen"));
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "library".to_string());
    let digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_os_str().to_string_lossy().as_bytes());
        hasher.finalize()
    };
    let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    home.join("ledgers").join(format!("{name}-{key}"))
}

/// The pre-fix location, `<parent>/<name>.ledger`. Read only; never written
/// or removed.
pub fn legacy_directory(library: &Path) -> PathBuf {
    let library = library
        .canonicalize()
        .unwrap_or_else(|_| library.to_path_buf());
    let file_name = library
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("library"));
    let mut name = file_name.to_os_string();
    name.push(".ledger");
    match library.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Resolve `path` through its nearest existing ancestor.
fn resolve(path: &Path) -> PathBuf {
    let path = absolute(path);
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let mut out = canonical;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path,
        }
    }
}

fn starts_with_folded(path: &Path, root: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        let p = path.to_string_lossy().to_lowercase();
        let r = root.to_string_lossy().to_lowercase();
        Path::new(&p).starts_with(Path::new(&r))
    } else {
        path.starts_with(root)
    }
}

/// Refuse a ledger directory inside `source`, or in the folder that holds it.
pub fn refuse_near_source(ledger: &Path, source: &Path) -> Result<(), IngestError> {
    let ledger = resolve(ledger);
    let source = resolve(source);
    let parent = source.parent().map(Path::to_path_buf);
    // Inside the source, the parent folder itself, or a direct child of the
    // parent (a sibling of the source, like the old `<dir>.ledger`).
    let same = |a: &Path, b: &Path| starts_with_folded(a, b) && starts_with_folded(b, a);
    let near = starts_with_folded(&ledger, &source)
        || parent.as_deref().is_some_and(|parent| {
            same(&ledger, parent) || ledger.parent().is_some_and(|up| same(up, parent))
        });
    if near {
        return Err(IngestError::store(
            "ledger",
            ledger.display().to_string(),
            "ledger directory is inside the source or in the folder that holds it",
        ));
    }
    Ok(())
}

/// Copy a pre-fix `<dir>.ledger` into the new location once. The old folder
/// is only read.
fn migrate_legacy(library: &Path, target: &Path) -> Result<(), IngestError> {
    let legacy = legacy_directory(library);
    if !legacy.is_dir() || legacy == target {
        return Ok(());
    }
    for name in ["analyses.jsonl", "connections.jsonl"] {
        let from = legacy.join(name);
        let to = target.join(name);
        if from.is_file() && !to.exists() {
            let file = File::open(&from).map_err(|err| IngestError::io(&from, err))?;
            for line in BufReader::new(file).lines() {
                let line = line.map_err(|err| IngestError::io(&from, err))?;
                if !line.trim().is_empty() {
                    dest::append_line(&to, line.trim())?;
                }
            }
        }
    }
    Ok(())
}

/// One stored file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub member: Option<String>,
}

/// One analysis that ran.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Analysis {
    pub tool: String,
    #[serde(rename = "where")]
    pub place: String,
    pub at: String,
}

/// One network destination a command contacted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Destination {
    pub via: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub at: String,
}

/// The account `albumen ledger` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub object_count: u64,
    pub objects: Vec<Object>,
    pub analyses: Vec<Analysis>,
    pub destinations: Vec<Destination>,
}

impl Ledger {
    pub fn to_json(&self) -> Result<String, IngestError> {
        serde_json::to_string_pretty(self)
            .map_err(|err| IngestError::store("ledger", "json", err.to_string()))
    }

    pub fn from_json(text: &str) -> Result<Self, IngestError> {
        serde_json::from_str(text)
            .map_err(|err| IngestError::store("ledger", "json", err.to_string()))
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from("ledger\n");
        text.push_str(&format!("  objects: {}\n", self.object_count));
        for object in &self.objects {
            text.push_str(&format!(
                "  object {} sha256={} bytes={}",
                object.path, object.sha256, object.bytes
            ));
            if let Some(member) = &object.member {
                text.push_str(&format!(" member={member}"));
            }
            text.push('\n');
        }
        text.push_str(&format!("  analyses: {}\n", self.analyses.len()));
        for analysis in &self.analyses {
            text.push_str(&format!(
                "  analysis {} {} {}\n",
                analysis.tool, analysis.place, analysis.at
            ));
        }
        text.push_str(&format!("  destinations: {}\n", self.destinations.len()));
        for destination in &self.destinations {
            text.push_str(&format!(
                "  destination {} {} {}:{} {}\n",
                destination.via,
                destination.kind,
                destination.host,
                destination.port,
                destination.at
            ));
        }
        text
    }
}

/// Hash every file in `library` and read the logs beside it.
///
/// `connection_log` is an extra file, such as the one the egress job collects.
/// The library is opened read-only. This function does not connect anywhere.
pub fn build(library: &Path, connection_log: Option<&Path>) -> Result<Ledger, IngestError> {
    if !library.is_dir() {
        return Err(IngestError::store(
            "ledger",
            library.display().to_string(),
            "library is not a directory",
        ));
    }
    let source = SourceRoot::open(library)?;
    let mut relative_paths = Vec::new();
    source.walk(None, &mut |relative| {
        relative_paths.push(relative.to_string());
        true
    })?;
    let mut listed = Vec::with_capacity(relative_paths.len());
    for relative in relative_paths {
        let path = source.canonical().join(relative_path(&relative));
        let meta = source.metadata_nofollow(&path)?;
        if !meta.is_file() {
            continue;
        }
        listed.push(Object {
            member: member_of(&relative),
            path: relative,
            sha256: sha256_hex(&path)?,
            bytes: meta.len(),
        });
    }
    let analyses = read_analyses(library)?;
    let mut destinations =
        read_records::<Destination>(&directory(library).join("connections.jsonl"))?;
    destinations.extend(read_records::<Destination>(
        &legacy_directory(library).join("connections.jsonl"),
    )?);
    if let Some(extra) = connection_log {
        let sibling = directory(library).join("connections.jsonl");
        if extra != sibling {
            destinations.extend(read_records::<Destination>(extra)?);
        }
    } else if let Some(extra) = env_path(CONNECTION_LOG_ENV) {
        let sibling = directory(library).join("connections.jsonl");
        if extra != sibling {
            destinations.extend(read_records::<Destination>(&extra)?);
        }
    }
    destinations.sort();
    destinations.dedup();
    Ok(Ledger {
        object_count: listed.len() as u64,
        objects: listed,
        analyses,
        destinations,
    })
}

fn read_analyses(library: &Path) -> Result<Vec<Analysis>, IngestError> {
    let mut analyses = read_records::<Analysis>(&directory(library).join("analyses.jsonl"))?;
    analyses.extend(read_records::<Analysis>(
        &legacy_directory(library).join("analyses.jsonl"),
    )?);
    if let Some(extra) = env_path(ANALYSIS_LOG_ENV) {
        let sibling = directory(library).join("analyses.jsonl");
        if extra != sibling {
            analyses.extend(read_records::<Analysis>(&extra)?);
        }
    }
    analyses.sort();
    analyses.dedup();
    Ok(analyses)
}

fn read_records<T: for<'de> Deserialize<'de> + Ord>(path: &Path) -> Result<Vec<T>, IngestError> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|err| IngestError::io(path, err))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|err| IngestError::io(path, err))?;
        let line = line.trim();
        if line.is_empty() || !seen.insert(line.to_string()) {
            continue;
        }
        let record = serde_json::from_str(line).map_err(|err| {
            IngestError::store("ledger", path.display().to_string(), err.to_string())
        })?;
        out.push(record);
    }
    Ok(out)
}

/// Append one analysis line. `place` is where it ran, such as `on-device`.
pub fn record_analysis(library: Option<&Path>, tool: &str, place: &str) -> Result<(), IngestError> {
    let library = library
        .map(Path::to_path_buf)
        .or_else(|| anchored().map(|(path, _)| path));
    let record = Analysis {
        tool: tool.to_string(),
        place: place.to_string(),
        at: timestamp(),
    };
    let line = serde_json::to_string(&record)
        .map_err(|err| IngestError::store("ledger", "analysis", err.to_string()))?;
    write_log(
        &line,
        library.as_deref(),
        "analyses.jsonl",
        ANALYSIS_LOG_ENV,
    )
}

/// Append one destination the process is about to contact.
pub fn record_connection(
    library: Option<&Path>,
    via: &str,
    kind: &str,
    host: &str,
    port: u16,
) -> Result<(), IngestError> {
    if host.is_empty() || host.chars().any(|ch| ch.is_control()) {
        return Err(IngestError::store(
            "ledger",
            host,
            "destination host is empty",
        ));
    }
    let library = library
        .map(Path::to_path_buf)
        .or_else(|| anchored().map(|(path, _)| path));
    let via = if via.is_empty() {
        anchored()
            .map(|(_, via)| via)
            .unwrap_or_else(|| "export".to_string())
    } else {
        via.to_string()
    };
    let record = Destination {
        via,
        kind: kind.to_string(),
        host: host.to_string(),
        port,
        at: timestamp(),
    };
    let line = serde_json::to_string(&record)
        .map_err(|err| IngestError::store("ledger", "connection", err.to_string()))?;
    write_log(
        &line,
        library.as_deref(),
        "connections.jsonl",
        CONNECTION_LOG_ENV,
    )
}

/// Record the anchored library, or the process environment, before a connect.
pub fn record_anchored_connection(kind: &str, host: &str, port: u16) -> Result<(), IngestError> {
    let via = anchored()
        .map(|(_, via)| via)
        .unwrap_or_else(|| "export".to_string());
    let library = anchored().map(|(path, _)| path);
    record_connection(library.as_deref(), &via, kind, host, port)
}

fn write_log(
    line: &str,
    library: Option<&Path>,
    file_name: &str,
    env_name: &str,
) -> Result<(), IngestError> {
    let mut paths = Vec::new();
    if let Some(library) = library {
        let dir = directory(library);
        refuse_near_source(&dir, library)?;
        migrate_legacy(library, &dir)?;
        paths.push(dir.join(file_name));
    }
    if let Some(extra) = env_path(env_name) {
        if !paths.iter().any(|path| path == &extra) {
            paths.push(extra);
        }
    }
    for path in paths {
        dest::append_line(&path, line)?;
    }
    Ok(())
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var(name).ok()?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn relative_path(relative: &str) -> PathBuf {
    relative.split('/').collect()
}

fn member_of(relative: &str) -> Option<String> {
    let mut parts = relative.split('/');
    if parts.next() != Some("members") {
        return None;
    }
    let member = parts.next()?;
    if member.is_empty() || member == "." || member == ".." {
        return None;
    }
    Some(member.to_string())
}

/// Host and port of an `http://` or `https://` endpoint.
pub fn endpoint_host_port(endpoint: &str) -> Result<(String, u16), IngestError> {
    let endpoint = endpoint.trim();
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| IngestError::store("ledger", endpoint, "endpoint needs a scheme"))?;
    let default_port: u16 = match scheme {
        "https" => 443,
        "http" => 80,
        _ => {
            return Err(IngestError::store(
                "ledger",
                endpoint,
                "endpoint scheme must be http or https",
            ))
        }
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(host) = authority.strip_prefix('[') {
        let (host, port) = host
            .split_once(']')
            .ok_or_else(|| IngestError::store("ledger", endpoint, "endpoint host is not closed"))?;
        let port = if let Some(port) = port.strip_prefix(':') {
            parse_port(endpoint, port)?
        } else if port.is_empty() {
            default_port
        } else {
            return Err(IngestError::store(
                "ledger",
                endpoint,
                "endpoint host is not a host and port",
            ));
        };
        return Ok((host.to_string(), port));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && !host.contains(':') {
            return Ok((host.to_string(), parse_port(endpoint, port)?));
        }
    }
    if authority.is_empty() {
        return Err(IngestError::store(
            "ledger",
            endpoint,
            "endpoint host is empty",
        ));
    }
    Ok((authority.to_string(), default_port))
}

fn parse_port(endpoint: &str, port: &str) -> Result<u16, IngestError> {
    port.parse::<u16>()
        .map_err(|_| IngestError::store("ledger", endpoint, "endpoint port is not a number"))
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a civil date. Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Tmp;
    use std::fs;

    #[test]
    fn endpoint_keeps_the_explicit_port() {
        assert_eq!(
            endpoint_host_port("http://127.0.0.1:19000/library").unwrap(),
            ("127.0.0.1".to_string(), 19000)
        );
        assert_eq!(
            endpoint_host_port("https://s3.example.com").unwrap(),
            ("s3.example.com".to_string(), 443)
        );
        assert_eq!(
            endpoint_host_port("http://[::1]:9000").unwrap(),
            ("::1".to_string(), 9000)
        );
    }

    #[test]
    fn unix_epoch_day_is_civil() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2023-11-14 is 19675 days after 1970-01-01.
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
    }

    #[test]
    fn ledger_lists_every_file_and_the_recorded_destination() {
        let tmp = Tmp::new();
        let library = tmp.path().join("library");
        fs::create_dir_all(library.join("members/ab")).unwrap();
        fs::write(library.join("note.txt"), b"note").unwrap();
        fs::write(library.join("members/ab/photo.jpg"), b"jpeg").unwrap();
        let before = fs::read(library.join("note.txt")).unwrap();
        let before_mtime = fs::metadata(library.join("note.txt"))
            .unwrap()
            .modified()
            .unwrap();

        record_analysis(Some(&library), "exiftool", "on-device").unwrap();
        record_connection(Some(&library), "export", "connect", "127.0.0.1", 19000).unwrap();
        record_connection(Some(&library), "receive", "bind", "127.0.0.1", 18080).unwrap();

        let ledger = build(&library, None).unwrap();
        assert_eq!(ledger.object_count, 2);
        assert_eq!(ledger.objects.len(), 2);
        let note = ledger
            .objects
            .iter()
            .find(|object| object.path == "note.txt")
            .unwrap();
        assert_eq!(note.bytes, 4);
        assert_eq!(
            note.sha256,
            "edb465624291e4053c6c5ea4b7eb320dec773e10a57d26b95dcf0564f8e310f8"
        );
        assert_eq!(note.member, None);
        let photo = ledger
            .objects
            .iter()
            .find(|object| object.path == "members/ab/photo.jpg")
            .unwrap();
        assert_eq!(photo.member.as_deref(), Some("ab"));
        assert_eq!(photo.bytes, 4);
        assert_eq!(
            photo.sha256,
            sha256_hex(&library.join("members/ab/photo.jpg")).unwrap()
        );
        assert_eq!(ledger.analyses.len(), 1);
        assert_eq!(ledger.analyses[0].tool, "exiftool");
        assert_eq!(ledger.analyses[0].place, "on-device");
        assert!(ledger.analyses[0].at.ends_with('Z'));
        assert!(ledger.destinations.iter().any(|destination| {
            destination.via == "export"
                && destination.kind == "connect"
                && destination.host == "127.0.0.1"
                && destination.port == 19000
        }));
        assert!(ledger
            .destinations
            .iter()
            .any(|destination| { destination.kind == "bind" && destination.port == 18080 }));
        assert_eq!(fs::read(library.join("note.txt")).unwrap(), before);
        assert_eq!(
            fs::metadata(library.join("note.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            before_mtime
        );
        assert!(!library.join("connections.jsonl").exists());
        let text = ledger.to_text();
        assert!(text.contains("objects: 2"), "{text}");
        assert!(text.contains("member=ab"), "{text}");
        assert!(text.contains("analysis exiftool on-device"), "{text}");
        assert!(
            text.contains("destination export connect 127.0.0.1:19000"),
            "{text}"
        );
    }
}
