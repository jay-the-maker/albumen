// SPDX-License-Identifier: Apache-2.0

//! `albumen gui`: a small local web page over the CLI.
//!
//! Serves on 127.0.0.1 only. The page lists the library (read-only), runs
//! `scan`, `import-card`, `backup`, and `verify-manifest` as child processes
//! of this same binary, and shows `albumen status` for each backup folder.
//! Every write goes through those commands, so the GUI has no write path of
//! its own into a library or a source. `--demo` builds a synthetic library.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use albumen_ingest::{IngestError, SourceRoot};

pub mod demo;
pub mod http;
pub mod listing;
pub mod thumb;
pub mod work;

const PAGE: &str = include_str!("page.html");

/// Image types the grid shows as thumbnails.
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "avif"];

pub struct GuiArgs {
    pub library: Option<PathBuf>,
    pub backups: Vec<String>,
    pub card: Option<PathBuf>,
    pub work: Option<PathBuf>,
    pub demo: bool,
    pub port: u16,
    pub open: bool,
    pub max_age: String,
}

/// What the server knows. Paths are fixed at start.
pub struct Config {
    pub library: PathBuf,
    pub backups: Vec<String>,
    pub card: Option<PathBuf>,
    /// Folder for scan manifests and backup state, outside the library.
    pub state: PathBuf,
    pub max_age_secs: u64,
    /// True for `--demo`: the page shows the synthetic-images banner.
    pub demo: bool,
}

impl Config {
    /// Grid previews are cached here, inside the work folder only.
    pub fn thumbs(&self) -> PathBuf {
        self.state.join("thumbs")
    }
}

#[derive(Default)]
struct Job {
    kind: String,
    lines: Vec<String>,
    done: u64,
    total: u64,
    exit: Option<i32>,
}

struct Server {
    config: Config,
    jobs: Mutex<Vec<Job>>,
    /// The last finished scan, passed as `--previous` to the next one.
    last_scan: Mutex<Option<PathBuf>>,
}

pub fn run(args: GuiArgs) -> Result<(), IngestError> {
    let max_age_secs = crate::status::parse_max_age(&args.max_age)
        .map_err(|message| IngestError::Usage { message })?;
    let config = if args.demo {
        let work = args
            .work
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("albumen-gui-demo"));
        let built = demo::build(&work)?;
        Config {
            library: built.library,
            backups: built.backups,
            card: Some(built.card),
            state: built.state,
            max_age_secs,
            demo: true,
        }
    } else {
        let library = args.library.clone().ok_or_else(|| IngestError::Usage {
            message: "albumen gui needs --library DIR, or --demo".into(),
        })?;
        let state = match &args.work {
            Some(work) => work.clone(),
            None => albumen_ingest::ledger::directory(&library).join("gui"),
        };
        Config {
            library,
            backups: args.backups.clone(),
            card: args.card.clone(),
            state,
            max_age_secs,
            demo: false,
        }
    };
    // Scan state and the preview cache both live in the work folder, and
    // neither may be inside (or hard-linked into) the library.
    let thumbs = config.thumbs();
    SourceRoot::open(&config.library)?.refuse_destinations(&[&config.state, &thumbs])?;
    thumb::refuse_cache_in_library(&config.library, &thumbs)?;
    work::ensure(&config.library, &config.state)?;
    let listener = TcpListener::bind(("127.0.0.1", args.port))
        .map_err(|err| io_err(Path::new("127.0.0.1"), err))?;
    let port = listener
        .local_addr()
        .map_err(|err| io_err(Path::new("127.0.0.1"), err))?
        .port();
    let url = format!("http://127.0.0.1:{port}/");
    // The banner is best-effort: a closed stdout (`albumen gui | head -1`)
    // returns EPIPE, which println! would turn into a panic. Ignore write
    // errors and keep serving.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "albumen gui: {url}");
    let _ = writeln!(out, "  library: {}", config.library.display());
    for b in &config.backups {
        let _ = writeln!(out, "  backup: {b}");
    }
    let _ = writeln!(out, "  Ctrl-C stops it.");
    let _ = out.flush();
    drop(out);
    if args.open {
        open_browser(&url);
    }
    serve(listener, config, port);
    Ok(())
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn serve(listener: TcpListener, config: Config, port: u16) {
    let server = Arc::new(Server {
        config,
        jobs: Mutex::new(Vec::new()),
        last_scan: Mutex::new(None),
    });
    for stream in listener.incoming().flatten() {
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = handle(&server, stream, port);
        });
    }
}

