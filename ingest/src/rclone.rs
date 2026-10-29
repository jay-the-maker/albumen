// SPDX-License-Identifier: Apache-2.0

//! rclone transport for SFTP, WebDAV, and Backblaze B2.
//!
//! Albumen does not vendor rclone. The binary is a separate process. Each
//! `put_verified` uploads through rclone, reads the object back, and checks
//! SHA-256. SFTP, WebDAV, and B2 do not keep nanosecond mtimes, so the exact
//! source mtime and digest are stored in a sibling `.{name}.albumen-verify`
//! object and restored on read. That sibling is transport metadata, not a
//! camera sidecar. An existing object is not overwritten.

use crate::dest;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::checksum::sha256_hex;
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::export::{check_relative, scratch_name_chars, source_fingerprint, ExportDest, Placed};
use crate::local::finalize_verified;
use crate::mtime::{from_unix_nanos_string, mtime_of, set_mtime, to_unix_nanos_string};
use crate::store::{BlobStore, VerifiedObject};

const VERIFY_SUFFIX: &str = ".albumen-verify";

/// A remote reached by shelling out to rclone.
pub struct RcloneStore {
    bin: PathBuf,
    /// Remote path of the directory, with a trailing slash. `:sftp:` or
    /// `:b2:bucket/prefix/`.
    root: String,
    flags: Vec<String>,
    secrets: Vec<String>,
    label: String,
    /// Host and port this store contacts. Logged once, before the first rclone run.
    contact: Option<(String, u16)>,
    contact_logged: AtomicBool,
}

