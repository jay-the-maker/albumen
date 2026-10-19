// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::IngestError;

pub fn sha256_file(path: &Path) -> Result<[u8; 32], IngestError> {
    let file =
        File::open(crate::platform::io_path(path)).map_err(|err| IngestError::io(path, err))?;
    sha256_reader(file, path)
}

pub(crate) fn sha256_reader<R: Read>(mut reader: R, path: &Path) -> Result<[u8; 32], IngestError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
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
    let mut buf = vec![0u8; 1024 * 1024];
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
    let mut buf = vec![0u8; 1024 * 1024];
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
    }
    let hash_at = crate::copy_profile::tic();
    let digest = hasher.finalize();
    crate::copy_profile::add_readback_hash(hash_at);
    Ok(digest.into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