fn handle(server: &Arc<Server>, mut stream: TcpStream, port: u16) -> std::io::Result<()> {
    let Some(req) = http::read_request(&mut stream)? else {
        return Ok(());
    };
    // Only this page may call the API: the Host must be our loopback
    // address (no DNS rebinding) and writes need a custom header.
    let host_ok = matches!(
        req.header("host"),
        Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    );
    if !host_ok {
        return http::respond(&mut stream, 403, "text/plain", b"forbidden host");
    }
    if req.method == "POST" && req.header("x-albumen") != Some("1") {
        return http::respond(&mut stream, 403, "text/plain", b"missing X-Albumen header");
    }
    let (path, _) = req.target.split_once('?').unwrap_or((&req.target, ""));
    match (req.method.as_str(), path) {
        ("GET", "/") => http::respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        ),
        ("GET", "/api/library") => {
            let scan = server.last_scan.lock().unwrap().clone();
            let body = library_view(&server.config, scan.as_deref()).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/status") => {
            let body = status_json(&server.config, SystemTime::now()).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/img") => {
            let rel = http::query_param(&req.target, "p").unwrap_or_default();
            match read_image(&server.config.library, &rel) {
                Some((bytes, mime)) => http::respond(&mut stream, 200, mime, &bytes),
                None => http::respond(&mut stream, 404, "text/plain", b"not found"),
            }
        }
        ("GET", "/thumb") => {
            let rel = http::query_param(&req.target, "p").unwrap_or_default();
            if image_path(&server.config.library, &rel).is_none() {
                return http::respond(&mut stream, 404, "text/plain", b"not found");
            }
            match thumb::preview(&server.config.library, &server.config.thumbs(), &rel) {
                Ok(thumb::Preview::Jpeg(bytes)) => {
                    http::respond(&mut stream, 200, "image/jpeg", &bytes)
                }
                Ok(thumb::Preview::Original) => match read_image(&server.config.library, &rel) {
                    Some((bytes, mime)) => http::respond(&mut stream, 200, mime, &bytes),
                    None => http::respond(&mut stream, 404, "text/plain", b"not found"),
                },
                Err(message) => http::respond(&mut stream, 422, "text/plain", message.as_bytes()),
            }
        }
        ("GET", "/api/proof") => {
            // Only a path that a finished verify job printed as `proof: …`.
            let want = http::query_param(&req.target, "path").unwrap_or_default();
            let known = server.jobs.lock().unwrap().iter().any(|j| {
                j.kind == "verify"
                    && j.lines
                        .iter()
                        .any(|l| l.strip_prefix("proof: ") == Some(want.as_str()))
            });
            let raw = http::query_param(&req.target, "raw").is_some();
            match known.then(|| std::fs::read(&want).ok()).flatten() {
                Some(bytes) if raw => {
                    http::respond(&mut stream, 200, "text/plain; charset=utf-8", &bytes)
                }
                Some(bytes) => {
                    use sha2::Digest as _;
                    let sha = format!("{:x}", sha2::Sha256::digest(&bytes));
                    let body = serde_json::json!({
                        "path": want, "bytes": bytes.len(), "sha256": sha,
                        "text": String::from_utf8_lossy(&bytes),
                    });
                    http::respond(
                        &mut stream,
                        200,
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                }
                None => http::respond(
                    &mut stream,
                    404,
                    "text/plain",
                    b"not a proof this window wrote",
                ),
            }
        }
        ("GET", "/api/job") => {
            let id: usize = http::query_param(&req.target, "id")
                .and_then(|v| v.parse().ok())
                .unwrap_or(usize::MAX);
            let jobs = server.jobs.lock().unwrap();
            match jobs.get(id) {
                Some(job) => {
                    let body = serde_json::json!({
                        "kind": job.kind, "lines": job.lines, "done": job.done,
                        "total": job.total, "exit": job.exit,
                    });
                    http::respond(
                        &mut stream,
                        200,
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                }
                None => http::respond(&mut stream, 404, "text/plain", b"no such job"),
            }
        }
        ("POST", "/api/job") => {
            let kind = http::query_param(&req.target, "kind").unwrap_or_default();
            let dest = http::query_param(&req.target, "dest");
            match start_job(server, &kind, dest.as_deref()) {
                Ok(id) => {
                    let body = serde_json::json!({ "id": id }).to_string();
                    http::respond(&mut stream, 200, "application/json", body.as_bytes())
                }
                Err(message) => http::respond(&mut stream, 400, "text/plain", message.as_bytes()),
            }
        }
        _ => http::respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

/// Relative paths of every file in the library, read-only, sorted. Entries
/// the walk leaves out are in [`listing::list`]'s `skipped`.
fn library_files(library: &Path) -> Vec<String> {
    listing::list(library).files
}

fn is_image(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    ext.is_some_and(|e| IMAGE_EXT.contains(&e.as_str()))
}

/// What a scan manifest knows about one file.
struct Scanned {
    bytes: u64,
    /// ExifTool's DateTimeOriginal, or the next best date tag, as ISO.
    date: Option<String>,
    camera: Option<String>,
}

/// Capture dates and cameras from a scan manifest, by path.
fn scan_dates(manifest: &Path) -> std::collections::HashMap<String, Scanned> {
    let mut out = std::collections::HashMap::new();
    let Ok(file) = std::fs::File::open(manifest) else {
        return out;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let (Some(path), Some(bytes)) = (v["path"].as_str(), v["bytes"].as_u64()) else {
            continue;
        };
        let date = v["metadata"]["captured"].as_str().and_then(exif_date);
        let camera = v["metadata"]["camera"].as_str().map(str::to_string);
        out.insert(
            path.to_string(),
            Scanned {
                bytes,
                date,
                camera,
            },
        );
    }
    out
}

/// `2015:06:20 14:33:10[+offset]` as `2015-06-20T14:33:10`. Zero dates
/// (`0000:00:00`) give `None`.
pub fn exif_date(text: &str) -> Option<String> {
    let b = text.as_bytes();
    if b.len() < 19
        || !b[..19].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b':' || *c == b'-',
            10 => *c == b' ' || *c == b'T',
            13 | 16 => *c == b':',
            _ => c.is_ascii_digit(),
        })
        || text.starts_with("0000")
    {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}",
        &text[0..4],
        &text[5..7],
        &text[8..10],
        &text[11..19]
    ))
}

