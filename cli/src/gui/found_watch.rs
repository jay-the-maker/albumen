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
pub static EVENTS: AtomicU64 = AtomicU64::new(0);

pub fn json() -> serde_json::Value {
    serde_json::json!({
        "incr": INCR.load(Relaxed),
        "full": FULL.load(Relaxed),
        "lost": LOST.load(Relaxed),
        "unavailable": UNAVAILABLE.load(Relaxed),
        "new_dirs": NEW_DIRS.load(Relaxed),
        "events": EVENTS.load(Relaxed),
    })
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

static W: Mutex<Option<(std::path::PathBuf, Option<imp::Watcher>)>> = Mutex::new(None);

/// The tombstones (library-relative, `/`) changed since the last call, or
/// None: stamp them all. `watch` is every tombstone the note follows.
pub fn take(lib: &Path, watch: &[String]) -> Option<HashSet<String>> {
    let r = take_inner(lib, watch);
    if r.is_some() {
        INCR.fetch_add(1, Relaxed);
    } else {
        FULL.fetch_add(1, Relaxed);
    }
    r
}

fn take_inner(lib: &Path, watch: &[String]) -> Option<HashSet<String>> {
    if !on() {
        UNAVAILABLE.fetch_add(1, Relaxed);
        return None;
    }
    let mut g = W.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(l, _)| l != lib) {
        *g = Some((lib.to_path_buf(), imp::Watcher::new(lib)));
        // Nothing before the watch was seen: this poll stamps everything.
        if let Some((_, Some(w))) = g.as_mut() {
            let dirs: HashSet<&str> = watch
                .iter()
                .map(|r| r.rsplit_once('/').map_or("", |(d, _)| d))
                .collect();
            w.ensure(lib, &dirs);
            NEW_DIRS.fetch_add(1, Relaxed);
            return None;
        }
    }
    let (_, w) = g.as_mut()?;
    let Some(w) = w.as_mut() else {
        UNAVAILABLE.fetch_add(1, Relaxed);
        return None;
    };
    let dirs: HashSet<&str> = watch
        .iter()
        .map(|r| r.rsplit_once('/').map_or("", |(d, _)| d))
        .collect();
    // A folder watched only from now: its earlier changes were never seen.
    let new = w.ensure(lib, &dirs);
    let got = w.drain();
    let dropped = drop_hook(lib);
    match got {
        Some(paths) if !new && !dropped => {
            EVENTS.fetch_add(paths.len() as u64, Relaxed);
            Some(paths)
        }
        Some(_) if new && !dropped => {
            NEW_DIRS.fetch_add(1, Relaxed);
            None
        }
        _ => {
            LOST.fetch_add(1, Relaxed);
            None
        }
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

        /// Every queued event, as library-relative paths; None if any was lost.
        pub fn drain(&mut self) -> Option<HashSet<String>> {
            let mut out = HashSet::new();
            let mut lost = std::mem::take(&mut self.broken);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                // SAFETY: reading into our own buffer.
                let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() != std::io::ErrorKind::WouldBlock {
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
                        lost = true;
                        continue;
                    }
                    if ev.mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0
                    {
                        // The folder itself went away or moved: watch it anew.
                        if let Some(d) = self.wds.remove(&ev.wd) {
                            self.dirs.remove(&d);
                        }
                        lost = true;
                        continue;
                    }
                    let Some(d) = self.wds.get(&ev.wd) else {
                        lost = true;
                        continue;
                    };
                    let name = String::from_utf8_lossy(name);
                    out.insert(if d.is_empty() {
                        name.into_owned()
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
            // SAFETY: our handle; the buffer and OVERLAPPED are boxed (fixed
            // address) and outlive the read (Drop waits for it).
            let ok = unsafe {
                ReadDirectoryChangesW(
                    self.h,
                    self.buf.as_mut_ptr().cast(),
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
                    &mut *self.ov,
                    None,
                )
            };
            self.armed = ok != 0;
            self.armed
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
                    self.armed = false;
                    return None;
                }
                self.armed = false;
                if n == 0 {
                    // The system's buffer overflowed: changes were lost.
                    lost = true;
                } else {
                    let bytes: &[u8] = bytemuck_bytes(&self.buf[..], n as usize);
                    let mut off = 0usize;
                    loop {
                        if off + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > bytes.len() {
                            lost = true;
                            break;
                        }
                        // SAFETY: the system wrote a record at this DWORD-aligned offset.
                        let fni = unsafe {
                            &*(bytes.as_ptr().add(off) as *const FILE_NOTIFY_INFORMATION)
                        };
                        let len = fni.FileNameLength as usize / 2;
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
