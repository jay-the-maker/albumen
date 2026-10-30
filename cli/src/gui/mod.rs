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
pub mod taken;

const PAGE: &str = include_str!("page.html");

/// Image types the grid shows as thumbnails.
/// Formats a browser shows directly.
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "avif"];
/// Photos a browser cannot show yet: listed, counted and dated, with a
/// placeholder tile. Never silently dropped.
const PLACEHOLDER_EXT: &[&str] = &[
    "heic", "heif", "tif", "tiff", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf",
];

/// Camera RAW types that pair with a same-stem JPEG in the same folder.
const RAW_EXT: &[&str] = &["dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf"];

/// Folder plus lowercase stem, the key a RAW + JPEG pair shares.
fn pair_key(rel: &str) -> Option<String> {
    let (stem, _) = rel.rsplit_once('.')?;
    Some(stem.to_lowercase())
}

fn is_raw(rel: &str) -> bool {
    ext(rel).is_some_and(|e| RAW_EXT.contains(&e.as_str()))
}

fn is_jpeg(rel: &str) -> bool {
    ext(rel).is_some_and(|e| e == "jpg" || e == "jpeg")
}

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
        }
    };
    SourceRoot::open(&config.library)?.refuse_destination(&config.state)?;
    albumen_ingest::dest::create_dir_all(&config.state)?;
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
            let body = library_json(&server.config.library).to_string();
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

/// Relative paths of every file in the library, read-only, sorted.
fn library_files(library: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(root) = SourceRoot::open(library) {
        let _ = root.walk(None, &mut |relative| {
            out.push(relative.to_string());
            true
        });
    }
    out.sort();
    out
}

