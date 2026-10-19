// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::checksum::{copy_hashed, sha256_file, sha256_readback, sha256_slice, to_hex};
use crate::dest;
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::mtime::{mtime_matches, mtime_of, sync_file, to_unix_nanos_string};
use crate::source::{parent_dir, SourceRoot};
use crate::store::{BlobStore, VerifiedObject};

/// Filesystem destination. The directory is created if it is missing.
/// Existing files are never overwritten.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl BlobStore for LocalStore {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        let actual = file_name_str(source)?;
        if actual != filename {
            return Err(IngestError::FilenameMismatch {
                source_name: actual,
                requested: filename.to_string(),
            });
        }
        copy_verify_file(source, &self.root)
    }

    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        copy_verify_file(&self.root.join(filename), dest_dir)
    }

    fn delete(&self, filename: &str) -> Result<(), IngestError> {
        check_filename(filename)?;
        let path = self.root.join(filename);
        match dest::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(IngestError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

/// Copy one file into `dest_dir`, keeping its filename and mtime.
///
/// The source is read once: SHA-256 is updated as those bytes are written.
/// Named streams are copied next, because a stream write can change the
/// file's mtime. The preserved mtime is then set and the file is fsynced
/// once, so the bytes and the time are durable together. The destination
/// path is opened again and hashed. That read-back is required. The
/// directory is synced after it. The file is not reported stored until
/// both syncs have returned. On mismatch the destination is removed and
/// the error names both SHA-256 digests.
pub fn copy_verify_file(source: &Path, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
    if !source.is_file() {
        return Err(IngestError::NotAFile(source.to_path_buf()));
    }
    let filename = file_name_str(source)?;
    check_filename(&filename)?;
    let mtime = mtime_of(source)?;
    store_checked(source, &filename, dest_dir, mtime, |root, output, dest| {
        let input = root.open_read(source)?;
        copy_hashed(input, output, source, dest)
    })
}

/// Files at or under this size are read into memory by [`prefetch_source`].
/// Larger files are copied as a stream by [`copy_verify_file`].
pub const PREFETCH_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A source file read once and hashed, before any destination is opened.
///
/// The digest is of exactly these bytes, and these bytes are what
/// [`store_prefetched`] writes. The destination read-back still has to
/// match the digest before the file is reported stored.
#[derive(Debug)]
pub struct PrefetchedSource {
    source: PathBuf,
    filename: String,
    mtime: std::time::SystemTime,
    data: Vec<u8>,
    sha256: [u8; 32],
}

impl PrefetchedSource {
    pub fn source(&self) -> &Path {
        &self.source
    }
}

/// Read and hash `source` into memory, ready for [`store_prefetched`].
///
/// This only reads the source, through the read-only [`SourceRoot`]. It
/// returns `Ok(None)` when the file is larger than [`PREFETCH_MAX_BYTES`];
/// the caller copies that file with [`copy_verify_file`].
pub fn prefetch_source(source: &Path) -> Result<Option<PrefetchedSource>, IngestError> {
    prefetch_source_into(source, Vec::new())
}

/// [`prefetch_source`] into `buf`, which is cleared first. A pipeline hands
/// back the buffer of a file it has written, so the next read lands in
/// memory that is already mapped.
fn prefetch_source_into(
    source: &Path,
    data: Vec<u8>,
) -> Result<Option<PrefetchedSource>, IngestError> {
    Ok(read_source_into(source, data)?.map(ReadSource::hashed))
}

/// A source file read into memory but not yet hashed. Only
/// [`ReadSource::hashed`] turns it into a [`PrefetchedSource`], so nothing
/// can be written without the digest of exactly these bytes.
struct ReadSource {
    source: PathBuf,
    filename: String,
    mtime: std::time::SystemTime,
    data: Vec<u8>,
}

impl ReadSource {
    /// SHA-256 of the bytes that were read. In the pipeline this runs on its
    /// own thread while the next source is read.
    fn hashed(self) -> PrefetchedSource {
        let hash_at = crate::copy_profile::tic();
        let sha256 = sha256_slice(&self.data);
        crate::copy_profile::add_hash(hash_at);
        PrefetchedSource {
            source: self.source,
            filename: self.filename,
            mtime: self.mtime,
            data: self.data,
            sha256,
        }
    }
}

/// Read `source` into `data` (cleared first) through the read-only
/// [`SourceRoot`]. `Ok(None)` when it is over [`PREFETCH_MAX_BYTES`].
fn read_source_into(source: &Path, mut data: Vec<u8>) -> Result<Option<ReadSource>, IngestError> {
    if !source.is_file() {
        return Err(IngestError::NotAFile(source.to_path_buf()));
    }
    let filename = file_name_str(source)?;
    check_filename(&filename)?;
    let root = SourceRoot::open(parent_dir(source))?;
    let mtime = mtime_of(source)?;
    let mut input = root.open_read(source)?;
    let len = input
        .metadata()
        .map_err(|err| IngestError::io(source, err))?
        .len();
    if len > PREFETCH_MAX_BYTES {
        return Ok(None);
    }
    data.clear();
    data.reserve(usize::try_from(len).unwrap_or(0));
    let read_at = crate::copy_profile::tic();
    input
        .read_to_end(&mut data)
        .map_err(|err| IngestError::io(source, err))?;
    crate::copy_profile::add_source_read(read_at);
    if data.len() as u64 > PREFETCH_MAX_BYTES {
        return Ok(None);
    }
    Ok(Some(ReadSource {
        source: source.to_path_buf(),
        filename,
        mtime,
        data,
    }))
}

/// Write a [`PrefetchedSource`] into `dest_dir` and verify it.
///
/// The destination checks, named streams, preserved mtime, file sync,
/// independent read-back, and directory sync are the same as in
/// [`copy_verify_file`]. The file is not reported stored until both syncs
/// have returned and the read-back matched.
pub fn store_prefetched(
    prefetched: PrefetchedSource,
    dest_dir: &Path,
) -> Result<VerifiedObject, IngestError> {
    let PrefetchedSource {
        source,
        filename,
        mtime,
        data,
        sha256,
    } = prefetched;
    store_checked(&source, &filename, dest_dir, mtime, |_, output, dest| {
        let write_at = crate::copy_profile::tic();
        let mut writer = output;
        writer
            .write_all(&data)
            .map_err(|err| IngestError::io(dest, err))?;
        crate::copy_profile::add_write(write_at);
        Ok(sha256)
    })
}

/// Read-back threads used by [`copy_verify_files_pipelined`] for `lanes`
/// writer lanes: between 2 and 4.
pub fn pipelined_verifiers(lanes: usize) -> usize {
    lanes.clamp(2, 4)
}

/// Copy `sources` into `dest_dir` with `lanes` writer lanes and
/// [`pipelined_verifiers`] read-back threads.
///
/// Each lane has three threads. The first reads the next source. The
/// second hashes what was read, while the first reads the following file.
/// The third writes the current file, syncs it, then syncs the directory,
/// so each lane has one full sync in flight. Synced files go into one
/// bounded queue shared by the read-back threads, which open each
/// destination again and check its SHA-256 and mtime. A file is reported to
/// `on_done` only after its own file sync, its own directory sync, and its
/// read-back. Files over [`PREFETCH_MAX_BYTES`] go through
/// [`copy_verify_file`]. `on_done` gets the index into `sources`. It
/// returns `false` to stop the copy early; files not yet read back are
/// then removed.
pub fn copy_verify_files_pipelined<F>(
    sources: &[PathBuf],
    dest_dir: &Path,
    lanes: usize,
    on_done: F,
) where
    F: Fn(usize, Result<VerifiedObject, IngestError>) -> bool + Sync,
{
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::sync::Mutex;

    type Read = (usize, Result<Option<ReadSource>, IngestError>);
    type Hashed = (usize, Result<Option<PrefetchedSource>, IngestError>);

    let lanes = lanes.max(1);
    let verifiers = pipelined_verifiers(lanes);
    let profile = crate::copy_profile::thread_enabled();
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (sync_tx, sync_rx) = sync_channel::<(usize, Synced)>(2 * lanes);
    let sync_rx = Mutex::new(sync_rx);
    std::thread::scope(|scope| {
        for _ in 0..lanes {
            let (read_tx, read_rx) = sync_channel::<Read>(1);
            let (hash_tx, hash_rx) = sync_channel::<Hashed>(1);
            let (spare_tx, spare_rx) = sync_channel::<Vec<u8>>(4);
            let sync_tx = sync_tx.clone();
            let next = &next;
            let stop = &stop;
            scope.spawn(move || {
                if profile {
                    crate::copy_profile::enable_thread();
                }
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(source) = sources.get(index) else {
                        break;
                    };
                    let buf = spare_rx.try_recv().unwrap_or_default();
                    if read_tx
                        .send((index, read_source_into(source, buf)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            scope.spawn(move || {
                if profile {
                    crate::copy_profile::enable_thread();
                }
                for (index, read) in read_rx {
                    let hashed = read.map(|read| read.map(ReadSource::hashed));
                    if hash_tx.send((index, hashed)).is_err() || stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
            scope.spawn(move || {
                if profile {
                    crate::copy_profile::enable_thread();
                }
                for (index, prefetched) in hash_rx {
                    let synced = match prefetched {
                        Ok(Some(prefetched)) => {
                            let (written, buf) = write_prefetched(prefetched, dest_dir);
                            let _ = spare_tx.try_send(buf);
                            match written.and_then(|pending| {
                                // The directory sync runs here, right after
                                // this file's sync, so a lane has one
                                // full sync in flight at a time.
                                let dir_at = crate::copy_profile::tic();
                                let synced = dest::sync_dir(dest_dir);
                                crate::copy_profile::add_dir_sync(dir_at);
                                synced.map(|()| pending)
                            }) {
                                Ok(pending) => Synced::Pending(pending),
                                Err(err) => Synced::Done(Err(err)),
                            }
                        }
                        Ok(None) => Synced::Done(copy_verify_file(&sources[index], dest_dir)),
                        Err(err) => Synced::Done(Err(err)),
                    };
                    if sync_tx.send((index, synced)).is_err() || stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
        }
        // Only the lane writers hold senders, so the queue closes when the
        // last writer is done.
        drop(sync_tx);
        for _ in 0..verifiers {
            let sync_rx = &sync_rx;
            let stop = &stop;
            let on_done = &on_done;
            scope.spawn(move || {
                if profile {
                    crate::copy_profile::enable_thread();
                }
                loop {
                    let received = match sync_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => break,
                    };
                    let Ok((index, synced)) = received else {
                        break;
                    };
                    if stop.load(Ordering::Relaxed) {
                        // Dropping a pending file removes it: it was never
                        // read back, so it is not reported stored.
                        continue;
                    }
                    let result = match synced {
                        Synced::Done(result) => result,
                        Synced::Pending(pending) => pending.verify().map(Verified::keep),
                    };
                    if !on_done(index, result) {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });
}

enum Synced {
    Done(Result<VerifiedObject, IngestError>),
    Pending(Pending),
}

/// A destination file that is written, has its mtime, and is synced, but
/// has not been read back. Dropping it removes the file.
struct Pending {
    dest: PathBuf,
    filename: String,
    sha256: [u8; 32],
    mtime: std::time::SystemTime,
    guard: RemoveOnDrop,
}

impl Pending {
    fn verify(self) -> Result<Verified, IngestError> {
        let record = finalize_verified(&self.dest, &self.filename, &self.sha256, self.mtime)?;
        Ok(Verified {
            record,
            guard: self.guard,
        })
    }
}

/// Read back and checked. Still removed on drop until the directory sync.
struct Verified {
    record: VerifiedObject,
    guard: RemoveOnDrop,
}

impl Verified {
    fn keep(mut self) -> VerifiedObject {
        self.guard.armed = false;
        self.record
    }
}

/// Write and file-sync a prefetched file. The buffer comes back for reuse.
fn write_prefetched(
    prefetched: PrefetchedSource,
    dest_dir: &Path,
) -> (Result<Pending, IngestError>, Vec<u8>) {
    let PrefetchedSource {
        source,
        filename,
        mtime,
        data,
        sha256,
    } = prefetched;
    let written = write_synced(&source, &filename, dest_dir, mtime, |_, output, dest| {
        let write_at = crate::copy_profile::tic();
        let mut writer = output;
        writer
            .write_all(&data)
            .map_err(|err| IngestError::io(dest, err))?;
        crate::copy_profile::add_write(write_at);
        Ok(sha256)
    });
    (written, data)
}

/// The destination half of a copy: checks, write, streams, mtime, file
/// sync, read-back, and directory sync. `write` puts the bytes in `output`
/// and returns the SHA-256 of what it wrote.
fn store_checked<W>(
    source: &Path,
    filename: &str,
    dest_dir: &Path,
    mtime: std::time::SystemTime,
    write: W,
) -> Result<VerifiedObject, IngestError>
where
    W: FnOnce(&SourceRoot, &fs::File, &Path) -> Result<[u8; 32], IngestError>,
{
    let verified = write_synced(source, filename, dest_dir, mtime, write)?.verify()?;
    let dir_at = crate::copy_profile::tic();
    dest::sync_dir(dest_dir)?;
    crate::copy_profile::add_dir_sync(dir_at);
    Ok(verified.keep())
}

/// Checks, write, streams, mtime, and file sync. The read-back and the
/// directory sync are left to the caller.
fn write_synced<W>(
    source: &Path,
    filename: &str,
    dest_dir: &Path,
    mtime: std::time::SystemTime,
    write: W,
) -> Result<Pending, IngestError>
where
    W: FnOnce(&SourceRoot, &fs::File, &Path) -> Result<[u8; 32], IngestError>,
{
    let filename = filename.to_string();
    let root = SourceRoot::open(parent_dir(source))?;
    root.refuse_destination(dest_dir)?;
    let dest = dest_dir.join(&filename);
    root.refuse_destination(&dest)?;
    dest::create_dir_all(dest_dir)?;
    if let Some(existing) = crate::platform::case_collision(dest_dir, &filename)? {
        return Err(IngestError::CaseCollision {
            filename: filename.clone(),
            existing,
            destination: dest.display().to_string(),
        });
    }
    if crate::platform::io_path(&dest).exists() {
        return Err(IngestError::AlreadyExists {
            filename: filename.clone(),
            destination: dest.display().to_string(),
        });
    }

    let guard = RemoveOnDrop {
        path: dest.clone(),
        armed: true,
    };
    let output = dest::open_new(&dest)?;
    let sha256 = write(&root, &output, &dest)?;
    // Streams land before the mtime is set. The write handle stays open so
    // the fsync below covers the bytes this handle wrote and the mtime.
    copy_named_streams(source, &dest, &filename)?;
    let expected_mtime = crate::mtime::representable_mtime(mtime);
    dest::set_mtime_on(&output, &dest, expected_mtime)?;
    sync_file(&output, &dest)?;
    drop(output);
    Ok(Pending {
        dest,
        filename,
        sha256,
        mtime,
        guard,
    })
}

fn copy_named_streams(source: &Path, dest: &Path, filename: &str) -> Result<(), IngestError> {
    let streams = crate::platform::list_named_streams(source)?;
    for stream in streams {
        let src_path = dest::stream_path(source, &stream.name);
        let dst_path = dest::stream_path(dest, &stream.name);
        let mut input = fs::File::open(&src_path).map_err(|err| IngestError::io(&src_path, err))?;
        let output = dest::open_stream(dest, &stream.name).inspect_err(|_| {
            dest::remove_stream(dest, &stream.name);
        })?;
        let written = copy_hashed(&mut input, &output, &src_path, &dst_path)?;
        sync_file(&output, &dst_path)?;
        drop(output);
        let actual = sha256_file(&dst_path)?;
        if actual != written {
            dest::remove_stream(dest, &stream.name);
            return Err(IngestError::AlternateStream {
                filename: filename.to_string(),
                name: stream.name,
                message: format!(
                    "checksum mismatch: expected {}, got {}",
                    to_hex(&written),
                    to_hex(&actual)
                ),
            });
        }
    }
    Ok(())
}

/// Stream `reader` into a new file at `dest` and verify it the same way as
/// [`copy_verify_file`].
///
/// The destination is created with `create_new`. An existing path is left
/// untouched. On a hash or mtime mismatch, only that new file is removed.
pub(crate) fn copy_verify_reader<R: Read>(
    reader: &mut R,
    source_label: &Path,
    dest: &Path,
    mtime: std::time::SystemTime,
) -> Result<VerifiedObject, IngestError> {
    let filename = file_name_str(dest)?;
    check_filename(&filename)?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            dest::create_dir_all(parent)?;
        }
    }
    if fs::symlink_metadata(dest).is_ok() {
        return Err(IngestError::AlreadyExists {
            filename: filename.clone(),
            destination: dest.display().to_string(),
        });
    }

    let mut guard = RemoveOnDrop {
        path: dest.to_path_buf(),
        armed: true,
    };
    let output = dest::open_new(dest)?;
    let written_hash = copy_hashed(reader, &output, source_label, dest)?;
    let expected_mtime = crate::mtime::representable_mtime(mtime);
    dest::set_mtime_on(&output, dest, expected_mtime)?;
    sync_file(&output, dest)?;
    drop(output);

    let record = finalize_verified(dest, &filename, &written_hash, mtime)?;
    if let Some(parent) = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let dir_at = crate::copy_profile::tic();
        dest::sync_dir(parent)?;
        crate::copy_profile::add_dir_sync(dir_at);
    }
    guard.armed = false;
    Ok(record)
}

pub(crate) fn finalize_verified(
    path: &Path,
    filename: &str,
    expected: &[u8; 32],
    mtime: std::time::SystemTime,
) -> Result<VerifiedObject, IngestError> {
    // The caller has already set the mtime and fsynced the file. This
    // re-reads the destination path and hashes those bytes. Camera-to-cloud
    // CI times this re-read only. The hash made while copying, and the
    // fsync before it, stay outside the clock.
    let started = std::time::Instant::now();
    let actual = sha256_readback(path)?;
    let sha256_verify_ms = started.elapsed().as_millis();
    if actual != *expected {
        let removed = dest::remove_file(path).is_ok();
        return Err(IngestError::checksum_mismatch(
            filename,
            to_hex(expected),
            to_hex(&actual),
            path.display().to_string(),
            removed,
        ));
    }

    let expected_mtime = crate::mtime::representable_mtime(mtime);
    let stored_mtime = mtime_of(path)?;
    if !mtime_matches(expected_mtime, stored_mtime) {
        let removed = dest::remove_file(path).is_ok();
        return Err(IngestError::MtimeNotPreserved {
            filename: filename.to_string(),
            expected_unix_nanos: to_unix_nanos_string(expected_mtime),
            actual_unix_nanos: to_unix_nanos_string(stored_mtime),
            destination: path.display().to_string(),
            removed,
        });
    }

    let byte_len = fs::metadata(crate::platform::io_path(path))
        .map_err(|err| IngestError::io(path, err))?
        .len();
    Ok(VerifiedObject {
        filename: filename.to_string(),
        sha256: to_hex(expected),
        byte_len,
        mtime: stored_mtime,
        sha256_verify_ms,
    })
}

struct RemoveOnDrop {
    path: PathBuf,
    armed: bool,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if self.armed {
            dest::remove_file_best_effort(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_file, Tmp};
    use std::time::{Duration, UNIX_EPOCH};

    fn fixed_mtime() -> std::time::SystemTime {
        // 100 ns aligned so NTFS can store this exact tick.
        UNIX_EPOCH + Duration::new(1_339_746_000, 123_456_700)
    }

    #[test]
    fn copy_keeps_the_finest_mtime_the_filesystem_stores() {
        let tmp = Tmp::new();
        let requested = UNIX_EPOCH + Duration::new(1_339_746_000, 123_456_789);
        let src_dir = tmp.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        let source = write_file(&src_dir, "tick.jpg", b"tick-bytes", requested);
        let stored = mtime_of(&source).unwrap();
        assert_eq!(stored, crate::mtime::representable_mtime(requested));
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();
        copy_verify_file(&source, &dest_dir).unwrap();
        assert_eq!(mtime_of(&dest_dir.join("tick.jpg")).unwrap(), stored);
        assert_eq!(fs::read(dest_dir.join("tick.jpg")).unwrap(), b"tick-bytes");
    }

    #[test]
    fn a_different_case_is_a_collision() {
        let tmp = Tmp::new();
        let src = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dest_dir).unwrap();
        fs::write(dest_dir.join("IMG.JPG"), b"already").unwrap();
        let source = write_file(&src, "img.jpg", b"new", fixed_mtime());
        let err = copy_verify_file(&source, &dest_dir).unwrap_err();
        assert!(matches!(err, IngestError::CaseCollision { .. }), "{err}");
        assert_eq!(fs::read(dest_dir.join("IMG.JPG")).unwrap(), b"already");
        let names: Vec<_> = fs::read_dir(&dest_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["IMG.JPG".to_string()]);
    }

    #[cfg(windows)]
    #[test]
    fn a_named_stream_is_copied_with_the_file() {
        let tmp = Tmp::new();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "IMG_0001.JPG", b"jpeg", fixed_mtime());
        let mut stream = source.as_os_str().to_os_string();
        stream.push(":note");
        fs::write(stream, b"kept").unwrap();
        let dest_dir = tmp.path().join("dest");
        copy_verify_file(&source, &dest_dir).unwrap();
        let mut copied = dest_dir.join("IMG_0001.JPG").into_os_string();
        copied.push(":note");
        assert_eq!(fs::read(copied).unwrap(), b"kept");
        assert_eq!(fs::read(dest_dir.join("IMG_0001.JPG")).unwrap(), b"jpeg");
    }

    #[test]
    fn pipelined_copy_matches_copy_verify_file() {
        let tmp = Tmp::new();
        let src_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src_dir).unwrap();
        let mut sources = Vec::new();
        for index in 0..5u32 {
            let bytes: Vec<u8> = (0..200_000u32).map(|i| ((i + index) % 251) as u8).collect();
            sources.push(write_file(
                &src_dir,
                &format!("IMG_{index:04}.jpg"),
                &bytes,
                fixed_mtime(),
            ));
        }
        let done = std::sync::Mutex::new(Vec::new());
        copy_verify_files_pipelined(&sources, &dest_dir, 2, |index, result| {
            done.lock().unwrap().push((index, result.unwrap()));
            true
        });
        let mut done = done.into_inner().unwrap();
        done.sort_by_key(|(index, _)| *index);
        assert_eq!(done.len(), sources.len());
        for (index, stored) in done {
            let source = &sources[index];
            let dest = dest_dir.join(&stored.filename);
            assert_eq!(fs::read(&dest).unwrap(), fs::read(source).unwrap());
            assert_eq!(stored.sha256, crate::sha256_hex(source).unwrap());
            assert_eq!(mtime_of(source).unwrap(), mtime_of(&dest).unwrap());
        }
    }

    #[test]
    fn pipelined_copy_stopped_early_keeps_only_reported_files() {
        let tmp = Tmp::new();
        let src_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src_dir).unwrap();
        let sources: Vec<PathBuf> = (0..24u32)
            .map(|index| {
                write_file(
                    &src_dir,
                    &format!("IMG_{index:04}.jpg"),
                    &[index as u8; 4096],
                    fixed_mtime(),
                )
            })
            .collect();
        let reported = std::sync::Mutex::new(Vec::new());
        copy_verify_files_pipelined(&sources, &dest_dir, 3, |_, result| {
            let mut reported = reported.lock().unwrap();
            reported.push(result.unwrap().filename);
            reported.len() < 2
        });
        let mut reported = reported.into_inner().unwrap();
        reported.sort();
        let mut kept: Vec<String> = fs::read_dir(&dest_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        kept.sort();
        assert!(!reported.is_empty());
        assert_eq!(kept, reported);
    }

    #[test]
    fn pipelined_copy_refuses_to_overwrite() {
        let tmp = Tmp::new();
        let src_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dest_dir).unwrap();
        let source = write_file(&src_dir, "a.jpg", b"new", fixed_mtime());
        write_file(&dest_dir, "a.jpg", b"old", fixed_mtime());
        let errors = std::sync::Mutex::new(0);
        copy_verify_files_pipelined(std::slice::from_ref(&source), &dest_dir, 1, |_, result| {
            assert!(matches!(result, Err(IngestError::AlreadyExists { .. })));
            *errors.lock().unwrap() += 1;
            false
        });
        assert_eq!(*errors.lock().unwrap(), 1);
        assert_eq!(fs::read(dest_dir.join("a.jpg")).unwrap(), b"old");
    }

    #[test]
    fn copy_preserves_bytes_name_and_mtime() {
        let tmp = Tmp::new();
        let src_dir = tmp.path().join("src");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir(&src_dir).unwrap();
        let bytes: Vec<u8> = (0..1_500_000).map(|i| (i % 251) as u8).collect();
        let source = write_file(&src_dir, "IMG 0001.jpg", &bytes, fixed_mtime());
        let stored = copy_verify_file(&source, &dest_dir).unwrap();
        let dest = dest_dir.join("IMG 0001.jpg");
        assert_eq!(fs::read(&dest).unwrap(), bytes);
        assert_eq!(stored.filename, "IMG 0001.jpg");
        assert_eq!(stored.sha256, crate::sha256_hex(&source).unwrap());
        assert_eq!(mtime_of(&source).unwrap(), mtime_of(&dest).unwrap());
        assert!(mtime_matches(fixed_mtime(), mtime_of(&dest).unwrap()));
        assert_eq!(stored.byte_len, bytes.len() as u64);
    }

    #[test]
    fn mismatch_removes_destination_and_names_both_hashes() {
        let tmp = Tmp::new();
        let path = write_file(tmp.path(), "bad.jpg", b"tampered-bytes", fixed_mtime());
        let expected = [7u8; 32];
        let err = finalize_verified(&path, "bad.jpg", &expected, fixed_mtime()).unwrap_err();
        match err {
            IngestError::ChecksumMismatch {
                filename,
                expected_sha256,
                actual_sha256,
                removed,
                message,
                ..
            } => {
                assert_eq!(filename, "bad.jpg");
                assert!(removed);
                assert_ne!(expected_sha256, actual_sha256);
                assert!(message.contains("checksum mismatch"));
                assert!(message.contains(&expected_sha256));
                assert!(message.contains(&actual_sha256));
                assert!(message.contains("removed destination"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_overwrite() {
        let tmp = Tmp::new();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.jpg", b"one", fixed_mtime());
        let dest = tmp.path().join("out");
        copy_verify_file(&source, &dest).unwrap();
        let err = copy_verify_file(&source, &dest).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_eq!(fs::read(dest.join("a.jpg")).unwrap(), b"one");
    }

    #[test]
    fn copy_refuses_a_destination_inside_the_source() {
        let tmp = Tmp::new();
        let src = tmp.path().join("library");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.txt", b"alpha", fixed_mtime());
        let nested = src.join("nested");
        let err = copy_verify_file(&source, &nested).unwrap_err();
        assert!(
            matches!(err, IngestError::DestinationInsideSource { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("sources are read-only"), "{err}");
        assert!(!nested.exists());
        assert_eq!(fs::read(&source).unwrap(), b"alpha");
    }

    #[test]
    fn copy_refuses_a_hardlink_of_a_source_file() {
        let tmp = Tmp::new();
        let src = tmp.path().join("library");
        fs::create_dir(&src).unwrap();
        let source = write_file(&src, "a.txt", b"alpha", fixed_mtime());
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let link = outside.join("a.txt");
        fs::hard_link(&source, &link).unwrap();
        let err = copy_verify_file(&source, &outside).unwrap_err();
        assert!(
            matches!(err, IngestError::DestinationInsideSource { .. }),
            "{err}"
        );
        assert_eq!(fs::read(&source).unwrap(), b"alpha");
        assert_eq!(fs::read(&link).unwrap(), b"alpha");
    }

    #[test]
    fn copy_allows_a_sibling_library() {
        let tmp = Tmp::new();
        let export = tmp.path().join("export");
        fs::create_dir(&export).unwrap();
        for index in 0..8 {
            write_file(
                &export,
                &format!("p{index}.heic"),
                b"\xff\xd8\xff\xd9",
                fixed_mtime(),
            );
        }
        let library = tmp.path().join("library");
        fs::create_dir(&library).unwrap();
        let root = crate::source::SourceRoot::open(&export).unwrap();
        root.refuse_destination(&library).unwrap();
        let copied = copy_verify_file(&export.join("p0.heic"), &library).unwrap();
        assert_eq!(copied.byte_len, 4);
        assert_eq!(
            fs::read(library.join("p0.heic")).unwrap(),
            b"\xff\xd8\xff\xd9"
        );
        assert_eq!(
            fs::read(export.join("p0.heic")).unwrap(),
            b"\xff\xd8\xff\xd9"
        );
    }
}
