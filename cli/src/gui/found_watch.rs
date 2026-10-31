//! A9 option (c) (XO 12:52Z FRAGO (d)): `/api/found` re-stamps only the
//! tombstones a change notification named (inotify on Linux,
//! ReadDirectoryChangesW on Windows), drained at the poll itself (no
//! thread, no timer, no periodic sweep). Any doubt is a full stamp (fail
//! closed): no watcher (other targets, or it failed), a queue overflow, a
//! watched folder removed or moved, a folder not watched until now, or a
//! read error. `take` returns None for each of those.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// Polls answered from notifications / with a full stamp, and why full.
pub static INCR: AtomicU64 = AtomicU64::new(0);
pub static FULL: AtomicU64 = AtomicU64::new(0);
pub static LOST: AtomicU64 = AtomicU64::new(0);
pub static UNAVAILABLE: AtomicU64 = AtomicU64::new(0);
pub static NEW_DIRS: AtomicU64 = AtomicU64::new(0);
/// Events discarded silently by the blind test hook.
pub static BLINDED: AtomicU64 = AtomicU64::new(0);
pub static EVENTS: AtomicU64 = AtomicU64::new(0);
/// The OS error code of the last `io_error` loss (0: none yet).
pub static IO_LAST_CODE: AtomicU64 = AtomicU64::new(0);

/// Why each loss was reported (XO 14:14Z: show whether a full stamp
/// followed a real overflow): counts by reason, in WHY_NAMES order.
pub static WHY: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
const WHY_NAMES: [&str; 9] = [
    "overflow",
    "short_record",
    "io_error",
    "arm_failed",
    "read_error",
    "self_gone",
    "unknown_wd",
    "watch_failed",
    "drop_hook",
];
pub(crate) fn why(i: usize) {
    WHY[i].fetch_add(1, Relaxed);
}
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_OVERFLOW: usize = 0;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_SHORT: usize = 1;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_IO: usize = 2;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_ARM: usize = 3;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_READ: usize = 4;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_GONE: usize = 5;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_WD: usize = 6;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_WATCH: usize = 7;
#[allow(dead_code)] // each target uses its own reasons
pub(crate) const W_DROP: usize = 8;

pub fn json() -> serde_json::Value {
    let why: serde_json::Map<_, _> = WHY_NAMES
        .iter()
        .zip(&WHY)
        .map(|(n, c)| (n.to_string(), c.load(Relaxed).into()))
        .collect();
    serde_json::json!({
        "incr": INCR.load(Relaxed),
        "full": FULL.load(Relaxed),
        "lost": LOST.load(Relaxed),
        "unavailable": UNAVAILABLE.load(Relaxed),
        "new_dirs": NEW_DIRS.load(Relaxed),
        "events": EVENTS.load(Relaxed),
        "req_incr": REQ_INCR.load(Relaxed),
        "req_full": REQ_FULL.load(Relaxed),
        "lost_why": why,
        "takes": TAKES.load(Relaxed),
        "take_us": TAKE_NS.load(Relaxed) / 1000,
        "watches": WATCHES.load(Relaxed),
        "stops": STOPS.load(Relaxed),
        "blinded": BLINDED.load(Relaxed),
        "io_error_last_code": IO_LAST_CODE.load(Relaxed),
    })
}

/// `ALBUMEN_TEST_MUTATE=gui-arm-on-caller` (test-hooks/debug only): the
/// Windows read is started on the calling (connection) thread again, as
/// before the fix; its exit cancels the read.
#[allow(dead_code)] // Windows only
fn arm_on_caller_mutant() -> bool {
    #[cfg(any(debug_assertions, feature = "test-hooks"))]
    {
        std::env::var("ALBUMEN_TEST_MUTATE").is_ok_and(|v| v == "gui-arm-on-caller")
    }
    #[cfg(not(any(debug_assertions, feature = "test-hooks")))]
    {
        false
    }
}

/// Off with ALBUMEN_FOUND_NOTIFY=0 (every poll a full stamp, as before).
fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("ALBUMEN_FOUND_NOTIFY").as_deref() != Ok("0"))
}

/// Test hook: while `<library>/.albumen-test-notify-drop` exists, every
/// event drained is dropped and reported lost (as an overflow is).
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn drop_hook(lib: &Path) -> bool {
    lib.join(".albumen-test-notify-drop").exists()
}
#[cfg(not(any(debug_assertions, feature = "test-hooks")))]
fn drop_hook(_: &Path) -> bool {
    false
}