fn ext(rel: &str) -> Option<String> {
    rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// A format the browser can show, so `/img` serves it.
fn is_image(rel: &str) -> bool {
    ext(rel).is_some_and(|e| IMAGE_EXT.contains(&e.as_str()))
}

/// Any photo in the library, previewable or not.
fn is_photo(rel: &str) -> bool {
    is_image(rel) || ext(rel).is_some_and(|e| PLACEHOLDER_EXT.contains(&e.as_str()))
}

pub fn library_json(library: &Path) -> serde_json::Value {
    let files = library_files(library);
    let mut bytes = 0u64;
    // RAW + JPEG pairs (same folder, same stem): one photo, shown as the JPEG.
    let mut raw_of: std::collections::HashMap<String, String> = Default::default();
    for rel in files.iter().filter(|r| is_raw(r)) {
        if let Some(k) = pair_key(rel) {
            raw_of.entry(k).or_insert_with(|| rel.clone());
        }
    }
    let mut paired_raw: std::collections::HashSet<String> = Default::default();
    for rel in files.iter().filter(|r| is_jpeg(r)) {
        if let Some(raw) = pair_key(rel).and_then(|k| raw_of.get(&k)) {
            paired_raw.insert(raw.clone());
        }
    }
    let size = |rel: &str| {
        std::fs::metadata(library.join(rel))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let items: Vec<serde_json::Value> = files
        .iter()
        .filter(|rel| is_photo(rel) && !paired_raw.contains(*rel))
        .map(|rel| {
            let len = size(rel);
            bytes += len;
            let when = taken::taken(library, rel);
            let raw = is_jpeg(rel)
                .then(|| pair_key(rel).and_then(|k| raw_of.get(&k)))
                .flatten();
            let mut item = serde_json::json!({
                "path": rel,
                "bytes": len,
                "taken": when.as_ref().map(|t| t.when.clone()),
                "taken_from": when.as_ref().map(|t| t.source),
                "preview": is_image(rel),
                "format": ext(rel).unwrap_or_default().to_ascii_uppercase(),
            });
            if let Some(raw) = raw {
                bytes += size(raw);
                item["format"] = "RAW + JPEG".into();
                item["raw"] = raw.as_str().into();
            }
            item
        })
        .collect();
    serde_json::json!({
        "library": library.display().to_string(),
        "files": files.len(),
        "events": events(&items),
        "images": items,
        "image_bytes": bytes,
    })
}

/// Events: photos grouped by the day they were taken, oldest first, each
/// sorted by time. Photos with no known date go last under "No date".
pub fn events(images: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut by_day: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut undated = Vec::new();
    for img in images {
        let path = img["path"].as_str().unwrap_or_default().to_string();
        match img["taken"].as_str() {
            Some(when) if when.len() >= 10 => by_day
                .entry(when[..10].to_string())
                .or_default()
                .push((when.to_string(), path)),
            _ => undated.push(path),
        }
    }
    let mut out: Vec<serde_json::Value> = by_day
        .into_iter()
        .map(|(day, mut photos)| {
            photos.sort();
            let paths: Vec<String> = photos.into_iter().map(|(_, p)| p).collect();
            serde_json::json!({
                "title": taken::day_title(&day),
                "day": day,
                "count": paths.len(),
                "key": paths[0],
                "photos": paths,
            })
        })
        .collect();
    if !undated.is_empty() {
        undated.sort();
        out.push(serde_json::json!({
            "title": "No date",
            "day": null,
            "count": undated.len(),
            "key": undated[0],
            "photos": undated,
        }));
    }
    out
}

/// A library image by its relative path. `..`, absolute paths, and anything
/// that resolves outside the library are refused.
pub fn read_image(library: &Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
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
    if !full.starts_with(&root) {
        return None;
    }
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
    fn job_args_only_known_kinds_and_folders() {
        let config = Config {
            library: "/lib".into(),
            backups: vec!["/b1".into()],
            card: None,
            state: "/state".into(),
            max_age_secs: 1,
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

    #[test]
    fn events_group_by_capture_day_and_put_undated_last() {
        let img = |p: &str, t: Option<&str>| serde_json::json!({ "path": p, "taken": t });
        let ev = events(&[
            img("b.png", Some("2026-10-04T08:00:00")),
            img("x.png", None),
            img("a2.png", Some("2026-10-03T12:00:00")),
            img("a1.png", Some("2026-10-03T09:00:00")),
        ]);
        let titles: Vec<&str> = ev.iter().map(|e| e["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Oct 3, 2026", "Oct 4, 2026", "No date"]);
        assert_eq!(ev[0]["key"], "a1.png");
        assert_eq!(ev[0]["count"], 2);
    }

    #[test]
    fn heic_and_raw_are_listed_counted_and_dated_never_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let copy = |from: &str, to: &str| {
            let bytes = std::fs::read(fixtures.join(from)).unwrap();
            let mut f = albumen_ingest::dest::create_file(&lib.join(to)).unwrap();
            f.write_all(&bytes).unwrap();
        };
        copy("heic/IMG_0002.heic", "IMG_0002.HEIC");
        copy("raw-jpeg/IMG_0003.dng", "IMG_0003.dng");
        for name in [
            "a.cr2", "b.cr3", "c.nef", "d.arw", "e.orf", "f.rw2", "g.raf", "h.heif",
        ] {
            let mut f = albumen_ingest::dest::create_file(&lib.join(name)).unwrap();
            f.write_all(b"not a real raw").unwrap();
        }
        let v = library_json(lib);
        let images = v["images"].as_array().unwrap();
        assert_eq!(images.len(), 10, "{v}");
        assert!(images.iter().all(|i| i["preview"] == false), "{v}");
        let heic = images
            .iter()
            .find(|i| i["path"] == "IMG_0002.HEIC")
            .unwrap();
        assert_eq!(heic["taken"], "2018-04-01T12:00:00");
        assert_eq!(heic["format"], "HEIC");
        let dng = images.iter().find(|i| i["path"] == "IMG_0003.dng").unwrap();
        assert_eq!(dng["taken"], "2015-09-03T18:45:12");
        let ev = v["events"].as_array().unwrap();
        let total: u64 = ev.iter().map(|e| e["count"].as_u64().unwrap()).sum();
        assert_eq!(total, 10);
        let last = ev.last().unwrap();
        assert_eq!(
            (last["title"].as_str(), last["count"].as_u64()),
            (Some("No date"), Some(8))
        );
        // Listed, but /img still only serves what a browser can show.
        assert!(read_image(lib, "IMG_0002.HEIC").is_none());
    }

    #[test]
    fn raw_and_jpeg_with_the_same_stem_count_as_one_photo() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let copy = |from: &str, to: &str| {
            let bytes = std::fs::read(fixtures.join(from)).unwrap();
            let mut f = albumen_ingest::dest::create_file(&lib.join(to)).unwrap();
            f.write_all(&bytes).unwrap();
        };
        for sub in ["roll", "other"] {
            std::fs::create_dir(lib.join(sub)).unwrap();
        }
        copy("raw-jpeg/IMG_0003.dng", "roll/IMG_0003.dng");
        copy("raw-jpeg/IMG_0003.jpg", "roll/IMG_0003.JPG");
        // Same stem in another folder is not a pair; a lone RAW stays a RAW.
        copy("raw-jpeg/IMG_0003.dng", "other/IMG_0003.dng");
        copy("raw-jpeg/IMG_0003.jpg", "other/IMG_0004.jpg");
        let v = library_json(lib);
        let images = v["images"].as_array().unwrap();
        assert_eq!(images.len(), 3, "{v}");
        let total: u64 = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["count"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 3, "{v}");
        let pair = images
            .iter()
            .find(|i| i["path"] == "roll/IMG_0003.JPG")
            .unwrap();
        assert_eq!(pair["format"], "RAW + JPEG");
        assert_eq!(pair["raw"], "roll/IMG_0003.dng");
        assert_eq!(pair["preview"], true);
        assert!(!images.iter().any(|i| i["path"] == "roll/IMG_0003.dng"));
        let lone = images
            .iter()
            .find(|i| i["path"] == "other/IMG_0003.dng")
            .unwrap();
        assert_eq!(lone["format"], "DNG");
        assert!(PAGE.contains("im.raw?' \\u00b7 RAW + JPEG'"));
    }
}