impl std::fmt::Debug for RcloneStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RcloneStore")
            .field("root", &self.root)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl RcloneStore {
    pub fn sftp(
        bin: impl Into<PathBuf>,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<Self, IngestError> {
        let bin = bin.into();
        let obscured = obscure(&bin, password)?;
        Ok(Self {
            flags: vec![
                "--sftp-host".into(),
                host.into(),
                "--sftp-port".into(),
                port.to_string(),
                "--sftp-user".into(),
                user.into(),
                "--sftp-pass".into(),
                obscured.clone(),
                "--sftp-known-hosts-file".into(),
                "none".into(),
            ],
            secrets: vec![obscured, password.to_string()],
            root: ":sftp:".into(),
            label: format!("sftp://{user}@{host}:{port}"),
            bin,
            contact: Some((host.to_string(), port)),
            contact_logged: AtomicBool::new(false),
        })
    }

    pub fn webdav(
        bin: impl Into<PathBuf>,
        url: &str,
        user: &str,
        password: &str,
    ) -> Result<Self, IngestError> {
        let bin = bin.into();
        let obscured = obscure(&bin, password)?;
        let (host, port) = crate::ledger::endpoint_host_port(url)?;
        Ok(Self {
            flags: vec![
                "--webdav-url".into(),
                url.trim_end_matches('/').into(),
                "--webdav-user".into(),
                user.into(),
                "--webdav-pass".into(),
                obscured.clone(),
            ],
            secrets: vec![obscured, password.to_string()],
            root: ":webdav:".into(),
            label: format!("webdav://{url}"),
            bin,
            contact: Some((host, port)),
            contact_logged: AtomicBool::new(false),
        })
    }

    /// Backblaze B2 through rclone's `b2` backend.
    pub fn b2(
        bin: impl Into<PathBuf>,
        account: &str,
        key: &str,
        bucket: &str,
        prefix: &str,
    ) -> Result<Self, IngestError> {
        check_bucket(bucket)?;
        Ok(Self {
            flags: vec![
                "--b2-account".into(),
                account.into(),
                "--b2-key".into(),
                key.into(),
            ],
            secrets: vec![account.to_string(), key.to_string()],
            root: format!(":b2:{}/{}", bucket, normalize_prefix(prefix)),
            label: format!("b2://{bucket}/{prefix}"),
            bin: bin.into(),
            contact: Some(("api.backblazeb2.com".to_string(), 443)),
            contact_logged: AtomicBool::new(false),
        })
    }

    /// S3-compatible endpoint through rclone. CI uses this against MinIO as
    /// the stand-in for B2, which has no local server.
    pub fn s3(
        bin: impl Into<PathBuf>,
        endpoint: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
        prefix: &str,
    ) -> Result<Self, IngestError> {
        check_bucket(bucket)?;
        let (host, port) = crate::ledger::endpoint_host_port(endpoint)?;
        let store = Self {
            flags: vec![
                "--s3-provider".into(),
                "Minio".into(),
                "--s3-endpoint".into(),
                endpoint.trim_end_matches('/').into(),
                "--s3-access-key-id".into(),
                access_key.into(),
                "--s3-secret-access-key".into(),
                secret_key.into(),
                "--s3-force-path-style".into(),
            ],
            secrets: vec![access_key.to_string(), secret_key.to_string()],
            root: format!(":s3:{}/{}", bucket, normalize_prefix(prefix)),
            label: format!("s3://{bucket}/{prefix}"),
            bin: bin.into(),
            contact: Some((host, port)),
            contact_logged: AtomicBool::new(false),
        };
        store.ensure_s3_bucket(bucket)?;
        Ok(store)
    }

    fn ensure_s3_bucket(&self, bucket: &str) -> Result<(), IngestError> {
        let output = self.run(&["mkdir".into(), format!(":s3:{bucket}")])?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("BucketAlready") || stderr.to_ascii_lowercase().contains("already") {
            return Ok(());
        }
        Err(self.fail("mkdir", bucket, &output))
    }

    fn remote(&self, name: &str) -> String {
        format!("{}{name}", self.root)
    }

    /// `a/b.jpg` -> `a/.b.jpg.albumen-verify`, beside the object it describes.
    fn verify_name(relative: &str) -> String {
        match relative.rsplit_once('/') {
            Some((dir, name)) => format!("{dir}/.{name}{VERIFY_SUFFIX}"),
            None => format!(".{relative}{VERIFY_SUFFIX}"),
        }
    }

    /// Point the store at a subdirectory of its remote, e.g. an export prefix.
    pub fn under(mut self, prefix: &str) -> Result<Self, IngestError> {
        let prefix = prefix.trim_matches('/');
        if prefix.is_empty() {
            return Ok(self);
        }
        check_relative(prefix)?;
        self.root = format!("{}{prefix}/", self.root);
        self.label = format!("{}/{prefix}", self.label.trim_end_matches('/'));
        Ok(self)
    }

    fn exists(&self, name: &str) -> Result<bool, IngestError> {
        let output = self.run(&[
            "lsf".into(),
            self.root.clone(),
            "--files-only".into(),
            "--max-depth".into(),
            "1".into(),
        ])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_missing(&stderr) {
                return Ok(false);
            }
            return Err(self.fail("lsf", name, &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.lines().any(|line| line.trim() == name))
    }

    fn put_new(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        let actual = file_name_str(source)?;
        if actual != filename {
            return Err(IngestError::FilenameMismatch {
                source_name: actual,
                requested: filename.to_string(),
            });
        }
        if self.exists(filename)? {
            return Err(IngestError::AlreadyExists {
                filename: filename.to_string(),
                destination: format!("{}{filename}", self.label),
            });
        }
        self.upload_verified(source, filename)
    }

    /// Upload, write the verify record, read both back, and compare SHA-256.
    /// Anything this call wrote is removed on failure. The caller has
    /// already checked that nothing is at `filename`.
    fn upload_verified(
        &self,
        source: &Path,
        filename: &str,
    ) -> Result<VerifiedObject, IngestError> {
        let source_str = path_arg(source)?;
        let digest = sha256_hex(source)?;
        let mtime = mtime_of(source)?;
        let nanos = to_unix_nanos_string(mtime);
        let data = self.remote(filename);
        let verify = self.remote(&Self::verify_name(filename));
        if let Err(err) = self.copy_to(source_str, &data, filename) {
            let _ = self.delete_name(filename);
            return Err(err);
        }
        let record = format!("{nanos}\n{digest}\n");
        if let Err(err) = self.write_text(&verify, &record, filename) {
            let _ = self.delete_name(filename);
            return Err(err);
        }
        let read_back = match self.cat(&verify, filename) {
            Ok(text) => text,
            Err(err) => {
                let _ = self.delete_name(filename);
                return Err(err);
            }
        };
        if read_back != record {
            let _ = self.delete_name(filename);
            return Err(IngestError::store(
                "verify record",
                filename,
                "the mtime record read back did not match what was written",
            ));
        }
        let downloaded = temp_path(filename);
        let guard = TempPath {
            path: downloaded.clone(),
            armed: true,
        };
        if let Err(err) = self.copy_to(&data, path_arg(&downloaded)?, filename) {
            let _ = self.delete_name(filename);
            return Err(err);
        }
        let actual_digest = match sha256_hex(&downloaded) {
            Ok(hash) => hash,
            Err(err) => {
                let _ = self.delete_name(filename);
                return Err(err);
            }
        };
        if actual_digest != digest {
            let removed = self.delete_name(filename).is_ok();
            return Err(IngestError::checksum_mismatch(
                filename,
                digest,
                actual_digest,
                data,
                removed,
            ));
        }
        drop(guard);
        let byte_len = fs::metadata(source)
            .map_err(|err| IngestError::io(source, err))?
            .len();
        Ok(VerifiedObject {
            filename: filename.to_string(),
            sha256: digest,
            byte_len,
            mtime,
            sha256_verify_ms: 0,
        })
    }

    fn copy_to(&self, source: &str, dest: &str, filename: &str) -> Result<(), IngestError> {
        let output = self.run(&["copyto".into(), source.into(), dest.into()])?;
        if output.status.success() {
            return Ok(());
        }
        Err(self.fail("copyto", filename, &output))
    }

    fn write_text(&self, remote: &str, text: &str, filename: &str) -> Result<(), IngestError> {
        let path = temp_path(filename);
        let guard = TempPath {
            path: path.clone(),
            armed: true,
        };
        let mut file = dest::create_file(&path)?;
        file.write_all(text.as_bytes())
            .map_err(|err| IngestError::io(&path, err))?;
        file.sync_all().map_err(|err| IngestError::io(&path, err))?;
        drop(file);
        self.copy_to(path_arg(&path)?, remote, filename)?;
        drop(guard);
        Ok(())
    }

    fn cat(&self, remote: &str, filename: &str) -> Result<String, IngestError> {
        let output = self.run(&["cat".into(), remote.into()])?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(self.fail("cat", filename, &output))
    }

    fn delete_name(&self, filename: &str) -> Result<(), IngestError> {
        self.delete_remote(&self.remote(filename), filename)?;
        self.delete_remote(&self.remote(&Self::verify_name(filename)), filename)?;
        Ok(())
    }

    fn delete_remote(&self, remote: &str, filename: &str) -> Result<(), IngestError> {
        let output = self.run(&["deletefile".into(), remote.into()])?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_missing(&stderr) {
            return Ok(());
        }
        Err(self.fail("deletefile", filename, &output))
    }

    fn note_contact(&self) -> Result<(), IngestError> {
        let Some((host, port)) = &self.contact else {
            return Ok(());
        };
        if self.contact_logged.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        if let Err(err) = crate::ledger::record_anchored_connection("connect", host, *port) {
            self.contact_logged.store(false, Ordering::Relaxed);
            return Err(err);
        }
        Ok(())
    }

    fn run(&self, args: &[String]) -> Result<Output, IngestError> {
        self.note_contact()?;
        let mut command = Command::new(&self.bin);
        command
            .arg("--config")
            .arg(empty_rclone_config()?)
            .arg("--log-level")
            .arg("ERROR")
            .arg("--retries")
            .arg("1")
            .arg("--low-level-retries")
            .arg("1");
        command.args(args);
        command.args(&self.flags);
        command
            .output()
            .map_err(|err| IngestError::store("rclone", &self.label, err))
    }

    fn fail(&self, operation: &'static str, filename: &str, output: &Output) -> IngestError {
        let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        for secret in &self.secrets {
            if !secret.is_empty() {
                stderr = stderr.replace(secret, "***");
            }
        }
        let code = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".into());
        IngestError::store(
            operation,
            filename,
            format!("rclone exited {code}: {}", stderr.trim()),
        )
    }
}

impl BlobStore for RcloneStore {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        if !source.is_file() {
            return Err(IngestError::NotAFile(source.to_path_buf()));
        }
        self.put_new(source, filename)
    }

    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        dest::create_dir_all(dest_dir)?;
        let dest = dest_dir.join(filename);
        if dest.exists() {
            return Err(IngestError::AlreadyExists {
                filename: filename.to_string(),
                destination: dest.display().to_string(),
            });
        }
        let data = self.remote(filename);
        if let Err(err) = self.copy_to(&data, path_arg(&dest)?, filename) {
            let _ = dest::remove_file(&dest);
            return Err(err);
        }
        let record = match self.cat(&self.remote(&Self::verify_name(filename)), filename) {
            Ok(text) => text,
            Err(err) => {
                let _ = dest::remove_file(&dest);
                return Err(err);
            }
        };
        let (nanos, expected) = match parse_verify(&record) {
            Ok(parsed) => parsed,
            Err(err) => {
                let _ = dest::remove_file(&dest);
                return Err(IngestError::store("verify record", filename, err));
            }
        };
        let mtime = match from_unix_nanos_string(&nanos, filename) {
            Ok(mtime) => mtime,
            Err(err) => {
                let _ = dest::remove_file(&dest);
                return Err(err);
            }
        };
        if let Err(err) = set_mtime(&dest, mtime) {
            let _ = dest::remove_file(&dest);
            return Err(err);
        }
        let expected_bytes = match parse_hex32(&expected) {
            Ok(bytes) => bytes,
            Err(err) => {
                let _ = dest::remove_file(&dest);
                return Err(IngestError::store("verify record", filename, err));
            }
        };
        finalize_verified(&dest, filename, &expected_bytes, mtime)
    }

    fn delete(&self, filename: &str) -> Result<(), IngestError> {
        check_filename(filename)?;
        self.delete_name(filename)
    }
}