/// Test hook (PL 14:31Z): while `<library>/.albumen-test-notify-blind`
/// exists, every event drained is discarded silently: no loss is reported,
/// as a network or synced folder that never sends the event. Hiding must
/// not depend on the event.
#[cfg(any(debug_assertions, feature = "test-hooks"))]
fn blind_hook(lib: &Path) -> bool {
    lib.join(".albumen-test-notify-blind").exists()
}
#[cfg(not(any(debug_assertions, feature = "test-hooks")))]
fn blind_hook(_: &Path) -> bool {
    false
}

/// The consumers of one watcher: the `/api/found` note and the request
/// prestamp (XO 14:00Z). Every drain feeds both; any doubt marks both
/// full, so neither can miss an event the other drained.
pub const NOTE: usize = 0;
pub const REQ: usize = 1;

#[derive(Default)]
struct Sink {
    dirty: HashSet<String>,
    full: bool,
}

struct State {
    lib: std::path::PathBuf,
    w: Option<imp::Watcher>,
    sinks: [Sink; 2],
}

static W: Mutex<Option<State>> = Mutex::new(None);

/// XO 14:22Z (c): the library is closed (the server stops serving it):
/// drop the watcher (its handle, buffer and watches) and both consumers'
/// state. The next take for any library starts over with full stamps.
pub fn stop() {
    let old = W.lock().unwrap_or_else(|e| e.into_inner()).take();
    if old.is_some() {
        STOPS.fetch_add(1, Relaxed);
    }
    drop(old);
}

/// XO 14:22Z (d) (DESIGN_PHILOSOPHY:63, measured): the watcher has no
/// thread; between polls it costs no CPU, only kernel state (Windows one
/// 64 KiB buffer, Linux one inotify watch per folder). Each poll pays the
/// drain, timed here.
pub static TAKE_NS: AtomicU64 = AtomicU64::new(0);
pub static TAKES: AtomicU64 = AtomicU64::new(0);
pub static STOPS: AtomicU64 = AtomicU64::new(0);
pub static WATCHES: AtomicU64 = AtomicU64::new(0);

struct Timed(std::time::Instant);
impl Drop for Timed {
    fn drop(&mut self) {
        TAKE_NS.fetch_add(self.0.elapsed().as_nanos() as u64, Relaxed);
        TAKES.fetch_add(1, Relaxed);
    }
}

/// Polls of the request prestamp answered from notifications / in full.
pub static REQ_INCR: AtomicU64 = AtomicU64::new(0);
pub static REQ_FULL: AtomicU64 = AtomicU64::new(0);

/// The tombstones (library-relative, `/`) changed since the last call, or
/// None: stamp them all. `watch` is every tombstone the note follows.
pub fn take(lib: &Path, watch: &[String]) -> Option<HashSet<String>> {
    let dirs: HashSet<&str> = watch
        .iter()
        .map(|r| r.rsplit_once('/').map_or("", |(d, _)| d))
        .collect();
    let r = take_inner(lib, &dirs, NOTE);
    if r.is_some() {
        INCR.fetch_add(1, Relaxed);
    } else {
        FULL.fetch_add(1, Relaxed);
    }
    r
}

/// For the request prestamp: the paths changed (each library-relative,
/// `/`, a folder's own change as its bare path) since its last call, or
/// None: stamp everything. `dirs` must hold the parent of every path the
/// caller follows (and of every folder it stamps).
pub fn take_req(lib: &Path, dirs: &HashSet<&str>) -> Option<HashSet<String>> {
    let r = take_inner(lib, dirs, REQ);
    if r.is_some() {
        REQ_INCR.fetch_add(1, Relaxed);
    } else {
        REQ_FULL.fetch_add(1, Relaxed);
    }
    r
}

