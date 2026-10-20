// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::IngestError;

/// Bytes per read. 8 MiB used no less CPU than 1 MiB on Linux: the bytes
/// fall out of cache between the read, the hash, and the write.
const IO_BUF_BYTES: usize = 1024 * 1024;

/// Bytes per read in the destination read-back on macOS (#138). The
/// destination was written with `F_NOCACHE`, so each read-back read goes to
/// the device; fewer, larger reads cut the per-read device round trips. The
/// bytes read and hashed are the same. Other platforms keep [`IO_BUF_BYTES`].
const READBACK_BUF_BYTES: usize = if cfg!(target_vendor = "apple") {
    8 * 1024 * 1024
} else {
    IO_BUF_BYTES
};

thread_local! {
    static SPARE: std::cell::Cell<Option<Vec<u8>>> = const { std::cell::Cell::new(None) };
    /// The read-back buffer, reused across files on each verifier thread.
    static READBACK_SPARE: std::cell::Cell<Option<Vec<u8>>> = const { std::cell::Cell::new(None) };
}

/// One I/O buffer per thread, reused across files. Reads land in it and
/// SHA-256 and the write both use those bytes in place. A fresh buffer per
/// file cost a page fault per 4 KiB page each time.
struct IoBuf(Vec<u8>);

impl IoBuf {
    fn take() -> Self {
        let buf = SPARE
            .with(|spare| spare.take())
            .unwrap_or_else(|| vec![0u8; IO_BUF_BYTES]);
        Self(buf)
    }
}

impl Drop for IoBuf {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.0);
        SPARE.with(|spare| spare.set(Some(buf)));
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
    let mut buf = READBACK_SPARE
        .with(|spare| spare.take())
        .unwrap_or_else(|| vec![0u8; READBACK_BUF_BYTES]);
    let result = (|| loop {
        let read_at = crate::copy_profile::tic();
        let n = file
            .read(&mut buf)
            .map_err(|err| IngestError::io(path, err))?;
        crate::copy_profile::add_readback_read(read_at);
        if n == 0 {
            return Ok(());
        }
        let hash_at = crate::copy_profile::tic();
        hasher.update(&buf[..n]);
        crate::copy_profile::add_readback_hash(hash_at);
    })();
    READBACK_SPARE.with(|spare| spare.set(Some(buf)));
    result?;
    let hash_at = crate::copy_profile::tic();
    let digest = hasher.finalize();
    crate::copy_profile::add_readback_hash(hash_at);
    Ok(digest.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read-back digest equals a plain hash around the read-back buffer
    /// size, and repeated read-backs on one thread agree.
    #[test]
    fn readback_matches_a_plain_hash_at_buffer_edges() {
        let tmp = crate::test_support::Tmp::new();
        for len in [
            0,
            1,
            READBACK_BUF_BYTES - 1,
            READBACK_BUF_BYTES,
            READBACK_BUF_BYTES + 1,
            2 * READBACK_BUF_BYTES + 777,
        ] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 253) as u8).collect();
            let path = tmp.path().join(format!("f{len}"));
            std::fs::write(&path, &data).unwrap();
            let plain: [u8; 32] = Sha256::digest(&data).into();
            assert_eq!(sha256_readback(&path).unwrap(), plain, "len {len}");
            assert_eq!(sha256_readback(&path).unwrap(), plain, "again {len}");
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
