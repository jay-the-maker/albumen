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
pub mod live;
pub mod taken;
pub mod thumbs;

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

/// The key a RAW + JPEG pair shares: the folder exactly as it is on disk,
/// plus the file stem compared case-insensitively.
fn pair_key(rel: &str) -> Option<String> {
    let (stem, _) = rel.rsplit_once('.')?;
    let (folder, name) = stem.rsplit_once('/').unwrap_or(("", stem));
    Some(format!("{folder}/{}", name.to_lowercase()))
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
    /// `--demo` only: also serves /api/demo/clock, a fixture hook that lets
    /// gui-behaviour compare the page's local() with the Rust formatter.
    pub demo: bool,
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
    /// HEIC thumbnail index state, refreshed off the request path.
    thumbs: Mutex<ThumbView>,
    /// Refreshes started; the view keeps the newest finished one.
    thumb_gen: std::sync::atomic::AtomicU64,
    /// The last built /api/library answer (one walk per refresh, not per
    /// request); None until the first refresh finishes.
    library: Mutex<Option<Arc<LibraryView>>>,
    /// Photos the running walk has found so far, shown while indexing so a
    /// first open (no saved index) never sits at 0.
    found: std::sync::atomic::AtomicUsize,
    /// The first photos a first-ever walk finds, in the order found (at most
    /// [`FOUND_SHOWN`]), so the grid has tiles before the listing is ready.
    found_paths: Mutex<Vec<String>>,
    /// Why the last index save failed, if it did; shown as one quiet line.
    index_note: Mutex<Option<String>>,
}

/// How many walk-order photos the page may show while a first walk runs.
const FOUND_SHOWN: usize = 600;

/// One library walk, kept compact so /api/library never walks per request:
/// each image's JSON record in one byte arena, each path in another, and
/// events as indices. The full answer is written as a stream, never held.
#[derive(Default)]
struct LibraryView {
    generation: u64,
    /// False while the background walk is still adding images.
    ready: bool,
    library: String,
    files: usize,
    image_bytes: u64,
    records: Vec<u8>,
    record_ends: Vec<u32>,
    /// Length of the full answer once ready, counted on first request.
    full_len: std::sync::OnceLock<usize>,
    paths: Vec<u8>,
    path_ends: Vec<u32>,
    events: Vec<EventIdx>,
    /// While indexing: photos found so far by the running walk (set from the
    /// server just before answering). Never below the images already here.
    found: std::sync::atomic::AtomicUsize,
}

struct EventIdx {
    title: String,
    day: Option<String>,
    cover: u32,
    photos: Vec<u32>,
}

fn slice_at<'a>(buf: &'a [u8], ends: &[u32], i: usize) -> &'a [u8] {
    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
    &buf[start..ends[i] as usize]
}

impl LibraryView {
    fn len(&self) -> usize {
        self.record_ends.len()
    }

    fn path(&self, i: u32) -> &str {
        std::str::from_utf8(slice_at(&self.paths, &self.path_ends, i as usize)).unwrap_or("")
    }

    fn push(&mut self, item: &serde_json::Value) {
        let _ = serde_json::to_writer(&mut self.records, item);
        self.record_ends.push(self.records.len() as u32);
        self.paths
            .extend_from_slice(item["path"].as_str().unwrap_or_default().as_bytes());
        self.path_ends.push(self.paths.len() as u32);
    }

    /// Walk the library once. `partial` gets an "indexing" copy of the
    /// first page as soon as it exists, so the first request never waits
    /// for the whole walk.
    fn build(
        server: &Server,
        generation: u64,
        files: Vec<String>,
        cached: &std::collections::HashSet<String>,
        partial: &mut dyn FnMut(LibraryView),
    ) -> Self {
        const FIRST_PAGE: usize = 200;
        let lib = &server.config.library;
        let cache = live::cache_dir(lib);
        let mut v = LibraryView {
            generation,
            library: lib.display().to_string(),
            ..Default::default()
        };
        let mut when: Vec<(Option<Box<str>>, bool)> = Vec::new();
        let (files, bytes) =
            library_items(lib, files, cache.as_deref(), cached, &mut |files, item| {
                v.push(&item);
                when.push((
                    item["taken"].as_str().map(Into::into),
                    item["preview"] != false,
                ));
                if v.len() == FIRST_PAGE {
                    let mut early = LibraryView {
                        generation,
                        library: v.library.clone(),
                        files,
                        records: v.records.clone(),
                        record_ends: v.record_ends.clone(),
                        paths: v.paths.clone(),
                        path_ends: v.path_ends.clone(),
                        ..Default::default()
                    };
                    early.events = Vec::new();
                    partial(early);
                }
            });
        v.files = files;
        v.image_bytes = bytes;
        v.events = event_indices(&when);
        v.ready = true;
        v
    }

    /// The answer as JSON: everything, or one page of images (no events).
    fn write_json(&self, w: &mut dyn Write, page: Option<(usize, usize)>) -> std::io::Result<()> {
        let n = self.len();
        let (a, b) = match page {
            Some((o, l)) => (o.min(n), o.saturating_add(l).min(n)),
            None => (0, n),
        };
        let js = |s: &str| serde_json::Value::from(s).to_string();
        write!(
            w,
            "{{\"library\":{},\"files\":{},\"image_bytes\":{},\"thumbs_generation\":{},\"status\":\"{}\",\"total_images\":{},\"found\":{},\"offset\":{}",
            js(&self.library),
            self.files,
            self.image_bytes,
            self.generation,
            if self.ready { "ready" } else { "indexing" },
            n,
            if self.ready {
                n
            } else {
                n.max(self.found.load(std::sync::atomic::Ordering::Relaxed))
            },
            a
        )?;
        if page.is_none() {
            w.write_all(b",\"events\":[")?;
            for (k, e) in self.events.iter().enumerate() {
                if k > 0 {
                    w.write_all(b",")?;
                }
                write!(
                    w,
                    "{{\"title\":{},\"day\":{},\"count\":{},\"key\":{},\"photos\":[",
                    js(&e.title),
                    e.day.as_deref().map_or("null".into(), js),
                    e.photos.len(),
                    js(self.path(e.cover))
                )?;
                for (j, &p) in e.photos.iter().enumerate() {
                    if j > 0 {
                        w.write_all(b",")?;
                    }
                    // Straight into the stream: no String per path (380k
                    // of them per answer made R2 slower than its baseline).
                    serde_json::to_writer(&mut *w, self.path(p))?;
                }
                w.write_all(b"]}")?;
            }
            w.write_all(b"]")?;
        }
        w.write_all(b",\"images\":[")?;
        for i in a..b {
            if i > a {
                w.write_all(b",")?;
            }
            w.write_all(slice_at(&self.records, &self.record_ends, i))?;
        }
        w.write_all(b"]}")
    }

    /// Send the answer: Content-Length from a counting pass, then stream.
    fn respond(&self, stream: &mut TcpStream, page: Option<(usize, usize)>) -> std::io::Result<()> {
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0 += b.len();
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let count = || -> std::io::Result<usize> {
            let mut c = Count(0);
            self.write_json(&mut c, page)?;
            Ok(c.0)
        };
        // A ready listing never changes, so its full length is counted once.
        let len = match (page, self.ready, self.full_len.get()) {
            (None, true, Some(&n)) => n,
            (None, true, None) => {
                let n = count()?;
                let _ = self.full_len.set(n);
                n
            }
            _ => count()?,
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
            len
        )?;
        let mut w = std::io::BufWriter::with_capacity(1 << 16, &mut *stream);
        self.write_json(&mut w, page)?;
        w.flush()
    }
}

/// `events`, from each image's capture time and preview flag, by index.
/// Images arrive in path order, so (time, index) sorts like (time, path).
fn event_indices(when: &[(Option<Box<str>>, bool)]) -> Vec<EventIdx> {
    let mut by_day: std::collections::BTreeMap<&str, Vec<(&str, u32)>> = Default::default();
    let mut undated = Vec::new();
    for (i, (t, _)) in when.iter().enumerate() {
        match t.as_deref() {
            Some(t) if t.len() >= 10 => by_day.entry(&t[..10]).or_default().push((t, i as u32)),
            _ => undated.push(i as u32),
        }
    }
    let cover = |ps: &[u32]| *ps.iter().find(|&&p| when[p as usize].1).unwrap_or(&ps[0]);
    let mut out: Vec<EventIdx> = by_day
        .into_iter()
        .map(|(day, mut ps)| {
            ps.sort();
            let photos: Vec<u32> = ps.into_iter().map(|(_, i)| i).collect();
            EventIdx {
                title: taken::day_title(day),
                day: Some(day.to_string()),
                cover: cover(&photos),
                photos,
            }
        })
        .collect();
    if !undated.is_empty() {
        out.push(EventIdx {
            title: "No date".into(),
            day: None,
            cover: cover(&undated),
            photos: undated,
        });
    }
    out
}