/// A file time as `YYYY-MM-DDTHH:MM:SSZ` (UTC). The page shows it in the
/// viewer's local time.
fn utc_iso(t: SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The newest `scan-*.jsonl` in the work folder, if any.
fn newest_scan(state: &Path) -> Option<PathBuf> {
    std::fs::read_dir(state)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("scan-") && n.ends_with(".jsonl")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// What the photos screen shows: the library summary and every grid image
/// with its folder and date. Dates come from the last scan's EXIF when it
/// has one (`date_source: "photo"`), else the file's modified time
/// (`"file"`). Read-only: the library is only listed and stat-ed.
pub fn library_view(config: &Config, scan: Option<&Path>) -> serde_json::Value {
    let library = &config.library;
    let listing = listing::list(library);
    let files = listing.files;
    let manifest = scan
        .map(Path::to_path_buf)
        .or_else(|| newest_scan(&config.state));
    let dates = manifest.as_deref().map(scan_dates).unwrap_or_default();
    let mut all_bytes = 0u64;
    let mut image_bytes = 0u64;
    let mut items = Vec::new();
    for rel in &files {
        let meta = std::fs::symlink_metadata(library.join(rel)).ok();
        let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        all_bytes += len;
        if !is_image(rel) {
            continue;
        }
        image_bytes += len;
        let mtime = meta.as_ref().and_then(|m| m.modified().ok()).map(utc_iso);
        // Scan facts count only while the file's size still matches.
        let scanned = dates.get(rel).filter(|s| s.bytes == len);
        let (date, source) = match scanned.and_then(|s| s.date.clone()) {
            Some(date) => (Some(date), "photo"),
            None => (mtime.clone(), "file"),
        };
        let camera = scanned.and_then(|s| s.camera.clone());
        let folder = rel.rsplit_once('/').map(|(f, _)| f).unwrap_or("");
        items.push(serde_json::json!({
            "path": rel, "bytes": len, "folder": folder,
            "date": date, "date_source": source, "mtime": mtime, "camera": camera,
        }));
    }
    let name = library
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| library.display().to_string());
    serde_json::json!({
        "library": library.display().to_string(),
        "name": name,
        "demo": config.demo,
        "files": files.len(),
        "bytes": all_bytes,
        "images": items,
        "image_bytes": image_bytes,
        "dates_from_scan": !dates.is_empty(),
        "skipped": listing::summary(&listing.skipped),
    })
}

/// The checked full path of a grid image, or `None` for `..`, absolute
/// paths, non-images, and anything that resolves outside the library.
fn image_path(library: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || !is_image(rel)
    {
        return None;
    }
    let root = library.canonicalize().ok()?;
    let full = root.join(rel_path).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

/// A library image by its relative path. `..`, absolute paths, and anything
/// that resolves outside the library are refused.
pub fn read_image(library: &Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    let full = image_path(library, rel)?;
    let mime = match rel.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        _ => "image/jpeg",
    };
    let mut bytes = Vec::new();
    std::fs::File::open(full)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    Some((bytes, mime))
}

/// The GUI's word for a status state.
pub fn state_word(state: crate::status::State) -> &'static str {
    use crate::status::State;
    match state {
        State::Ok => "ok",
        State::Stale => "stale",
        State::Damaged => "damaged",
        State::NotBackedUp => "not backed up",
        State::Unreadable => "unreadable",
    }
}

