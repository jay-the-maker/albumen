// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::IngestError;

/// Bytes per read. 8 MiB used no less CPU than 1 MiB on Linux: the bytes
/// fall out of cache between the read, the hash, and the write.
const IO_BUF_BYTES: usize = 1024 * 1024;

thread_local! {
    static SPARE: std::cell::RefCell<Vec<Vec<u8>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// One I/O buffer per thread, reused across files. Reads land in it and
/// SHA-256 and the write both use those bytes in place. A fresh buffer per
/// file cost a page fault per 4 KiB page each time.
struct IoBuf(Vec<u8>);

impl IoBuf {
    fn take() -> Self {
        let buf = SPARE
            .with(|spare| spare.borrow_mut().pop())
            .unwrap_or_else(|| vec![0u8; IO_BUF_BYTES]);
        Self(buf)
    }
}

impl Drop for IoBuf {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.0);
        if buf.len() == IO_BUF_BYTES {
            SPARE.with(|spare| {
                let mut spare = spare.borrow_mut();
                // The pipelined read-back keeps two buffers per thread.
                if spare.len() < 2 {
                    spare.push(buf);
                }
            });
        }
    }
}

impl std::ops::Deref for IoBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl std::ops::DerefMut for IoBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

pub fn sha256_file(path: &Path) -> Result<[u8; 32], IngestError> {
    let file =
        File::open(crate::platform::io_path(path)).map_err(|err| IngestError::io(path, err))?;
    sha256_reader(file, path)
}

pub(crate) fn sha256_reader<R: Read>(mut reader: R, path: &Path) -> Result<[u8; 32], IngestError> {
    let mut hasher = Sha256::new();
    let mut buf = IoBuf::take();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

pub fn sha256_hex(path: &Path) -> Result<String, IngestError> {
    Ok(to_hex(&sha256_file(path)?))
}

pub(crate) fn sha256_slice(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Copy `reader` to `writer`, hashing the bytes as they pass.
///
/// The source is read once. The returned digest is the bytes written.
pub(crate) fn copy_hashed<R: Read, W: std::io::Write>(
    mut reader: R,
    mut writer: W,
    source_path: &Path,
    dest_path: &Path,
) -> Result<[u8; 32], IngestError> {
    let mut hasher = Sha256::new();
    let mut buf = IoBuf::take();
    loop {
        let read_at = crate::copy_profile::tic();
        let n = reader
            .read(&mut buf)
            .map_err(|err| IngestError::io(source_path, err))?;
        crate::copy_profile::add_source_read(read_at);
        if n == 0 {
            break;
        }
        let hash_at = crate::copy_profile::tic();
        hasher.update(&buf[..n]);
        crate::copy_profile::add_hash(hash_at);
        let write_at = crate::copy_profile::tic();
        writer
            .write_all(&buf[..n])
            .map_err(|err| IngestError::io(dest_path, err))?;
        crate::copy_profile::add_write(write_at);
    }
    let hash_at = crate::copy_profile::tic();
    let digest = hasher.finalize();
    crate::copy_profile::add_hash(hash_at);
    Ok(digest.into())
}

/// Hash a destination that was just written, splitting the read from the hash.
///
/// [`sha256_file`] is the untimed read-back used everywhere else. This one
/// is the independent check in `finalize_verified`, so a profile can see
/// that second read without folding it into the copy.
pub(crate) fn sha256_readback(path: &Path) -> Result<[u8; 32], IngestError> {
    let mut file =
        File::open(crate::platform::io_path(path)).map_err(|err| IngestError::io(path, err))?;
    let mut hasher = Sha256::new();
    let mut buf = IoBuf::take();
    loop {
        let read_at = crate::copy_profile::tic();
        let n = file
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        crate::copy_profile::add_readback_read(read_at);
        if n == 0 {
            break;
        }
        let hash_at = crate::copy_profile::tic();
        hasher.update(&buf[..n]);
        crate::copy_profile::add_readback_hash(hash_at);
        if n == buf.len() {
            // More than one buffer: overlap the rest of the read with the hash.
            hasher = readback_overlapped(&mut file, path, hasher, buf)?;
            break;
        }
    }
    let hash_at = crate::copy_profile::tic();
    let digest = hasher.finalize();
    crate::copy_profile::add_readback_hash(hash_at);
    Ok(digest.into())
}

/// The rest of [`sha256_readback`], with the read of one buffer running
/// while the hasher thread hashes the one before (#138, wall time only).
///
/// Verification is the same: the same fresh read of the stored file, in
/// order, into the same hasher, which only this read-back ever fed. Each
/// buffer is hashed before the reader may reuse it. The caller still
/// compares the digest with the copy's.
fn readback_overlapped(
    file: &mut File,
    path: &Path,
    hasher: Sha256,
    first: IoBuf,
) -> Result<Sha256, IngestError> {
    use std::sync::mpsc::{channel, sync_channel};
    let profile = crate::copy_profile::thread_enabled();
    let (full_tx, full_rx) = sync_channel::<(IoBuf, usize)>(1);
    let (empty_tx, empty_rx) = channel::<IoBuf>();
    std::thread::scope(|scope| {
        let hashing = scope.spawn(move || {
            if profile {
                crate::copy_profile::enable_thread();
            }
            let mut hasher = hasher;
            for (buf, n) in full_rx {
                let hash_at = crate::copy_profile::tic();
                hasher.update(&buf[..n]);
                crate::copy_profile::add_readback_hash(hash_at);
                if empty_tx.send(buf).is_err() {
                    break;
                }
            }
            hasher
        });
        let mut spare = Some(first);
        let mut second = Some(IoBuf::take());
        let read = (|| loop {
            let mut buf = match spare.take().or_else(|| second.take()) {
                Some(buf) => buf,
                None => match empty_rx.recv() {
                    Ok(buf) => buf,
                    Err(_) => return Ok(()),
                },
            };
            let read_at = crate::copy_profile::tic();
            let n = file
                .read(&mut buf)
                .map_err(|err| IngestError::io(path, err))?;
            crate::copy_profile::add_readback_read(read_at);
            if n == 0 {
                return Ok(());
            }
            if full_tx.send((buf, n)).is_err() {
                return Ok(());
            }
        })();
        drop(full_tx);
        let hasher = hashing.join().expect("read-back hasher thread panicked");
        read.map(|()| hasher)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlapped read-back gives the same digest as one plain pass,
    /// across buffer-boundary sizes.
    #[test]
    fn overlapped_readback_matches_a_plain_hash() {
        let tmp = crate::test_support::Tmp::new();
        for len in [
            0,
            1,
            IO_BUF_BYTES - 1,
            IO_BUF_BYTES,
            IO_BUF_BYTES + 1,
            3 * IO_BUF_BYTES + 12345,
        ] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 253) as u8).collect();
            let path = tmp.path().join(format!("f{len}"));
            std::fs::write(&path, &data).unwrap();
            let plain: [u8; 32] = Sha256::digest(&data).into();
            assert_eq!(sha256_readback(&path).unwrap(), plain, "len {len}");
        }
    }

    #[test]
    fn empty_file_is_the_well_known_sha256() {
        let tmp = crate::test_support::Tmp::new();
        let path = tmp.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            sha256_hex(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