/// Export through rclone. Same rules as a local directory: identical bytes are
/// left in place, a different file at the path is `AlreadyExists`, and
/// `remove_placed` only removes what this export uploaded.
impl ExportDest for RcloneStore {
    fn place(&self, source: &Path, relative: &str) -> Result<Placed, IngestError> {
        check_relative(relative)?;
        let last = relative.rsplit('/').next().unwrap_or(relative);
        let actual = file_name_str(source)?;
        if actual != last {
            return Err(IngestError::FilenameMismatch {
                source_name: actual,
                requested: relative.to_string(),
            });
        }
        let (source_hash, source_len) = source_fingerprint(source)?;
        if let Some((hash, len)) = self.read_hash(relative)? {
            if hash == source_hash && len == source_len {
                return Ok(Placed {
                    object: VerifiedObject {
                        filename: relative.to_string(),
                        sha256: hash,
                        byte_len: len,
                        mtime: mtime_of(source)?,
                        sha256_verify_ms: 0,
                    },
                    copied: false,
                });
            }
            return Err(IngestError::AlreadyExists {
                filename: relative.to_string(),
                destination: format!("{}/{relative}", self.label.trim_end_matches('/')),
            });
        }
        let object = self.upload_verified(source, relative)?;
        Ok(Placed {
            object,
            copied: true,
        })
    }