/// The generation of the last finished thumbnail-index refresh (what it
/// found lives in the published library view).
#[derive(Default)]
struct ThumbView {
    generation: u64,
}

/// Refresh the HEIC thumbnail index in the background (at start and after
/// each scan), so /api/library never reads the index or stats every HEIC.
/// A newer generation always wins over an older one that finishes late.
fn refresh_thumbs(server: &Arc<Server>) {
    use std::sync::atomic::Ordering;
    let gen = server.thumb_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let s = Arc::clone(server);
    std::thread::spawn(move || {
        let lib = &s.config.library;
        // One listing for the thumbnails and the page, from the M3 index.
        let files = if s.config.demo {
            library_files(lib)
        } else {
            let files = library_files_indexed(lib, &s.found, &s.found_paths, &s.index_note);
            retry_pending_save(&s);
            files
        };
        let Some(cache) = live::cache_dir(lib) else {
            publish_library(&s, gen, files, &Default::default());
            return;
        };
        let scan = s
            .last_scan
            .lock()
            .unwrap()
            .clone()
            .or_else(|| newest_scan(&s.config.state));
        let heics: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|r| thumbs::is_heic(r))
            .collect();
        let r = thumbs::refresh(lib, &heics, &thumbs::thumbs_root(&cache), scan.as_deref());
        publish_library(&s, gen, files, &r.cached);
        {
            let mut view = s.thumbs.lock().unwrap();
            if gen > view.generation {
                *view = ThumbView { generation: gen };
            }
        }
    });
}

/// One background retry loop at a time for an index save left pending.
static RETRYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
const RETRY_TRIES: u32 = 120;

/// D1(b): when the index save was left pending (another writer kept the
/// lock), retry it in the background every [`RETRY_EVERY`] until it lands,
/// so a save is never lost and no request ever waits on the lock. The page
/// shows one quiet line (`/api/index` "pending") until then.
fn retry_pending_save(s: &Arc<Server>) {
    use std::sync::atomic::Ordering;
    if s.index_note.lock().unwrap().is_none() || RETRYING.swap(true, Ordering::SeqCst) {
        return;
    }
    let s = Arc::clone(s);
    std::thread::spawn(move || {
        let dir = albumen_ingest::index::default_index_dir();
        for _ in 0..RETRY_TRIES {
            std::thread::sleep(RETRY_EVERY);
            match albumen_ingest::index::LibraryIndex::open(&s.config.library, dir.as_deref()) {
                Ok(index) => {
                    let still = index.save_error.clone();
                    let done = still.is_none();
                    *s.index_note.lock().unwrap() = still;
                    if done {
                        break;
                    }
                }
                Err(err) => *s.index_note.lock().unwrap() = Some(err.to_string()),
            }
        }
        RETRYING.store(false, Ordering::SeqCst);
    });
}

