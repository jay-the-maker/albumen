// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::IngestError;

pub fn sha256_file(path: &Path) -> Result<[u8; 32], IngestError> {
    let mut file = File::open(path).map_err(|err| IngestError::io(path, err))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
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

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Hash the bytes read from `reader` while writing them to `writer`.
pub(crate) fn copy_hashed<R: Read, W: std::io::Write>(
    mut reader: R,
    mut writer: W,
    source_path: &Path,
    dest_path: &Path,
) -> Result<[u8; 32], IngestError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|err| IngestError::io(source_path, err))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer
            .write_all(&buf[..n])
            .map_err(|err| IngestError::io(dest_path, err))?;
    }
    Ok(hasher.finalize().into())
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