    fn read_hash(&self, relative: &str) -> Result<Option<(String, u64)>, IngestError> {
        check_relative(relative)?;
        let downloaded = temp_path(relative);
        let _guard = TempPath {
            path: downloaded.clone(),
            armed: true,
        };
        let output = self.run(&[
            "copyto".into(),
            self.remote(relative),
            path_arg(&downloaded)?.into(),
        ])?;
        if !output.status.success() {
            if is_missing(&String::from_utf8_lossy(&output.stderr)) {
                return Ok(None);
            }
            return Err(self.fail("copyto", relative, &output));
        }
        if !downloaded.is_file() {
            return Ok(None);
        }
        Ok(Some(source_fingerprint(&downloaded)?))
    }

    fn list(&self) -> Result<Vec<String>, IngestError> {
        let output = self.run(&[
            "lsf".into(),
            self.root.clone(),
            "--files-only".into(),
            "-R".into(),
        ])?;
        if !output.status.success() {
            if is_missing(&String::from_utf8_lossy(&output.stderr)) {
                return Ok(Vec::new());
            }
            return Err(self.fail("lsf", &self.label, &output));
        }
        let mut out: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty() && !is_verify_record(line))
            .map(str::to_string)
            .collect();
        out.sort();
        Ok(out)
    }

    fn remove_placed(&self, relative: &str) -> Result<(), IngestError> {
        check_relative(relative)?;
        self.delete_name(relative)
    }
}

fn is_verify_record(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.starts_with('.') && name.ends_with(VERIFY_SUFFIX)
}