pub fn status_json(config: &Config, now: SystemTime) -> serde_json::Value {
    let reports: Vec<crate::status::Report> = config
        .backups
        .iter()
        .map(|d| crate::status::check(d, config.max_age_secs, now))
        .collect();
    let dests: Vec<serde_json::Value> = config
        .backups
        .iter()
        .zip(&reports)
        .map(|(dest, report)| {
            serde_json::json!({
                "dest": dest,
                "state": state_word(report.state),
                "text": report.text,
                "next": report.next,
            })
        })
        .collect();
    let exit = if reports.is_empty() {
        0
    } else {
        crate::status::render(&reports).1
    };
    let (summary, summary_next) = match crate::status::summary(&reports) {
        Some((line, next)) => (Some(line), next),
        None => (None, None),
    };
    serde_json::json!({
        "max_age_secs": config.max_age_secs,
        "destinations": dests,
        "summary": summary,
        "summary_next": summary_next,
        "exit": exit,
        "library": config.library.display().to_string(),
    })
}

/// Arguments for one job, run as this binary. Only these four kinds exist.
fn job_args(
    config: &Config,
    kind: &str,
    dest: Option<&str>,
    previous: Option<&Path>,
) -> Result<Vec<String>, String> {
    let stamp: String = crate::proof::utc(SystemTime::now())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let lib = config.library.display().to_string();
    let pick_dest = || -> Result<String, String> {
        let d = dest.ok_or("this action needs a backup folder")?;
        if config.backups.iter().any(|b| b == d) {
            Ok(d.to_string())
        } else {
            Err(format!("{d} is not one of this GUI's backup folders"))
        }
    };
    match kind {
        "scan" => {
            let mut n = 1;
            let mut manifest = config.state.join(format!("scan-{stamp}.jsonl"));
            while manifest.exists() {
                n += 1;
                manifest = config.state.join(format!("scan-{stamp}-{n}.jsonl"));
            }
            let mut args = vec![
                "scan".into(),
                lib,
                "--manifest".into(),
                manifest.display().to_string(),
            ];
            if let Some(prev) = previous {
                args.push("--previous".into());
                args.push(prev.display().to_string());
            }
            Ok(args)
        }
        "import" => {
            let card = config.card.as_ref().ok_or("no --card folder was given")?;
            Ok(vec![
                "import-card".into(),
                "--from".into(),
                card.display().to_string(),
                "--to".into(),
                lib,
            ])
        }
        "backup" => Ok(vec![
            "backup".into(),
            "--from".into(),
            lib,
            "--to".into(),
            pick_dest()?,
            "--state".into(),
            config.state.join("backup").display().to_string(),
        ]),
        "verify" => {
            let d = pick_dest()?;
            let manifest = Path::new(&d).join(crate::status::RECORD_NAME);
            let proofs = Path::new(&d).join(crate::backup::PROOF_DIR);
            let mut proof = proofs.join(format!("verify-{stamp}"));
            let mut n = 1;
            while proof.with_extension("md").exists() || proof.with_extension("json").exists() {
                n += 1;
                proof = proofs.join(format!("verify-{stamp}-{n}"));
            }
            Ok(vec![
                "verify-manifest".into(),
                "--manifest".into(),
                manifest.display().to_string(),
                "--against".into(),
                d,
                "--proof".into(),
                proof.display().to_string(),
            ])
        }
        _ => Err(format!("unknown action {kind:?}")),
    }
}
fn start_job(server: &Arc<Server>, kind: &str, dest: Option<&str>) -> Result<usize, String> {
    let previous = server.last_scan.lock().unwrap().clone();
    let args = job_args(&server.config, kind, dest, previous.as_deref())?;
    let total = match kind {
        "scan" => library_files(&server.config.library).len() as u64,
        _ => 0,
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let id = {
        let mut jobs = server.jobs.lock().unwrap();
        jobs.push(Job {
            kind: kind.to_string(),
            lines: vec![format!("$ albumen {}", args.join(" "))],
            total,
            ..Job::default()
        });
        jobs.len() - 1
    };
    let manifest = (kind == "scan").then(|| PathBuf::from(&args[3]));
    let scan_manifest = manifest.clone();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let s = Arc::clone(server);
    let err_reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(e) = stderr {
            out.extend(BufReader::new(e).lines().map_while(Result::ok));
        }
        out
    });
    let s2 = Arc::clone(server);
    let poller = manifest.map(|m| {
        std::thread::spawn(move || loop {
            let lines = std::fs::read_to_string(&m)
                .map(|t| t.lines().count() as u64)
                .unwrap_or(0);
            let finished = {
                let mut jobs = s2.jobs.lock().unwrap();
                jobs[id].done = lines.min(jobs[id].total);
                jobs[id].exit.is_some()
            };
            if finished {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        })
    });
    std::thread::spawn(move || {
        if let Some(o) = stdout {
            for line in BufReader::new(o).lines().map_while(Result::ok) {
                let mut jobs = s.jobs.lock().unwrap();
                jobs[id].lines.push(line);
                if jobs[id].total == 0 {
                    jobs[id].done += 1;
                }
            }
        }
        let errs = err_reader.join().unwrap_or_default();
        let code = child.wait().ok().and_then(|st| st.code()).unwrap_or(-1);
        if code == 0 {
            if let Some(m) = scan_manifest {
                *s.last_scan.lock().unwrap() = Some(m);
            }
        }
        let mut jobs = s.jobs.lock().unwrap();
        jobs[id].lines.extend(errs);
        if jobs[id].total > 0 && code == 0 {
            jobs[id].done = jobs[id].total;
        }
        jobs[id].exit = Some(code);
        drop(jobs);
        if let Some(p) = poller {
            let _ = p.join();
        }
    });
    Ok(id)
}