/// Build the listing for `gen` and publish it: an early first page while
/// indexing, then the whole thing. An older generation never replaces a
/// newer finished one.
fn publish_library(
    s: &Arc<Server>,
    gen: u64,
    files: Vec<String>,
    cached: &std::collections::HashSet<String>,
) {
    let put = |v: LibraryView| {
        let mut lv = s.library.lock().unwrap();
        let newer = lv
            .as_ref()
            .is_none_or(|o| gen > o.generation || (gen == o.generation && !o.ready));
        if newer && (v.ready || lv.as_ref().is_none_or(|o| !o.ready)) {
            *lv = Some(Arc::new(v));
        }
    };
    let full = LibraryView::build(s, gen, files, cached, &mut |early| put(early));
    put(full);
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
        thumbs: Mutex::new(ThumbView::default()),
        thumb_gen: Default::default(),
        library: Mutex::new(None),
        found: Default::default(),
        found_paths: Default::default(),
        index_note: Default::default(),
    });
    *SERVER.lock().unwrap() = Some(Arc::downgrade(&server));
    if server.config.demo {
        // The demo library is tiny: have it ready before the first request.
        publish_library(
            &server,
            0,
            library_files(&server.config.library),
            &Default::default(),
        );
    }
    refresh_thumbs(&server);
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
        ("GET", "/api/thumbs") => {
            // S-H6: one page of the HEIC thumbnail index, streamed from disk.
            let num = |k: &str, d: usize| {
                http::query_param(&req.target, k)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d)
            };
            let (offset, limit) = (num("offset", 0), num("limit", 200).min(1000));
            let rows: Vec<serde_json::Value> = thumbs::thumbs_dir(&server.config.library)
                .map(|root| {
                    thumbs::page(&root, offset, limit)
                        .0
                        .iter()
                        .map(|e| {
                            serde_json::json!({
                                "path": e.path, "key": e.key,
                                "thumb": thumbs::thumb_state(&root, e),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let body = serde_json::json!({ "offset": offset, "entries": rows }).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/found") => {
            // While a first walk runs: the photos found so far, in the order
            // found (append-only, so tiles already shown never move).
            let body = found_json(server).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/index") => {
            // X4: whether an index save is still owed (retried in the
            // background); never waits on the index or its lock.
            let note = server.index_note.lock().unwrap().clone();
            let body = serde_json::json!({ "pending": note.is_some(), "detail": note }).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/library") => {
            // Served from the last background walk, never walked here. Until
            // its first page exists the answer is an empty "indexing" one;
            // the page polls until "status" is "ready".
            let cur = server.library.lock().unwrap().clone();
            if http::query_param(&req.target, "refresh").is_some() {
                refresh_thumbs(server);
            }
            let page = http::query_param(&req.target, "offset").map(|o| {
                let limit = http::query_param(&req.target, "limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(500);
                (o.parse().unwrap_or(0), limit)
            });
            let found = server.found.load(std::sync::atomic::Ordering::Relaxed);
            // M7: hides made since the listing apply on this request.
            let cur = cur.map(|v| current_view(server, v));
            match cur {
                Some(v) => {
                    if !v.ready {
                        v.found.store(found, std::sync::atomic::Ordering::Relaxed);
                    }
                    v.respond(&mut stream, page)
                }
                None => LibraryView {
                    library: server.config.library.display().to_string(),
                    found: found.into(),
                    ..Default::default()
                }
                .respond(&mut stream, page),
            }
        }
        ("GET", "/video") => {
            // A Live Photo's movie, bytes as on disk, streamed with Range
            // support so the player can seek. No transcode.
            let rel = http::query_param(&req.target, "p").unwrap_or_default();
            let open = (!hidden_now(server, &rel))
                .then(|| live::open_video(&server.config.library, &rel))
                .flatten();
            match open {
                Some((mut file, len)) => {
                    live::write_video(&mut stream, &mut file, len, req.header("range"))
                }
                None => http::respond(&mut stream, 404, "text/plain", b"not found"),
            }
        }
        ("GET", "/api/status") => {
            let body = status_json(&server.config, SystemTime::now()).to_string();
            http::respond(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/img") => {
            let rel = http::query_param(&req.target, "p").unwrap_or_default();
            let read = (!hidden_now(server, &rel))
                .then(|| read_image(&server.config.library, &rel))
                .flatten();
            match read {
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
        ("GET", "/api/demo/clock") if server.config.demo => {
            // Demo-only parity hook: the shared Rust formatter at an explicit
            // offset, so the page's local() can be checked against it.
            let num =
                |k: &str| http::query_param(&req.target, k).and_then(|v| v.parse::<i64>().ok());
            match (num("unix"), num("offset")) {
                (Some(unix), Some(offset)) if offset.abs() <= 14 * 60 => {
                    let body = serde_json::json!({
                        "stamp": albumen_ingest::clock::stamp_at(unix, offset),
                    });
                    http::respond(
                        &mut stream,
                        200,
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                }
                _ => http::respond(&mut stream, 400, "text/plain", b"unix and offset needed"),
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
    without_hidden(library, out)
}

/// The last listing's hide problems as one quiet line (None when there were
/// none). Shown on the page via `/api/found` "hide_note"; one gui per
/// process, so one note.
static HIDE_NOTE: Mutex<Option<String>> = Mutex::new(None);

/// One quiet line for hide problems: the photo stays hidden (fail closed),
/// and the user is told which file made that call.
fn hide_note(errors: &[String]) -> Option<String> {
    use albumen_ingest::hides::FAMILY_HIDDEN;
    // A family line is shown as is, first line only: "Family photos hidden:
    // can't read the family log <path>" or "... a family log entry can't be
    // read: <path>/log/<seq>.json" (any detail goes to the log).
    let family = errors
        .iter()
        .find(|e| e.starts_with(FAMILY_HIDDEN))
        .map(|e| e.split('\n').next().unwrap_or(e).to_string());
    let rest: Vec<&String> = errors
        .iter()
        .filter(|e| !e.starts_with(FAMILY_HIDDEN))
        .collect();
    let receiver = match rest.as_slice() {
        [] => None,
        [one] => Some(format!("Hidden because a hide file can't be read: {one}")),
        [first, more @ ..] => Some(format!(
            "{} hide files can't be read, so their photos stay hidden. First: {first}",
            more.len() + 1
        )),
    };
    match (family, receiver) {
        (Some(f), Some(r)) => Some(format!("{f} · {r}")),
        (f, r) => f.or(r),
    }
}

/// The hide files the last listing read (receiver tombstones). None until
/// a listing exists. `/api/found` re-reads them (and the family log) on
/// each poll so the quiet line follows a file that breaks or is fixed.
static HIDE_WATCH: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// The quiet line as of now: re-checked against the files once a listing
/// exists, else what the first walk found so far.
fn current_hide_note(library: &Path) -> Option<String> {
    let watch = HIDE_WATCH.lock().unwrap().clone();
    let serve: Vec<String> = SERVE_NOTES.lock().unwrap().values().cloned().collect();
    if let Some(watch) = watch {
        let mut hidden = albumen_ingest::hides::hidden_paths(library, &watch, None);
        for e in &serve {
            let tomb = e.split(':').next().unwrap_or(e);
            if !hidden.errors.iter().any(|h| h.starts_with(tomb)) {
                hidden.errors.push(e.clone());
            }
        }
        *HIDE_NOTE.lock().unwrap() = hide_note(&hidden.errors);
        return HIDE_NOTE.lock().unwrap().clone();
    }
    HIDE_NOTE
        .lock()
        .unwrap()
        .clone()
        .or_else(|| hide_note(&serve))
}

/// `paths` minus what the hide files (receiver tombstones, the family log)
/// say is hidden, read now on every listing (M7, A8). The index stays a
/// plain path cache; the files are the only source of truth. Hide problems
/// are logged one per line and kept as the page's quiet line, never a panic.
fn without_hidden(library: &Path, mut paths: Vec<String>) -> Vec<String> {
    let hidden = albumen_ingest::hides::hidden_paths(library, &paths, None);
    for err in &hidden.errors {
        eprintln!("albumen gui: hide: {}", err.replace('\n', ": "));
    }
    *HIDE_NOTE.lock().unwrap() = hide_note(&hidden.errors);
    *HIDE_WATCH.lock().unwrap() = Some(
        paths
            .iter()
            .filter(|p| p.ends_with(albumen_ingest::hides::TOMBSTONE_SUFFIX))
            .cloned()
            .collect(),
    );
    if !hidden.paths.is_empty() {
        paths.retain(|p| !hidden.paths.contains(p));
    }
    paths
}

/// M7: hide state as of the last request, for one ready listing. Keyed by
/// the stat of every directory holding a listed photo (a new tombstone
/// changes its directory), every watched tombstone (a rewrite in place) and
/// the family log directories (a new entry), so nothing is re-read until a
/// file changes. Reads only; nothing in the library is written.
struct Fresh {
    generation: u64,
    dirs: Vec<String>,
    stamp: Vec<Option<(SystemTime, u64)>>,
    hidden: Arc<std::collections::HashSet<String>>,
}

static FRESH: Mutex<Option<Fresh>> = Mutex::new(None);

/// An entry modified this close to a check may change again in the same
/// mtime tick (coarse filesystem clocks), so it is re-checked next request.
const RECENT: std::time::Duration = std::time::Duration::from_secs(2);

/// Hide files `/img` or `/video` could not read (fail closed), by the path
/// they hide, until a later request reads them; shown in the quiet line.
static SERVE_NOTES: Mutex<std::collections::BTreeMap<String, String>> =
    Mutex::new(std::collections::BTreeMap::new());

/// An entry's mtime plus a change key mtime alone misses: `rsync -t`/`-a`
/// brings a hide in and puts the folder's old mtime back (A8). On Unix the
/// key is the ctime, which no user call can set back. Elsewhere it is, for
/// a folder, its entry count plus a hash of its tombstone names and sizes;
/// for a file, its size.
fn mtime(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::symlink_metadata(p).ok()?;
    Some((m.modified().ok()?, change_key(p, &m)))
}

#[cfg(unix)]
fn change_key(_: &Path, m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    (m.ctime() as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(m.ctime_nsec() as u64)
}

#[cfg(not(unix))]
fn change_key(p: &Path, m: &std::fs::Metadata) -> u64 {
    use std::hash::{Hash, Hasher};
    if !m.is_dir() {
        return m.len();
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut count = 0u64;
    let mut tombs: Vec<(String, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            count += 1;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(albumen_ingest::hides::TOMBSTONE_SUFFIX) {
                tombs.push((name, e.metadata().map_or(0, |m| m.len())));
            }
        }
    }
    tombs.sort();
    count.hash(&mut h);
    tombs.hash(&mut h);
    h.finish()
}

/// Family log directories (`members/<m>/log`), or none outside a family
/// library. Their own mtimes change when an entry is added.
fn family_logs(library: &Path) -> Vec<PathBuf> {
    if !library.join("genesis.json").is_file() {
        return Vec::new();
    }
    let mut out = vec![library.join("members")];
    if let Ok(rd) = std::fs::read_dir(library.join("members")) {
        for e in rd.flatten() {
            out.push(e.path().join("log"));
        }
    }
    out.sort();
    out
}

fn stamp(library: &Path, dirs: &[String]) -> Vec<Option<(SystemTime, u64)>> {
    let mut st: Vec<Option<(SystemTime, u64)>> =
        dirs.iter().map(|d| mtime(&library.join(d))).collect();
    for f in family_logs(library) {
        st.push(mtime(&f));
    }
    for t in HIDE_WATCH.lock().unwrap().clone().unwrap_or_default() {
        st.push(mtime(&library.join(t)));
    }
    st
}

/// The listed paths hidden right now. Re-evaluated with
/// [`albumen_ingest::hides::hidden_paths`] whenever the stamp changed:
/// tombstones are found by listing each photo directory again, the family
/// log is re-read. Fails closed: a directory that can't be listed hides its
/// photos, and hide errors keep their photos hidden and set the quiet line.
fn hidden_for(server: &Server, v: &LibraryView) -> Arc<std::collections::HashSet<String>> {
    use albumen_ingest::hides::TOMBSTONE_SUFFIX;
    let lib = &server.config.library;
    let mut fresh = FRESH.lock().unwrap();
    if fresh.as_ref().is_none_or(|f| f.generation != v.generation) {
        let mut dirs: Vec<String> = (0..v.len() as u32)
            .map(|i| {
                v.path(i)
                    .rsplit_once('/')
                    .map_or("", |(d, _)| d)
                    .to_string()
            })
            .collect();
        dirs.sort();
        dirs.dedup();
        *fresh = Some(Fresh {
            generation: v.generation,
            dirs,
            stamp: Vec::new(),
            hidden: Default::default(),
        });
    }
    let f = fresh.as_mut().unwrap();
    // Stamped BEFORE the re-read, so a hide written during it differs from
    // the saved stamp next time. Anything modified within RECENT of now
    // (the same mtime tick) is not trusted: the stamp is not saved and the
    // next request re-reads.
    let checked_at = SystemTime::now();
    let now = stamp(lib, &f.dirs);
    if !f.stamp.is_empty() && now == f.stamp {
        return Arc::clone(&f.hidden);
    }
    let recent = now
        .iter()
        .any(|t| t.is_none_or(|(t, _)| checked_at.duration_since(t).map_or(true, |d| d < RECENT)));
    let mut all: Vec<String> = (0..v.len() as u32).map(|i| v.path(i).to_string()).collect();
    let mut unlisted = std::collections::HashSet::new();
    let mut tombs = Vec::new();
    for d in &f.dirs {
        match std::fs::read_dir(lib.join(d)) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.ends_with(TOMBSTONE_SUFFIX) {
                        tombs.push(if d.is_empty() {
                            name
                        } else {
                            format!("{d}/{name}")
                        });
                    }
                }
            }
            Err(err) => {
                eprintln!("albumen gui: hide: can't list {d} ({err}); hiding its photos");
                unlisted.insert(d.clone());
            }
        }
    }
    all.extend(tombs.iter().cloned());
    let h = albumen_ingest::hides::hidden_paths(lib, &all, None);
    for err in &h.errors {
        eprintln!("albumen gui: hide: {}", err.replace('\n', ": "));
    }
    *HIDE_NOTE.lock().unwrap() = hide_note(&h.errors);
    *HIDE_WATCH.lock().unwrap() = Some(tombs);
    let mut hidden: std::collections::HashSet<String> = h.paths.into_iter().collect();
    for i in 0..v.len() as u32 {
        let p = v.path(i);
        if unlisted.contains(p.rsplit_once('/').map_or("", |(d, _)| d)) {
            hidden.insert(p.to_string());
        }
    }
    // An unhide needs the photo back, which the view no longer holds: list
    // again in the background (hides only ever narrow the view here).
    let unhid = f.hidden.iter().any(|p| !hidden.contains(p));
    f.stamp = if recent { Vec::new() } else { now };
    f.hidden = Arc::new(hidden);
    let out = Arc::clone(&f.hidden);
    drop(fresh);
    if unhid {
        if let Some(s) = SERVER.lock().unwrap().as_ref().and_then(|w| w.upgrade()) {
            refresh_thumbs(&s);
        }
    }
    out
}

/// The server, for the background re-list an unhide needs.
static SERVER: Mutex<Option<std::sync::Weak<Server>>> = Mutex::new(None);

/// The cached view minus anything hidden since it was built. A ready view
/// that lost photos is replaced (so pages and counts stay consistent) unless
/// a newer listing was published meanwhile.
fn current_view(server: &Server, v: Arc<LibraryView>) -> Arc<LibraryView> {
    if !v.ready {
        return v;
    }
    let hidden = hidden_for(server, &v);
    if !(0..v.len() as u32).any(|i| hidden.contains(v.path(i))) {
        return v;
    }
    let mut out = LibraryView {
        generation: v.generation,
        ready: true,
        library: v.library.clone(),
        files: v.files,
        image_bytes: v.image_bytes,
        ..Default::default()
    };
    let mut map = vec![u32::MAX; v.len()];
    for (i, slot) in map.iter_mut().enumerate() {
        if hidden.contains(v.path(i as u32)) {
            out.files = out.files.saturating_sub(1);
            continue;
        }
        *slot = out.record_ends.len() as u32;
        out.records
            .extend_from_slice(slice_at(&v.records, &v.record_ends, i));
        out.record_ends.push(out.records.len() as u32);
        out.paths.extend_from_slice(v.path(i as u32).as_bytes());
        out.path_ends.push(out.paths.len() as u32);
    }
    for e in &v.events {
        let photos: Vec<u32> = e
            .photos
            .iter()
            .map(|&p| map[p as usize])
            .filter(|&p| p != u32::MAX)
            .collect();
        let Some(&first) = photos.first() else {
            continue;
        };
        let cover = match map[e.cover as usize] {
            u32::MAX => first,
            c => c,
        };
        out.events.push(EventIdx {
            title: e.title.clone(),
            day: e.day.clone(),
            cover,
            photos,
        });
    }
    let out = Arc::new(out);
    let mut lv = server.library.lock().unwrap();
    if lv.as_ref().is_some_and(|c| Arc::ptr_eq(c, &v)) {
        *lv = Some(Arc::clone(&out));
    }
    out
}

/// Whether `rel` is hidden right now, for `/img` and `/video`: its own
/// tombstone (read now, fail closed) or the current hide state of the
/// listing. Before a listing is ready only the tombstone is checked.
fn hidden_now(server: &Server, rel: &str) -> bool {
    match albumen_ingest::hides::receiver_hidden(&server.config.library, rel) {
        Ok(h) => {
            SERVE_NOTES.lock().unwrap().remove(rel);
            if h {
                return true;
            }
        }
        Err(err) => {
            eprintln!("albumen gui: hide: {err}");
            SERVE_NOTES.lock().unwrap().insert(rel.to_string(), err);
            return true;
        }
    }
    let cur = server.library.lock().unwrap().clone();
    match cur {
        Some(v) if v.ready => hidden_for(server, &v).contains(rel),
        _ => false,
    }
}

/// The library's files from the persisted M3 path index (the one search
/// keeps outside the library): load it, stat each indexed directory, list
/// again only the changed ones, save. A cold open with a saved index never
/// walks the whole tree. No index dir, or any index error: a plain walk.
/// Sorted like [`library_files`], so both give the same listing.
/// `found` counts the photos a first build (no saved index) lists, as it
/// lists them, for the "photos so far" line.
fn library_files_indexed(
    library: &Path,
    found: &std::sync::atomic::AtomicUsize,
    found_paths: &Mutex<Vec<String>>,
    note: &Mutex<Option<String>>,
) -> Vec<String> {
    let dir = albumen_ingest::index::default_index_dir();
    library_files_indexed_in(library, dir.as_deref(), found, found_paths, note)
}

/// The `/api/found` answer: the running count and the photos found so far.
fn found_json(server: &Server) -> serde_json::Value {
    let paths = server.found_paths.lock().unwrap().clone();
    serde_json::json!({
        "found": server.found.load(std::sync::atomic::Ordering::Relaxed),
        "paths": paths,
        "index_note": server.index_note.lock().unwrap().clone(),
        "hide_note": current_hide_note(&server.config.library),
    })
}

/// [`library_files_indexed`] with an explicit index directory.
fn library_files_indexed_in(
    library: &Path,
    dir: Option<&Path>,
    found: &std::sync::atomic::AtomicUsize,
    found_paths: &Mutex<Vec<String>>,
    note: &Mutex<Option<String>>,
) -> Vec<String> {
    use std::sync::atomic::Ordering;
    let mut n = 0usize;
    found.store(0, Ordering::Relaxed);
    found_paths.lock().unwrap().clear();
    let opened =
        albumen_ingest::index::LibraryIndex::open_with_progress(library, dir, &mut |rel| {
            // A first build streams tiles before the listing exists, so a
            // photo whose tombstone says hidden (or can't be read: fail
            // closed) is never counted or shown here either.
            if is_photo(rel) {
                match albumen_ingest::hides::receiver_hidden(library, rel) {
                    Ok(false) => {}
                    Ok(true) => return,
                    Err(err) => {
                        HIDE_NOTE
                            .lock()
                            .unwrap()
                            .get_or_insert_with(|| hide_note(std::slice::from_ref(&err)).unwrap());
                        return;
                    }
                }
                n += 1;
                if n <= FOUND_SHOWN {
                    found_paths.lock().unwrap().push(rel.to_string());
                }
                found.store(n, Ordering::Relaxed);
            }
        });
    match opened {
        Ok(index) => {
            *note.lock().unwrap() = index.save_error.clone();
            let mut paths = index.into_paths();
            paths.sort();
            let paths = without_hidden(library, paths);
            let photos = paths.iter().filter(|p| is_photo(p)).count();
            // The visible count, not the larger walk count: hidden photos
            // never inflate the header.
            found.store(photos, Ordering::Relaxed);
            paths
        }
        Err(_) => library_files(library),
    }
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

#[cfg(test)]
pub fn library_json(library: &Path) -> serde_json::Value {
    let cache = live::cache_dir(library);
    library_json_with(library, cache.as_deref())
}

/// The newest `scan-*.jsonl` in the state folder, if any.
fn newest_scan(state: &Path) -> Option<PathBuf> {
    std::fs::read_dir(state)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("scan-") && n.ends_with(".jsonl")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

/// `library_json` with an explicit preview cache folder (None: no cache).
#[cfg(test)]
pub fn library_json_with(library: &Path, cache: Option<&Path>) -> serde_json::Value {
    library_json_scan(library, cache, &Default::default())
}

#[cfg(test)]
/// `library_json_with`, with the HEIC paths that the last background
/// thumbnail-index refresh found cached. The index itself is not read here.
pub fn library_json_scan(
    library: &Path,
    cache: Option<&Path>,
    thumbs_cached: &std::collections::HashSet<String>,
) -> serde_json::Value {
    let mut items = Vec::new();
    let (files, bytes) = library_items(
        library,
        library_files(library),
        cache,
        thumbs_cached,
        &mut |_, i| items.push(i),
    );
    serde_json::json!({
        "library": library.display().to_string(),
        "files": files,
        "events": events(&items),
        "images": items,
        "image_bytes": bytes,
    })
}

/// Every image record of `library_json_scan`, in path order, handed to
/// `sink` one at a time (with the file count) so no caller has to hold
/// them all. Returns (files, image bytes).
fn library_items(
    library: &Path,
    files: Vec<String>,
    cache: Option<&Path>,
    thumbs_cached: &std::collections::HashSet<String>,
    sink: &mut dyn FnMut(usize, serde_json::Value),
) -> (usize, u64) {
    // Live Photos: a still and a movie with the same folder and stem AND the
    // same ContentIdentifier are one photo. Same stem without a matching ID
    // stays two items, each with an Info line saying why.
    let mut mov_of: std::collections::HashMap<String, String> = Default::default();
    for rel in files.iter().filter(|r| live::is_video(r)) {
        if let Some(k) = pair_key(rel) {
            mov_of.entry(k).or_insert_with(|| rel.clone());
        }
    }
    let candidates: Vec<(&String, &String)> = files
        .iter()
        .filter(|r| live::is_still(r))
        .filter_map(|r| pair_key(r).and_then(|k| mov_of.get(&k)).map(|m| (r, m)))
        .collect();
    let mut probe: Vec<&str> = candidates
        .iter()
        .flat_map(|(s, m)| [s.as_str(), m.as_str()])
        .collect();
    probe.sort();
    probe.dedup();
    let ids = live::identifiers(library, &probe, cache);
    let mut live_of: std::collections::HashMap<String, String> = Default::default();
    let mut info: std::collections::HashMap<String, String> = Default::default();
    let mut merged_mov: std::collections::HashSet<String> = Default::default();
    let name = |rel: &str| rel.rsplit('/').next().unwrap_or(rel).to_string();
    for (still, mov) in candidates {
        let a = ids.get(still.as_str()).cloned().flatten();
        let b = ids.get(mov.as_str()).cloned().flatten();
        if a.is_some() && a == b && !merged_mov.contains(mov) {
            live_of.insert(still.clone(), mov.clone());
            merged_mov.insert(mov.clone());
        } else {
            let line = match (&a, &b) {
                (Some(_), Some(_)) => format!(
                    "Not joined as a Live Photo: {} and {} have different Live Photo IDs.",
                    name(still),
                    name(mov)
                ),
                (None, _) => format!(
                    "Not joined as a Live Photo with {}: {} has no Live Photo ID.",
                    name(mov),
                    name(still)
                ),
                (Some(_), None) => format!(
                    "Not joined as a Live Photo with {}: {} has no Live Photo ID.",
                    name(still),
                    name(mov)
                ),
            };
            info.insert(still.clone(), line.clone());
            info.entry(mov.clone()).or_insert(line);
        }
    }
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
    let nfiles = files.len();
    for rel in files.iter().filter(|rel| {
        (is_photo(rel) || live::is_video(rel))
            && !paired_raw.contains(*rel)
            && !merged_mov.contains(*rel)
    }) {
        let len = size(rel);
        bytes += len;
        let mut when = taken::taken(library, rel);
        let raw = is_jpeg(rel)
            .then(|| pair_key(rel).and_then(|k| raw_of.get(&k)))
            .flatten();
        // A pair's date: the JPEG's metadata, else the RAW's metadata,
        // then a date in the file name. Never a made-up date.
        if let Some(raw) = raw {
            let jpeg_meta = when.as_ref().is_some_and(|t| t.source == "photo metadata");
            if !jpeg_meta {
                if let Some(t) = taken::taken(library, raw).filter(|t| t.source == "photo metadata")
                {
                    when = Some(taken::Taken {
                        when: t.when,
                        source: "RAW photo metadata",
                    });
                }
            }
        }
        let mut item = serde_json::json!({
            "path": rel,
            "bytes": len,
            "taken": when.as_ref().map(|t| t.when.clone()),
            "taken_from": when.as_ref().map(|t| t.source),
            "preview": is_image(rel),
            "format": ext(rel).unwrap_or_default().to_ascii_uppercase(),
        });
        if let Some(raw) = raw {
            let raw_len = size(raw);
            bytes += raw_len;
            item["raw_bytes"] = raw_len.into();
            item["format"] = "RAW + JPEG".into();
            item["raw"] = raw.as_str().into();
        }
        if thumbs::is_heic(rel) {
            // No decoder: the tile is the honest placeholder.
            item["thumb"] = if thumbs_cached.contains(rel.as_str()) {
                "cached"
            } else {
                "none"
            }
            .into();
        }
        if live::is_video(rel) {
            item["video"] = true.into();
            // A lone H.264 movie gets a browser-drawn poster tile; HEVC
            // keeps the plain can't-preview copy (no decoder, no transcode).
            let (ok, codec) = live::playable(&library.join(rel));
            item["playable"] = ok.into();
            item["codec"] = codec.into();
        }
        if let Some(mov) = live_of.get(rel.as_str()) {
            let mov_len = size(mov);
            bytes += mov_len;
            item["live"] = mov.as_str().into();
            item["live_bytes"] = mov_len.into();
            // Only H.264 plays in the browser; HEVC gets the AC3 copy.
            let (ok, codec) = live::playable(&library.join(mov));
            item["live_playable"] = ok.into();
            item["live_codec"] = codec.into();
        }
        if let Some(line) = info.get(rel.as_str()) {
            item["info"] = line.as_str().into();
        }
        sink(nfiles, item);
    }
    (nfiles, bytes)
}

/// Events: photos grouped by the day they were taken, oldest first, each
/// sorted by time. Photos with no known date go last under "No date".
#[cfg(test)]
pub fn events(images: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut by_day: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut undated = Vec::new();
    let viewable: std::collections::HashSet<&str> = images
        .iter()
        .filter(|i| i["preview"] != false)
        .filter_map(|i| i["path"].as_str())
        .collect();
    // The cover is the first photo a browser can show; a placeholder only
    // when the event has none.
    let cover = |paths: &[String]| {
        paths
            .iter()
            .find(|p| viewable.contains(p.as_str()))
            .unwrap_or(&paths[0])
            .clone()
    };
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
                "key": cover(&paths),
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
            "key": cover(&undated),
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
                refresh_thumbs(&s);
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

    /// F1: the compact listing streams the same images and events as the
    /// per-request listing did, and a page is a slice of it.
    /// /api/found: a first walk (no saved index) records the photos it
    /// finds, at most one page, never other files; a second open from the
    /// saved index lists the same files.
    #[test]
    fn a_hide_problem_is_one_quiet_line_naming_the_file() {
        assert_eq!(hide_note(&[]), None);
        let one = hide_note(&["a.jpg.albumen-tombstone: not valid JSON (x); hiding it".into()]);
        assert_eq!(
            one.as_deref(),
            Some("Hidden because a hide file can't be read: a.jpg.albumen-tombstone: not valid JSON (x); hiding it")
        );
        let two = hide_note(&["a.jpg.albumen-tombstone: e".into(), "b: e".into()]).unwrap();
        assert!(two.starts_with("2 hide files can't be read"), "{two}");
        assert!(two.contains("a.jpg.albumen-tombstone"), "{two}");
        let fam = hide_note(&[
            "Family photos hidden: can't read the family log /l (x)/members\nio error".into(),
        ]);
        assert_eq!(
            fam.as_deref(),
            Some("Family photos hidden: can't read the family log /l (x)/members")
        );
    }

    #[test]
    fn api_found_lists_photos_found_by_a_first_walk() {
        let lib = tempfile::tempdir().unwrap();
        let idx = tempfile::tempdir().unwrap();
        let sub = lib.path().join("2024");
        std::fs::create_dir_all(&sub).unwrap();
        for k in 0..FOUND_SHOWN + 5 {
            std::fs::write(sub.join(format!("p{k:04}.jpg")), b"synthetic").unwrap();
        }
        std::fs::write(lib.path().join("notes.txt"), b"not a photo").unwrap();
        let found = std::sync::atomic::AtomicUsize::default();
        let found_paths = Mutex::new(Vec::new());
        let files = library_files_indexed_in(
            lib.path(),
            Some(idx.path()),
            &found,
            &found_paths,
            &Default::default(),
        );
        let server = Server {
            config: Config {
                library: lib.path().to_path_buf(),
                backups: Vec::new(),
                card: None,
                state: lib.path().to_path_buf(),
                max_age_secs: 0,
                demo: false,
            },
            jobs: Mutex::new(Vec::new()),
            last_scan: Mutex::new(None),
            thumbs: Mutex::new(ThumbView::default()),
            thumb_gen: Default::default(),
            library: Mutex::new(None),
            found,
            found_paths,
            index_note: Default::default(),
        };
        let v = found_json(&server);
        assert_eq!(v["found"], FOUND_SHOWN + 5);
        let paths: Vec<&str> = v["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert_eq!(paths.len(), FOUND_SHOWN);
        assert!(paths
            .iter()
            .all(|p| p.starts_with("2024/") && p.ends_with(".jpg")));
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), FOUND_SHOWN);
        assert!(paths.iter().all(|p| files.iter().any(|f| f == p)));
        // A second open from the saved index lists the same files.
        let again = library_files_indexed_in(
            lib.path(),
            Some(idx.path()),
            &Default::default(),
            &Mutex::new(Vec::new()),
            &Default::default(),
        );
        assert_eq!(again, files);
    }

    #[test]
    fn compact_listing_matches_library_json() {
        let work = tempfile::tempdir().unwrap();
        let built = demo::build(work.path()).unwrap();
        let server = Server {
            config: Config {
                library: built.library.clone(),
                backups: Vec::new(),
                card: None,
                state: built.state.clone(),
                max_age_secs: 0,
                demo: true,
            },
            jobs: Mutex::new(Vec::new()),
            last_scan: Mutex::new(None),
            thumbs: Mutex::new(ThumbView::default()),
            thumb_gen: Default::default(),
            library: Mutex::new(None),
            found: Default::default(),
            found_paths: Default::default(),
            index_note: Default::default(),
        };
        let mut early = 0;
        let files = library_files(&built.library);
        let view = LibraryView::build(&server, 7, files, &Default::default(), &mut |_| early += 1);
        let want = library_json(&built.library);
        let mut out = Vec::new();
        view.write_json(&mut out, None).unwrap();
        let got: serde_json::Value = serde_json::from_slice(&out).unwrap();
        for k in ["library", "files", "events", "images", "image_bytes"] {
            assert_eq!(got[k], want[k], "{k}");
        }
        assert_eq!(got["status"], "ready");
        assert_eq!(got["thumbs_generation"], 7);
        let n = want["images"].as_array().unwrap().len();
        assert!(n >= 3);
        let mut out = Vec::new();
        view.write_json(&mut out, Some((1, 2))).unwrap();
        let page: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            page["images"].as_array().unwrap()[..],
            want["images"].as_array().unwrap()[1..3]
        );
        assert_eq!(page["total_images"], n);
        assert!(page.get("events").is_none());
        assert_eq!(early, usize::from(n >= 200));
    }

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
            demo: false,
        };
        assert!(job_args(&config, "rm", None, None).is_err());
        assert!(job_args(&config, "backup", Some("/elsewhere"), None).is_err());
        assert!(job_args(&config, "import", None, None).is_err());
        let v = job_args(&config, "verify", Some("/b1"), None).unwrap();
        assert_eq!(v[0], "verify-manifest");
        assert_eq!(v[5], "--proof");
        // The proof path is built with Path::join, so compare as a path,
        // not as a '/'-separated string (Windows joins with '\\').
        let proof = Path::new(&v[6]);
        assert_eq!(
            proof.parent(),
            Some(Path::new("/b1").join("ALBUMEN-PROOFS").as_path())
        );
        assert!(proof
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("verify-")));
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

    #[test]
    fn pairs_need_the_exact_folder_and_take_jpeg_too() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let copy = |from: &str, to: &str| {
            let bytes = std::fs::read(fixtures.join(from)).unwrap();
            let mut f = albumen_ingest::dest::create_file(&lib.join(to)).unwrap();
            f.write_all(&bytes).unwrap();
        };
        std::fs::create_dir(lib.join("a")).unwrap();
        std::fs::create_dir(lib.join("Roll")).unwrap();
        // NTFS and APFS are case-insensitive by default: "roll" is then the
        // same folder as "Roll", so the case-only half cannot be built.
        let case_sensitive = !lib.join("roll").exists();
        if case_sensitive {
            std::fs::create_dir(lib.join("roll")).unwrap();
        }
        // .jpeg pairs; the JPEG has no date, so the RAW's metadata dates it.
        copy("raw-jpeg/IMG_0003.dng", "a/IMG_0003.DNG");
        let mut f = albumen_ingest::dest::create_file(&lib.join("a/img_0003.jpeg")).unwrap();
        f.write_all(b"no exif here").unwrap();
        // Folder names differ only in case: two folders, no pair.
        if case_sensitive {
            copy("raw-jpeg/IMG_0003.dng", "Roll/IMG_1.dng");
            copy("raw-jpeg/IMG_0003.jpg", "roll/IMG_1.jpg");
        }
        let v = library_json(lib);
        let images = v["images"].as_array().unwrap();
        let paths: Vec<&str> = images.iter().map(|i| i["path"].as_str().unwrap()).collect();
        let want: &[&str] = if case_sensitive {
            &["Roll/IMG_1.dng", "a/img_0003.jpeg", "roll/IMG_1.jpg"]
        } else {
            &["a/img_0003.jpeg"]
        };
        assert_eq!(paths, want, "{v}");
        let pair = &images[usize::from(case_sensitive)];
        assert_eq!(pair["format"], "RAW + JPEG");
        assert_eq!(pair["raw"], "a/IMG_0003.DNG");
        assert_eq!(pair["taken"], "2015-09-03T18:45:12");
        assert_eq!(pair["taken_from"], "RAW photo metadata");
        assert!(pair["raw_bytes"].as_u64().unwrap() > 0);
        if case_sensitive {
            assert!(images[2].get("raw").is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn job_args_take_windows_drive_paths() {
        let config = Config {
            library: r"C:\Photos\Library".into(),
            backups: vec![r"D:\Backup 1".into()],
            card: None,
            state: r"C:\Albumen\state".into(),
            max_age_secs: 1,
            demo: false,
        };
        let v = job_args(&config, "verify", Some(r"D:\Backup 1"), None).unwrap();
        assert_eq!(
            v[2],
            r"D:\Backup 1\".to_string() + crate::status::RECORD_NAME
        );
        let proof = Path::new(&v[6]);
        assert_eq!(
            proof.parent(),
            Some(Path::new(r"D:\Backup 1\ALBUMEN-PROOFS"))
        );
        let b = job_args(&config, "backup", Some(r"D:\Backup 1"), None).unwrap();
        assert_eq!(
            &b[..5],
            [
                "backup",
                "--from",
                r"C:\Photos\Library",
                "--to",
                r"D:\Backup 1"
            ]
        );
        assert_eq!(b[6], r"C:\Albumen\state\backup");
        assert!(job_args(&config, "backup", Some(r"D:\Other"), None).is_err());
    }

    #[test]
    fn event_cover_is_a_viewable_photo_when_one_exists() {
        let ev = events(&[
            serde_json::json!({"path": "a.dng", "taken": "2015-09-03T10:00:00", "preview": false}),
            serde_json::json!({"path": "b.jpg", "taken": "2015-09-03T11:00:00", "preview": true}),
            serde_json::json!({"path": "c.nef", "taken": null, "preview": false}),
        ]);
        assert_eq!(ev[0]["key"], "b.jpg");
        assert_eq!(ev[0]["photos"][0], "a.dng");
        assert_eq!(ev[1]["key"], "c.nef", "placeholder only when none can show");
    }

    // ---- M5 Live viewing (spec 1AyJZOLnjRe-V0gQp9ll4lfgKV9Pv4bvs) ----

    fn live_library(lib: &Path) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let copy = |from: &str, to: &str| {
            let bytes = std::fs::read(fixtures.join(from)).unwrap();
            if let Some(dir) = Path::new(to).parent() {
                std::fs::create_dir_all(lib.join(dir)).unwrap();
            }
            let mut f = albumen_ingest::dest::create_file(&lib.join(to)).unwrap();
            f.write_all(&bytes).unwrap();
        };
        copy("live-photo/IMG_0005.heic", "roll/IMG_0005.HEIC");
        copy("live-photo/IMG_0005.mov", "roll/IMG_0005.MOV");
        // Same stem, but the still carries no Live Photo ID: two items.
        copy("heic/IMG_0002.heic", "roll/IMG_0006.heic");
        copy("live-photo/IMG_0005.mov", "roll/IMG_0006.mov");
        // A movie with no still at all.
        copy("live-photo/IMG_0005.mov", "clips/IMG_0007.mov");
    }

    /// Path, length, mtime and SHA-256 of every file under `root`.
    fn snapshot(root: &Path) -> Vec<(String, u64, SystemTime, String)> {
        use sha2::Digest as _;
        let mut out = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(dir) = todo.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let e = e.unwrap();
                let p = e.path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                if m.is_dir() {
                    todo.push(p);
                    continue;
                }
                let bytes = std::fs::read(&p).unwrap();
                out.push((
                    p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                    m.len(),
                    m.modified().unwrap(),
                    format!("{:x}", sha2::Sha256::digest(&bytes)),
                ));
            }
        }
        out.sort();
        out
    }

    /// AC1: a matched still + movie is one item with a LIVE movie; a
    /// same-stem pair without a matching ID stays two items, both explained.
    #[test]
    fn live_photo_pairs_by_content_identifier_only() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let v = library_json_with(&lib, None);
        let images = v["images"].as_array().unwrap();
        let paths: Vec<&str> = images.iter().map(|i| i["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "clips/IMG_0007.mov",
                "roll/IMG_0005.HEIC",
                "roll/IMG_0006.heic",
                "roll/IMG_0006.mov"
            ],
            "{v}"
        );
        let find = |p: &str| images.iter().find(|i| i["path"] == p).unwrap();
        let pair = find("roll/IMG_0005.HEIC");
        assert_eq!(pair["live"], "roll/IMG_0005.MOV");
        assert_eq!(pair["live_bytes"], 12865);
        assert!(pair.get("info").is_none(), "{pair}");
        for p in ["roll/IMG_0006.heic", "roll/IMG_0006.mov"] {
            let it = find(p);
            assert!(it.get("live").is_none(), "{it}");
            assert_eq!(
                it["info"],
                "Not joined as a Live Photo with IMG_0006.mov: IMG_0006.heic has no Live Photo ID."
            );
        }
        let lone = find("clips/IMG_0007.mov");
        assert_eq!(lone["video"], true);
        assert_eq!(lone["preview"], false);
        assert_eq!(lone["format"], "MOV");
        assert!(lone.get("info").is_none());
        let total: u64 = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["count"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 4, "the Live pair counts as one photo");
    }

    /// AC1: two movies whose IDs differ from the still are not joined.
    #[test]
    fn different_identifiers_stay_apart_with_an_info_line() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        let still = std::fs::read(fixtures.join("live-photo/IMG_0005.heic")).unwrap();
        let mut movie = std::fs::read(fixtures.join("live-photo/IMG_0005.mov")).unwrap();
        // Synthetic edit of a copy: change one hex digit of the movie's ID.
        let key = b"com.apple.quicktime.content.identifier";
        let at = movie.windows(key.len()).position(|w| w == key).unwrap();
        let uuid = b"B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5F";
        let u = at
            + movie[at..]
                .windows(uuid.len())
                .position(|w| w == uuid)
                .unwrap();
        movie[u] = b'C';
        for (name, bytes) in [("IMG_0009.heic", &still), ("IMG_0009.mov", &movie)] {
            let mut f = albumen_ingest::dest::create_file(&lib.join(name)).unwrap();
            f.write_all(bytes).unwrap();
        }
        let v = library_json_with(lib, None);
        let images = v["images"].as_array().unwrap();
        assert_eq!(images.len(), 2, "{v}");
        for i in images {
            assert!(i.get("live").is_none());
            assert_eq!(
                i["info"],
                "Not joined as a Live Photo: IMG_0009.heic and IMG_0009.mov have different Live Photo IDs."
            );
        }
    }

    /// AC2: the preview cache lives under ALBUMEN_HOME, is disposable, and
    /// rebuilds with the same result after it is moved away.
    #[test]
    fn preview_cache_is_outside_the_library_and_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let home = dir.path().join("home");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let cache = live::cache_dir_in(&home, &lib).unwrap();
        assert!(cache.starts_with(&home));
        let first = library_json_with(&lib, Some(&cache));
        let file = cache.join("live-ids-v1.json");
        assert!(file.is_file(), "cache written under ALBUMEN_HOME");
        let warm = library_json_with(&lib, Some(&cache));
        assert_eq!(first["images"], warm["images"]);
        // Move the cache away (never deleted) and rebuild.
        std::fs::rename(&cache, dir.path().join("moved-away")).unwrap();
        let rebuilt = library_json_with(&lib, Some(&cache));
        assert_eq!(first["images"], rebuilt["images"]);
        assert!(file.is_file(), "cache rebuilt");
        // A cache inside the library is refused, and the listing still works.
        assert!(live::cache_dir_in(&lib.join(".albumen"), &lib).is_none());
    }

    /// AC4: listing, caching and serving leave every original bit-exact
    /// (same names, sizes, mtimes and SHA-256) and add nothing to the library.
    #[test]
    fn live_viewing_never_writes_to_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let home = dir.path().join("home");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let before = snapshot(&lib);
        let cache = live::cache_dir_in(&home, &lib).unwrap();
        for _ in 0..2 {
            let _ = library_json_with(&lib, Some(&cache));
        }
        let _ = library_json_with(&lib, None);
        let (movie, _) = live::read_video(&lib, "roll/IMG_0005.MOV").unwrap();
        assert_eq!(movie.len(), 12865);
        assert!(read_image(&lib, "roll/IMG_0005.HEIC").is_none());
        assert_eq!(before, snapshot(&lib));
    }

    // ---- M5 AC2-AC4 HEIC extension (S-H4, S-H5) ----

    /// S-H4: /api/library says thumb "none" for HEIC and never offers a preview.
    #[test]
    fn sh4_heic_items_report_thumb_none_and_no_preview() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let home = dir.path().join("home");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let cache = live::cache_dir_in(&home, &lib).unwrap();
        for c in [None, Some(cache.as_path())] {
            let v = library_json_with(&lib, c);
            let heic: Vec<_> = v["images"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| thumbs::is_heic(i["path"].as_str().unwrap()))
                .collect();
            assert_eq!(heic.len(), 2);
            for i in heic {
                assert_eq!(i["thumb"], "none");
                assert_eq!(i["preview"], false);
                assert_eq!(i["format"], "HEIC");
            }
        }
        assert!(read_image(&lib, "roll/IMG_0005.HEIC").is_none());
        assert!(PAGE.contains("Can't preview this file here. Your original is kept unchanged."));
    }

    /// S-H5: scan, GUI listing, index build, invalidate and rebuild leave every
    /// original identical (SHA-256, size, mtime) and add nothing. Covers HEIC,
    /// a Live pair, RAW+JPEG and an HEVC movie.
    #[test]
    fn sh5_originals_never_touched_across_scan_index_invalidate_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let home = dir.path().join("home");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
        // Synthetic HEVC stub: the fixture movie with its sample entry set to hvc1.
        let mut hevc = std::fs::read(fixtures.join("live-photo/IMG_0005.mov")).unwrap();
        let at = hevc.windows(4).position(|w| w == b"avc1").unwrap();
        hevc[at..at + 4].copy_from_slice(b"hvc1");
        let put = |rel: &str, bytes: &[u8]| {
            let p = lib.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            albumen_ingest::dest::create_file(&p)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        };
        put("raw/DSC_0001.NEF", b"synthetic raw bytes");
        put("raw/DSC_0001.JPG", b"synthetic jpeg bytes");
        put("clips/hevc.mov", &hevc);
        let before = snapshot(&lib);
        let kinds = |s: &[(String, u64, SystemTime, String)], e: &str| {
            s.iter()
                .filter(|f| f.0.to_ascii_lowercase().ends_with(e))
                .count()
        };
        assert!(kinds(&before, ".heic") >= 2 && kinds(&before, ".nef") == 1);
        assert!(kinds(&before, ".mov") >= 3);
        let hevc_codec = live::playable(&lib.join("clips/hevc.mov"));
        assert_eq!(hevc_codec, (false, Some("HEVC")));
        let state = dir.path().join("state");
        // Scan (M3): the real scanner writes its manifest outside the library.
        let manifest = state.join("scan-1.jsonl");
        let root = SourceRoot::open(&lib).unwrap();
        let stats = albumen_ingest::scan_tree(&root, &manifest).unwrap();
        assert_eq!(stats.files as usize, before.len());
        assert_eq!(before, snapshot(&lib), "after scan");
        let cache = live::cache_dir_in(&home, &lib).unwrap();
        let troot = thumbs::thumbs_root(&cache);
        // GUI listing: never builds or reads the thumbnail index (F1).
        let v = library_json_with(&lib, Some(&cache));
        assert!(v["images"].as_array().unwrap().len() >= 5);
        assert!(
            !troot.join(thumbs::INDEX_FILE).exists(),
            "listing left the index alone"
        );
        let heics: Vec<&str> = before
            .iter()
            .map(|f| f.0.as_str())
            .filter(|r| thumbs::is_heic(r))
            .collect();
        let heics: Vec<String> = heics.iter().map(|r| r.replace('\\', "/")).collect();
        let refs: Vec<&str> = heics.iter().map(String::as_str).collect();
        // Index build, as the background refresh does it, reusing the scan's hashes.
        let cold = thumbs::refresh(&lib, &refs, &troot, Some(&manifest));
        assert_eq!((cold.hashed, cold.from_scan), (0, refs.len() as u64));
        let warm = thumbs::refresh(&lib, &refs, &troot, Some(&manifest));
        assert_eq!(warm.hashed, 0, "warm index build re-hashes nothing");
        assert_eq!(warm.entries as usize, refs.len());
        assert_eq!(before, snapshot(&lib), "after GUI open and index build");
        // Invalidate: move the cache away (never delete), then rebuild.
        std::fs::rename(&cache, dir.path().join("moved-away")).unwrap();
        let rebuilt = thumbs::refresh(&lib, &refs, &troot, Some(&manifest));
        assert_eq!((rebuilt.hashed, rebuilt.from_scan), (0, refs.len() as u64));
        let _ = library_json_with(&lib, Some(&cache));
        let _ = library_json_with(&lib, None);
        assert_eq!(before, snapshot(&lib), "after invalidate and rebuild");
        assert_eq!(hevc_codec, live::playable(&lib.join("clips/hevc.mov")));
    }

    /// AC5: /video serves the original movie bytes unchanged, and the page
    /// plays it only on request, muted, with controls.
    #[test]
    fn live_movie_is_served_as_is_and_played_muted_on_request() {
        let dir = tempfile::tempdir().unwrap();
        live_library(dir.path());
        let want = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo/IMG_0005.mov"),
        )
        .unwrap();
        let (bytes, mime) = live::read_video(dir.path(), "roll/IMG_0005.MOV").unwrap();
        assert_eq!(bytes, want, "no transcode");
        assert_eq!(mime, "video/quicktime");
        assert!(live::read_video(dir.path(), "roll/IMG_0005.HEIC").is_none());
        assert!(live::read_video(dir.path(), "roll/../roll/IMG_0005.MOV").is_none());
        assert!(PAGE.contains("'/video?p='"));
        assert!(PAGE.contains("v.muted=true;v.controls=true"));
        assert!(PAGE.contains("l.textContent='LIVE'"));
        assert!(!PAGE.contains("autoplay"));
    }

    /// AC3: one plain sentence for anything the browser cannot show, with the
    /// file name and type; the old "not available yet" copy is gone.
    #[test]
    fn cant_preview_copy_names_the_file_and_type() {
        assert!(PAGE.contains(
            "const CANT=\"Can't preview this file here. Your original is kept unchanged.\";"
        ));
        // G3: tiles stay quiet (the copy is the hover title); one banner
        // per view says it once.
        assert!(PAGE.contains("i.title=CANT;"));
        assert!(!PAGE.contains("m.textContent=CANT;"));
        assert!(PAGE.contains("\" can't be previewed here yet. Your original\""));
        assert!(!PAGE.contains("\\u2014 '+base(p)"));
        assert!(PAGE.contains("im.format+'. '+CANT"));
        assert!(!PAGE.contains("preview not available yet"));
    }
    /// Gate ask 3: an HEVC Live Photo movie is flagged not playable, so the
    /// page shows the can't-preview copy instead of a player.
    #[test]
    fn hevc_live_movie_is_not_offered_a_player() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo");
        let still = std::fs::read(fx.join("IMG_0005.heic")).unwrap();
        let mut stub = std::fs::read(fx.join("IMG_0005.mov")).unwrap();
        let at = stub.windows(4).position(|w| w == b"avc1").unwrap();
        stub[at..at + 4].copy_from_slice(b"hvc1");
        for (name, bytes) in [
            ("IMG_0005.heic", &still),
            (
                "IMG_0005.mov",
                &std::fs::read(fx.join("IMG_0005.mov")).unwrap(),
            ),
            ("IMG_0010.heic", &still),
            ("IMG_0010.mov", &stub),
        ] {
            let mut f = albumen_ingest::dest::create_file(&lib.join(name)).unwrap();
            f.write_all(bytes).unwrap();
        }
        let v = library_json_with(lib, None);
        let find = |p: &str| {
            v["images"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["path"] == p)
                .unwrap()
                .clone()
        };
        let h264 = find("IMG_0005.heic");
        assert_eq!(h264["live_playable"], true);
        assert_eq!(h264["live_codec"], "H.264");
        let hevc = find("IMG_0010.heic");
        assert_eq!(hevc["live"], "IMG_0010.mov");
        assert_eq!(hevc["live_playable"], false);
        assert_eq!(hevc["live_codec"], "HEVC");
        assert!(PAGE.contains("if(im.live_playable===false){"));
    }
    /// G1/G2: a lone H.264 movie is marked playable (poster tile), a lone
    /// HEVC movie is not (quiet tile, can't-preview copy, no player).
    #[test]
    fn lone_movies_carry_their_codec() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/live-photo");
        let movie = std::fs::read(fx.join("IMG_0005.mov")).unwrap();
        let mut stub = movie.clone();
        let at = stub.windows(4).position(|w| w == b"avc1").unwrap();
        stub[at..at + 4].copy_from_slice(b"hvc1");
        for (name, bytes) in [("IMG_0008.MOV", &movie), ("IMG_0009.MOV", &stub)] {
            let mut f = albumen_ingest::dest::create_file(&lib.join(name)).unwrap();
            f.write_all(bytes).unwrap();
        }
        let v = library_json_with(lib, None);
        let find = |p: &str| {
            v["images"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["path"] == p)
                .unwrap()
                .clone()
        };
        let h264 = find("IMG_0008.MOV");
        assert_eq!(h264["video"], true);
        assert_eq!(h264["playable"], true);
        assert_eq!(h264["codec"], "H.264");
        let hevc = find("IMG_0009.MOV");
        assert_eq!(hevc["playable"], false);
        assert_eq!(hevc["codec"], "HEVC");
        assert!(PAGE.contains("const POSTER_MAX=6;"));
    }
    /// Re-gate 2: header counts treat a Live pair (one item) as one photo
    /// and a lone movie as a video, with singular and plural wording.
    #[test]
    fn header_counts_photos_and_videos_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        live_library(&lib);
        let v = library_json_with(&lib, None);
        let images = v["images"].as_array().unwrap();
        let videos = images.iter().filter(|i| i["video"] == true).count();
        // IMG_0005 pair = 1 item; IMG_0006 heic + mov unpaired; IMG_0007 lone.
        assert_eq!((images.len() - videos, videos), (2, 2), "{v}");
        for s in [
            "function countText(ps){",
            "if(ph||!v)t.push(ph+(ph==1?' photo':' photos'));",
            "if(v)t.push(v+(v==1?' video':' videos'));return t.join(', ');}",
            "$('photos-count').textContent=countText(",
            "$('lib-count').textContent=countText(",
            "countText(e.photos)",
        ] {
            assert!(PAGE.contains(s), "{s}");
        }
        assert!(!PAGE.contains("lib.images.length+' photos'"));
    }
}