fn obscure(bin: &Path, password: &str) -> Result<String, IngestError> {
    let output = Command::new(bin)
        .arg("--config")
        .arg(empty_rclone_config()?)
        .arg("obscure")
        .arg(password)
        .output()
        .map_err(|err| IngestError::store("rclone obscure", "password", err))?;
    if !output.status.success() {
        return Err(IngestError::store(
            "rclone obscure",
            "password",
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err(IngestError::store(
            "rclone obscure",
            "password",
            "rclone obscure returned an empty password",
        ));
    }
    Ok(text)
}

fn parse_verify(text: &str) -> Result<(String, String), String> {
    let mut lines = text.lines();
    let nanos = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| "mtime record is empty".to_string())?;
    let digest = lines
        .next()
        .filter(|line| line.len() == 64 && line.chars().all(|ch| ch.is_ascii_hexdigit()))
        .ok_or_else(|| "mtime record is missing a sha256".to_string())?;
    if lines.next().is_some() {
        return Err("mtime record has extra lines".to_string());
    }
    Ok((nanos.to_string(), digest.to_string()))
}

fn parse_hex32(value: &str) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| format!("invalid sha256 {value}"))?;
    }
    Ok(out)
}

fn is_missing(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("not found")
        || text.contains("doesn't exist")
        || text.contains("does not exist")
        || text.contains("404")
}

fn check_bucket(bucket: &str) -> Result<(), IngestError> {
    if bucket.is_empty() || bucket.contains('/') || bucket.contains(' ') || bucket.contains('\0') {
        return Err(IngestError::store(
            "bucket",
            bucket,
            "bucket name must be a single path segment",
        ));
    }
    Ok(())
}

fn normalize_prefix(prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    }
}

fn empty_rclone_config() -> Result<&'static Path, IngestError> {
    static CONFIG: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = CONFIG.get() {
        return Ok(path);
    }
    let path = std::env::temp_dir().join(format!("albumen-rclone-{}.conf", std::process::id()));
    dest::create_file(&path)?;
    Ok(CONFIG.get_or_init(|| path))
}

fn path_arg(path: &Path) -> Result<&str, IngestError> {
    path.to_str()
        .ok_or_else(|| IngestError::store("path", path.display().to_string(), "path is not utf-8"))
}

fn temp_path(filename: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let safe: String = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(scratch_name_chars())
        .collect();
    std::env::temp_dir().join(format!(
        "albumen-rclone-{nanos}-{n}-{}-{safe}",
        std::process::id()
    ))
}

struct TempPath {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.armed {
            let _ = dest::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag_after(flags: &[String], name: &str) -> String {
        let index = flags.iter().position(|flag| flag == name).unwrap();
        flags[index + 1].clone()
    }

    #[test]
    fn verify_record_is_mtime_then_sha256() {
        let (nanos, digest) = parse_verify(
            "1400000000123456789\nabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789\n",
        )
        .unwrap();
        assert_eq!(nanos, "1400000000123456789");
        assert_eq!(digest.len(), 64);
        assert!(parse_verify("only-one-line\n").is_err());
    }

    #[test]
    fn verify_record_sits_beside_a_nested_object_and_is_not_listed() {
        assert_eq!(
            RcloneStore::verify_name("a/b.jpg"),
            "a/.b.jpg.albumen-verify"
        );
        assert_eq!(RcloneStore::verify_name("b.jpg"), ".b.jpg.albumen-verify");
        assert!(is_verify_record("a/.b.jpg.albumen-verify"));
        assert!(!is_verify_record("a/b.jpg"));
        assert!(!is_verify_record(".hidden.jpg"));
    }

    #[test]
    fn under_refuses_parent_components() {
        let store = RcloneStore::b2("/bin/rclone", "a", "k", "bucket", "").unwrap();
        assert!(store.under("../x").is_err());
    }

    #[test]
    fn b2_remote_uses_the_b2_backend() {
        let store =
            RcloneStore::b2("/bin/rclone", "account", "key", "albumen-bucket", "roll").unwrap();
        assert_eq!(store.root, ":b2:albumen-bucket/roll/");
        assert_eq!(flag_after(&store.flags, "--b2-account"), "account");
        assert_eq!(flag_after(&store.flags, "--b2-key"), "key");
        let shown = format!("{store:?}");
        assert!(!shown.contains("key"));
        assert!(!shown.contains("account"));
    }
}