pub(crate) fn io_err(path: &Path, source: std::io::Error) -> IngestError {
    IngestError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_image_refuses_traversal_and_non_images() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        albumen_ingest::dest::create_dir_all(&lib).unwrap();
        let mut f = albumen_ingest::dest::create_file(&lib.join("a.png")).unwrap();
        f.write_all(b"png").unwrap();
        let mut f = albumen_ingest::dest::create_file(&dir.path().join("secret.png")).unwrap();
        f.write_all(b"secret").unwrap();
        assert_eq!(read_image(&lib, "a.png").unwrap().0, b"png");
        assert!(read_image(&lib, "../secret.png").is_none());
        assert!(read_image(&lib, "/etc/passwd").is_none());
        assert!(read_image(&lib, "a.txt").is_none());
        assert!(read_image(&lib, "").is_none());
    }

    #[test]
    fn exif_and_file_dates_are_iso() {
        assert_eq!(
            exif_date("2015:06:20 14:33:10").as_deref(),
            Some("2015-06-20T14:33:10")
        );
        assert_eq!(
            exif_date("2015:06:20 14:33:10-07:00").as_deref(),
            Some("2015-06-20T14:33:10")
        );
        assert_eq!(exif_date("0000:00:00 00:00:00"), None);
        assert_eq!(exif_date("June 2015"), None);
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_443_399_020);
        assert_eq!(utc_iso(t), "2015-09-28T00:10:20Z");
        assert_eq!(utc_iso(std::time::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    /// A preview cache inside the library is refused before the original is
    /// read, and nothing is written into the library.
    #[test]
    fn preview_cache_inside_the_library_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        albumen_ingest::dest::create_dir_all(&lib).unwrap();
        let mut f = albumen_ingest::dest::create_file(&lib.join("a.jpg")).unwrap();
        f.write_all(b"\xFF\xD8 not really a jpeg").unwrap();
        drop(f);
        let inside = lib.join("thumbs");
        assert!(thumb::refuse_cache_in_library(&lib, &inside).is_err());
        assert!(thumb::refuse_cache_in_library(&lib, &lib).is_err());
        assert!(thumb::preview(&lib, &inside, "a.jpg").is_err());
        assert!(!inside.exists(), "no cache folder was made in the library");
        let outside = dir.path().join("work").join("thumbs");
        assert!(thumb::refuse_cache_in_library(&lib, &outside).is_ok());
    }

    #[test]
    fn job_args_only_known_kinds_and_folders() {
        let config = Config {
            library: "/lib".into(),
            backups: vec!["/b1".into()],
            card: None,
            state: "/state".into(),
            max_age_secs: 1,
            demo: false,
        };
        assert!(job_args(&config, "rm", None, None).is_err());
        assert!(job_args(&config, "backup", Some("/elsewhere"), None).is_err());
        assert!(job_args(&config, "import", None, None).is_err());
        let v = job_args(&config, "verify", Some("/b1"), None).unwrap();
        assert_eq!(v[0], "verify-manifest");
        assert_eq!(v[5], "--proof");
        assert!(v[6].starts_with("/b1/ALBUMEN-PROOFS/verify-"));
        let s = job_args(&config, "scan", None, Some(Path::new("/state/a.jsonl"))).unwrap();
        assert_eq!(&s[4..], ["--previous", "/state/a.jsonl"]);
        let b = job_args(&config, "backup", Some("/b1"), None).unwrap();
        assert_eq!(&b[..5], ["backup", "--from", "/lib", "--to", "/b1"]);
    }
}
