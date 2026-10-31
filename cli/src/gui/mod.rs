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

    /// Count the full answer's length once (off the request path).
    fn count_full_len(&self) {
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
        if self.ready && self.full_len.get().is_none() {
            let mut c = Count(0);
            if self.write_json(&mut c, None).is_ok() {
                let _ = self.full_len.set(c.0);
            }
        }
    }

    /// Send the answer: Content-Length known (cached) and streamed, or
    /// built once into a buffer.
    fn respond(&self, stream: &mut TcpStream, page: Option<(usize, usize)>) -> std::io::Result<()> {
        // D2 (08:09Z): the first answer for a view (and every page) is
        // built once into a buffer and sent, not counted in one pass and
        // written in a second: the counting pass was ~half of the first
        // request after each re-walk (per-request timing, lib_req lines).
        let header = |stream: &mut TcpStream, len: usize| {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
                len
            )
        };
        if let (None, true, Some(&n)) = (page, self.ready, self.full_len.get()) {
            header(stream, n)?;
            let mut w = std::io::BufWriter::with_capacity(1 << 16, &mut *stream);
            self.write_json(&mut w, page)?;
            return w.flush();
        }
        let mut buf = Vec::with_capacity(self.full_len.get().copied().unwrap_or(1 << 16));
        self.write_json(&mut buf, page)?;
        if page.is_none() && self.ready {
            let _ = self.full_len.set(buf.len());
        }
        header(stream, buf.len())?;
        stream.write_all(&buf)
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
    refresh_thumbs_try(server, 0);
}

/// Walks that panicked (FRAGO 06:29Z (3)); a photo already sent on a walk
/// is sent again once this moves (see `grew_sent`).
static WALK_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Walks started again after one panicked.
static WALK_RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const WALK_TRIES: u32 = 4;

/// Test hook: the first N walks after startup panic
/// (`ALBUMEN_TEST_PANIC_WALKS=N`), so a test proves a new photo's walk is
/// retried.
fn walk_panic_hook() {
    static LEFT: std::sync::OnceLock<std::sync::atomic::AtomicI64> = std::sync::OnceLock::new();
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let left = LEFT.get_or_init(|| {
        std::sync::atomic::AtomicI64::new(
            std::env::var("ALBUMEN_TEST_PANIC_WALKS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        )
    });
    // The startup walk is never failed (the page waits on it).
    if SEEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
        return;
    }
    if left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) > 0 {
        panic!("test hook: walk panic");
    }
}

/// Walks under way now (for the per-request timing, D2).
static WALKS_RUNNING: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn refresh_thumbs_try(server: &Arc<Server>, attempt: u32) {
    use std::sync::atomic::Ordering;
    let gen = server.thumb_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let s = Arc::clone(server);
    std::thread::spawn(move || {
        WALKS_RUNNING.fetch_add(1, Ordering::SeqCst);
        let walked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            walk_panic_hook();
            walk_once(&s, gen);
        }));
        WALKS_RUNNING.fetch_sub(1, Ordering::SeqCst);
        if walked.is_err() {
            // A new photo's walk is not lost: logged, counted and started
            // again after 1, 2, 4 s; past that the next re-list that sees
            // the photo sends it again (WALK_FAILURES clears `grew_sent`).
            WALK_FAILURES.fetch_add(1, Ordering::SeqCst);
            eprintln!(
                "albumen gui: walk panicked (attempt {}); {}",
                attempt + 1,
                if attempt + 1 < WALK_TRIES {
                    "retrying"
                } else {
                    "giving up until the next change"
                }
            );
            if attempt + 1 < WALK_TRIES {
                std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
                WALK_RETRIES.fetch_add(1, Ordering::SeqCst);
                refresh_thumbs_try(&s, attempt + 1);
            }
        }
    });
}