fn take_inner(lib: &Path, dirs: &HashSet<&str>, who: usize) -> Option<HashSet<String>> {
    let _t = Timed(std::time::Instant::now());
    if !on() {
        UNAVAILABLE.fetch_add(1, Relaxed);
        return None;
    }
    let mut g = W.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|s| s.lib != lib) {
        // PL 14:31Z (3): another library: the old library's watcher (handle,
        // buffer, watches) is stopped before the new one starts.
        if g.take().is_some() {
            STOPS.fetch_add(1, Relaxed);
        }
        // Nothing before the watch was seen: both consumers stamp all.
        let mut st = State {
            lib: lib.to_path_buf(),
            w: imp::Watcher::new(lib),
            sinks: Default::default(),
        };
        for k in &mut st.sinks {
            k.full = true;
        }
        if let Some(w) = st.w.as_mut() {
            w.ensure(lib, dirs);
            NEW_DIRS.fetch_add(1, Relaxed);
        }
        *g = Some(st);
    }
    let st = g.as_mut()?;
    let Some(w) = st.w.as_mut() else {
        UNAVAILABLE.fetch_add(1, Relaxed);
        return None;
    };
    // A folder watched only from now: its earlier changes were never seen.
    let new = w.ensure(lib, dirs);
    WATCHES.store(w.count(), Relaxed);
    let mut got = w.drain();
    if blind_hook(lib) {
        if let Some(p) = got.as_mut() {
            BLINDED.fetch_add(p.len() as u64, Relaxed);
            p.clear();
        }
    }
    let dropped = drop_hook(lib);
    if dropped {
        why(W_DROP);
    }
    match got {
        Some(paths) if !new && !dropped => {
            EVENTS.fetch_add(paths.len() as u64, Relaxed);
            for k in &mut st.sinks {
                k.dirty.extend(paths.iter().cloned());
            }
        }
        Some(_) if new && !dropped => {
            NEW_DIRS.fetch_add(1, Relaxed);
            for k in &mut st.sinks {
                k.full = true;
            }
        }
        _ => {
            LOST.fetch_add(1, Relaxed);
            for k in &mut st.sinks {
                k.full = true;
            }
        }
    }
    let k = &mut st.sinks[who];
    let dirty = std::mem::take(&mut k.dirty);
    if std::mem::take(&mut k.full) {
        None
    } else {
        Some(dirty)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub struct Watcher {
        fd: i32,
        wds: HashMap<i32, String>,
        dirs: HashSet<String>,
        /// A watch failed or the folder went away: every drain is lost
        /// until it is watched again.
        broken: bool,
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            // SAFETY: our own inotify descriptor.
            unsafe { libc::close(self.fd) };
        }
    }

    const MASK: u32 = libc::IN_MODIFY
        | libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;

    impl Watcher {
        pub fn new(_lib: &Path) -> Option<Self> {
            // SAFETY: plain syscall.
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            (fd >= 0).then(|| Watcher {
                fd,
                wds: HashMap::new(),
                dirs: HashSet::new(),
                broken: false,
            })
        }

        /// Watch every folder in `dirs`; true when one was not watched yet.
        pub fn ensure(&mut self, lib: &Path, dirs: &HashSet<&str>) -> bool {
            let mut new = false;
            for d in dirs {
                if self.dirs.contains(*d) {
                    continue;
                }
                new = true;
                let p = if d.is_empty() {
                    lib.to_path_buf()
                } else {
                    lib.join(d)
                };
                let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
                    self.broken = true;
                    continue;
                };
                // SAFETY: our descriptor and a NUL-terminated path.
                let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
                if wd < 0 {
                    self.broken = true;
                    continue;
                }
                self.wds.insert(wd, d.to_string());
                self.dirs.insert(d.to_string());
            }
            if new {
                // Watched again from here; the caller stamps everything.
                self.broken = false;
            }
            new
        }

        pub fn count(&self) -> u64 {
            self.wds.len() as u64
        }

        /// Every queued event, as library-relative paths; None if any was lost.
        pub fn drain(&mut self) -> Option<HashSet<String>> {
            let mut out = HashSet::new();
            let mut lost = std::mem::take(&mut self.broken);
            if lost {
                super::why(super::W_WATCH);
            }
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                // SAFETY: reading into our own buffer.
                let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        super::why(super::W_READ);
                        lost = true;
                    }
                    break;
                }
                if n == 0 {
                    break;
                }
                let n = n as usize;
                let head = std::mem::size_of::<libc::inotify_event>();
                let mut off = 0;
                while off + head <= n {
                    // SAFETY: the kernel wrote a whole event at `off`.
                    let ev: libc::inotify_event =
                        unsafe { std::ptr::read_unaligned(buf[off..].as_ptr().cast()) };
                    let len = ev.len as usize;
                    let name = &buf[off + head..(off + head + len).min(n)];
                    let name = name.split(|&b| b == 0).next().unwrap_or(&[]);
                    off += head + len;
                    if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                        super::why(super::W_OVERFLOW);
                        lost = true;
                        continue;
                    }
                    if ev.mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0
                    {
                        // The folder itself went away or moved: watch it anew.
                        if let Some(d) = self.wds.remove(&ev.wd) {
                            self.dirs.remove(&d);
                        }
                        super::why(super::W_GONE);
                        lost = true;
                        continue;
                    }
                    let Some(d) = self.wds.get(&ev.wd) else {
                        super::why(super::W_WD);
                        lost = true;
                        continue;
                    };
                    let name = String::from_utf8_lossy(name);
                    out.insert(if d.is_empty() {
                        name.into_owned()
                    } else if name.is_empty() {
                        // The watched folder itself (IN_ATTRIB on it).
                        d.clone()
                    } else {
                        format!("{d}/{name}")
                    });
                }
            }
            (!lost).then_some(out)
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::HashSet;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_IO_INCOMPLETE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
        FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    const BUF_WORDS: usize = 16 * 1024; // 64 KiB, DWORD aligned

    /// One read to start: (handle, buffer, OVERLAPPED) as addresses, and
    /// where to send whether it started.
    type Job = (usize, usize, usize, std::sync::mpsc::SyncSender<bool>);

    /// Windows cancels a thread's pending I/O when that thread exits. The
    /// GUI answers each connection on its own short-lived thread, so a read
    /// started there was cancelled as soon as the answer was sent and the
    /// next drain saw ERROR_OPERATION_ABORTED, a reported loss and a full
    /// stamp on most polls (CI 1471012: io_error 16-18 per silent-miss test,
    /// 687 at 10k). Every read is started on this one thread instead, which
    /// lives as long as the process.
    fn armer() -> Option<&'static std::sync::Mutex<std::sync::mpsc::Sender<Job>>> {
        static A: std::sync::OnceLock<Option<std::sync::Mutex<std::sync::mpsc::Sender<Job>>>> =
            std::sync::OnceLock::new();
        A.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("found-watch-arm".into())
                .spawn(move || {
                    for (h, buf, ov, done) in rx {
                        // SAFETY: the caller holds the watcher (handle, boxed
                        // buffer and OVERLAPPED) until `done` answers, and
                        // they outlive the read (Drop cancels and waits).
                        let ok = unsafe {
                            start_read(h as HANDLE, buf as *mut u32, ov as *mut OVERLAPPED)
                        };
                        let _ = done.send(ok);
                    }
                })
                .ok()
                .map(|_| std::sync::Mutex::new(tx))
        })
        .as_ref()
    }

    /// SAFETY: `h` an open directory handle (overlapped), `buf` BUF_WORDS
    /// words and `ov` an idle OVERLAPPED, both valid until the read ends.
    unsafe fn start_read(h: HANDLE, buf: *mut u32, ov: *mut OVERLAPPED) -> bool {
        // SAFETY: as the function's contract.
        unsafe {
            ReadDirectoryChangesW(
                h,
                buf.cast(),
                (BUF_WORDS * 4) as u32,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_CREATION
                    | FILE_NOTIFY_CHANGE_SECURITY,
                std::ptr::null_mut(),
                ov,
                None,
            ) != 0
        }
    }

    /// One subtree watch on the library root (every folder at once).
    pub struct Watcher {
        h: HANDLE,
        ov: Box<OVERLAPPED>,
        buf: Box<[u32; BUF_WORDS]>,
        armed: bool,
    }

    // SAFETY: the handle and the pinned buffers are only used under the
    // module's mutex.
    unsafe impl Send for Watcher {}

    impl Drop for Watcher {
        fn drop(&mut self) {
            // SAFETY: our own handle; a pending read is cancelled and waited
            // for before the buffers it writes are freed.
            unsafe {
                if self.armed {
                    CancelIoEx(self.h, &*self.ov);
                    let mut n = 0u32;
                    GetOverlappedResult(self.h, &*self.ov, &mut n, 1);
                }
                CloseHandle(self.h);
            }
        }
    }

    impl Watcher {
        pub fn new(lib: &Path) -> Option<Self> {
            use std::os::windows::fs::OpenOptionsExt;
            use std::os::windows::io::IntoRawHandle;
            let f = std::fs::OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED)
                .open(lib)
                .ok()?;
            let h = f.into_raw_handle() as HANDLE;
            if h == INVALID_HANDLE_VALUE || h.is_null() {
                return None;
            }
            let mut me = Watcher {
                h,
                // SAFETY: OVERLAPPED is plain data; zeroed is its idle state.
                ov: Box::new(unsafe { std::mem::zeroed() }),
                buf: Box::new([0u32; BUF_WORDS]),
                armed: false,
            };
            me.arm().then_some(me)
        }

        fn arm(&mut self) -> bool {
            let (h, buf, ov) = (
                self.h as usize,
                self.buf.as_mut_ptr() as usize,
                &mut *self.ov as *mut OVERLAPPED as usize,
            );
            let ok = if super::arm_on_caller_mutant() {
                // SAFETY: our handle; the buffer and OVERLAPPED are boxed
                // (fixed address) and outlive the read (Drop waits for it).
                unsafe { start_read(self.h, self.buf.as_mut_ptr(), &mut *self.ov) }
            } else if let Some(a) = armer() {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                let sent = a
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send((h, buf, ov, tx));
                sent.is_ok() && rx.recv().unwrap_or(false)
            } else {
                false
            };
            self.armed = ok;
            self.armed
        }

        pub fn count(&self) -> u64 {
            1
        }

        /// The subtree watch covers every folder from the start.
        pub fn ensure(&mut self, _lib: &Path, _dirs: &HashSet<&str>) -> bool {
            false
        }

        pub fn drain(&mut self) -> Option<HashSet<String>> {
            let mut out = HashSet::new();
            let mut lost = false;
            loop {
                if !self.armed && !self.arm() {
                    super::why(super::W_ARM);
                    return None;
                }
                let mut n = 0u32;
                // SAFETY: our handle and OVERLAPPED; no wait.
                let ok = unsafe { GetOverlappedResult(self.h, &*self.ov, &mut n, 0) };
                if ok == 0 {
                    // SAFETY: plain call.
                    if unsafe { GetLastError() } == ERROR_IO_INCOMPLETE {
                        break;
                    }
                    // SAFETY: plain call.
                    let code = unsafe { GetLastError() };
                    super::IO_LAST_CODE.store(code as u64, std::sync::atomic::Ordering::Relaxed);
                    self.armed = false;
                    super::why(super::W_IO);
                    return None;
                }
                self.armed = false;
                if n == 0 {
                    // The system's buffer overflowed: changes were lost.
                    super::why(super::W_OVERFLOW);
                    lost = true;
                } else {
                    let bytes: &[u8] = bytemuck_bytes(&self.buf[..], n as usize);
                    let mut off = 0usize;
                    loop {
                        // The fixed header is 12 bytes (offset of FileName); a
                        // record with a one-character name is 14 bytes, shorter
                        // than size_of (16, padded), and was wrongly reported
                        // lost. Bound the header and the name separately.
                        const HEAD: usize = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
                        if off + HEAD > bytes.len() {
                            super::why(super::W_SHORT);
                            lost = true;
                            break;
                        }
                        // SAFETY: the system wrote a record at this DWORD-aligned offset.
                        let fni = unsafe {
                            &*(bytes.as_ptr().add(off) as *const FILE_NOTIFY_INFORMATION)
                        };
                        let len = fni.FileNameLength as usize / 2;
                        if off + HEAD + len * 2 > bytes.len() {
                            super::why(super::W_SHORT);
                            lost = true;
                            break;
                        }
                        // SAFETY: FileName holds `len` UTF-16 units inside the buffer.
                        let name =
                            unsafe { std::slice::from_raw_parts(fni.FileName.as_ptr(), len) };
                        out.insert(String::from_utf16_lossy(name).replace('\\', "/"));
                        if fni.NextEntryOffset == 0 {
                            break;
                        }
                        off += fni.NextEntryOffset as usize;
                    }
                }
                if !self.arm() {
                    super::why(super::W_ARM);
                    return None;
                }
            }
            (!lost).then_some(out)
        }
    }

    fn bytemuck_bytes(w: &[u32], n: usize) -> &[u8] {
        // SAFETY: a u32 slice viewed as its bytes, cut to `n`.
        let all = unsafe { std::slice::from_raw_parts(w.as_ptr().cast::<u8>(), w.len() * 4) };
        &all[..n.min(all.len())]
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use std::collections::HashSet;
    use std::path::Path;
    pub struct Watcher;
    impl Watcher {
        pub fn count(&self) -> u64 {
            0
        }
        pub fn new(_: &Path) -> Option<Self> {
            None
        }
        pub fn ensure(&mut self, _: &Path, _: &HashSet<&str>) -> bool {
            true
        }
        pub fn drain(&mut self) -> Option<HashSet<String>> {
            None
        }
    }
}
