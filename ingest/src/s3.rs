// SPDX-License-Identifier: Apache-2.0

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aws_credential_types::provider::SharedCredentialsProvider;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{
    BehaviorVersion, Builder, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

use crate::checksum::{sha256_file, to_hex};
use crate::discover::check_filename;
use crate::error::IngestError;
use crate::local::finalize_verified;
use crate::mtime::{from_unix_nanos_string, mtime_of, to_unix_nanos_string};
use crate::store::{BlobStore, VerifiedObject};

const MTIME_META: &str = "albumen-mtime-ns";
const SHA_META: &str = "albumen-sha256";

/// Connection settings for an S3-compatible store.
///
/// `endpoint` is required so tests can point at MinIO. Credentials are dummy
/// values in CI; this crate does not ship secrets.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
}

impl S3Config {
    /// Read `ALBUMEN_S3_ENDPOINT`, `ALBUMEN_S3_BUCKET`, `ALBUMEN_S3_ACCESS_KEY`,
    /// and `ALBUMEN_S3_SECRET_KEY`. `ALBUMEN_S3_REGION` defaults to `us-east-1`.
    /// `ALBUMEN_S3_PREFIX` defaults to empty.
    pub fn from_env() -> Result<Self, IngestError> {
        Ok(Self {
            endpoint: env_required("ALBUMEN_S3_ENDPOINT")?,
            bucket: env_required("ALBUMEN_S3_BUCKET")?,
            region: std::env::var("ALBUMEN_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            access_key: env_required("ALBUMEN_S3_ACCESS_KEY")?,
            secret_key: env_required("ALBUMEN_S3_SECRET_KEY")?,
            prefix: std::env::var("ALBUMEN_S3_PREFIX").unwrap_or_default(),
        })
    }
}

fn env_required(name: &str) -> Result<String, IngestError> {
    std::env::var(name).map_err(|_| IngestError::store("env", name, "variable is not set"))
}

/// S3-compatible store. Call from a non-async context; it owns a Tokio runtime.
///
/// Original mtime is not S3 `Last-Modified` (that is the upload time). It is
/// stored in user metadata `albumen-mtime-ns` as signed Unix nanoseconds and
/// restored on read. The body is uploaded and downloaded unchanged.
pub struct S3Store {
    client: Client,
    runtime: tokio::runtime::Runtime,
    bucket: String,
    endpoint: String,
    prefix: String,
}

impl std::fmt::Debug for S3Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3Store")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl S3Store {
    pub fn connect(config: S3Config) -> Result<Self, IngestError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|err| IngestError::store("runtime", &config.bucket, err))?;
        let client_config = Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region))
            .endpoint_url(config.endpoint.clone())
            .force_path_style(true)
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                config.access_key,
                config.secret_key,
                None,
                None,
                "albumen-s3",
            )))
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .build();
        let client = Client::from_conf(client_config);
        let store = Self {
            client,
            runtime,
            bucket: config.bucket,
            endpoint: config.endpoint,
            prefix: normalize_prefix(&config.prefix),
        };
        store.block_on(store.ensure_bucket())?;
        Ok(store)
    }

    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    fn key(&self, filename: &str) -> String {
        format!("{}{filename}", self.prefix)
    }

    async fn ensure_bucket(&self) -> Result<(), IngestError> {
        match self
            .client
            .create_bucket()
            .bucket(&self.bucket)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if sdk_already_exists(&err) => Ok(()),
            Err(err) => Err(IngestError::store(
                "create bucket",
                &self.bucket,
                format!("{err}; endpoint {}", self.endpoint),
            )),
        }
    }
}

impl BlobStore for S3Store {
    fn put_verified(&self, source: &Path, filename: &str) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        if !source.is_file() {
            return Err(IngestError::NotAFile(source.to_path_buf()));
        }
        let actual = crate::discover::file_name_str(source)?;
        if actual != filename {
            return Err(IngestError::FilenameMismatch {
                source_name: actual,
                requested: filename.to_string(),
            });
        }
        let mtime = mtime_of(source)?;
        let expected = sha256_file(source)?;
        let expected_hex = to_hex(&expected);
        let mtime_string = to_unix_nanos_string(mtime);
        let key = self.key(filename);
        let len = fs::metadata(source)
            .map_err(|err| IngestError::io(source, err))?
            .len();