fn walk_once(s: &Arc<Server>, gen: u64) {
    {
        let lib = &s.config.library;
        // One listing for the thumbnails and the page, from the M3 index.
        let files = if s.config.demo {
            library_files(lib)
        } else {
            let files = library_files_indexed(lib, &s.found, &s.found_paths, &s.index_note);
            retry_pending_save(s);
            files
        };
        let Some(cache) = live::cache_dir(lib) else {
            publish_library(s, gen, files, &Default::default());
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
        publish_library(s, gen, files, &r.cached);
        {
            let mut view = s.thumbs.lock().unwrap();
            if gen > view.generation {
                *view = ThumbView { generation: gen };
            }
        }
    }
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
    // D2 (08:09Z): the full answer's length is counted here, on the walk
    // thread, so the first request after a re-walk streams at once (the
    // count was ~5 ms of that request at 5,000 photos, lib_req lines).
    let ready = full.ready;
    put(full);
    // D2 (08:09Z): and the hide filter for it is applied here too (what the
    // first request after the re-walk did inline), so that request finds the
    // filtered view in place. Same function the request calls; a request
    // arriving first simply does it itself. Not for the startup walk: its
    // first answer is checked in full on the request (and says so).
    if ready && gen > 1 {
        let cur = s.library.lock().unwrap().clone();
        if let Some(v) = cur.filter(|v| v.generation == gen) {
            current_view(s, v).count_full_len();
        }
    }
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
        ("GET", "/api/hide-status") => {
            // XO 04:41Z (2): the page's poll. A few numbers, no paths, no
            // file reads: folders pending, the hidden set's version and the
            // listing's generation. The page fetches /api/library only
            // when the version or generation changes.
            let body = hide_status_json(server).to_string();
            refresh_hide_note_in_background(&server.config.library);
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
            // D2: per-request timing (ALBUMEN_HIDE_DIAG_FILE): what each
            // listing request waited on.
            let t0 = std::time::Instant::now();
            let walks = WALKS_RUNNING.load(std::sync::atomic::Ordering::Relaxed);
            let cur = server.library.lock().unwrap().clone();
            let t_lock = t0.elapsed();
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
            let t_view = t0.elapsed();
            match cur {
                Some(v) => {
                    if !v.ready {
                        v.found.store(found, std::sync::atomic::Ordering::Relaxed);
                    }
                    let r = v.respond(&mut stream, page);
                    hide_diag::line(&format!(
                        "lib_req gen={} walks={walks} lock_ms={:.2} view_ms={:.2} total_ms={:.2}",
                        v.generation,
                        t_lock.as_secs_f64() * 1e3,
                        (t_view - t_lock).as_secs_f64() * 1e3,
                        t0.elapsed().as_secs_f64() * 1e3
                    ));
                    r
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

/// Bumped whenever the quiet hide line changes (FRAGO 06:29Z (2)), so the
/// page reads /api/found only when it moved, not on idle ticks.
static HIDE_NOTE_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// A background re-check of the hide line is running (one at a time).
static NOTE_REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn set_hide_note(note: Option<String>) {
    let mut cur = HIDE_NOTE.lock().unwrap();
    if *cur != note {
        *cur = note;
        HIDE_NOTE_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Re-check the hide line off the request path (the status poll stays a
/// few numbers): a tombstone that breaks or is fixed moves the note
/// version, which the next status poll carries to the page.
fn refresh_hide_note_in_background(library: &Path) {
    use std::sync::atomic::Ordering::AcqRel;
    if NOTE_REFRESHING
        .compare_exchange(false, true, AcqRel, std::sync::atomic::Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let library = library.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(|| current_hide_note(&library));
        NOTE_REFRESHING.store(false, std::sync::atomic::Ordering::Release);
    });
}

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

/// Per-tombstone state `/api/found` keeps between polls (XO 02:30Z (c)).
static FOUND_TOMBS: Mutex<Option<std::collections::HashMap<String, TombState>>> = Mutex::new(None);

/// The quiet line as of now: re-checked against the files once a listing
/// exists, else what the first walk found so far. Every watched tombstone
/// is stamped on every poll (the same key as the request path: ctime on
/// Unix, ChangeTime on Windows, a content hash with the portable key) and
/// only a changed, new or same-tick one is read again, so a tombstone that
/// breaks shows in the note on the next poll. The family log is read every
/// poll, as before.
fn current_hide_note(library: &Path) -> Option<String> {
    let watch = HIDE_WATCH.lock().unwrap().clone();
    let serve: Vec<String> = SERVE_NOTES.lock().unwrap().values().cloned().collect();
    if let Some(watch) = watch {
        let checked_at = SystemTime::now();
        let mut errors = Vec::new();
        {
            let mut guard = FOUND_TOMBS.lock().unwrap();
            let cache = guard.get_or_insert_with(Default::default);
            let keep: std::collections::HashSet<&String> = watch.iter().collect();
            cache.retain(|k, _| keep.contains(k));
            // One listing per folder: each tombstone's metadata comes with
            // it (Windows: no stat per tombstone, FRAGO 06:13Z (2)); a
            // tombstone the listing lacks gets the plain stat.
            // One enumeration per folder (Windows, FRAGO 06:29Z (1)): the
            // handle-based directory listing carries each entry's
            // ChangeTime and LastWriteTime, so a tombstone's stamp costs no
            // open of its own; only a tombstone whose stamp moved is opened
            // and read again. Elsewhere (and with the portable key) the
            // plain stat, as before (Linux: a stat is what a listing entry
            // costs; the listing made p50 0.25 -> 5 ms on the box).
            // XO 08:22Z (1): Windows CI 435dadd listed every watched folder
            // (all of its entries) on every poll, so 20 tombstones in 20
            // folders of 1,000 photos cost 20,000 entries a poll (/api/found
            // p50 46.7 ms at 0 tombstones). A folder is listed only when it
            // holds enough tombstones that one listing beats a stat each
            // (LIST_MIN); otherwise each tombstone gets its own stamp. Every
            // tombstone is still stamped on every poll (a hide corrupted in
            // place shows within one poll).
            const LIST_MIN: usize = 64;
            let mut per_dir: std::collections::HashMap<&str, usize> = Default::default();
            for rel in &watch {
                *per_dir
                    .entry(rel.rsplit_once('/').map_or("", |(d, _)| d))
                    .or_default() += 1;
            }
            let mut listed: std::collections::HashMap<
                &str,
                std::collections::HashMap<std::ffi::OsString, (SystemTime, u64)>,
            > = Default::default();
            for rel in &watch {
                let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel.as_str()));
                let st = if per_dir.get(dir).copied().unwrap_or(0) < LIST_MIN {
                    mtime(&library.join(rel))
                } else {
                    let entries = listed.entry(dir).or_insert_with(|| {
                        if key_kind() != KeyKind::ChangeTime {
                            return Default::default();
                        }
                        dir_change_stamps(&library.join(dir))
                    });
                    match entries.get(std::ffi::OsStr::new(name)) {
                        Some(st) => Some(*st),
                        None => mtime(&library.join(rel)),
                    }
                };
                if key_kind() == KeyKind::Portable {
                    // mtime() hashed the bytes.
                    TOMB_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let reuse = cache
                    .get(rel)
                    .is_some_and(|t| t.trusted && st.is_some() && st == t.stamp);
                if !reuse {
                    cache.insert(rel.clone(), read_tomb_quiet(library, rel, checked_at));
                }
                if let Some(Err(e)) = cache.get(rel).map(|t| &t.verdict) {
                    errors.push(e.clone());
                }
            }
        }
        if library.join("genesis.json").is_file() {
            errors.extend(albumen_ingest::hides::hidden_paths(library, &[], None).errors);
        }
        for e in &serve {
            let tomb = e.split(':').next().unwrap_or(e);
            if !errors.iter().any(|h| h.starts_with(tomb)) {
                errors.push(e.clone());
            }
        }
        set_hide_note(hide_note(&errors));
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
    set_hide_note(hide_note(&hidden.errors));
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

/// M7: hide state as of the last request, for one ready listing, kept per
/// directory and per tombstone so a request re-reads only what changed
/// (XO 02:04Z). Reads only; nothing in the library is written.
/// What a pending folder withholds: everything (`all`: not checked yet, or
/// its listing failed, so fail closed) or only `set`, the photos that were
/// already off screen plus those whose tombstone came or changed. Kept as
/// the (small) withheld part rather than the (large) kept part, so the cost
/// follows the tombstones, not the photos (XO 05:06Z (c)).
#[derive(Debug, Clone)]
struct Withhold {
    all: bool,
    set: std::collections::HashSet<String>,
}

impl Default for Withhold {
    fn default() -> Self {
        Self {
            all: true,
            set: Default::default(),
        }
    }
}

impl Withhold {
    fn holds(&self, p: &str) -> bool {
        self.all || self.set.contains(p)
    }
}

struct Fresh {
    generation: u64,
    /// Every directory holding a listed photo, and the listed photos in it.
    dirs: Vec<String>,
    photos_in: Vec<Vec<u32>>,
    /// The unfiltered view `photos_in` indexes (later views of the same
    /// generation are filtered copies with fewer entries).
    base: Arc<LibraryView>,
    state: Vec<DirState>,
    /// Family log stamps and what the family log hid as of them.
    family: Option<FamilyState>,
    primed: bool,
    /// A new generation took over the folders' state: the hidden set is
    /// rebuilt for its view on the next check even if no folder changed.
    rebuild: bool,
    /// Bumped whenever `hidden` changes, so a view already filtered for it
    /// is served as is.
    version: u64,
    hidden: Arc<std::collections::HashSet<String>>,
    /// The part of `hidden` that is only withheld (pending folders).
    withheld: std::collections::HashSet<String>,
    /// The part of `hidden` hidden by a tombstone, family log or unlisted
    /// folder at the last check (not only withheld).
    hard: Arc<std::collections::HashSet<String>>,
    /// Withheld photos that were in `hard` (or `hard_w`) before.
    hard_w: std::collections::HashSet<String>,
    /// Each folder's pending flag and keep set as of the last build, so the
    /// next build revisits only the folders that changed (XO 05:00Z (a)).
    built: Vec<(bool, Arc<Withhold>)>,
    /// Each folder's state version as of the last build and what it added
    /// to `hard`, so a build revisits only the folders whose tombstones or
    /// listing changed (XO 05:41Z (1)): the cost follows the changed
    /// folders, not every tombstone of the library.
    cver: Vec<u64>,
    contrib: Vec<Contrib>,
    /// How many sources (folders, the family log) hide each `hard` photo.
    hard_n: std::collections::HashMap<String, u32>,
    /// What the family log added to `hard` at the last build.
    fam_contrib: Vec<String>,
    /// The family log was read again since the last build.
    fam_dirty: bool,
    /// A confirmed listing holds a photo the view lacks: list the library
    /// again (XO 04:41Z (1)).
    grew: bool,
    /// Photos that already started a re-walk this generation: a re-list
    /// while that walk runs (slow on Windows CI: 41/42 walks for 22)
    /// must not start another for the same photo.
    grew_sent: std::collections::HashSet<String>,
    /// WALK_FAILURES when `grew_sent` was last trusted: a walk that panicked
    /// may have lost those photos, so they are sent again.
    grew_epoch: u64,
    /// The previous generation's view, kept until this generation's first
    /// check has decided which photos of a pending folder were on screen.
    prev: Option<Prev>,
}

/// What the previous generation showed, by folder (XO 03:36Z (1)).
struct Prev {
    dir_idx: std::collections::HashMap<String, usize>,
    photos_in: Vec<Vec<u32>>,
    base: Arc<LibraryView>,
    hidden: Arc<std::collections::HashSet<String>>,
}

#[derive(Default)]
struct DirState {
    stamp: Option<(SystemTime, u64)>,
    /// False when the stamp was taken within [`recent`] of its mtime (the
    /// same tick) or could not be taken: the folder is listed again.
    trusted: bool,
    /// The folder could not be listed: its photos are hidden (fail closed).
    unlisted: bool,
    /// Being listed and read in the background (A8, XO 03:22Z): until that
    /// is done, every photo in the folder is hidden (fail closed). The
    /// state itself is with the worker; `since` is when it was handed over.
    pending: Option<std::time::Instant>,
    /// For a pending folder: the photos that were on screen and whose own
    /// tombstone is absent, or unchanged with a trusted show verdict. They
    /// stay shown; the folder's other photos are withheld (XO 03:36Z (1)).
    keep: Arc<Withhold>,
    tombs: std::collections::BTreeMap<String, TombState>,
    /// The photo files the last listing saw (taken by [`apply_relist`] to
    /// spot photos synced in that the view does not have yet).
    photos: Vec<String>,
    /// Listings in a row that kept panicking, and when the folder is sent
    /// again (backoff; its shown photos stay under `keep` meanwhile).
    fails: u32,
    retry_at: Option<std::time::Instant>,
    /// Set on the result of a listing that kept panicking.
    failed: bool,
    /// Changes whenever `tombs` or `unlisted` may have (0: an empty state,
    /// such as one handed to a worker).
    ver: u64,
    /// For a pending folder: its stamp when it was handed to the worker.
    sent_st: Option<(SystemTime, u64)>,
    /// For a pending folder: its tombstones that read as a trusted show
    /// when it was handed over (their stamps), so a re-check of its carried
    /// show does not withhold them while they are unchanged.
    sent_shows: Arc<Vec<(String, (SystemTime, u64))>>,
}

/// One folder's part of the hard-hidden set, its tombstone files and
/// their errors, as of its state version.
#[derive(Default, Clone)]
struct Contrib {
    hides: Vec<String>,
    tombs: Vec<String>,
    errors: Vec<String>,
}

/// A new version for a folder state whose tombstones or listing changed.
fn next_ver() -> u64 {
    static VER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    VER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct TombState {
    stamp: Option<(SystemTime, u64)>,
    trusted: bool,
    verdict: Result<bool, String>,
}

struct FamilyState {
    stamp: Vec<Option<(SystemTime, u64)>>,
    trusted: bool,
    paths: Vec<String>,
    errors: Vec<String>,
}

static FRESH: Mutex<Option<Fresh>> = Mutex::new(None);

/// Tombstone files the request-path hide check has read (or hashed, off
/// Unix) since start; shown in `/api/found` as `hide_tombstone_reads` so a
/// test can prove an unchanged request reads none.
static TOMB_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Request-path counters only (not `/api/found`'s own polls): tombstones
/// read and folders listed by [`hidden_for`]; `/api/found` shows them as
/// `hide_recheck_reads` and `hide_dirs_listed`.
static RECHECK_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIRS_LISTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Set when the library is on ext2/3/4 (0xEF53): see [`stamp_window`].
static EXT_STAMPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The same-tick window for one stamp, and the time it is measured from.
///
/// The time is the later of the mtime and (ctime key) the ctime: `rsync -t`
/// sets an mtime back, but the ctime is when the entry really changed.
///
/// ext2/3/4 (XO 02:53Z): the filesystem window is 2 s because 0xEF53 may
/// keep whole seconds. A ctime with a non-zero sub-second part, though,
/// proves this inode is being stamped in ns (on disk, or in memory for a
/// 128-byte inode, where a reload drops the ns and so changes the key and
/// forces a re-list): a later change gets a different ctime unless it lands
/// in the same kernel clock tick, which the fine window covers. A ctime of
/// exactly .000000000 keeps the 2 s window (fail safe; a fine stamp is
/// that by chance 1 time in 10^9).
///
/// Windows ChangeTime (XO 03:12Z): there is no statfs, so the filesystem
/// window is 2 s, but NTFS and ReFS keep ChangeTime in 100 ns ticks. A
/// ChangeTime with a non-zero sub-second part proves this entry is stamped
/// finely, so it gets the fine window (covering the clock tick); a whole-
/// second one (a coarse SMB server, FAT has none at all) keeps 2 s. A
/// ChangeTime before 1970 keeps the mtime and 2 s (fail safe).
fn stamp_window(
    mtime: SystemTime,
    key: u64,
    kind: KeyKind,
    ext: bool,
    fs_window: std::time::Duration,
) -> (SystemTime, std::time::Duration) {
    let fine = RECENT_FINE.min(fs_window);
    match kind {
        KeyKind::Portable => (mtime, fs_window),
        KeyKind::Ctime => {
            let ctime = SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(key);
            let w = if ext && !key.is_multiple_of(1_000_000_000) {
                fine
            } else {
                fs_window
            };
            (mtime.max(ctime), w)
        }
        KeyKind::ChangeTime => {
            // 100 ns ticks since 1601-01-01.
            const TO_UNIX: u64 = 116_444_736_000_000_000;
            let Some(ticks) = key.checked_sub(TO_UNIX) else {
                return (mtime, fs_window);
            };
            let changed =
                SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ticks.saturating_mul(100));
            let w = if !key.is_multiple_of(10_000_000) {
                fine
            } else {
                fs_window
            };
            (mtime.max(changed), w)
        }
    }
}

fn trusted_at(st: &Option<(SystemTime, u64)>, checked_at: SystemTime) -> bool {
    st.is_some_and(|(t, key)| {
        let (since, w) = stamp_window(
            t,
            key,
            key_kind(),
            EXT_STAMPS.load(std::sync::atomic::Ordering::Relaxed),
            recent(),
        );
        checked_at.duration_since(since).is_ok_and(|d| d >= w)
    })
}

/// Stamp, then read, one tombstone (stamped BEFORE the read, so a rewrite
/// during it differs next time). Logs an unreadable one.
fn read_tomb(lib: &Path, rel: &str, checked_at: SystemTime) -> TombState {
    RECHECK_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = read_tomb_quiet(lib, rel, checked_at);
    if let Err(e) = &t.verdict {
        eprintln!("albumen gui: hide: {}", e.replace('\n', ": "));
    }
    t
}

/// [`read_tomb`] without the log line (the `/api/found` poll).
fn read_tomb_quiet(lib: &Path, rel: &str, checked_at: SystemTime) -> TombState {
    let path = lib.join(rel);
    TOMB_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(err) = tomb_error_hook(rel) {
        return TombState {
            stamp: None,
            trusted: false,
            verdict: Err(format!("{rel}: {err}; hiding it")),
        };
    }
    #[cfg(windows)]
    if key_kind() == KeyKind::ChangeTime {
        // One open (XO 03:12Z): stamp from the handle, then read it.
        if let Some((stamp, verdict)) = stamp_and_read_windows(&path) {
            return TombState {
                trusted: trusted_at(&stamp, checked_at),
                stamp,
                verdict: verdict.map_err(|err| format!("{rel}: {err}; hiding it")),
            };
        }
    }
    let stamp = mtime(&path);
    let verdict = albumen_ingest::hides::read_receiver_tombstone(&path)
        .map_err(|err| format!("{rel}: {err}; hiding it"));
    TombState {
        trusted: trusted_at(&stamp, checked_at),
        stamp,
        verdict,
    }
}

/// Same-tick window where the filesystem keeps a fine-grained ctime
/// (XO 02:30Z (a)): ext4, xfs, btrfs, tmpfs, f2fs, zfs; APFS on macOS.
const RECENT_FINE: std::time::Duration = std::time::Duration::from_millis(50);
/// Everywhere else (unknown, network or FUSE, FAT/exFAT, HFS+, overlayfs,
/// and Windows, where there is no statfs): an entry modified this close to
/// a check may change again in the same coarse tick.
const RECENT_COARSE: std::time::Duration = std::time::Duration::from_secs(2);

/// The window in force, in ms; set from the library's filesystem when a
/// listing's hide state is first built. Starts coarse (fail safe).
static RECENT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2000);

fn recent() -> std::time::Duration {
    std::time::Duration::from_millis(RECENT_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Test hook: `ALBUMEN_TEST_FS_MAGIC` (a Linux statfs f_type, hex `0x...`
/// or decimal) replaces the real filesystem type, on any platform, so a
/// test can force the 2 s fallback (or the fine window). Honoured only in
/// debug builds or with the `test-hooks` cargo feature (CI's `--release`
/// test builds enable it); a plain release build ignores it.
#[cfg(not(any(debug_assertions, feature = "test-hooks")))]
fn fs_magic_override() -> Option<u64> {
    None
}

#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn fs_magic_override() -> Option<u64> {
    let v = std::env::var("ALBUMEN_TEST_FS_MAGIC").ok()?;
    let v = v.trim();
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

/// Linux statfs f_type magics with a fine-grained (ns) ctime.
/// ext2/3/4 (0xEF53) is NOT here: all three share the magic, and ext2/3 or
/// an ext4 made with 128-byte inodes stores whole seconds only, so it gets
/// the 2 s window (safe; costs only the first 2 s after a change).
const FINE_MAGICS: &[(u64, &str)] = &[
    (0x5846_5342, "xfs"),
    (0x9123_683E, "btrfs"),
    (0x0102_1994, "tmpfs"),
    (0xF2F5_2010, "f2fs"),
    (0x2FC1_2FC1, "zfs"),
];

/// Known coarse or untrusted ones (named in `/api/found` only; anything not
/// in [`FINE_MAGICS`] gets the 2 s window).
const COARSE_MAGICS: &[(u64, &str)] = &[
    (0xEF53, "ext2/3/4"),
    (0x6969, "nfs"),
    (0x517B, "smb"),
    (0xFF53_4D42, "cifs"),
    (0xFE53_4D42, "smb2"),
    (0x6573_5546, "fuse"),
    (0x4D44, "msdos"),
    (0x2011_BAB0, "exfat"),
    (0x4244, "hfs"),
    (0x482B, "hfsplus"),
    (0x794C_7630, "overlayfs"),
    (0x7366_746E, "ntfs3"),
];

/// The window for a statfs f_type (None: unknown), and a name for it.
fn window_for_magic(magic: Option<u64>) -> (std::time::Duration, String) {
    let Some(m) = magic else {
        return (RECENT_COARSE, "unknown".into());
    };
    if let Some((_, n)) = FINE_MAGICS.iter().find(|(k, _)| *k == m) {
        return (RECENT_FINE, (*n).into());
    }
    let name = COARSE_MAGICS
        .iter()
        .find(|(k, _)| *k == m)
        .map_or_else(|| format!("unknown {m:#x}"), |(_, n)| (*n).to_string());
    (RECENT_COARSE, name)
}

#[cfg(target_os = "linux")]
fn fs_window(library: &Path) -> (std::time::Duration, String) {
    use std::os::unix::ffi::OsStrExt;
    if let Some(m) = fs_magic_override() {
        return window_for_magic(Some(m));
    }
    let Ok(c) = std::ffi::CString::new(library.as_os_str().as_bytes()) else {
        return window_for_magic(None);
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a statfs buffer of the right type.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return window_for_magic(None);
    }
    #[allow(clippy::unnecessary_cast)]
    window_for_magic(Some(st.f_type as u64))
}

#[cfg(target_os = "macos")]
fn fs_window(library: &Path) -> (std::time::Duration, String) {
    use std::os::unix::ffi::OsStrExt;
    if let Some(m) = fs_magic_override() {
        return window_for_magic(Some(m));
    }
    let Ok(c) = std::ffi::CString::new(library.as_os_str().as_bytes()) else {
        return window_for_magic(None);
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a statfs buffer of the right type.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return window_for_magic(None);
    }
    // SAFETY: f_fstypename is a NUL-terminated C string inside the buffer.
    let name = unsafe { std::ffi::CStr::from_ptr(st.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    if name == "apfs" {
        (RECENT_FINE, name)
    } else {
        (RECENT_COARSE, name)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fs_window(_library: &Path) -> (std::time::Duration, String) {
    window_for_magic(fs_magic_override())
}

/// Filesystem the window was chosen for (shown in `/api/found`).
static FS_NAME: Mutex<String> = Mutex::new(String::new());

fn set_window_for(library: &Path) {
    let (w, name) = fs_window(library);
    RECENT_MS.store(w.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    EXT_STAMPS.store(name == "ext2/3/4", std::sync::atomic::Ordering::Relaxed);
    *FS_NAME.lock().unwrap() = name;
}

/// Hide files `/img` or `/video` could not read (fail closed), by the path
/// they hide, until a later request reads them; shown in the quiet line.
static SERVE_NOTES: Mutex<std::collections::BTreeMap<String, String>> =
    Mutex::new(std::collections::BTreeMap::new());

/// An entry's mtime plus a change key mtime alone misses: `rsync -t`/`-a`
/// brings a hide in and puts the folder's old mtime back (A8). On Unix the
/// key is the ctime alone, which no user call can set back; on Windows the
/// ChangeTime (`GetFileInformationByHandleEx(FileBasicInfo)`), likewise.
/// The portable key (other targets, or forced by `ALBUMEN_TEST_HIDE_KEY=
/// portable`) is, for a folder, its entry count plus a hash of its
/// tombstone names and sizes; for a file, its size plus a hash of its bytes
/// (see [`content_key`]). No key: no stamp, so the entry is never trusted
/// and is read again (fail closed, and counted, never a silent fallback).
fn mtime(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::symlink_metadata(p).ok()?;
    stamp_of(p, &m)
}

/// The stamp from metadata already in hand (a folder listing's entry: free
/// on Windows, where a stat costs a file open on NTFS).
fn stamp_of(p: &Path, m: &std::fs::Metadata) -> Option<(SystemTime, u64)> {
    let key = os_key(p, m)?;
    Some((m.modified().ok()?, key))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyKind {
    Ctime,
    ChangeTime,
    Portable,
}

impl KeyKind {
    fn name(self) -> &'static str {
        match self {
            KeyKind::Ctime => "ctime",
            KeyKind::ChangeTime => "changetime",
            KeyKind::Portable => "portable",
        }
    }
}

/// The change key in use: the platform's own, unless a test forces the
/// portable one (`ALBUMEN_TEST_HIDE_KEY=portable`).
fn key_kind() -> KeyKind {
    static KIND: std::sync::OnceLock<KeyKind> = std::sync::OnceLock::new();
    *KIND.get_or_init(|| {
        if std::env::var("ALBUMEN_TEST_HIDE_KEY").is_ok_and(|v| v == "portable") {
            KeyKind::Portable
        } else if cfg!(unix) {
            KeyKind::Ctime
        } else if cfg!(windows) {
            KeyKind::ChangeTime
        } else {
            KeyKind::Portable
        }
    })
}

/// Windows ChangeTime keys taken, and change keys that could not be taken
/// (shown in `/api/found` so a test proves the ChangeTime path is used).
static CHANGETIME_KEYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEY_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn os_key(p: &Path, m: &std::fs::Metadata) -> Option<u64> {
    let key = match key_kind() {
        KeyKind::Portable => Some(portable_key(p, m)),
        KeyKind::Ctime => ctime_key(m),
        KeyKind::ChangeTime => change_time(p),
    };
    if key.is_none() {
        KEY_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    key
}

#[cfg(unix)]
fn ctime_key(m: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let nsec = if coarse_ctime_override() {
        0
    } else {
        m.ctime_nsec() as u64
    };
    Some(
        (m.ctime() as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(nsec),
    )
}

/// Test hook: `ALBUMEN_TEST_COARSE_CTIME=1` drops the ctime's nanoseconds,
/// as a filesystem storing whole seconds would (ext2/3, 128-byte-inode
/// ext4), so a test proves the 2 s window is then used on ext. Honoured
/// only in debug builds or with the `test-hooks` feature, like
/// `ALBUMEN_TEST_FS_MAGIC`.
#[cfg(all(unix, not(any(debug_assertions, feature = "test-hooks"))))]
fn coarse_ctime_override() -> bool {
    false
}

#[cfg(all(unix, any(debug_assertions, feature = "test-hooks")))]
fn coarse_ctime_override() -> bool {
    static COARSE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *COARSE.get_or_init(|| std::env::var("ALBUMEN_TEST_COARSE_CTIME").is_ok_and(|v| v == "1"))
}

#[cfg(not(unix))]
fn ctime_key(_: &std::fs::Metadata) -> Option<u64> {
    None
}

/// The NTFS/ReFS ChangeTime (100 ns ticks), from a handle opened for
/// attributes only (directories too, via FILE_FLAG_BACKUP_SEMANTICS).
/// None if it can't be read or is zero (the filesystem keeps none).
#[cfg(windows)]
fn change_time(p: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)
        .ok()?;
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call and the buffer is a
    // FILE_BASIC_INFO of the size passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } != 0;
    if !ok || basic.ChangeTime == 0 {
        return None;
    }
    CHANGETIME_KEYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(basic.ChangeTime as u64)
}

/// A tombstone's stamp and verdict from ONE open handle (Windows): the
/// stamp (LastWriteTime as std's `modified()` gives it, and ChangeTime) is
/// taken before the read, as [`read_tomb_quiet`] does elsewhere. A file not
/// a plain file, or a stamp that can't be read, gets no stamp (untrusted,
/// read again next time); an unreadable one is an Err (hidden). None for
/// a directory or reparse point (or an open failure): the caller takes the
/// usual path (stat, ChangeTime, read), which follows links as before.
#[cfg(windows)]
#[allow(clippy::type_complexity)]
fn stamp_and_read_windows(p: &Path) -> Option<(Option<(SystemTime, u64)>, Result<bool, String>)> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(p)
    {
        Ok(f) => f,
        Err(_) => return None,
    };
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call and the buffer is a
    // FILE_BASIC_INFO of the size passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } != 0;
    if ok && basic.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return None;
    }
    // 100 ns ticks since 1601, as std converts LastWriteTime for modified().
    const TO_UNIX: i64 = 116_444_736_000_000_000;
    let stamp = if ok && basic.ChangeTime != 0 && basic.LastWriteTime >= TO_UNIX {
        CHANGETIME_KEYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_nanos(((basic.LastWriteTime - TO_UNIX) as u64) * 100);
        Some((t, basic.ChangeTime as u64))
    } else {
        KEY_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        None
    };
    // XO 03:22Z (2): NTFS only promises a file's times are right once the
    // handle that changed it is closed, so a writer still holding the file
    // open may have changed bytes the stamp does not show yet. Fail closed:
    // the stamp is kept only if the size and times are the same before and
    // after the read on this handle and the bytes read are the whole file;
    // otherwise the tombstone is untrusted and read again next time (the
    // writer's close then changes the times, which forces a read anyway).
    let size_before = end_of_file(&file);
    let mut bytes = Vec::new();
    let verdict = match file.read_to_end(&mut bytes) {
        Ok(_) => albumen_ingest::hides::parse_receiver_tombstone(&bytes),
        Err(err) => Err(format!("could not read ({err})")),
    };
    let mut again: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let ok2 = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileBasicInfo,
            (&mut again as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } != 0;
    let steady = ok2
        && again.ChangeTime == basic.ChangeTime
        && again.LastWriteTime == basic.LastWriteTime
        && size_before.is_some()
        && size_before == end_of_file(&file)
        && size_before == Some(bytes.len() as u64);
    if stamp.is_some() && !steady {
        WRITER_UNTRUSTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Some((stamp.filter(|_| steady), verdict))
}

/// A file's size from its handle (FileStandardInfo), as NTFS keeps it now.
#[cfg(windows)]
fn end_of_file(file: &std::fs::File) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
    };
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call and the buffer is a
    // FILE_STANDARD_INFO of the size passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } != 0;
    ok.then_some(info.EndOfFile as u64)
}

/// Tombstones left untrusted because their size or times moved during
/// the read (a writer still open, Windows): `hide_writer_untrusted`.
static WRITER_UNTRUSTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Every tombstone's stamp in one folder from ONE directory handle
/// (Windows, FileIdBothDirectoryInfo): (LastWriteTime as std gives it,
/// ChangeTime), the same stamp [`mtime`] takes with a file open each.
/// Directories, reparse points and entries with no ChangeTime are left out
/// (the caller stats those). Empty if the folder can't be listed.
#[cfg(windows)]
fn dir_change_stamps(
    dir: &Path,
) -> std::collections::HashMap<std::ffi::OsString, (SystemTime, u64)> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY,
    };
    let mut out = std::collections::HashMap::new();
    let suffix: Vec<u16> = albumen_ingest::hides::TOMBSTONE_SUFFIX
        .encode_utf16()
        .collect();
    let Ok(file) = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
    else {
        return out;
    };
    // u64 words keep the buffer 8-byte aligned, as the records need.
    let mut buf = vec![0u64; 8 * 1024];
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        // SAFETY: the handle is open for the call and the buffer is the
        // size passed.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                class,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
            )
        } != 0;
        if !ok {
            break;
        }
        class = FileIdBothDirectoryInfo;
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        loop {
            // SAFETY: `off` follows NextEntryOffset within the buffer the
            // call filled; each record is aligned and holds FileNameLength
            // bytes of name after its header.
            let rec = unsafe { &*(base.add(off) as *const FILE_ID_BOTH_DIR_INFO) };
            let len = rec.FileNameLength as usize / 2;
            let name = unsafe { std::slice::from_raw_parts(rec.FileName.as_ptr(), len) };
            let plain =
                rec.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) == 0;
            // Only tombstones are kept (no name copy for the photos).
            let tomb = name.len() > suffix.len() && name.ends_with(&suffix);
            if tomb && plain && rec.ChangeTime != 0 && rec.LastWriteTime > 0 {
                // FILETIME (100 ns since 1601) -> SystemTime, as std does.
                const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
                let t = rec.LastWriteTime - EPOCH_DIFF;
                let when = if t >= 0 {
                    SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(t as u64 * 100)
                } else {
                    SystemTime::UNIX_EPOCH - std::time::Duration::from_nanos((-t) as u64 * 100)
                };
                CHANGETIME_KEYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                out.insert(
                    std::ffi::OsString::from_wide(name),
                    (when, rec.ChangeTime as u64),
                );
            }
            if rec.NextEntryOffset == 0 {
                break;
            }
            off += rec.NextEntryOffset as usize;
        }
    }
    out
}

#[cfg(not(windows))]
fn dir_change_stamps(_: &Path) -> std::collections::HashMap<std::ffi::OsString, (SystemTime, u64)> {
    Default::default()
}

#[cfg(not(windows))]
fn change_time(_: &Path) -> Option<u64> {
    None
}

/// Largest stamped file (a tombstone) hashed whole; tombstones are tiny.
const CONTENT_KEY_MAX: u64 = 16 * 1024;

/// Portable key: a hash of a stamped file's bytes, so a same-size rewrite
/// with its mtime set back (hidden true <-> false) is seen. A file over the
/// cap or that can't be read gets a fresh key every time: it is always
/// re-read (caching is off for it).
fn content_key(p: &Path, m: &std::fs::Metadata) -> u64 {
    use std::hash::{Hash, Hasher};
    static UNSTABLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let fresh = || UNSTABLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) | 1 << 63;
    if m.len() > CONTENT_KEY_MAX {
        return fresh();
    }
    match std::fs::read(p) {
        Ok(bytes) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            h.finish()
        }
        Err(_) => fresh(),
    }
}

fn portable_key(p: &Path, m: &std::fs::Metadata) -> u64 {
    use std::hash::{Hash, Hasher};
    if !m.is_dir() {
        return m.len().wrapping_mul(31).wrapping_add(content_key(p, m));
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

/// The listed paths hidden right now, and the hide-state version.
///
/// Incremental (XO 02:04Z): each request stats every photo directory (off
/// Unix: lists it, for the entry count and tombstone names and sizes). A
/// directory whose stamp changed, or that was stamped in the same tick, is
/// listed again and only its new or changed tombstones are read. In an
/// unchanged directory only tombstones that do NOT hide right now
/// (`"hidden": false`, or unreadable) are stamped (Unix: ctime; elsewhere a
/// content hash) and re-read on change, so a hide (false -> true, also a
/// same-size swap with mtimes set back) applies on the next request. A
/// tombstone that hides is re-read when its directory changes (a rename
/// replace, as albumen's own hide and unhide do) or at the next listing;
/// an in-place rewrite to unhide waits for that (docs/m7-hide-limits.md).
/// Fails closed: a directory that can't be listed hides its photos, and an
/// unreadable tombstone hides its photo and sets the quiet line.
fn hidden_for(
    server: &Server,
    v: &Arc<LibraryView>,
) -> (Arc<std::collections::HashSet<String>>, u64) {
    use albumen_ingest::hides::TOMBSTONE_SUFFIX;
    let lib = &server.config.library;
    hide_diag::start();
    let mut fresh = FRESH.lock().unwrap();
    hide_diag::mark("lock");
    if fresh.as_ref().is_none_or(|f| f.generation != v.generation) {
        // Grouped by borrowed folder name (one String per folder, not per
        // photo): this runs on the first request of every re-walk.
        let mut by_dir: std::collections::BTreeMap<&str, Vec<u32>> = Default::default();
        for i in 0..v.len() as u32 {
            let d = v.path(i).rsplit_once('/').map_or("", |(d, _)| d);
            by_dir.entry(d).or_default().push(i);
        }
        let version = fresh.as_ref().map_or(0, |f| f.version + 1);
        set_window_for(lib);
        // A post-sync rebuild keeps each folder's state (stamp, trust and
        // tombstones, XO 03:12Z): a folder whose stamp is unchanged and
        // trusted is not listed again, as within one generation, so the
        // first check after a sync lists and reads only what changed. Its
        // trust rests on the same stamp test, so this stays fail-closed.
        let (mut carried, primed, prev, (hard, hard_w)) = match fresh.take() {
            Some(old) => {
                let dir_idx = old
                    .dirs
                    .iter()
                    .enumerate()
                    .map(|(i, d)| (d.clone(), i))
                    .collect();
                let prev = Prev {
                    dir_idx,
                    photos_in: old.photos_in,
                    base: old.base,
                    hidden: old.hidden,
                };
                (
                    old.dirs
                        .into_iter()
                        .zip(old.state)
                        .collect::<std::collections::HashMap<_, _>>(),
                    old.primed,
                    Some(prev),
                    (old.hard, old.hard_w),
                )
            }
            None => (Default::default(), false, None, Default::default()),
        };
        let (dirs, photos_in): (Vec<String>, Vec<Vec<u32>>) =
            by_dir.into_iter().map(|(d, p)| (d.to_string(), p)).unzip();
        let mut state: Vec<DirState> = dirs
            .iter()
            .map(|d| carried.remove(d).unwrap_or_default())
            .collect();
        // A folder still pending across the re-walk withholds what it did,
        // plus every photo the old view did not have (nothing new shows
        // before its listing is confirmed; fail closed).
        if let Some(prev) = &prev {
            for ((d, ds), photos) in dirs.iter().zip(state.iter_mut()).zip(&photos_in) {
                if ds.pending.is_none() || ds.keep.all {
                    continue;
                }
                let old: std::collections::HashSet<&str> = prev
                    .dir_idx
                    .get(d)
                    .map(|&k| {
                        prev.photos_in[k]
                            .iter()
                            .map(|&p| prev.base.path(p))
                            .collect()
                    })
                    .unwrap_or_default();
                let add: Vec<String> = photos
                    .iter()
                    .map(|&p| v.path(p))
                    .filter(|p| !old.contains(p))
                    .map(str::to_string)
                    .collect();
                if !add.is_empty() {
                    Arc::make_mut(&mut ds.keep).set.extend(add);
                }
            }
        }
        *fresh = Some(Fresh {
            generation: v.generation,
            dirs,
            photos_in,
            base: Arc::clone(v),
            state,
            family: None,
            primed,
            rebuild: true,
            version,
            hidden: Default::default(),
            withheld: Default::default(),
            hard,
            hard_w,
            built: Vec::new(),
            cver: Vec::new(),
            contrib: Vec::new(),
            hard_n: Default::default(),
            fam_contrib: Vec::new(),
            fam_dirty: true,
            grew: false,
            grew_sent: Default::default(),
            grew_epoch: WALK_FAILURES.load(std::sync::atomic::Ordering::SeqCst),
            prev,
        });
    }
    hide_diag::mark("init");
    let f = fresh.as_mut().unwrap();
    let v = Arc::clone(&f.base);
    let checked_at = SystemTime::now();
    let primed = f.primed;
    // An unchanged folder: stamp only its non-hiding tombstones (inline,
    // cheap). Returns whether the hidden set may have changed.
    let cheap = |ds: &mut DirState| -> bool {
        let mut changed = false;
        {
            for (rel, t) in ds.tombs.iter_mut() {
                // A tombstone that hides is not re-read in an unchanged
                // folder (even in its first tick): only an unhide or a note
                // change could follow, and those wait for a folder change.
                if t.verdict == Ok(true) {
                    continue;
                }
                if t.trusted && t.stamp.is_some() && mtime(&lib.join(rel)) == t.stamp {
                    if key_kind() == KeyKind::Portable {
                        // mtime() hashed the bytes off Unix.
                        TOMB_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    continue;
                }
                let new = read_tomb(lib, rel, checked_at);
                changed |= new.verdict != t.verdict;
                *t = new;
            }
        }
        if changed {
            ds.ver = next_ver();
        }
        changed
    };
    // Stamp every folder first (one stat each). Unchanged trusted folders
    // take the cheap path; the rest are listed again and their tombstones
    // read on up to 8 threads. Before the first answer that is done in
    // full; after it, the request waits only if a shown photo would blink (BLINK_BUDGET)
    // and every folder not done by then stays pending (its photos withheld
    // but for the carried show, fail closed) until the background finishes it (XO 03:22Z),
    // so a request never waits on thousands of tombstone reads.
    let mut changed = !primed;
    changed |= drain_relists(f);
    let sts: Vec<_> = f.dirs.iter().map(|d| mtime(&lib.join(d))).collect();
    let mut jobs: Vec<RelistJob> = Vec::new();
    // Pending folders that changed after they were handed over: their
    // carried show is checked again below.
    let mut moved: Vec<RelistJob> = Vec::new();
    for (i, ((d, ds), st)) in f.dirs.iter().zip(f.state.iter_mut()).zip(sts).enumerate() {
        if let Some(since) = ds.pending {
            // XO 08:24Z (c): the rsync-style flake. A folder still being
            // listed in the background (its listing started before a hide
            // arrived) kept the carried show computed at hand-over, so a
            // photo whose tombstone came after it stayed shown (fail open)
            // until that stale listing landed. When the folder's stamp
            // moved (or is not yet trusted) the show is re-checked against
            // the tombstones present now, none of them trusted (fail
            // closed); the listing itself is redone when it lands.
            if primed && (st != ds.sent_st || !trusted_at(&ds.sent_st, checked_at)) {
                ds.sent_st = st;
                let mut probe = DirState::default();
                for (rel, stamp) in ds.sent_shows.iter() {
                    probe.tombs.insert(
                        rel.clone(),
                        TombState {
                            stamp: Some(*stamp),
                            trusted: true,
                            verdict: Ok(false),
                        },
                    );
                }
                moved.push(RelistJob {
                    i,
                    dir: d.clone(),
                    ds: probe,
                    st,
                });
            }
            match ds.retry_at {
                // A listing that kept panicking: sent again after its
                // backoff (XO 04:41Z (4)); shown photos stay under `keep`.
                Some(t) if std::time::Instant::now() < t => continue,
                Some(_) => {
                    eprintln!(
                        "albumen gui: hide: listing {d} again after {} failed tries",
                        ds.fails
                    );
                }
                None => {
                    // XO 03:36Z (5): a folder never stays pending silently. A
                    // worker that died or hangs is logged and the folder sent
                    // again (its photos stay withheld meanwhile).
                    if since.elapsed() < PENDING_LIMIT {
                        continue;
                    }
                    eprintln!(
                        "albumen gui: hide: {d} still pending after {:?}; listing it again",
                        since.elapsed()
                    );
                    REDISPATCHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        if primed && ds.trusted && st.is_some() && st == ds.stamp {
            changed |= cheap(ds);
        } else if APPLY_ONLY.get() {
            // A worker applying its results starts no re-list; the next
            // request does.
        } else {
            let fails = ds.fails;
            let taken = std::mem::take(ds);
            ds.fails = fails;
            ds.pending = Some(std::time::Instant::now());
            ds.sent_st = st;
            ds.sent_shows = Arc::new(
                taken
                    .tombs
                    .iter()
                    .filter(|(_, t)| t.trusted && t.verdict == Ok(false))
                    .filter_map(|(r, t)| Some((r.clone(), t.stamp?)))
                    .collect(),
            );
            jobs.push(RelistJob {
                i,
                dir: d.clone(),
                ds: taken,
                st,
            });
        }
    }
    hide_diag::mark("stamps");
    if !moved.is_empty() {
        let (keeps, _) = keep_shown(lib, f, &moved);
        for (j, keep) in moved.iter().zip(keeps) {
            f.state[j.i].keep = Arc::new(keep);
            f.state[j.i].ver = next_ver();
        }
        changed = true;
    }
    // Folders (index in `f`) where a shown photo would blink out.
    let mut blinking: std::collections::HashSet<usize> = Default::default();
    if !jobs.is_empty() && primed {
        // Which photos of each folder about to go pending stay on screen:
        // one stat of each shown photo's own tombstone, on up to 8 threads.
        let t = std::time::Instant::now();
        let (keeps, b) = keep_shown(lib, f, &jobs);
        for ((j, keep), b) in jobs.iter().zip(keeps).zip(b) {
            f.state[j.i].keep = Arc::new(keep);
            if b {
                blinking.insert(j.i);
            }
        }
        let us = t.elapsed().as_micros() as u64;
        KEEP_STAT_LAST_US.store(us, std::sync::atomic::Ordering::Relaxed);
        KEEP_STAT_MAX_US.fetch_max(us, std::sync::atomic::Ordering::Relaxed);
    }
    hide_diag::mark("keep");
    // The previous view has served once this generation has been checked.
    if !APPLY_ONLY.get() {
        f.prev = None;
    }
    if !jobs.is_empty() {
        changed = true;
        // Folders that would blink are listed first, so the wait for them
        // is not spent behind the others.
        jobs.sort_by_key(|j| !blinking.contains(&j.i));
        let rx = spawn_relists(lib.to_path_buf(), jobs, checked_at, f.generation);
        // Once primed the request does not wait at all (XO 04:08Z): the
        // per-photo carried show keeps what is on screen, and a coalescing
        // background applier puts finished folders (and the rebuilt view)
        // in place. Only the very first answer is done in full.
        let waits = primed && !blinking.is_empty();
        let deadline = primed.then(|| {
            std::time::Instant::now()
                + if !blinking.is_empty() {
                    BLINK_BUDGET
                } else {
                    Default::default()
                }
        });
        loop {
            let got = match deadline {
                None => rx.recv().ok(),
                Some(t) => rx
                    .recv_timeout(t.saturating_duration_since(std::time::Instant::now()))
                    .ok(),
            };
            match got {
                Some(done) => {
                    // Waiting is only for the folders that would blink:
                    // once they are in, the rest finish in the background
                    // (their shown photos stay under `keep`), XO 05:41Z (2).
                    if done.generation == f.generation {
                        blinking.remove(&done.i);
                    }
                    apply_relist(f, done);
                    if waits && blinking.is_empty() {
                        break;
                    }
                }
                None => break,
            }
        }
        // Whatever is still running reports through the shared queue.
        RELISTS.lock().unwrap().push(rx);
    }
    hide_diag::mark("wait");
    PENDING_DIRS.store(
        f.state.iter().filter(|d| d.pending.is_some()).count() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    if lib.join("genesis.json").is_file() {
        let st: Vec<_> = family_logs(lib).iter().map(|p| mtime(p)).collect();
        let fresh_family = f
            .family
            .as_ref()
            .is_some_and(|fam| fam.trusted && fam.stamp == st);
        if !fresh_family {
            let paths: Vec<String> = (0..v.len() as u32).map(|i| v.path(i).to_string()).collect();
            let h = albumen_ingest::hides::hidden_paths(lib, &paths, None);
            for err in &h.errors {
                eprintln!("albumen gui: hide: {}", err.replace('\n', ": "));
            }
            let trusted = st.iter().all(|s| trusted_at(s, checked_at));
            f.family = Some(FamilyState {
                stamp: st,
                trusted,
                paths: h.paths.into_iter().collect(),
                errors: h.errors,
            });
            f.fam_dirty = true;
            changed = true;
        }
    } else if f.family.take().is_some() {
        f.fam_dirty = true;
        changed = true;
    }
    f.primed = true;
    changed |= std::mem::take(&mut f.rebuild);
    if !changed {
        return (Arc::clone(&f.hidden), f.version);
    }
    let built = std::time::Instant::now();
    // The hard-hidden set, kept up to date per changed folder: a folder
    // whose state version is unchanged since the last build is skipped, so
    // a request with one changed folder does not walk every tombstone.
    let mut hard = std::mem::take(&mut f.hard);
    let full = f.cver.len() != f.state.len();
    let old_full = full.then(|| Arc::clone(&hard));
    if full {
        f.cver = vec![u64::MAX; f.state.len()];
        f.contrib = vec![Contrib::default(); f.state.len()];
        f.hard_n.clear();
        f.fam_contrib.clear();
        f.fam_dirty = true;
        hard = Default::default();
    }
    let mut added: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut watch_changed = full;
    let mut errors_changed = full;
    {
        let hs = Arc::make_mut(&mut hard);
        let hard_n = &mut f.hard_n;
        let mut swap =
            |old: &[String], new: &[String], added: &mut Vec<String>, removed: &mut Vec<String>| {
                let o: std::collections::HashSet<&str> = old.iter().map(String::as_str).collect();
                let n: std::collections::HashSet<&str> = new.iter().map(String::as_str).collect();
                for p in old.iter().filter(|p| !n.contains(p.as_str())) {
                    if let Some(c) = hard_n.get_mut(p) {
                        *c -= 1;
                        if *c == 0 {
                            hard_n.remove(p);
                            hs.remove(p);
                            removed.push(p.clone());
                        }
                    }
                }
                for p in new.iter().filter(|p| !o.contains(p.as_str())) {
                    let c = hard_n.entry(p.clone()).or_insert(0);
                    *c += 1;
                    if *c == 1 {
                        hs.insert(p.clone());
                        added.push(p.clone());
                    }
                }
            };
        for (i, (ds, photos)) in f.state.iter().zip(&f.photos_in).enumerate() {
            if f.cver[i] == ds.ver {
                continue;
            }
            f.cver[i] = ds.ver;
            let mut c = Contrib::default();
            for (rel, t) in &ds.tombs {
                c.tombs.push(rel.clone());
                let hides = match &t.verdict {
                    Ok(h) => *h,
                    Err(e) => {
                        c.errors.push(e.clone());
                        true
                    }
                };
                if hides {
                    if let Some(o) = rel.strip_suffix(TOMBSTONE_SUFFIX) {
                        c.hides.push(o.to_string());
                    }
                }
            }
            if ds.unlisted {
                c.hides
                    .extend(photos.iter().map(|&k| v.path(k).to_string()));
            }
            let old = std::mem::take(&mut f.contrib[i]);
            watch_changed |= old.tombs != c.tombs;
            errors_changed |= old.errors != c.errors;
            swap(&old.hides, &c.hides, &mut added, &mut removed);
            f.contrib[i] = c;
        }
        if std::mem::take(&mut f.fam_dirty) {
            let new: Vec<String> = f
                .family
                .as_ref()
                .map_or(Vec::new(), |fam| fam.paths.clone());
            let old = std::mem::take(&mut f.fam_contrib);
            swap(&old, &new, &mut added, &mut removed);
            f.fam_contrib = new;
            errors_changed = true;
        }
    }
    if let Some(old) = &old_full {
        added = hard.iter().filter(|p| !old.contains(*p)).cloned().collect();
        removed = old.iter().filter(|p| !hard.contains(*p)).cloned().collect();
    }
    if errors_changed {
        let mut errors: Vec<String> = f
            .contrib
            .iter()
            .flat_map(|c| c.errors.iter().cloned())
            .collect();
        if let Some(fam) = &f.family {
            errors.extend(fam.errors.iter().cloned());
        }
        set_hide_note(hide_note(&errors));
    }
    if watch_changed {
        *HIDE_WATCH.lock().unwrap() = Some(
            f.contrib
                .iter()
                .flat_map(|c| c.tombs.iter().cloned())
                .collect(),
        );
    }
    let hidden: &std::collections::HashSet<String> = &hard;
    // An unhide needs the photo back, which the view no longer holds: list
    // again in the background (hides only ever narrow the view here). A
    // photo only withheld (pending folder) is in the base view, so putting
    // that back is enough. The photos that left the hard set are `removed`
    // (plus `hard_w`), so only those are walked here.
    // (Checked after the withheld update below: a hard-hidden photo whose
    // folder just went pending moves from the hard set to the withheld set,
    // which is not an unhide; testing it against the old withheld set made
    // every re-list of a folder with tombstones look like one, and each
    // re-walked the library: XO 05:06Z (c).)
    let mut unhid = false;
    // The withheld set is updated in place, folder by folder: only folders
    // whose pending flag or keep set changed since the last build are
    // revisited, so the cost follows the changed folders, not the library
    // (XO 05:00Z (a)). A new generation starts from empty.
    let new_gen = f.built.len() != f.state.len();
    if new_gen {
        f.built = vec![(false, Default::default()); f.state.len()];
        let old: Vec<String> = f.withheld.drain().collect();
        // Photos withheld under the old generation all count as changed.
        let hidden_all = Arc::make_mut(&mut f.hidden);
        // (A new generation is a full build: `old_full` is the old hard set.)
        for p in old {
            if !old_full.as_ref().is_some_and(|h| h.contains(&p)) {
                hidden_all.remove(&p);
            }
        }
    }
    let mut touched: Vec<String> = Vec::new();
    let mut released = false;
    for (i, (ds, photos)) in f.state.iter().zip(&f.photos_in).enumerate() {
        let now = (ds.pending.is_some(), &ds.keep);
        let i_photos = photos;
        let was = f.built[i].clone();
        if was.0 == now.0 && (!now.0 || Arc::ptr_eq(&was.1, now.1)) {
            continue;
        }
        let mut visit = |p: &str, f_withheld: &mut std::collections::HashSet<String>| {
            let w0 = was.0 && was.1.holds(p);
            let w1 = now.0 && now.1.holds(p);
            if w0 == w1 {
                return;
            }
            if w1 {
                f_withheld.insert(p.to_string());
            } else {
                f_withheld.remove(p);
                released |= !hidden.contains(p);
            }
            touched.push(p.to_string());
        };
        if (was.0 && was.1.all) || (now.0 && now.1.all) {
            for &k in i_photos {
                visit(v.path(k), &mut f.withheld);
            }
        } else {
            let mut seen = std::collections::HashSet::new();
            for p in was.1.set.iter().chain(&now.1.set) {
                if seen.insert(p.as_str()) {
                    visit(p, &mut f.withheld);
                }
            }
        }
        f.built[i] = (now.0, Arc::clone(now.1));
    }
    let withheld = &f.withheld;
    // A photo hidden, then withheld while its folder was pending, then
    // shown is an unhide too: test against the hard-hidden set, not only
    // the photos withheld at the last check (the base view lacks it).
    if let Some(p) = removed
        .iter()
        .chain(&f.hard_w)
        .find(|p| !hidden.contains(*p) && !withheld.contains(*p))
    {
        unhid = true;
        rewalk_cause(format!("unhid {p}"));
    }
    // Hard-hidden before and now only withheld; walks the (small) hard sets.
    let hard_w: std::collections::HashSet<String> = removed
        .iter()
        .chain(&f.hard_w)
        .filter(|p| withheld.contains(*p) && !hidden.contains(*p))
        .cloned()
        .collect();
    // The served set (hard plus withheld) changes only at the touched
    // photos and where the hard set changed.
    touched.extend(removed.iter().cloned());
    if new_gen {
        touched.extend(hidden.iter().cloned());
    } else {
        touched.extend(added.iter().cloned());
    }
    let mut delta = false;
    {
        let all = Arc::make_mut(&mut f.hidden);
        for p in touched {
            let want = hidden.contains(&p) || f.withheld.contains(&p);
            delta |= if want { all.insert(p) } else { all.remove(&p) };
        }
    }
    if unhid || released || delta {
        f.version += 1;
    }
    f.hard_w = hard_w;
    f.hard = hard;
    HIDE_VERSION.store(f.version, std::sync::atomic::Ordering::Relaxed);
    let us = built.elapsed().as_micros() as u64;
    BUILD_LAST_US.store(us, std::sync::atomic::Ordering::Relaxed);
    BUILD_MAX_US.fetch_max(us, std::sync::atomic::Ordering::Relaxed);
    let out = (Arc::clone(&f.hidden), f.version);
    let grew = std::mem::take(&mut f.grew);
    if grew {
        GREW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if released {
        let mut lv = server.library.lock().unwrap();
        if lv.as_ref().is_some_and(|c| c.generation == f.generation) {
            *lv = Some(Arc::clone(&f.base));
        }
    }
    drop(fresh);
    hide_diag::mark("build");
    if unhid || grew {
        if let Some(s) = SERVER.lock().unwrap().as_ref().and_then(|w| w.upgrade()) {
            refresh_thumbs(&s);
        }
    }
    out
}

/// One folder handed to [`spawn_relists`]: its index in `Fresh` (valid
/// for `generation`), name, state and the stamp taken before the listing.
struct RelistJob {
    i: usize,
    dir: String,
    ds: DirState,
    st: Option<(SystemTime, u64)>,
}

struct RelistDone {
    generation: u64,
    i: usize,
    dir: String,
    ds: DirState,
}

/// Once primed, a request waits for re-lists only when a photo on screen
/// would otherwise be withheld (its tombstone changed or is untrusted), and
/// then at most this long (XO 03:22Z / 04:08Z).
const BLINK_BUDGET: std::time::Duration = std::time::Duration::from_millis(40);

/// Re-list workers still running, drained by each check.
static RELISTS: Mutex<Vec<std::sync::mpsc::Receiver<RelistDone>>> = Mutex::new(Vec::new());
/// Folders pending now, folders confirmed after being withheld, and the
/// longest and the latest time (from hand-over to applied) one was withheld.
static PENDING_DIRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WITHHELD_DIRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WITHHELD_MAX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WITHHELD_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Every withheld time (us, hand-over to applied), for the p95 in
/// `/api/found` (capped).
static WITHHELD_SAMPLES: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// The hidden set's version, so the page can redraw when it changes.
static HIDE_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Time to decide which photos of the folders going pending stay shown.
static KEEP_STAT_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEEP_STAT_MAX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEEP_STATS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Worker panics caught, and folders sent again after `PENDING_LIMIT`.
static WORKER_PANICS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REDISPATCHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Listings that kept panicking (all tries), and re-walks started because a
/// confirmed folder held a photo synced in.
static EXHAUSTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GREW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Why each library re-walk from the hide checks started (FRAGO 06:13Z
/// (3)): "grew <photo> <tombstone state>" or "unhid <photo>", first 200.
static REWALK_CAUSES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn rewalk_cause(c: String) {
    eprintln!("albumen gui: hide: re-walk: {c}");
    let mut v = REWALK_CAUSES.lock().unwrap();
    if v.len() < 200 {
        v.push(c);
    }
}
/// Time to build the hidden and withheld sets (the last and longest).
static BUILD_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BUILD_MAX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The longest backoff before a folder whose listing kept panicking is
/// listed again.
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);
/// A folder pending this long is logged and listed again (XO 03:36Z (5)).
const PENDING_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);
/// A worker applies what it has finished at least this often, so a
/// withheld photo comes back without waiting for the next request.
const APPLY_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// For each folder about to go pending: the photos the previous view showed
/// whose own tombstone is absent, or unchanged since a trusted read that
/// said show. One listing per folder (and a stat per tombstone present), on up to 8 threads. A stat that
/// fails other than "not found" withholds the photo (fail closed).
/// Also says, per folder, whether a shown photo is withheld (it would blink
/// out).
fn keep_shown(lib: &Path, f: &Fresh, jobs: &[RelistJob]) -> (Vec<Withhold>, Vec<bool>) {
    use albumen_ingest::hides::TOMBSTONE_SUFFIX;
    // Photos of each folder that were off screen (the reference hidden set,
    // grouped by folder): they stay withheld. Walks the hidden set, which
    // follows the tombstones, never the photos of the folder.
    let off: &std::collections::HashSet<String> = match &f.prev {
        None => &f.hidden,
        Some(prev) => &prev.hidden,
    };
    let want: std::collections::HashMap<&str, usize> = jobs
        .iter()
        .enumerate()
        .map(|(n, j)| (j.dir.as_str(), n))
        .collect();
    let mut sets: Vec<std::collections::HashSet<String>> = vec![Default::default(); jobs.len()];
    for p in off {
        let d = p.rsplit_once('/').map_or("", |(d, _)| d);
        if let Some(&n) = want.get(d) {
            sets[n].insert(p.clone());
        }
    }
    let threads = hide_threads();
    // One listing of the folder instead of one stat per shown photo (the
    // stats cost up to 150 ms a request on NTFS, Windows CI 0e54af1): a
    // photo whose tombstone is in the listing gets the stat and trusted-read
    // check and is withheld unless that clears it. A folder that cannot be
    // listed withholds everything (fail closed).
    let fold = |n: &str| -> String {
        if cfg!(windows) {
            n.to_lowercase()
        } else {
            n.to_string()
        }
    };
    let check = |j: &RelistJob, off: &std::collections::HashSet<String>| -> (Withhold, bool) {
        let all = (
            Withhold {
                all: true,
                set: Default::default(),
            },
            false,
        );
        if j.ds.unlisted {
            return (Withhold::default(), false);
        }
        KEEP_STATS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Folded photo name -> tombstone file name, for tombstones present.
        // The entry's metadata rides along (Windows: from the listing itself,
        // no per-tombstone stat; Windows CI 90f10f2 keep_max 100 ms at 10k).
        let mut tombs: std::collections::HashMap<String, Option<std::fs::Metadata>> =
            Default::default();
        match std::fs::read_dir(lib.join(&j.dir)) {
            Ok(rd) => {
                for e in rd {
                    let Ok(e) = e else { return all };
                    let n = e.file_name();
                    // Only a tombstone's name is copied: a folder of
                    // thousands of photos costs no allocation per photo
                    // (Windows: the suffix compared case-insensitively).
                    let b = n.as_encoded_bytes();
                    let sfx = TOMBSTONE_SUFFIX.as_bytes();
                    let is_tomb = b.len() >= sfx.len() && {
                        let tail = &b[b.len() - sfx.len()..];
                        if cfg!(windows) {
                            tail.eq_ignore_ascii_case(sfx)
                        } else {
                            tail == sfx
                        }
                    };
                    if !is_tomb {
                        continue;
                    }
                    let n = n.to_string_lossy();
                    let stem = &n[..n.len() - TOMBSTONE_SUFFIX.len()];
                    tombs.insert(fold(stem), e.metadata().ok());
                }
            }
            Err(_) => return all,
        }
        let mut set = off.clone();
        let mut blink = false;
        if tombs.is_empty() {
            return (Withhold { all: false, set }, false);
        }
        let photos: Vec<String> = if cfg!(windows) {
            // Case-folded names: match against the folder's photos.
            match &f.prev {
                None => f.photos_in[j.i]
                    .iter()
                    .map(|&p| f.base.path(p).to_string())
                    .collect(),
                Some(prev) => prev.dir_idx.get(&j.dir).map_or(Vec::new(), |&k| {
                    prev.photos_in[k]
                        .iter()
                        .map(|&p| prev.base.path(p).to_string())
                        .collect()
                }),
            }
        } else {
            tombs
                .keys()
                .map(|stem| {
                    if j.dir.is_empty() {
                        stem.clone()
                    } else {
                        format!("{}/{stem}", j.dir)
                    }
                })
                .collect()
        };
        for p in photos {
            if set.contains(&p) {
                continue;
            }
            let name = p.rsplit_once('/').map_or(p.as_str(), |(_, n)| n);
            let Some(meta) = tombs.get(&fold(name)) else {
                continue;
            };
            let rel = format!("{p}{TOMBSTONE_SUFFIX}");
            let path = lib.join(&rel);
            KEEP_STATS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Listed, so present: its metadata came with the listing (an
            // entry whose metadata could not be read is not kept).
            let kept = meta.as_ref().is_some_and(|m| {
                j.ds.tombs.get(&rel).is_some_and(|t| {
                    t.trusted
                        && t.verdict == Ok(false)
                        && t.stamp.is_some()
                        && stamp_of(&path, m) == t.stamp
                })
            });
            if !kept {
                // Only a photo on screen blinks out (Linux: the tombstone's
                // photo file exists; Windows: taken from the view).
                blink |= cfg!(windows) || lib.join(&p).is_file();
                set.insert(p);
            }
        }
        (Withhold { all: false, set }, blink)
    };
    let run = |n: usize| check(&jobs[n], &sets[n]);
    if threads == 1 || jobs.len() < 4 {
        return (0..jobs.len()).map(run).unzip();
    }
    let out: Vec<Mutex<Option<(Withhold, bool)>>> =
        (0..jobs.len()).map(|_| Mutex::new(None)).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| loop {
                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n >= jobs.len() {
                    return;
                }
                *out[n].lock().unwrap() = Some(run(n));
            });
        }
    });
    let out: Vec<(Withhold, bool)> = out
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap_or_default())
        .collect();
    out.into_iter().unzip()
}

/// Test hook: `ALBUMEN_TEST_RELIST_DELAY_MS` delays each background
/// re-list, so a test can hold a folder pending (XO 03:36Z (4)).
/// `ALBUMEN_TEST_RELIST_PANIC=<dir>` makes the re-list of that folder panic
/// once (XO 03:36Z (5)). Honoured only in debug builds or with the
/// `test-hooks` feature.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn relist_hooks(lib: &Path, dir: &str) {
    // `ALBUMEN_TEST_RELIST_GATE=<dir>` (XO 04:08Z): a test holds that
    // folder pending for certain rather than by timing.
    // While the file `<library>/.albumen-test-gate` exists the re-list
    // blocks; removing it releases the gate.
    static GATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if std::env::var("ALBUMEN_TEST_RELIST_GATE").is_ok_and(|d| d == dir) {
        let flag = lib.join(".albumen-test-gate");
        while flag.exists() {
            GATED.store(true, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    // `ALBUMEN_TEST_RELIST_PANIC_AFTER_GATE=1`: the panic below fires only
    // on a re-list that was held at the gate (not the startup listing).
    if std::env::var("ALBUMEN_TEST_RELIST_PANIC_AFTER_GATE").is_ok_and(|v| v == "1")
        && !GATED.load(std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    static DELAY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *DELAY.get_or_init(|| {
        std::env::var("ALBUMEN_TEST_RELIST_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    });
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    // `ALBUMEN_TEST_RELIST_PANIC_TIMES=<n>` (default 1): panic that often.
    static PANICKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let times = std::env::var("ALBUMEN_TEST_RELIST_PANIC_TIMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1u64);
    if std::env::var("ALBUMEN_TEST_RELIST_PANIC").is_ok_and(|d| d == dir)
        && PANICKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < times
    {
        panic!("ALBUMEN_TEST_RELIST_PANIC {dir}");
    }
}

#[cfg(not(any(debug_assertions, feature = "test-hooks")))]
fn relist_hooks(_: &Path, _: &str) {}

/// Test hook: `ALBUMEN_TEST_TOMB_ERROR=<rel>` makes reading that tombstone
/// fail (the worker_error variant of the no-flicker test).
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn tomb_error_hook(rel: &str) -> Option<&'static str> {
    std::env::var("ALBUMEN_TEST_TOMB_ERROR")
        .is_ok_and(|r| r == rel)
        .then_some("test hook: unreadable")
}

#[cfg(not(any(debug_assertions, feature = "test-hooks")))]
fn tomb_error_hook(_: &str) -> Option<&'static str> {
    None
}

thread_local! {
    /// Set on the thread that applies finished re-lists for a worker.
    static APPLY_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Apply finished re-lists now (from a worker), so a withheld photo comes
/// back without waiting for the next request (XO 03:36Z (2)).
fn apply_landed() {
    use std::sync::atomic::Ordering::SeqCst;
    // One applier at a time; a landing meanwhile makes it run once more,
    // so N finished folders cost a few rebuilds, not N.
    APPLY_AGAIN.store(true, SeqCst);
    if APPLYING.swap(true, SeqCst) {
        return;
    }
    APPLY_ONLY.set(true);
    while APPLY_AGAIN.swap(false, SeqCst) {
        let Some(s) = SERVER.lock().unwrap().as_ref().and_then(|w| w.upgrade()) else {
            break;
        };
        let cur = s.library.lock().unwrap().clone();
        if let Some(v) = cur {
            current_view(&s, v);
        }
    }
    APPLYING.store(false, SeqCst);
    // A landing between the last pass and the release above.
    if APPLY_AGAIN.load(SeqCst) {
        apply_landed();
    }
}

static APPLYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static APPLY_AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Threads for re-lists and the carried-show stats: one core is left for
/// requests (XO 04:08Z, the Windows k=1/k=2 stalls).
fn hide_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .saturating_sub(1)
        .clamp(1, 8)
}

/// List and read `jobs` on up to 8 threads, off the request; each folder
/// is sent as soon as it is done.
fn spawn_relists(
    lib: PathBuf,
    jobs: Vec<RelistJob>,
    checked_at: SystemTime,
    generation: u64,
) -> std::sync::mpsc::Receiver<RelistDone> {
    let (tx, rx) = std::sync::mpsc::channel();
    let threads = hide_threads().min(jobs.len());
    let mut work: Vec<Vec<RelistJob>> = (0..threads).map(|_| Vec::new()).collect();
    for (n, j) in jobs.into_iter().enumerate() {
        work[n % threads].push(j);
    }
    for w in work {
        let tx = tx.clone();
        let lib = lib.clone();
        let run = move || {
            let mut applied = std::time::Instant::now();
            let n = w.len();
            for (k, mut j) in w.into_iter().enumerate() {
                let mut first = Some(std::mem::take(&mut j.ds));
                let mut out = None;
                for attempt in 0..3 {
                    // A retry starts from nothing (every tombstone read
                    // again); the folder stays pending meanwhile, so what is
                    // on screen stays and nothing new shows.
                    let mut ds = first.take().unwrap_or_default();
                    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        relist_hooks(&lib, &j.dir);
                        relist_dir(&lib, &j.dir, &mut ds, j.st, checked_at);
                        ds
                    }));
                    match ok {
                        Ok(ds) => {
                            out = Some(ds);
                            break;
                        }
                        Err(_) => {
                            // XO 03:36Z (5) / 04:08Z: logged and retried.
                            eprintln!(
                                "albumen gui: hide: worker panicked; retrying {} (attempt {})",
                                j.dir,
                                attempt + 2
                            );
                            WORKER_PANICS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
                let ds = out.unwrap_or_else(|| {
                    eprintln!(
                        "albumen gui: hide: listing {} kept panicking; keeping its shown photos, withholding the rest and listing it again later",
                        j.dir
                    );
                    DirState {
                        failed: true,
                        ..Default::default()
                    }
                });
                let done = RelistDone {
                    generation,
                    i: j.i,
                    dir: j.dir,
                    ds,
                };
                if tx.send(done).is_err() {
                    return;
                }
                if k + 1 == n || applied.elapsed() >= APPLY_EVERY {
                    // On its own thread: a request may hold the lock while
                    // it waits for this worker's next folder.
                    let _ = std::thread::Builder::new()
                        .name("albumen-hide-apply".into())
                        .spawn(apply_landed);
                    applied = std::time::Instant::now();
                }
            }
        };
        if let Err(e) = std::thread::Builder::new()
            .name("albumen-relist".into())
            .spawn(run)
        {
            // No thread: the folders stay pending and are sent again after
            // PENDING_LIMIT.
            eprintln!("albumen gui: hide: can't start a re-list worker ({e})");
        }
    }
    rx
}

/// Test hook (`ALBUMEN_TEST_NO_GROW=1`): a re-listed folder never counts
/// as grown, so a test sees the unhide path alone bring a photo back
/// (FRAGO 06:29Z (4)).
fn test_no_grow() -> bool {
    static NO: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO.get_or_init(|| std::env::var("ALBUMEN_TEST_NO_GROW").is_ok_and(|v| v == "1"))
}

/// Put a finished folder back (by index if the generation is the same,
/// else by name; a folder no longer listed is dropped).
fn apply_relist(f: &mut Fresh, done: RelistDone) {
    let i = if done.generation == f.generation {
        Some(done.i)
    } else {
        f.dirs.iter().position(|d| *d == done.dir)
    };
    let Some(i) = i.filter(|&i| i < f.state.len()) else {
        return;
    };
    let slot = &mut f.state[i];
    if done.ds.failed {
        // XO 04:41Z (4): still pending (fail closed for anything new or
        // changed), its shown photos kept per photo, and sent again after
        // a backoff of 1, 2, 4 .. 60 s.
        slot.fails = slot.fails.saturating_add(1);
        let wait = std::time::Duration::from_secs(1 << (slot.fails - 1).min(6)).min(RETRY_MAX);
        slot.retry_at = Some(std::time::Instant::now() + wait);
        slot.pending.get_or_insert_with(std::time::Instant::now);
        EXHAUSTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let mut ds = done.ds;
    // A photo synced in: listed, not hidden by its own tombstone, and not
    // in the view (the view is from the last walk).
    let photos = std::mem::take(&mut ds.photos);
    let failed_walks = WALK_FAILURES.load(std::sync::atomic::Ordering::SeqCst);
    if failed_walks != f.grew_epoch {
        f.grew_sent.clear();
        f.grew_epoch = failed_walks;
    }
    if !photos.is_empty() && !f.grew && !test_no_grow() {
        let have: std::collections::HashSet<&str> =
            f.photos_in[i].iter().map(|&j| f.base.path(j)).collect();
        let why = photos.iter().find_map(|p| {
            let t = ds
                .tombs
                .get(&format!("{p}{}", albumen_ingest::hides::TOMBSTONE_SUFFIX));
            (!have.contains(p.as_str())
                && !f.grew_sent.contains(p)
                && t.is_none_or(|t| t.verdict == Ok(false)))
            .then(|| {
                format!(
                    "grew {p} tomb={} view_dir_photos={}",
                    match t {
                        None => "none".to_string(),
                        Some(t) => format!("{:?}", t.verdict),
                    },
                    have.len()
                )
            })
        });
        f.grew = why.is_some();
        if let Some(w) = why {
            // Every new photo this listing brings rides the same walk.
            for p in &photos {
                if !have.contains(p.as_str()) {
                    f.grew_sent.insert(p.clone());
                }
            }
            rewalk_cause(w);
        }
    }
    let slot = &mut f.state[i];
    if let Some(since) = slot.pending {
        let us = since.elapsed().as_micros() as u64;
        WITHHELD_DIRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WITHHELD_MAX_US.fetch_max(us, std::sync::atomic::Ordering::Relaxed);
        WITHHELD_LAST_US.store(us, std::sync::atomic::Ordering::Relaxed);
        let mut w = WITHHELD_SAMPLES.lock().unwrap();
        if w.len() < 100_000 {
            w.push(us);
        }
    }
    *slot = ds;
    slot.pending = None;
}

/// Apply every finished background re-list; whether any was applied.
fn drain_relists(f: &mut Fresh) -> bool {
    let mut any = false;
    let mut rs = RELISTS.lock().unwrap();
    rs.retain(|rx| loop {
        match rx.try_recv() {
            Ok(done) => {
                apply_relist(f, done);
                any = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
        }
    });
    any
}

/// List one folder again (stamp `st` taken before the listing) and read
/// each tombstone that is new or whose stamp changed. Returns whether the
/// hidden set may have changed.
fn relist_dir(
    lib: &Path,
    d: &str,
    ds: &mut DirState,
    st: Option<(SystemTime, u64)>,
    checked_at: SystemTime,
) -> bool {
    use albumen_ingest::hides::TOMBSTONE_SUFFIX;
    let mut changed = false;
    // Listed again (stamp taken before the listing). The hidden set
    // is rebuilt only if a tombstone came, went or reads differently.
    ds.trusted = trusted_at(&st, checked_at);
    ds.stamp = st;
    let mut old = std::mem::take(&mut ds.tombs);
    DIRS_LISTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match std::fs::read_dir(lib.join(d)) {
        Ok(rd) => {
            changed |= ds.unlisted;
            ds.unlisted = false;
            ds.photos.clear();
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = if d.is_empty() {
                    name.clone()
                } else {
                    format!("{d}/{name}")
                };
                if !name.ends_with(TOMBSTONE_SUFFIX) || name == TOMBSTONE_SUFFIX {
                    if is_photo(&rel) {
                        ds.photos.push(rel);
                    }
                    continue;
                }
                let keep = old.remove(&rel).filter(|t| {
                    (t.trusted || t.verdict == Ok(true)) && t.stamp.is_some() && {
                        if key_kind() == KeyKind::Portable {
                            TOMB_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        mtime(&lib.join(&rel)) == t.stamp
                    }
                });
                let t = match keep {
                    Some(t) => t,
                    None => {
                        let t = read_tomb(lib, &rel, checked_at);
                        changed = true;
                        t
                    }
                };
                ds.tombs.insert(rel, t);
            }
        }
        Err(err) => {
            eprintln!("albumen gui: hide: can't list {d} ({err}); hiding its photos");
            if !ds.unlisted {
                changed = true;
            }
            ds.unlisted = true;
            ds.trusted = false;
        }
    }
    changed |= !old.is_empty();
    ds.ver = next_ver();
    changed
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
    let (hidden, version) = hidden_for(server, &v);
    {
        let filtered = FILTERED.lock().unwrap();
        if filtered.as_ref().is_some_and(|(w, ver)| {
            *ver == version && w.upgrade().is_some_and(|f| Arc::ptr_eq(&f, &v))
        }) {
            hide_diag::finish("same");
            return v;
        }
    }
    // Which photos are hidden: one lookup per photo, split over the hide
    // threads for a large library (the first request after a sync rebuilds
    // the view: 380k lookups, XO 05:41Z (2)).
    let shown = shown_mask(&v, &hidden);
    hide_diag::mark("mask");
    if shown.iter().all(|&s| s) {
        *FILTERED.lock().unwrap() = Some((Arc::downgrade(&v), version));
        hide_diag::finish("none");
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
    // Copied a run of shown photos at a time (a few thousand hidden among
    // 380k leave long runs), the bytes on the hide threads: the arenas are
    // ~100 MB at 380k and a copy per photo took ~50 ms (XO 05:41Z (2)).
    let n = v.len();
    let mut map = vec![u32::MAX; n];
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !shown[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < n && shown[i] {
            i += 1;
        }
        runs.push((a, i));
    }
    let mut k = 0u32;
    for &(a, b) in &runs {
        for slot in &mut map[a..b] {
            *slot = k;
            k += 1;
        }
    }
    out.files = out.files.saturating_sub(n - k as usize);
    (out.records, out.record_ends) = gather_runs(&v.records, &v.record_ends, &runs);
    (out.paths, out.path_ends) = gather_runs(&v.paths, &v.path_ends, &runs);
    hide_diag::mark("runs");
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
    *FILTERED.lock().unwrap() = Some((Arc::downgrade(&out), version));
    hide_diag::finish("copy");
    out
}

/// The entries `runs` (half-open index ranges, ascending) of an arena with
/// end offsets `ends`, as a new arena and ends. Large copies are split over
/// the hide threads.
fn gather_runs(buf: &[u8], ends: &[u32], runs: &[(usize, usize)]) -> (Vec<u8>, Vec<u32>) {
    let start = |i: usize| if i == 0 { 0 } else { ends[i - 1] as usize };
    let mut out_ends = Vec::with_capacity(runs.iter().map(|r| r.1 - r.0).sum());
    // (source range, destination offset) of each run's bytes.
    let mut parts = Vec::with_capacity(runs.len());
    let mut at = 0usize;
    for &(a, b) in runs {
        let (from, to) = (start(a), ends[b - 1] as usize);
        out_ends.extend(ends[a..b].iter().map(|&e| (e as usize - from + at) as u32));
        parts.push((from..to, at));
        at += to - from;
    }
    let mut out = vec![0u8; at];
    let threads = if at < (8 << 20) { 1 } else { hide_threads() };
    let per = at.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|sc| {
        let mut rest: &mut [u8] = &mut out;
        let mut base = 0usize;
        let mut parts = parts.into_iter().peekable();
        while parts.peek().is_some() {
            // One thread's share: whole runs up to about `per` bytes.
            let mut mine = Vec::new();
            let mut len = 0usize;
            while let Some(p) = parts.next_if(|_| len < per) {
                len += p.0.len();
                mine.push(p);
            }
            let (chunk, tail) = std::mem::take(&mut rest).split_at_mut(len);
            rest = tail;
            let b0 = base;
            base += len;
            let job = move || {
                for (r, d) in mine {
                    chunk[d - b0..d - b0 + r.len()].copy_from_slice(&buf[r]);
                }
            };
            if threads == 1 {
                job();
            } else {
                sc.spawn(job);
            }
        }
    });
    (out, out_ends)
}

/// Opt-in timing of the hide work in one library request: with
/// ALBUMEN_HIDE_DIAG_FILE set, each filtered request appends one line of
/// phase times (ms since the request's hide work began) to that file. Off
/// (one cached env lookup) otherwise. For the 380k profile (XO 05:41Z).
mod hide_diag {
    use std::cell::RefCell;
    use std::time::Instant;
    type Marks = (Instant, Vec<(&'static str, f64)>);
    thread_local!(static MARKS: RefCell<Option<Marks>> = const { RefCell::new(None) });
    fn file() -> Option<&'static std::path::PathBuf> {
        static F: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        F.get_or_init(|| std::env::var_os("ALBUMEN_HIDE_DIAG_FILE").map(Into::into))
            .as_ref()
    }
    pub fn start() {
        if file().is_some() {
            MARKS.with(|m| *m.borrow_mut() = Some((Instant::now(), Vec::new())));
        }
    }
    pub fn mark(what: &'static str) {
        if file().is_some() {
            MARKS.with(|m| {
                if let Some((t, v)) = m.borrow_mut().as_mut() {
                    v.push((what, t.elapsed().as_secs_f64() * 1000.0));
                }
            });
        }
    }
    pub fn line(l: &str) {
        let Some(path) = file() else { return };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{l}");
        }
    }
    pub fn finish(what: &'static str) {
        let Some(path) = file() else { return };
        mark(what);
        let Some((_, v)) = MARKS.with(|m| m.borrow_mut().take()) else {
            return;
        };
        let mut line = String::from("hide_diag");
        for (k, ms) in v {
            line.push_str(&format!(" {k}={ms:.2}"));
        }
        line.push('\n');
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// For each photo of `v`, whether it is not in `hidden`.
fn shown_mask(v: &LibraryView, hidden: &std::collections::HashSet<String>) -> Vec<bool> {
    let n = v.len();
    let threads = if n < 50_000 { 1 } else { hide_threads() };
    if threads == 1 {
        return (0..n as u32).map(|i| !hidden.contains(v.path(i))).collect();
    }
    let mut out = vec![true; n];
    let chunk = n.div_ceil(threads);
    std::thread::scope(|sc| {
        for (k, part) in out.chunks_mut(chunk).enumerate() {
            sc.spawn(move || {
                for (j, s) in part.iter_mut().enumerate() {
                    *s = !hidden.contains(v.path((k * chunk + j) as u32));
                }
            });
        }
    });
    out
}

/// The view already filtered for a hide-state version: served as is, with
/// no per-photo lookup, until the version changes.
static FILTERED: Mutex<Option<(std::sync::Weak<LibraryView>, u64)>> = Mutex::new(None);

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
    // Outside a family library only `rel`'s own tombstone (read above) or
    // a folder that can't be listed can hide it, so the per-listing state
    // is not needed. A family library asks the full (cached) state.
    let lib = &server.config.library;
    if !lib.join("genesis.json").is_file() {
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        return std::fs::read_dir(lib.join(dir)).is_err();
    }
    let cur = server.library.lock().unwrap().clone();
    match cur {
        Some(v) if v.ready => hidden_for(server, &v).0.contains(rel),
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
fn withheld_p95_ms() -> f64 {
    let mut w = WITHHELD_SAMPLES.lock().unwrap().clone();
    if w.is_empty() {
        return 0.0;
    }
    w.sort_unstable();
    w[((w.len() * 95).div_ceil(100)).saturating_sub(1)] as f64 / 1000.0
}

fn hide_status_json(server: &Server) -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    let generation = server
        .library
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |v| v.generation);
    serde_json::json!({
        "pending": PENDING_DIRS.load(Relaxed),
        "version": HIDE_VERSION.load(Relaxed),
        "generation": generation,
        "found": server.found.load(Relaxed),
        "note_version": HIDE_NOTE_VERSION.load(Relaxed),
    })
}

fn found_json(server: &Server) -> serde_json::Value {
    let paths = server.found_paths.lock().unwrap().clone();
    serde_json::json!({
        "found": server.found.load(std::sync::atomic::Ordering::Relaxed),
        "paths": paths,
        "index_note": server.index_note.lock().unwrap().clone(),
        "hide_note": current_hide_note(&server.config.library),
        "hide_note_version": HIDE_NOTE_VERSION.load(std::sync::atomic::Ordering::Relaxed),
        "hide_tombstone_reads": TOMB_READS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_recheck_reads": RECHECK_READS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_dirs_listed": DIRS_LISTED.load(std::sync::atomic::Ordering::Relaxed),
        "hide_key": key_kind().name(),
        "hide_changetime_keys": CHANGETIME_KEYS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_key_failures": KEY_FAILURES.load(std::sync::atomic::Ordering::Relaxed),
        "hide_same_tick_ms": RECENT_MS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_fs": FS_NAME.lock().unwrap().clone(),
        "hide_writer_untrusted": WRITER_UNTRUSTED.load(std::sync::atomic::Ordering::Relaxed),
        "hide_pending_dirs": PENDING_DIRS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_withheld_dirs": WITHHELD_DIRS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_withheld_max_ms": WITHHELD_MAX_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
        "hide_withheld_last_ms": WITHHELD_LAST_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
        "hide_withheld_p95_ms": withheld_p95_ms(),
        "hide_withheld_samples": WITHHELD_SAMPLES.lock().unwrap().len(),
        "hide_version": HIDE_VERSION.load(std::sync::atomic::Ordering::Relaxed),
        "hide_keep_stats": KEEP_STATS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_keep_last_ms": KEEP_STAT_LAST_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
        "hide_keep_max_ms": KEEP_STAT_MAX_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
        "hide_worker_panics": WORKER_PANICS.load(std::sync::atomic::Ordering::Relaxed),
        "hide_redispatched": REDISPATCHED.load(std::sync::atomic::Ordering::Relaxed),
        "walk_failures": WALK_FAILURES.load(std::sync::atomic::Ordering::Relaxed),
        "walk_retries": WALK_RETRIES.load(std::sync::atomic::Ordering::Relaxed),
        "hide_exhausted": EXHAUSTED.load(std::sync::atomic::Ordering::Relaxed),
        "hide_grew": GREW.load(std::sync::atomic::Ordering::Relaxed),
        "hide_rewalk_causes": REWALK_CAUSES.lock().unwrap().clone(),
        "hide_build_last_ms": BUILD_LAST_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
        "hide_build_max_ms": BUILD_MAX_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1000.0,
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

    /// XO 03:22Z (2): what NTFS shows while a writer still holds a
    /// tombstone open. MS docs only promise a file's times are right once
    /// the writing handle is closed (and NTFS up to Server 2016 set them at
    /// close). Prints ChangeTime / LastWriteTime before, while open (read
    /// through another handle) and after close, and asserts the parts the
    /// design rests on: the bytes are visible at once, and the times after
    /// close differ from the ones before the write, so a stamp taken while
    /// the writer was open never matches again and the file is read again.
    #[cfg(windows)]
    #[test]
    fn ntfs_changetime_while_a_writer_is_open() {
        use std::io::Write;
        use std::os::windows::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("IMG_0001.JPG.albumen-tombstone");
        let showing = br#"{"hidden":false,"reason":"hide"}"#;
        let hiding = br#"{"hidden":true ,"reason":"hide"}"#;
        assert_eq!(showing.len(), hiding.len());
        std::fs::write(&p, showing).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let times = |p: &Path| {
            (
                change_time(p).unwrap(),
                std::fs::metadata(p).unwrap().last_write_time(),
            )
        };
        let before = times(&p);
        let (st_before, v_before) = stamp_and_read_windows(&p).unwrap();
        assert_eq!(v_before, Ok(false));
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut w = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        w.write_all(hiding).unwrap();
        w.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let open = times(&p);
        let (st_open, v_open) = stamp_and_read_windows(&p).unwrap();
        drop(w);
        let after = times(&p);
        let (st_after, v_after) = stamp_and_read_windows(&p).unwrap();
        println!(
            "M7 NTFS_CHANGETIME before={before:?} open={open:?} after={after:?} \
             changetime_lag_while_open={} lastwrite_lag_while_open={} \
             stamp_before={st_before:?} stamp_open={st_open:?} stamp_after={st_after:?}",
            open.0 == before.0,
            open.1 == before.1,
        );
        // The new bytes are read at once, writer open or not.
        assert_eq!(v_open, Ok(true), "the open writer's bytes were not seen");
        assert_eq!(v_after, Ok(true));
        // After close the times moved, so no earlier stamp matches again.
        assert_ne!(after.0, before.0, "ChangeTime never changed after close");
        assert_ne!(
            st_after, st_before,
            "the stamp after close equals the one before the write"
        );
        if open.0 == before.0 {
            // ChangeTime lagged while open: the stamp taken then equals the
            // old one, but the verdict read with it was the new one, and the
            // close moved the stamp on, so the next check reads again.
            assert_ne!(st_after, st_open);
        }
    }

    /// XO 03:36Z (6): would deny-write sharing (FILE_SHARE_READ only) in
    /// change_time() be safe? The probe opens a tombstone as change_time()
    /// would with it (read, share read only). It does see an open writer
    /// (sharing violation, which would count as untrusted), and a writer
    /// opening after the check is fine; but a writer opening WHILE the
    /// check holds its handle is refused (ERROR_SHARING_VIOLATION, 32), which
    /// is a failed write for a sync tool. So change_time() keeps sharing
    /// read/write/delete (docs/m7-hide-limits.md: a hidden photo can show).
    /// This test prints how many such writes failed here.
    #[cfg(windows)]
    #[test]
    fn deny_write_sharing_would_refuse_a_sync_tools_write() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        const SHARING_VIOLATION: i32 = 32;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("IMG_0001.JPG.albumen-tombstone");
        std::fs::write(&p, br#"{"hidden":false,"reason":"hide"}"#).unwrap();
        let probe = |p: &Path| {
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(p)
        };
        let writer = |p: &Path| std::fs::OpenOptions::new().write(true).open(p);
        // A writer already open: the probe is refused (would be untrusted).
        let w = writer(&p).unwrap();
        let seen = probe(&p).err().and_then(|e| e.raw_os_error());
        drop(w);
        assert_eq!(seen, Some(SHARING_VIOLATION), "an open writer went unseen");
        // A writer opening after the check: fine.
        drop(probe(&p).unwrap());
        writer(&p).unwrap();
        // A writer opening during checks: count the refused writes.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let checker = {
            let (p, stop) = (p.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(h) = probe(&p) {
                        std::thread::sleep(std::time::Duration::from_micros(200));
                        drop(h);
                    }
                    n += 1;
                }
                n
            })
        };
        let (mut ok, mut refused) = (0u32, 0u32);
        for _ in 0..2000 {
            match writer(&p) {
                Ok(_) => ok += 1,
                Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) => refused += 1,
                Err(e) => panic!("writer: {e}"),
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let checks = checker.join().unwrap();
        println!("M7 DENY_WRITE checks={checks} writer_ok={ok} writer_refused={refused}");
        // change_time() itself must never refuse a writer: it shares all.
        let w = writer(&p).unwrap();
        assert!(change_time(&p).is_some());
        drop(w);
    }

    /// F1: the compact listing streams the same images and events as the
    /// per-request listing did, and a page is a slice of it.
    /// /api/found: a first walk (no saved index) records the photos it
    /// finds, at most one page, never other files; a second open from the
    /// saved index lists the same files.
    #[test]
    fn an_ext_stamp_is_fine_only_with_a_sub_second_ctime() {
        use std::time::{Duration, UNIX_EPOCH};
        let two = Duration::from_secs(2);
        let m = UNIX_EPOCH + Duration::from_secs(1_000);
        // ext, ctime with ns: the fine window, from the later ctime.
        let (t, w) = stamp_window(m, 1_005_000_000_123, KeyKind::Ctime, true, two);
        assert_eq!(w, Duration::from_millis(50));
        assert_eq!(t, UNIX_EPOCH + Duration::from_nanos(1_005_000_000_123));
        // ext, whole-second ctime (maybe a 1 s inode): 2 s.
        assert_eq!(
            stamp_window(m, 1_005_000_000_000, KeyKind::Ctime, true, two).1,
            two
        );
        // not ext (nfs, unknown...): the filesystem's window even with ns.
        assert_eq!(
            stamp_window(m, 1_005_000_000_123, KeyKind::Ctime, false, two).1,
            two
        );
        // no ctime key (portable, ChangeTime): mtime and the fs window.
        assert_eq!(stamp_window(m, 7, KeyKind::Portable, true, two), (m, two));
        // Windows ChangeTime (100 ns ticks from 1601): a sub-second part
        // gets the fine window from the later of mtime and ChangeTime; a
        // whole second, or one before 1970, keeps 2 s.
        let to_unix = 116_444_736_000_000_000u64;
        let ct = to_unix + 1_005 * 10_000_000 + 7;
        let (t, w) = stamp_window(m, ct, KeyKind::ChangeTime, false, two);
        assert_eq!(w, RECENT_FINE);
        assert_eq!(
            t,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(1_005_000_000_700)
        );
        assert_eq!(
            stamp_window(
                m,
                to_unix + 1_005 * 10_000_000,
                KeyKind::ChangeTime,
                false,
                two
            )
            .1,
            two
        );
        assert_eq!(
            stamp_window(m, 5, KeyKind::ChangeTime, false, two),
            (m, two)
        );
        // rsync -t set the mtime back: measured from the ctime.
        let old = UNIX_EPOCH + Duration::from_secs(10);
        assert_eq!(
            stamp_window(old, 1_005_000_000_000, KeyKind::Ctime, true, two).0,
            UNIX_EPOCH + Duration::from_secs(1_005)
        );
    }

    #[test]
    fn the_same_tick_window_is_fine_only_on_known_fine_ctime_filesystems() {
        use std::time::Duration;
        for m in [
            0x5846_5342u64,
            0x9123_683E,
            0x0102_1994,
            0xF2F5_2010,
            0x2FC1_2FC1,
        ] {
            assert_eq!(
                window_for_magic(Some(m)).0,
                Duration::from_millis(50),
                "{m:#x}"
            );
        }
        // ext2/3/4 share 0xEF53 and may have 1 s timestamps: coarse.
        assert_eq!(window_for_magic(Some(0xEF53)).0, Duration::from_secs(2));
        assert_eq!(window_for_magic(Some(0xEF53)).1, "ext2/3/4");
        for m in [
            0xEF53u64,
            0x6969,
            0x517B,
            0xFF53_4D42,
            0xFE53_4D42,
            0x6573_5546,
            0x4D44,
            0x2011_BAB0,
            0x4244,
            0x482B,
            0x794C_7630,
            0xDEAD,
        ] {
            assert_eq!(
                window_for_magic(Some(m)).0,
                Duration::from_secs(2),
                "{m:#x}"
            );
        }
        assert_eq!(window_for_magic(None).0, Duration::from_secs(2));
        assert_eq!(window_for_magic(Some(0x6969)).1, "nfs");
    }

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