        self.block_on(async {
            if self.object_exists(&key).await? {
                return Err(IngestError::AlreadyExists {
                    filename: filename.to_string(),
                    destination: self.destination(&key),
                });
            }
            let body = ByteStream::from_path(source)
                .await
                .map_err(|err| IngestError::store("read", filename, err))?;
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .content_length(i64::try_from(len).unwrap_or(i64::MAX))
                .content_type("application/octet-stream")
                .metadata(MTIME_META, &mtime_string)
                .metadata(SHA_META, &expected_hex)
                .body(body)
                .send()
                .await
                .map_err(|err| IngestError::store("put", filename, err))?;

            let head = self
                .client
                .head_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|err| IngestError::store("head", filename, err))?;
            let meta = head.metadata().cloned().unwrap_or_default();
            if meta.get(MTIME_META) != Some(&mtime_string)
                || meta.get(SHA_META) != Some(&expected_hex)
            {
                self.delete_object(&key).await?;
                return Err(IngestError::MissingObjectMetadata {
                    key: key.clone(),
                    field: "albumen-mtime-ns or albumen-sha256 was not stored".to_string(),
                });
            }

            let downloaded = self.download_to_temp(&key, filename).await?;
            let actual = match sha256_file(&downloaded) {
                Ok(hash) => hash,
                Err(err) => {
                    let _ = fs::remove_file(&downloaded);
                    self.delete_object(&key).await?;
                    return Err(err);
                }
            };
            let _ = fs::remove_file(&downloaded);
            if actual != expected {
                let removed = self.delete_object(&key).await.is_ok();
                return Err(IngestError::checksum_mismatch(
                    filename,
                    expected_hex,
                    to_hex(&actual),
                    self.destination(&key),
                    removed,
                ));
            }
            Ok(VerifiedObject {
                filename: filename.to_string(),
                sha256: expected_hex,
                byte_len: len,
                mtime,
            })
        })
    }

    fn get_verified(&self, filename: &str, dest_dir: &Path) -> Result<VerifiedObject, IngestError> {
        check_filename(filename)?;
        fs::create_dir_all(dest_dir).map_err(|err| IngestError::io(dest_dir, err))?;
        let dest = dest_dir.join(filename);
        if dest.exists() {
            return Err(IngestError::AlreadyExists {
                filename: filename.to_string(),
                destination: dest.display().to_string(),
            });
        }
        let key = self.key(filename);
        self.block_on(async {
            let object = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|err| IngestError::store("get", filename, err))?;
            let meta = object.metadata().cloned().unwrap_or_default();
            let Some(mtime_string) = meta.get(MTIME_META).cloned() else {
                return Err(IngestError::MissingObjectMetadata {
                    key: key.clone(),
                    field: MTIME_META.to_string(),
                });
            };
            let Some(expected_hex) = meta.get(SHA_META).cloned() else {
                return Err(IngestError::MissingObjectMetadata {
                    key,
                    field: SHA_META.to_string(),
                });
            };
            let expected = parse_hex32(&expected_hex, filename)?;
            let mtime = from_unix_nanos_string(&mtime_string, filename)?;

            let mut guard = TempFile {
                path: dest.clone(),
                armed: true,
            };
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dest)
                .map_err(|err| IngestError::io(&dest, err))?;
            let mut body = object.body;
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|err| IngestError::store("get body", filename, err))?;
                std::io::Write::write_all(&mut output, &chunk)
                    .map_err(|err| IngestError::io(&dest, err))?;
            }
            output
                .sync_all()
                .map_err(|err| IngestError::io(&dest, err))?;
            drop(output);

            let record = finalize_verified(&dest, filename, &expected, mtime)?;
            guard.armed = false;
            Ok(record)
        })
    }

    fn delete(&self, filename: &str) -> Result<(), IngestError> {
        check_filename(filename)?;
        let key = self.key(filename);
        self.block_on(self.delete_object(&key))
    }
}

impl S3Store {
    fn destination(&self, key: &str) -> String {
        format!("s3://{}/{}", self.bucket, key)
    }

    async fn object_exists(&self, key: &str) -> Result<bool, IngestError> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err) if sdk_missing(&err) => Ok(false),
            Err(err) => Err(IngestError::store("head", key, err)),
        }
    }

    async fn delete_object(&self, key: &str) -> Result<(), IngestError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| IngestError::store("delete", key, err))?;
        Ok(())
    }

    async fn download_to_temp(&self, key: &str, filename: &str) -> Result<PathBuf, IngestError> {
        let path = temp_path(filename);
        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| IngestError::store("read-back", filename, err))?;
        let mut output = File::create(&path).map_err(|err| IngestError::io(&path, err))?;
        let mut body = object.body;
        let result = async {
            while let Some(chunk) = body.next().await {
                let chunk =
                    chunk.map_err(|err| IngestError::store("read-back body", filename, err))?;
                std::io::Write::write_all(&mut output, &chunk)
                    .map_err(|err| IngestError::io(&path, err))?;
            }
            output
                .sync_all()
                .map_err(|err| IngestError::io(&path, err))?;
            Ok::<(), IngestError>(())
        }
        .await;
        if let Err(err) = result {
            let _ = fs::remove_file(&path);
            return Err(err);
        }
        Ok(path)
    }
}

fn normalize_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

fn sdk_already_exists(err: &impl std::fmt::Debug) -> bool {
    let text = format!("{err:?}");
    text.contains("BucketAlreadyOwnedByYou") || text.contains("BucketAlreadyExists")
}

fn sdk_missing(err: &impl std::fmt::Debug) -> bool {
    let text = format!("{err:?}");
    text.contains("NotFound") || text.contains("NoSuchKey") || text.contains("404")
}

fn parse_hex32(value: &str, filename: &str) -> Result<[u8; 32], IngestError> {
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(IngestError::store(
            "parse sha256",
            filename,
            format!("invalid albumen-sha256 value {value:?}"),
        ));
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).expect("checked hex");
    }
    Ok(out)
}

fn temp_path(filename: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_nanos();
    std::env::temp_dir().join(format!(
        "albumen-s3-{nanos}-{n}-{}-{filename}",
        std::process::id()
    ))
}

struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
