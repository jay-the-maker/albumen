// SPDX-License-Identifier: Apache-2.0

//! A family library on storage the members already have.
//!
//! Each member's originals live under `members/<member-id>/` and are written
//! with `copy_verify`. The member id is the hex encoding of an Ed25519 public
//! key. Logs are append-only hash chains. The shared album manifest is the
//! merge of those signed records. Invite and revoke are records, not accounts.
//! A hide is a record in the viewer's own log.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::checksum::{sha256_file, sha256_hex, to_hex};
use crate::dest;
use crate::discover::{check_filename, file_name_str};
use crate::error::IngestError;
use crate::export::{ExportDest, Placed};
use crate::local::copy_verify_reader;
use crate::mtime::{mtime_of, to_unix_nanos_string};
use crate::s3::S3Store;

const FAMILY_VERSION: u64 = 1;
const INVITE_TTL_NS: i64 = 30 * 24 * 60 * 60 * 1_000_000_000;

fn family(message: impl Into<String>) -> IngestError {
    IngestError::Family {
        message: message.into(),
    }
}

fn now_ns() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX) * 1_000_000_000
        + i64::from(duration.subsec_nanos())
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn encode_hex(bytes: &[u8]) -> String {
    to_hex(bytes)
}

fn decode_hex(text: &str) -> Result<Vec<u8>, IngestError> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(family(format!("expected hex, got {text:?}")));
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for index in (0..text.len()).step_by(2) {
        let byte = u8::from_str_radix(&text[index..index + 2], 16)
            .map_err(|_| family(format!("expected hex, got {text:?}")))?;
        out.push(byte);
    }
    Ok(out)
}

fn random_bytes<const N: usize>() -> Result<[u8; N], IngestError> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|err| family(format!("could not read random bytes: {err}")))?;
    Ok(bytes)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut ordered = Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, canonical(child));
                }
            }
            Value::Object(ordered)
        }
        other => other.clone(),
    }
}

fn canonical_bytes(value: &Value) -> Result<Vec<u8>, IngestError> {
    serde_json::to_vec(&canonical(value))
        .map_err(|err| family(format!("could not encode record: {err}")))
}

fn object(entries: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

fn json_str(text: &str) -> Value {
    Value::String(text.to_string())
}

/// Signing key for one member on one device. The secret stays in this process.
pub struct FamilyKey {
    signing: SigningKey,
    member_id: String,
    device_id: String,
}

impl std::fmt::Debug for FamilyKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FamilyKey")
            .field("member_id", &self.member_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl FamilyKey {
    pub fn member_id(&self) -> &str {
        &self.member_id
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    fn sign(&self, payload: &Value) -> Result<Vec<u8>, IngestError> {
        let message = canonical_bytes(payload)?;
        let signature = self.signing.sign(&message);
        let envelope = object(&[
            ("payload", payload.clone()),
            ("signature", json_str(&encode_hex(&signature.to_bytes()))),
        ]);
        canonical_bytes(&envelope)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyFile {
    v: u8,
    member_id: String,
    device_id: String,
    secret_key: String,
}

fn key_from_parts(secret: &[u8; 32], device_id: String) -> FamilyKey {
    let signing = SigningKey::from_bytes(secret);
    let member_id = encode_hex(signing.verifying_key().as_bytes());
    FamilyKey {
        signing,
        member_id,
        device_id,
    }
}

/// Create a new key file outside the library. An existing file is left in place.
pub fn create_key(path: &Path) -> Result<FamilyKey, IngestError> {
    let secret = random_bytes::<32>()?;
    let device = encode_hex(&random_bytes::<16>()?);
    let key = key_from_parts(&secret, device);
    let file = KeyFile {
        v: 1,
        member_id: key.member_id.clone(),
        device_id: key.device_id.clone(),
        secret_key: encode_hex(&secret),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|err| family(format!("could not encode key file: {err}")))?;
    dest::write_private(path, &bytes)?;
    Ok(key)
}

/// Read a key file. The member id must be the public key of the secret.
pub fn load_key(path: &Path) -> Result<FamilyKey, IngestError> {
    let bytes = fs::read(path).map_err(|err| IngestError::io(path, err))?;
    let file: KeyFile = serde_json::from_slice(&bytes)
        .map_err(|err| family(format!("could not read key file {}: {err}", path.display())))?;
    if file.v != 1 {
        return Err(family(format!(
            "key file {} has version {}",
            path.display(),
            file.v
        )));
    }
    let secret = decode_hex(&file.secret_key)?;
    if secret.len() != 32 || file.device_id.len() != 32 {
        return Err(family(format!(
            "key file {} is not a family signing key",
            path.display()
        )));
    }
    let mut secret_bytes = [0u8; 32];
    secret_bytes.copy_from_slice(&secret);
    let key = key_from_parts(&secret_bytes, file.device_id);
    if key.member_id != file.member_id {
        return Err(family(format!(
            "key file {} member id does not match the signing key",
            path.display()
        )));
    }
    Ok(key)
}

fn check_member_id(member: &str) -> Result<(), IngestError> {
    if member.len() != 64 || !member.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(family(format!(
            "member id {member:?} is not a 32-byte public key"
        )));
    }
    let mut bytes = [0u8; 32];
    let decoded = decode_hex(member)?;
    bytes.copy_from_slice(&decoded);
    VerifyingKey::from_bytes(&bytes)
        .map_err(|err| family(format!("member id {member} is not an Ed25519 key: {err}")))?;
    Ok(())
}

/// The hex member id, lowercase, when it is an Ed25519 public key.
pub fn canonical_member_id(member: &str) -> Result<String, IngestError> {
    check_member_id(member)?;
    Ok(member.trim().to_ascii_lowercase())
}

/// `members/<member-id>` for one Ed25519 public key.
pub fn member_namespace_prefix(member: &str) -> Result<String, IngestError> {
    Ok(format!("members/{}", canonical_member_id(member)?))
}

/// A family library whose `members/<member>/devices` log exists.
///
/// Genesis and `family join` both write that log. A key that has not joined
/// has no directory here.
pub fn require_joined_member(library: &Path, member: &str) -> Result<String, IngestError> {
    let member = canonical_member_id(member)?;
    if !looks_like_genesis(&library.join("genesis.json")) {
        return Err(family(format!(
            "{} is not a family library",
            library.display()
        )));
    }
    let devices = library.join("members").join(&member).join("devices");
    if !devices.is_dir() {
        return Err(family(format!(
            "member {member} has not joined this family library"
        )));
    }
    Ok(member)
}

fn verifying_key(member: &str) -> Result<VerifyingKey, IngestError> {
    check_member_id(member)?;
    let decoded = decode_hex(member)?;
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&decoded);
    VerifyingKey::from_bytes(&bytes)
        .map_err(|err| family(format!("member id {member} is not an Ed25519 key: {err}")))
}

#[derive(Debug, Clone)]
struct SignedRecord {
    member: String,
    device: String,
    seq: u64,
    prev: String,
    kind: String,
    body: Value,
    file_sha256: String,
}

#[derive(Debug)]
struct DeviceChain {
    member: String,
    device: String,
    records: Vec<SignedRecord>,
    head_seq: u64,
    head_sha256: String,
    gap: Option<u64>,
}

fn read_envelope(path: &Path) -> Result<(Value, String), IngestError> {
    let raw = fs::read(path).map_err(|err| IngestError::io(path, err))?;
    let value: Value = serde_json::from_slice(&raw).map_err(|err| {
        family(format!(
            "family record {} is not json: {err}",
            path.display()
        ))
    })?;
    let payload = value
        .get("payload")
        .cloned()
        .ok_or_else(|| family(format!("family record {} has no payload", path.display())))?;
    let signature = value
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(|| family(format!("family record {} has no signature", path.display())))?
        .to_string();
    let signature_bytes = decode_hex(&signature)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| {
        family(format!(
            "family signature check failed for {}",
            path.display()
        ))
    })?;
    let member = payload
        .get("member")
        .and_then(Value::as_str)
        .ok_or_else(|| family(format!("family record {} has no member", path.display())))?;
    let key = verifying_key(member)?;
    let message = canonical_bytes(&payload)?;
    if key.verify(&message, &signature).is_err() {
        return Err(family(format!(
            "family signature check failed for {}",
            path.display()
        )));
    }
    let file_sha256 = encode_hex(&sha256_bytes(&raw));
    Ok((
        object(&[
            ("file_sha256", json_str(&file_sha256)),
            ("payload", payload),
        ]),
        file_sha256,
    ))
}

fn payload_of(wrapped: &Value) -> &Value {
    wrapped.get("payload").unwrap_or(wrapped)
}

fn field_str<'a>(payload: &'a Value, field: &str, path: &str) -> Result<&'a str, IngestError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| family(format!("family record {path} is missing {field}")))
}

fn field_u64(payload: &Value, field: &str, path: &str) -> Result<u64, IngestError> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| family(format!("family record {path} is missing {field}")))
}

fn is_hex_id(text: &str, len: usize) -> bool {
    text.len() == len && text.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn genesis_path(library: &Path) -> PathBuf {
    library.join("genesis.json")
}

fn load_genesis(library: &Path) -> Result<(String, String), IngestError> {
    let path = genesis_path(library);
    let raw = fs::read(&path).map_err(|err| IngestError::io(&path, err))?;
    let (wrapped, _) = read_envelope(&path)?;
    let payload = payload_of(&wrapped);
    let display = path.display().to_string();
    if field_u64(payload, "albumen_family", &display)? != FAMILY_VERSION {
        return Err(family(format!("{display} is not a family genesis record")));
    }
    if field_str(payload, "kind", &display)? != "genesis" {
        return Err(family(format!("{display} is not a family genesis record")));
    }
    let founder = field_str(payload, "member", &display)?.to_string();
    let library_id = encode_hex(&sha256_bytes(&raw));
    Ok((library_id, founder))
}

fn looks_like_genesis(path: &Path) -> bool {
    let Ok(raw) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    let Some(payload) = value.get("payload") else {
        return false;
    };
    payload.get("albumen_family").and_then(Value::as_u64) == Some(FAMILY_VERSION)
        && payload.get("kind").and_then(Value::as_str) == Some("genesis")
}

/// Refuse a destination that is a family library or anything inside one.
///
/// Family commands write through the signing member's namespace. Export, import,
/// MCP upload, and the camera receiver do not.
pub fn refuse_shared_write_root(path: &Path) -> Result<(), IngestError> {
    let mut cursor = path.to_path_buf();
    if !cursor.exists() {
        while !cursor.exists() {
            if !cursor.pop() {
                return Ok(());
            }
        }
    }
    if cursor.is_file() && !cursor.pop() {
        return Ok(());
    }
    loop {
        if looks_like_genesis(&cursor.join("genesis.json")) {
            return Err(family(format!(
                "refusing to write inside family library {}; each member's originals stay in their own namespace",
                cursor.display()
            )));
        }
        if !cursor.pop() {
            break;
        }
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    if let Some(parent) = path.parent() {
        dest::create_dir_all(parent)?;
    }
    let mut file = dest::open_new(path)?;
    file.write_all(bytes)
        .map_err(|err| IngestError::io(path, err))?;
    file.sync_all().map_err(|err| IngestError::io(path, err))?;
    drop(file);
    if let Some(parent) = path.parent() {
        dest::sync_dir(parent)?;
    }
    Ok(())
}

fn log_dir(library: &Path, member: &str, device: &str) -> PathBuf {
    library
        .join("members")
        .join(member)
        .join("devices")
        .join(device)
        .join("log")
}

fn list_log_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>, IngestError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(seq_text) = name.strip_suffix(".json") else {
            continue;
        };
        if seq_text.is_empty() || !seq_text.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let Ok(seq) = seq_text.parse::<u64>() else {
            continue;
        };
        files.push((seq, entry.path()));
    }
    files.sort_by_key(|(seq, _)| *seq);
    Ok(files)
}

fn read_device_chain(
    library: &Path,
    member: &str,
    device: &str,
) -> Result<DeviceChain, IngestError> {
    let dir = log_dir(library, member, device);
    let files = list_log_files(&dir)?;
    let mut records = Vec::new();
    let mut prev = String::new();
    let mut gap = None;
    let mut head_seq = 0u64;
    let mut head_sha256 = String::new();
    for (seq, path) in &files {
        let expected = records.len() as u64 + 1;
        let display = path.display().to_string();
        if *seq != expected {
            gap = Some(expected);
            break;
        }
        let (wrapped, file_sha256) = match read_envelope(path) {
            Ok(read) => read,
            Err(_) => break,
        };
        let payload = payload_of(&wrapped);
        if field_str(payload, "member", &display)? != member
            || field_str(payload, "device", &display)? != device
            || field_u64(payload, "seq", &display)? != *seq
            || field_str(payload, "prev", &display)? != prev
        {
            break;
        }
        let kind = field_str(payload, "kind", &display)?.to_string();
        let body = payload
            .get("body")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        records.push(SignedRecord {
            member: member.to_string(),
            device: device.to_string(),
            seq: *seq,
            prev: prev.clone(),
            kind,
            body,
            file_sha256: file_sha256.clone(),
        });
        prev = file_sha256.clone();
        head_seq = *seq;
        head_sha256 = file_sha256;
    }
    Ok(DeviceChain {
        member: member.to_string(),
        device: device.to_string(),
        records,
        head_seq,
        head_sha256,
        gap,
    })
}

fn member_dirs(library: &Path) -> Result<Vec<String>, IngestError> {
    let root = library.join("members");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&root).map_err(|err| IngestError::io(&root, err))? {
        let entry = entry.map_err(|err| IngestError::io(&root, err))?;
        if !entry
            .file_type()
            .map_err(|err| IngestError::io(entry.path(), err))?
            .is_dir()
        {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if is_hex_id(name, 64) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    Ok(ids)
}

fn device_dirs(library: &Path, member: &str) -> Result<Vec<String>, IngestError> {
    let root = library.join("members").join(member).join("devices");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&root).map_err(|err| IngestError::io(&root, err))? {
        let entry = entry.map_err(|err| IngestError::io(&root, err))?;
        if let Some(name) = entry.file_name().to_str() {
            if is_hex_id(name, 32) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    Ok(ids)
}

fn load_chains(library: &Path) -> Result<Vec<DeviceChain>, IngestError> {
    let mut chains = Vec::new();
    for member in member_dirs(library)? {
        for device in device_dirs(library, &member)? {
            chains.push(read_device_chain(library, &member, &device)?);
        }
    }
    Ok(chains)
}

#[derive(Clone, Debug)]
struct Head {
    member: String,
    device: String,
    seq: u64,
    head: String,
}

#[derive(Clone, Debug)]
struct ObjectRec {
    member: String,
    sha256: String,
    filename: String,
    bytes: u64,
    mtime_ns: String,
    sidecar_sha256: Option<String>,
}

#[derive(Clone, Debug)]
struct AlbumAdd {
    id: String,
    album: String,
    sha256: String,
    member: String,
}

struct View {
    library_id: String,
    founder: String,
    names: BTreeMap<String, String>,
    admitted: BTreeSet<String>,
    revoked: BTreeSet<String>,
    objects: Vec<ObjectRec>,
    conflicts: Vec<String>,
    albums: Vec<AlbumAdd>,
    hides: Vec<Value>,
    tombstones: Vec<Value>,
    heads: Vec<Head>,
    gaps: Vec<Value>,
    ignored: Vec<Value>,
    snapshots: Vec<Value>,
}

fn body_str<'a>(body: &'a Value, field: &str) -> Option<&'a str> {
    body.get(field).and_then(Value::as_str)
}

fn within_cutoff(
    cutoffs: &BTreeMap<String, BTreeMap<String, u64>>,
    member: &str,
    device: &str,
    seq: u64,
) -> bool {
    let Some(devices) = cutoffs.get(member) else {
        return true;
    };
    match devices.get(device) {
        Some(max) => seq <= *max,
        None => false,
    }
}

fn build_view(library: &Path) -> Result<View, IngestError> {
    let (library_id, founder) = load_genesis(library)?;
    let chains = load_chains(library)?;
    let mut heads = Vec::new();
    let mut gaps = Vec::new();
    let mut all = Vec::new();
    for chain in &chains {
        if chain.head_seq > 0 {
            heads.push(Head {
                member: chain.member.clone(),
                device: chain.device.clone(),
                seq: chain.head_seq,
                head: chain.head_sha256.clone(),
            });
        }
        if let Some(seq) = chain.gap {
            gaps.push(object(&[
                ("device", json_str(&chain.device)),
                ("member", json_str(&chain.member)),
                ("missing_seq", Value::from(seq)),
            ]));
        }
        all.extend(chain.records.iter().cloned());
    }

    let mut admitters: BTreeMap<String, String> = BTreeMap::new();
    let mut admitted = BTreeSet::new();
    admitted.insert(founder.clone());
    let mut changed = true;
    while changed {
        changed = false;
        for record in &all {
            if record.kind != "admit" || !admitted.contains(&record.member) {
                continue;
            }
            let Some(target) = body_str(&record.body, "member") else {
                continue;
            };
            if admitted.insert(target.to_string()) {
                admitters.insert(target.to_string(), record.member.clone());
                changed = true;
            }
        }
    }

    let mut revoked = BTreeSet::new();
    let mut cutoffs: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for record in &all {
        if record.kind != "revoke" {
            continue;
        }
        let Some(target) = body_str(&record.body, "member") else {
            continue;
        };
        let allowed = record.member == target
            || record.member == founder
            || admitters.get(target) == Some(&record.member);
        if !allowed {
            continue;
        }
        revoked.insert(target.to_string());
        let mut devices = BTreeMap::new();
        if let Some(list) = record.body.get("heads").and_then(Value::as_array) {
            for head in list {
                let Some(device) = head.get("device").and_then(Value::as_str) else {
                    continue;
                };
                let seq = head.get("seq").and_then(Value::as_u64).unwrap_or(0);
                devices.insert(device.to_string(), seq);
            }
        }
        cutoffs.insert(target.to_string(), devices);
    }

    let mut ignored = Vec::new();
    let mut names = BTreeMap::new();
    let mut applied = Vec::new();
    for record in &all {
        if !within_cutoff(&cutoffs, &record.member, &record.device, record.seq) {
            ignored.push(object(&[
                ("device", json_str(&record.device)),
                ("kind", json_str(&record.kind)),
                ("member", json_str(&record.member)),
                ("seq", Value::from(record.seq)),
            ]));
            continue;
        }
        if record.kind == "hello" {
            if let Some(name) = body_str(&record.body, "name") {
                names.insert(record.member.clone(), name.to_string());
            }
        }
        applied.push(record);
    }

    let mut objects_by_sha: BTreeMap<String, Vec<ObjectRec>> = BTreeMap::new();
    let mut seen_object = BTreeSet::new();
    for record in &applied {
        if record.kind != "object" {
            continue;
        }
        if !admitted.contains(&record.member) {
            continue;
        }
        let Some(sha) = body_str(&record.body, "sha256") else {
            continue;
        };
        let Some(filename) = body_str(&record.body, "filename") else {
            continue;
        };
        let Some(bytes) = record.body.get("bytes").and_then(Value::as_u64) else {
            continue;
        };
        let Some(mtime_ns) = body_str(&record.body, "mtime_ns") else {
            continue;
        };
        let sidecar = match record.body.get("sidecar_sha256") {
            Some(Value::String(value)) => Some(value.clone()),
            _ => None,
        };
        let key = format!("{}:{sha}", record.member);
        if !seen_object.insert(key) {
            continue;
        }
        objects_by_sha
            .entry(sha.to_string())
            .or_default()
            .push(ObjectRec {
                member: record.member.clone(),
                sha256: sha.to_string(),
                filename: filename.to_string(),
                bytes,
                mtime_ns: mtime_ns.to_string(),
                sidecar_sha256: sidecar,
            });
    }
    let mut conflicts = Vec::new();
    let mut objects = Vec::new();
    for (sha, group) in objects_by_sha {
        let same_len = group.iter().all(|item| item.bytes == group[0].bytes);
        if same_len {
            objects.extend(group);
        } else {
            conflicts.push(sha);
        }
    }

    let mut adds: BTreeMap<String, AlbumAdd> = BTreeMap::new();
    let mut omits: Vec<Vec<String>> = Vec::new();
    for record in &applied {
        match record.kind.as_str() {
            "album-add" => {
                let Some(id) = body_str(&record.body, "add_id") else {
                    continue;
                };
                let Some(album) = body_str(&record.body, "album") else {
                    continue;
                };
                let Some(sha) = body_str(&record.body, "sha256") else {
                    continue;
                };
                adds.insert(
                    id.to_string(),
                    AlbumAdd {
                        id: id.to_string(),
                        album: album.to_string(),
                        sha256: sha.to_string(),
                        member: record.member.clone(),
                    },
                );
            }
            "album-omit" => {
                let Some(list) = record.body.get("add_ids").and_then(Value::as_array) else {
                    continue;
                };
                let ids = list
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                omits.push(ids);
            }
            _ => {}
        }
    }
    for ids in omits {
        for id in ids {
            adds.remove(&id);
        }
    }
    let mut albums: Vec<_> = adds.into_values().collect();
    albums.sort_by(|left, right| left.id.cmp(&right.id));

    let mut latest_hide: BTreeMap<(String, String, String), bool> = BTreeMap::new();
    for record in &applied {
        if record.kind != "hide" {
            continue;
        }
        let Some(sha) = body_str(&record.body, "sha256") else {
            continue;
        };
        let hidden = record
            .body
            .get("hidden")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        latest_hide.insert(
            (
                record.member.clone(),
                record.device.clone(),
                sha.to_string(),
            ),
            hidden,
        );
    }
    let mut by_viewer: BTreeMap<(String, String), Vec<bool>> = BTreeMap::new();
    for ((viewer, _, sha), hidden) in latest_hide {
        by_viewer.entry((viewer, sha)).or_default().push(hidden);
    }
    let mut hides = Vec::new();
    for ((viewer, sha), flags) in by_viewer {
        let conflict = flags.iter().any(|flag| *flag != flags[0]);
        let hidden = !conflict && flags.iter().all(|flag| *flag);
        hides.push(object(&[
            ("conflict", Value::Bool(conflict)),
            ("hidden", Value::Bool(hidden)),
            ("sha256", json_str(&sha)),
            ("viewer", json_str(&viewer)),
        ]));
    }

    // A tombstone counts only from the member whose object it names. It is
    // a record: the stored original and sidecar stay on every copy.
    let mut tombstoned: BTreeSet<(String, String)> = BTreeSet::new();
    for record in &applied {
        if record.kind != "tombstone" {
            continue;
        }
        let Some(sha) = body_str(&record.body, "sha256") else {
            continue;
        };
        if objects
            .iter()
            .any(|obj| obj.member == record.member && obj.sha256 == sha)
        {
            tombstoned.insert((record.member.clone(), sha.to_string()));
        }
    }
    let tombstones = tombstoned
        .into_iter()
        .map(|(member, sha)| object(&[("member", json_str(&member)), ("sha256", json_str(&sha))]))
        .collect();

    let mut snapshots = Vec::new();
    for record in &applied {
        if record.kind != "snapshot" {
            continue;
        }
        let fresh = snapshot_is_fresh(&record.body, &chains, &record.file_sha256, &record.prev);
        snapshots.push(object(&[
            ("device", json_str(&record.device)),
            ("fresh", Value::Bool(fresh)),
            ("member", json_str(&record.member)),
            ("seq", Value::from(record.seq)),
        ]));
    }

    Ok(View {
        library_id,
        founder,
        names,
        admitted,
        revoked,
        objects,
        conflicts,
        albums,
        hides,
        tombstones,
        heads,
        gaps,
        ignored,
        snapshots,
    })
}

fn snapshot_is_fresh(body: &Value, chains: &[DeviceChain], file_sha256: &str, prev: &str) -> bool {
    let Some(named) = body.get("heads").and_then(Value::as_array) else {
        return false;
    };
    for head in named {
        let Some(member) = head.get("member").and_then(Value::as_str) else {
            return false;
        };
        let Some(device) = head.get("device").and_then(Value::as_str) else {
            return false;
        };
        let Some(sha) = head.get("head").and_then(Value::as_str) else {
            return false;
        };
        let Some(chain) = chains
            .iter()
            .find(|chain| chain.member == member && chain.device == device)
        else {
            return false;
        };
        if chain.head_sha256 == sha {
            continue;
        }
        if chain.head_sha256 == file_sha256 && prev == sha {
            continue;
        }
        return false;
    }
    true
}

fn manifest_value(view: &View) -> Value {
    let mut members = Vec::new();
    let mut ids = BTreeSet::new();
    ids.insert(view.founder.clone());
    ids.extend(view.admitted.iter().cloned());
    ids.extend(view.names.keys().cloned());
    ids.extend(view.revoked.iter().cloned());
    for id in ids {
        members.push(object(&[
            ("admitted", Value::Bool(view.admitted.contains(&id))),
            ("id", json_str(&id)),
            (
                "name",
                json_str(view.names.get(&id).map(String::as_str).unwrap_or("")),
            ),
            ("revoked", Value::Bool(view.revoked.contains(&id))),
        ]));
    }
    let objects: Vec<_> = view
        .objects
        .iter()
        .map(|object_rec| {
            object(&[
                ("bytes", Value::from(object_rec.bytes)),
                ("filename", json_str(&object_rec.filename)),
                ("member", json_str(&object_rec.member)),
                ("mtime_ns", json_str(&object_rec.mtime_ns)),
                ("sha256", json_str(&object_rec.sha256)),
                (
                    "sidecar_sha256",
                    object_rec
                        .sidecar_sha256
                        .as_ref()
                        .map(|value| json_str(value))
                        .unwrap_or(Value::Null),
                ),
            ])
        })
        .collect();
    let albums: Vec<_> = view
        .albums
        .iter()
        .map(|add| {
            object(&[
                ("album", json_str(&add.album)),
                ("id", json_str(&add.id)),
                ("member", json_str(&add.member)),
                ("sha256", json_str(&add.sha256)),
            ])
        })
        .collect();
    let heads: Vec<_> = view
        .heads
        .iter()
        .map(|head| {
            object(&[
                ("device", json_str(&head.device)),
                ("head", json_str(&head.head)),
                ("member", json_str(&head.member)),
                ("seq", Value::from(head.seq)),
            ])
        })
        .collect();
    let body = object(&[
        ("albums", Value::Array(albums)),
        ("albumen_family", Value::from(FAMILY_VERSION)),
        (
            "conflicts",
            Value::Array(view.conflicts.iter().map(|sha| json_str(sha)).collect()),
        ),
        ("founder", json_str(&view.founder)),
        ("gaps", Value::Array(view.gaps.clone())),
        ("heads", Value::Array(heads)),
        ("hides", Value::Array(view.hides.clone())),
        ("ignored_after_revoke", Value::Array(view.ignored.clone())),
        ("library_id", json_str(&view.library_id)),
        ("members", Value::Array(members)),
        ("objects", Value::Array(objects)),
        ("snapshots", Value::Array(view.snapshots.clone())),
    ]);
    // Present only once a tombstone exists, so libraries without one keep
    // the manifest bytes and hash they had.
    let body = if view.tombstones.is_empty() {
        body
    } else {
        let mut body = body;
        if let Value::Object(map) = &mut body {
            map.insert(
                "tombstones".to_string(),
                Value::Array(view.tombstones.clone()),
            );
        }
        body
    };
    let hash = encode_hex(&sha256_bytes(&canonical_bytes(&body).unwrap_or_default()));
    let mut with_hash = body;
    if let Value::Object(map) = &mut with_hash {
        map.insert("manifest_sha256".to_string(), json_str(&hash));
    }
    with_hash
}

/// Merge every verified log into the shared manifest.
/// Why a device's family log ends early, for hide derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogProblem {
    /// This entry can't be read or doesn't verify (bad JSON, bad signature,
    /// wrong member/device/seq/prev).
    Unreadable(PathBuf),
    /// This entry is missing while a later one exists (a hole in the middle).
    Missing(PathBuf),
}

/// Entries that end a device's chain early. The view silently drops such an
/// entry and every later one from that device; sync and export keep that
/// behaviour. Hide derivation calls this so it can fail closed instead (M7,
/// A8). A short tail (missing entries at the END) can't be seen locally and
/// is not reported: it only delays hides, never reverses one.
pub fn unreadable_log_entries(library: &Path) -> Result<Vec<LogProblem>, IngestError> {
    let mut bad = Vec::new();
    for member in member_dirs(library)? {
        for device in device_dirs(library, &member)? {
            let chain = read_device_chain(library, &member, &device)?;
            let dir = log_dir(library, &member, &device);
            if let Some(seq) = chain.gap {
                bad.push(LogProblem::Missing(dir.join(format!("{seq}.json"))));
                continue;
            }
            let files = list_log_files(&dir)?;
            if chain.records.len() < files.len() {
                bad.push(LogProblem::Unreadable(files[chain.records.len()].1.clone()));
            }
        }
    }
    Ok(bad)
}

pub fn manifest_json(library: &Path) -> Result<String, IngestError> {
    let view = build_view(library)?;
    let value = manifest_value(&view);
    serde_json::to_string_pretty(&value)
        .map_err(|err| family(format!("could not encode manifest: {err}")))
}

fn content_heads(library: &Path) -> Result<(String, Value), IngestError> {
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    let hash = value
        .get("manifest_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let heads = value
        .get("heads")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Ok((hash, heads))
}

fn next_link(library: &Path, key: &FamilyKey) -> Result<(u64, String), IngestError> {
    let chain = read_device_chain(library, key.member_id(), key.device_id())?;
    if chain.gap.is_some() {
        return Err(family(format!(
            "family log chain for {} device {} has a gap; refusing to append past it",
            key.member_id(),
            key.device_id()
        )));
    }
    if chain.head_seq == 0 {
        Ok((1, String::new()))
    } else {
        Ok((chain.head_seq + 1, chain.head_sha256))
    }
}

fn member_is_revoked(view_json: &Value, member: &str) -> bool {
    view_json
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(member)
                && entry.get("revoked").and_then(Value::as_bool) == Some(true)
        })
}

fn member_is_admitted(view_json: &Value, member: &str) -> bool {
    view_json
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(member)
                && entry.get("admitted").and_then(Value::as_bool) == Some(true)
        })
}

fn require_writer(library: &Path, key: &FamilyKey, kind: &str) -> Result<(), IngestError> {
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    if member_is_revoked(&value, key.member_id()) {
        return Err(family(format!(
            "member {} is revoked and cannot append; their originals stay in place",
            key.member_id()
        )));
    }
    if kind == "hello" || member_is_admitted(&value, key.member_id()) {
        return Ok(());
    }
    Err(family(format!(
        "member {} is not admitted and cannot append {kind}",
        key.member_id()
    )))
}

fn append_record(
    library: &Path,
    key: &FamilyKey,
    kind: &str,
    body: Value,
) -> Result<PathBuf, IngestError> {
    check_member_id(key.member_id())?;
    let (seq, prev) = next_link(library, key)?;
    let payload = object(&[
        ("body", body),
        ("device", json_str(key.device_id())),
        ("kind", json_str(kind)),
        ("member", json_str(key.member_id())),
        ("prev", json_str(&prev)),
        ("seq", Value::from(seq)),
        ("unix_ns", Value::from(now_ns())),
    ]);
    let bytes = key.sign(&payload)?;
    let path = log_dir(library, key.member_id(), key.device_id()).join(format!("{seq}.json"));
    let member_root = library.join("members").join(key.member_id());
    if !path.starts_with(&member_root) {
        return Err(family(format!(
            "member {} cannot write {}; that namespace belongs to another member",
            key.member_id(),
            path.display()
        )));
    }
    write_new(&path, &bytes)?;
    Ok(path)
}

/// Create `albumen-family/<library-id>/` under `parent` and sign the genesis record.
pub fn init_library(parent: &Path, key: &FamilyKey, name: &str) -> Result<PathBuf, IngestError> {
    check_name(name)?;
    refuse_shared_write_root(parent)?;
    let payload = object(&[
        ("albumen_family", Value::from(FAMILY_VERSION)),
        ("device", json_str(key.device_id())),
        ("kind", json_str("genesis")),
        ("member", json_str(key.member_id())),
        ("placement", json_str("shared-prefix")),
        ("unix_ns", Value::from(now_ns())),
    ]);
    let bytes = key.sign(&payload)?;
    let library_id = encode_hex(&sha256_bytes(&bytes));
    let library = parent.join("albumen-family").join(&library_id);
    let path = genesis_path(&library);
    write_new(&path, &bytes)?;
    append_record(
        &library,
        key,
        "hello",
        object(&[("name", json_str(name)), ("token_hash", json_str(""))]),
    )?;
    Ok(library)
}

/// Refuse a key file that resolves inside the shared library.
pub fn refuse_key_in_library(library: &Path, key_path: &Path) -> Result<(), IngestError> {
    if key_is_inside(library, key_path) {
        return Err(family(format!(
            "refusing to keep signing key {} inside family library {}",
            key_path.display(),
            library.display()
        )));
    }
    Ok(())
}

fn key_is_inside(library: &Path, key_path: &Path) -> bool {
    let Ok(library) = fs::canonicalize(library) else {
        return false;
    };
    let Ok(key_path) = fs::canonicalize(key_path) else {
        return false;
    };
    key_path.starts_with(library)
}

fn check_name(name: &str) -> Result<(), IngestError> {
    if name.is_empty()
        || name.len() > 200
        || name.chars().any(|ch| ch.is_control())
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(family(format!("display name {name:?} is not accepted")));
    }
    Ok(())
}

fn check_sha(sha: &str) -> Result<String, IngestError> {
    if sha.len() != 64 || !sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(family(format!("sha256 {sha:?} is not a digest")));
    }
    Ok(sha.to_ascii_lowercase())
}

fn check_album(name: &str) -> Result<(), IngestError> {
    check_filename(name)?;
    if name.len() > 200 {
        return Err(family(format!("album name {name:?} is too long")));
    }
    Ok(())
}

/// A verified original stored in the caller's namespace.
#[derive(Debug, Clone)]
pub struct AddedObject {
    pub sha256: String,
    pub bytes: u64,
    pub mtime_ns: String,
    pub sidecar_sha256: Option<String>,
    pub object_path: PathBuf,
    pub sidecar_path: Option<PathBuf>,
}

fn refuse_inside_library(library: &Path, source: &Path) -> Result<(), IngestError> {
    let library = fs::canonicalize(library).map_err(|err| IngestError::io(library, err))?;
    let source = fs::canonicalize(source).map_err(|err| IngestError::io(source, err))?;
    if source.starts_with(&library) {
        return Err(family(format!(
            "refusing to read {} as a new original from inside the family library",
            source.display()
        )));
    }
    Ok(())
}

fn object_paths(
    library: &Path,
    member: &str,
    sha: &str,
) -> Result<(PathBuf, PathBuf), IngestError> {
    check_member_id(member)?;
    let sha = check_sha(sha)?;
    let hh = &sha[..2];
    let dir = library
        .join("members")
        .join(member)
        .join("objects")
        .join("sha256")
        .join(hh);
    Ok((dir.join(&sha), dir.join(format!("{sha}.sidecar"))))
}

fn place_verified(source: &Path, dest: &Path) -> Result<(), IngestError> {
    if dest.is_file() {
        let existing = sha256_file(dest)?;
        let incoming = sha256_file(source)?;
        if existing != incoming {
            return Err(IngestError::AlreadyExists {
                filename: file_name_str(dest)?,
                destination: dest.display().to_string(),
            });
        }
        return Ok(());
    }
    let mtime = mtime_of(source)?;
    let mut input = File::open(source).map_err(|err| IngestError::io(source, err))?;
    copy_verify_reader(&mut input, source, dest, mtime)?;
    Ok(())
}

/// Copy an original, and an optional sidecar, into the signing member's namespace.
pub fn add_object(
    library: &Path,
    key: &FamilyKey,
    source: &Path,
    sidecar: Option<&Path>,
) -> Result<AddedObject, IngestError> {
    require_writer(library, key, "object")?;
    refuse_inside_library(library, source)?;
    if let Some(sidecar) = sidecar {
        refuse_inside_library(library, sidecar)?;
    }
    let filename = file_name_str(source)?;
    check_filename(&filename)?;
    let sha256 = sha256_hex(source)?;
    let mtime_ns = to_unix_nanos_string(mtime_of(source)?);
    let bytes = fs::metadata(source)
        .map_err(|err| IngestError::io(source, err))?
        .len();
    let (object_path, sidecar_path) = object_paths(library, key.member_id(), &sha256)?;
    let member_root = library.join("members").join(key.member_id());
    if !object_path.starts_with(&member_root) {
        return Err(family(format!(
            "member {} cannot write {}",
            key.member_id(),
            object_path.display()
        )));
    }
    place_verified(source, &object_path)?;
    let sidecar_sha256 = if let Some(sidecar) = sidecar {
        place_verified(sidecar, &sidecar_path)?;
        Some(sha256_hex(sidecar)?)
    } else {
        None
    };
    let stored_sidecar = sidecar_sha256
        .as_deref()
        .map(json_str)
        .unwrap_or(Value::Null);
    let already = manifest_lists_object(
        library,
        key.member_id(),
        &sha256,
        &filename,
        bytes,
        &mtime_ns,
        &sidecar_sha256,
    )?;
    if !already {
        append_record(
            library,
            key,
            "object",
            object(&[
                ("bytes", Value::from(bytes)),
                ("filename", json_str(&filename)),
                ("mtime_ns", json_str(&mtime_ns)),
                ("sha256", json_str(&sha256)),
                ("sidecar_sha256", stored_sidecar),
            ]),
        )?;
    }
    Ok(AddedObject {
        sha256,
        bytes,
        mtime_ns,
        sidecar_sha256,
        object_path,
        sidecar_path: sidecar.map(|_| sidecar_path),
    })
}

fn manifest_lists_object(
    library: &Path,
    member: &str,
    sha: &str,
    filename: &str,
    bytes: u64,
    mtime_ns: &str,
    sidecar: &Option<String>,
) -> Result<bool, IngestError> {
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    let Some(list) = value.get("objects").and_then(Value::as_array) else {
        return Ok(false);
    };
    Ok(list.iter().any(|entry| {
        entry.get("member").and_then(Value::as_str) == Some(member)
            && entry.get("sha256").and_then(Value::as_str) == Some(sha)
            && entry.get("filename").and_then(Value::as_str) == Some(filename)
            && entry.get("bytes").and_then(Value::as_u64) == Some(bytes)
            && entry.get("mtime_ns").and_then(Value::as_str) == Some(mtime_ns)
            && entry.get("sidecar_sha256").and_then(Value::as_str) == sidecar.as_deref()
    }))
}

/// Sign an invite for one public key. The token is returned once and is not stored.
pub fn invite(library: &Path, key: &FamilyKey, invitee: &str) -> Result<String, IngestError> {
    check_member_id(invitee)?;
    require_writer(library, key, "invite")?;
    let (library_id, _) = load_genesis(library)?;
    let token = random_bytes::<32>()?;
    let token_hex = encode_hex(&token);
    let token_hash = encode_hex(&sha256_bytes(&token));
    append_record(
        library,
        key,
        "invite",
        object(&[
            (
                "expires_unix_ns",
                Value::from(now_ns().saturating_add(INVITE_TTL_NS)),
            ),
            ("library_id", json_str(&library_id)),
            ("member", json_str(invitee)),
            ("token_hash", json_str(&token_hash)),
        ]),
    )?;
    Ok(token_hex)
}

fn token_hash_of(token_hex: &str) -> Result<String, IngestError> {
    let raw = decode_hex(token_hex)?;
    if raw.len() != 32 {
        return Err(family("family invite token was not accepted"));
    }
    Ok(encode_hex(&sha256_bytes(&raw)))
}

fn find_invite<'a>(chains: &'a [DeviceChain], token_hash: &str) -> Option<&'a SignedRecord> {
    let now = now_ns();
    chains
        .iter()
        .flat_map(|chain| chain.records.iter())
        .find(|record| {
            if record.kind != "invite" {
                return false;
            }
            if body_str(&record.body, "token_hash") != Some(token_hash) {
                return false;
            }
            let expires = record
                .body
                .get("expires_unix_ns")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            expires >= now
        })
}

/// Append hello in the invitee's own namespace.
pub fn join(
    library: &Path,
    key: &FamilyKey,
    token_hex: &str,
    name: &str,
) -> Result<(), IngestError> {
    check_name(name)?;
    let token_hash = token_hash_of(token_hex)?;
    let chains = load_chains(library)?;
    let Some(invite) = find_invite(&chains, &token_hash) else {
        return Err(family("family invite token was not accepted"));
    };
    if body_str(&invite.body, "member") != Some(key.member_id()) {
        return Err(family("family invite token was not accepted"));
    }
    let consumed = chains
        .iter()
        .flat_map(|chain| chain.records.iter())
        .any(|record| {
            record.kind == "admit"
                && body_str(&record.body, "token_hash") == Some(token_hash.as_str())
        });
    if consumed {
        return Err(family("family invite token was not accepted"));
    }
    require_writer(library, key, "hello")?;
    append_record(
        library,
        key,
        "hello",
        object(&[
            ("name", json_str(name)),
            ("token_hash", json_str(&token_hash)),
        ]),
    )?;
    Ok(())
}

/// Admit the invitee. The token hash must match an invite this member signed.
pub fn admit(
    library: &Path,
    key: &FamilyKey,
    member: &str,
    token_hex: &str,
) -> Result<(), IngestError> {
    check_member_id(member)?;
    require_writer(library, key, "admit")?;
    let token_hash = token_hash_of(token_hex)?;
    let chains = load_chains(library)?;
    let invite_ok = chains
        .iter()
        .flat_map(|chain| chain.records.iter())
        .any(|record| {
            record.kind == "invite"
                && record.member == key.member_id()
                && body_str(&record.body, "member") == Some(member)
                && body_str(&record.body, "token_hash") == Some(token_hash.as_str())
        });
    if !invite_ok {
        return Err(family("family invite token was not accepted"));
    }
    let hello_ok = chains
        .iter()
        .flat_map(|chain| chain.records.iter())
        .any(|record| {
            record.kind == "hello"
                && record.member == member
                && body_str(&record.body, "token_hash") == Some(token_hash.as_str())
        });
    if !hello_ok {
        return Err(family(format!(
            "member {member} has not joined with this invite"
        )));
    }
    let already = chains
        .iter()
        .flat_map(|chain| chain.records.iter())
        .any(|record| {
            record.kind == "admit"
                && body_str(&record.body, "token_hash") == Some(token_hash.as_str())
        });
    if already {
        return Ok(());
    }
    append_record(
        library,
        key,
        "admit",
        object(&[
            ("member", json_str(member)),
            ("token_hash", json_str(&token_hash)),
        ]),
    )?;
    Ok(())
}

fn heads_value(library: &Path, member: &str) -> Result<Value, IngestError> {
    let mut heads = Vec::new();
    for device in device_dirs(library, member)? {
        let chain = read_device_chain(library, member, &device)?;
        heads.push(object(&[
            ("device", json_str(&device)),
            ("head", json_str(&chain.head_sha256)),
            ("seq", Value::from(chain.head_seq)),
        ]));
    }
    Ok(Value::Array(heads))
}

/// Revoke a member. Their namespace, bytes, mtime, and sidecars stay.
pub fn revoke(library: &Path, key: &FamilyKey, member: &str) -> Result<(), IngestError> {
    check_member_id(member)?;
    require_writer(library, key, "revoke")?;
    let (_, founder) = load_genesis(library)?;
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    let admitter = value
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(member))
        .is_some();
    let allowed = key.member_id() == member || key.member_id() == founder;
    let invited = load_chains(library)?
        .iter()
        .flat_map(|chain| chain.records.iter())
        .any(|record| {
            record.kind == "admit"
                && record.member == key.member_id()
                && body_str(&record.body, "member") == Some(member)
        });
    if !allowed && !invited {
        return Err(family(format!(
            "member {} cannot revoke {member}",
            key.member_id()
        )));
    }
    if !admitter && member != founder && member != key.member_id() {
        return Err(family(format!("member {member} is not in this library")));
    }
    append_record(
        library,
        key,
        "revoke",
        object(&[
            ("heads", heads_value(library, member)?),
            ("member", json_str(member)),
        ]),
    )?;
    Ok(())
}

/// Record a per-viewer hide. The object's bytes are not opened for write.
pub fn hide(
    library: &Path,
    key: &FamilyKey,
    sha256: &str,
    hidden: bool,
) -> Result<(), IngestError> {
    let sha256 = check_sha(sha256)?;
    require_writer(library, key, "hide")?;
    append_record(
        library,
        key,
        "hide",
        object(&[
            ("hidden", Value::Bool(hidden)),
            ("sha256", json_str(&sha256)),
        ]),
    )?;
    Ok(())
}

/// Tombstone one of this member's own objects.
///
/// The removal is a signed record merged through the family manifest. The
/// stored original and sidecar are not opened for write and stay in place,
/// here and on every copy.
pub fn tombstone(library: &Path, key: &FamilyKey, sha256: &str) -> Result<(), IngestError> {
    let sha256 = check_sha(sha256)?;
    require_writer(library, key, "tombstone")?;
    let json = manifest_json(library)?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
    let owns = value
        .get("objects")
        .and_then(Value::as_array)
        .map(|objects| {
            objects.iter().any(|obj| {
                obj.get("member").and_then(Value::as_str) == Some(key.member_id())
                    && obj.get("sha256").and_then(Value::as_str) == Some(sha256.as_str())
            })
        })
        .unwrap_or(false);
    if !owns {
        return Err(family(format!(
            "member {} can tombstone only an object they added; {sha256} stays as it is",
            key.member_id()
        )));
    }
    append_record(
        library,
        key,
        "tombstone",
        object(&[("sha256", json_str(&sha256))]),
    )?;
    Ok(())
}

/// Add a photo to an album by SHA-256. Returns the add id.
pub fn album_add(
    library: &Path,
    key: &FamilyKey,
    album: &str,
    sha256: &str,
) -> Result<String, IngestError> {
    check_album(album)?;
    let sha256 = check_sha(sha256)?;
    require_writer(library, key, "album-add")?;
    let (seq, prev) = next_link(library, key)?;
    let add_id = format!("{}:{}:{seq}", key.member_id(), key.device_id());
    let payload = object(&[
        (
            "body",
            object(&[
                ("add_id", json_str(&add_id)),
                ("album", json_str(album)),
                ("sha256", json_str(&sha256)),
            ]),
        ),
        ("device", json_str(key.device_id())),
        ("kind", json_str("album-add")),
        ("member", json_str(key.member_id())),
        ("prev", json_str(&prev)),
        ("seq", Value::from(seq)),
        ("unix_ns", Value::from(now_ns())),
    ]);
    let bytes = key.sign(&payload)?;
    let path = log_dir(library, key.member_id(), key.device_id()).join(format!("{seq}.json"));
    write_new(&path, &bytes)?;
    Ok(add_id)
}

/// Drop album adds this member has seen. Unnamed concurrent adds stay. Bytes stay.
pub fn album_omit(library: &Path, key: &FamilyKey, add_ids: &[String]) -> Result<(), IngestError> {
    require_writer(library, key, "album-omit")?;
    if add_ids.is_empty() {
        return Err(family("album omit needs the add ids that were seen"));
    }
    for id in add_ids {
        if id.is_empty() || id.len() > 300 || id.chars().any(|ch| ch.is_control()) {
            return Err(family(format!("add id {id:?} is not accepted")));
        }
    }
    append_record(
        library,
        key,
        "album-omit",
        object(&[(
            "add_ids",
            Value::Array(add_ids.iter().map(|id| json_str(id)).collect()),
        )]),
    )?;
    Ok(())
}

/// Append a signed snapshot of the current heads. A moved head makes it stale.
pub fn sign_manifest(library: &Path, key: &FamilyKey) -> Result<String, IngestError> {
    require_writer(library, key, "snapshot")?;
    let (hash, heads) = content_heads(library)?;
    let albums = {
        let json = manifest_json(library)?;
        let value: Value =
            serde_json::from_str(&json).map_err(|err| family(format!("manifest: {err}")))?;
        value
            .get("albums")
            .and_then(Value::as_array)
            .map(|list| {
                Value::Array(
                    list.iter()
                        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                        .map(json_str)
                        .collect(),
                )
            })
            .unwrap_or_else(|| Value::Array(Vec::new()))
    };
    append_record(
        library,
        key,
        "snapshot",
        object(&[
            ("albums", albums),
            ("heads", heads),
            ("manifest_sha256", json_str(&hash)),
        ]),
    )?;
    manifest_json(library)
}

/// Bucket policy the storage owner can attach. Albumen does not apply it.
pub fn s3_member_policy(
    bucket: &str,
    library_prefix: &str,
    member: &str,
) -> Result<String, IngestError> {
    check_member_id(member)?;
    if bucket.is_empty() || bucket.contains('/') || bucket.contains(' ') {
        return Err(family(format!("bucket {bucket:?} is not accepted")));
    }
    let prefix = library_prefix.trim_matches('/');
    let member_key = if prefix.is_empty() {
        format!("members/{member}/*")
    } else {
        format!("{prefix}/members/{member}/*")
    };
    let read_key = if prefix.is_empty() {
        "*".to_string()
    } else {
        format!("{prefix}/*")
    };
    let policy = object(&[
        ("Version", json_str("2012-10-17")),
        (
            "Statement",
            Value::Array(vec![
                object(&[
                    ("Sid", json_str("AlbumenMemberWriteOwnNamespace")),
                    ("Effect", json_str("Allow")),
                    (
                        "Action",
                        Value::Array(vec![json_str("s3:PutObject"), json_str("s3:DeleteObject")]),
                    ),
                    (
                        "Resource",
                        json_str(&format!("arn:aws:s3:::{bucket}/{member_key}")),
                    ),
                ]),
                object(&[
                    ("Sid", json_str("AlbumenMemberReadLibrary")),
                    ("Effect", json_str("Allow")),
                    ("Action", Value::Array(vec![json_str("s3:GetObject")])),
                    (
                        "Resource",
                        json_str(&format!("arn:aws:s3:::{bucket}/{read_key}")),
                    ),
                ]),
            ]),
        ),
    ]);
    serde_json::to_string_pretty(&policy)
        .map_err(|err| family(format!("could not encode policy: {err}")))
}

/// Put one object under `members/<actor>/`. A key outside that prefix is refused
/// before the backend is called.
pub fn put_family_s3(
    store: &S3Store,
    actor: &str,
    relative: &str,
    source: &Path,
) -> Result<Placed, IngestError> {
    check_member_id(actor)?;
    let prefix = format!("members/{actor}/");
    if !relative.starts_with(&prefix) || relative.contains("..") {
        return Err(family(format!(
            "member {actor} cannot put {relative}; the key is outside their namespace"
        )));
    }
    store.place(source, relative)
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), IngestError> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|err| IngestError::io(dir, err))? {
        let entry = entry.map_err(|err| IngestError::io(dir, err))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| IngestError::io(&path, err))?;
        if file_type.is_symlink() {
            return Err(IngestError::Symlink(path));
        }
        if file_type.is_dir() {
            walk_files(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn relative_from(library: &Path, path: &Path) -> Result<String, IngestError> {
    let relative = path.strip_prefix(library).map_err(|_| {
        family(format!(
            "{} is outside {}",
            path.display(),
            library.display()
        ))
    })?;
    let text = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(text)
}

/// Copy the caller's namespace to S3. Other members' prefixes are not written.
pub fn publish_member_s3(library: &Path, actor: &str, store: &S3Store) -> Result<u64, IngestError> {
    check_member_id(actor)?;
    let root = library.join("members").join(actor);
    let mut files = Vec::new();
    walk_files(&root, &mut files)?;
    files.sort();
    for path in &files {
        let relative = relative_from(library, path)?;
        put_family_s3(store, actor, &relative, path)?;
    }
    Ok(files.len() as u64)
}

/// Append a log record even when the member is revoked. Tests use this to show
/// that a later record is ignored and that no object file is required.
#[cfg(test)]
fn append_ignoring_revoke(
    library: &Path,
    key: &FamilyKey,
    kind: &str,
    body: Value,
) -> Result<PathBuf, IngestError> {
    append_record(library, key, kind, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::S3Config;
    use crate::test_support::{write_file, Tmp};
    use std::time::Duration;

    fn fixed_mtime() -> SystemTime {
        UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789)
    }

    fn setup(tmp: &Tmp) -> (PathBuf, FamilyKey, FamilyKey) {
        let parent = tmp.path().join("parent");
        fs::create_dir_all(&parent).unwrap();
        let founder = create_key(&tmp.path().join("a.key")).unwrap();
        let other = create_key(&tmp.path().join("b.key")).unwrap();
        let library = init_library(&parent, &founder, "ada").unwrap();
        let token = invite(&library, &founder, other.member_id()).unwrap();
        join(&library, &other, &token, "bea").unwrap();
        admit(&library, &founder, other.member_id(), &token).unwrap();
        (library, founder, other)
    }

    fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
        let mut out = Vec::new();
        fn walk(path: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>, SystemTime)>) {
            let meta = fs::symlink_metadata(path).unwrap();
            if meta.is_dir() {
                let mut names: Vec<_> = fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                names.sort();
                for child in names {
                    let name = child.file_name().unwrap().to_string_lossy();
                    let next = if rel.is_empty() {
                        name.to_string()
                    } else {
                        format!("{rel}/{name}")
                    };
                    walk(&child, &next, out);
                }
            } else {
                out.push((
                    rel.to_string(),
                    fs::read(path).unwrap(),
                    meta.modified().unwrap(),
                ));
            }
        }
        walk(root, "", &mut out);
        out
    }

    #[test]
    fn member_cannot_change_another_members_original_or_sidecar() {
        let tmp = Tmp::new();
        let (library, founder, other) = setup(&tmp);
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let photo = write_file(
            &src,
            "note.txt",
            b"synthetic-family-original",
            fixed_mtime(),
        );
        let sidecar = write_file(&src, "note.xmp", b"<x:xmpmeta synthetic/>", fixed_mtime());
        let added = add_object(&library, &founder, &photo, Some(&sidecar)).unwrap();
        assert_eq!(
            fs::read(&added.object_path).unwrap(),
            b"synthetic-family-original"
        );
        assert_eq!(sha256_hex(&added.object_path).unwrap(), added.sha256);
        let before = snapshot_tree(&library.join("members").join(founder.member_id()));

        hide(&library, &other, &added.sha256, true).unwrap();
        let add_id = album_add(&library, &other, "trip", &added.sha256).unwrap();
        album_omit(&library, &other, &[add_id]).unwrap();
        let revoke_err = revoke(&library, &other, founder.member_id()).unwrap_err();
        assert!(revoke_err.to_string().contains("cannot revoke"));
        let other_src = write_file(&src, "other.txt", b"bea-bytes", fixed_mtime());
        add_object(&library, &other, &other_src, None).unwrap();

        let after = snapshot_tree(&library.join("members").join(founder.member_id()));
        assert_eq!(before, after);
        assert_eq!(sha256_hex(&added.object_path).unwrap(), added.sha256);
        let manifest: Value = serde_json::from_str(&manifest_json(&library).unwrap()).unwrap();
        let hides = manifest.get("hides").unwrap().as_array().unwrap();
        assert!(hides.iter().any(|hide| {
            hide.get("viewer").and_then(Value::as_str) == Some(other.member_id())
                && hide.get("sha256").and_then(Value::as_str) == Some(added.sha256.as_str())
                && hide.get("hidden").and_then(Value::as_bool) == Some(true)
        }));
        assert!(manifest
            .get("objects")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|object| {
                object.get("member").and_then(Value::as_str) == Some(founder.member_id())
                    && object.get("sha256").and_then(Value::as_str) == Some(added.sha256.as_str())
                    && object
                        .get("sidecar_sha256")
                        .and_then(Value::as_str)
                        .is_some()
            }));
    }

    #[test]
    fn revoke_leaves_bytes_and_ignores_a_later_record() {
        let tmp = Tmp::new();
        let (library, founder, other) = setup(&tmp);
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let photo = write_file(&src, "keep.txt", b"kept-original", fixed_mtime());
        let added = add_object(&library, &other, &photo, None).unwrap();
        let before = snapshot_tree(&library.join("members").join(other.member_id()));
        revoke(&library, &founder, other.member_id()).unwrap();
        let after_revoke = snapshot_tree(&library.join("members").join(other.member_id()));
        assert_eq!(before, after_revoke);
        let err = add_object(
            &library,
            &other,
            &write_file(&src, "new.txt", b"nope", fixed_mtime()),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("revoked"));
        assert_eq!(
            snapshot_tree(&library.join("members").join(other.member_id())),
            before
        );
        let ghost = "ab".repeat(32);
        append_ignoring_revoke(
            &library,
            &other,
            "object",
            object(&[
                ("bytes", Value::from(4)),
                ("filename", json_str("ghost.txt")),
                ("mtime_ns", json_str("1")),
                ("sha256", json_str(&ghost)),
                ("sidecar_sha256", Value::Null),
            ]),
        )
        .unwrap();
        let manifest: Value = serde_json::from_str(&manifest_json(&library).unwrap()).unwrap();
        let ignored = manifest
            .get("ignored_after_revoke")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(ignored.iter().any(|entry| {
            entry.get("member").and_then(Value::as_str) == Some(other.member_id())
                && entry.get("kind").and_then(Value::as_str) == Some("object")
        }));
        assert!(!manifest
            .get("objects")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|object| {
                object.get("sha256").and_then(Value::as_str) == Some(ghost.as_str())
            }));
        assert!(manifest
            .get("objects")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|object| {
                object.get("sha256").and_then(Value::as_str) == Some(added.sha256.as_str())
            }));
        assert_eq!(fs::read(&added.object_path).unwrap(), b"kept-original");
    }

    #[test]
    fn album_omit_drops_only_the_named_add() {
        let tmp = Tmp::new();
        let (library, founder, other) = setup(&tmp);
        let seen = album_add(&library, &founder, "trip", &"aa".repeat(32)).unwrap();
        let concurrent = album_add(&library, &other, "trip", &"bb".repeat(32)).unwrap();
        album_omit(&library, &founder, std::slice::from_ref(&seen)).unwrap();
        let manifest: Value = serde_json::from_str(&manifest_json(&library).unwrap()).unwrap();
        let ids: Vec<_> = manifest
            .get("albums")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .collect();
        assert!(!ids.contains(&seen.as_str()));
        assert!(ids.contains(&concurrent.as_str()));
    }

    #[test]
    fn tampered_record_stops_the_chain_and_keeps_earlier_bytes() {
        let tmp = Tmp::new();
        let (library, founder, _) = setup(&tmp);
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let photo = write_file(&src, "keep.txt", b"still-here", fixed_mtime());
        let added = add_object(&library, &founder, &photo, None).unwrap();
        hide(&library, &founder, &added.sha256, true).unwrap();
        let log = log_dir(&library, founder.member_id(), founder.device_id());
        let files = list_log_files(&log).unwrap();
        let last = files.last().unwrap().1.clone();
        let mut bytes = fs::read(&last).unwrap();
        let flip = bytes.len() - 1;
        bytes[flip] ^= 0x01;
        fs::write(&last, &bytes).unwrap();
        let manifest: Value = serde_json::from_str(&manifest_json(&library).unwrap()).unwrap();
        assert!(manifest
            .get("objects")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|object| {
                object.get("sha256").and_then(Value::as_str) == Some(added.sha256.as_str())
            }));
        assert!(!manifest
            .get("hides")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|hide| {
                hide.get("sha256").and_then(Value::as_str) == Some(added.sha256.as_str())
            }));
        assert_eq!(fs::read(&added.object_path).unwrap(), b"still-here");
    }

    #[test]
    fn snapshot_is_fresh_until_another_head_moves() {
        let tmp = Tmp::new();
        let (library, founder, other) = setup(&tmp);
        let signed = sign_manifest(&library, &founder).unwrap();
        let manifest: Value = serde_json::from_str(&signed).unwrap();
        assert!(manifest
            .get("snapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|snap| {
                snap.get("member").and_then(Value::as_str) == Some(founder.member_id())
                    && snap.get("fresh").and_then(Value::as_bool) == Some(true)
            }));
        hide(&library, &other, &"cd".repeat(32), true).unwrap();
        let later: Value = serde_json::from_str(&manifest_json(&library).unwrap()).unwrap();
        assert!(later
            .get("snapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|snap| {
                snap.get("member").and_then(Value::as_str) == Some(founder.member_id())
                    && snap.get("fresh").and_then(Value::as_bool) == Some(false)
            }));
    }

    #[test]
    fn refuse_write_root_and_key_mode() {
        let tmp = Tmp::new();
        let (library, _, _) = setup(&tmp);
        let err = refuse_shared_write_root(&library).unwrap_err();
        assert!(err.to_string().contains("family library"));
        let err = refuse_shared_write_root(&library.join("members")).unwrap_err();
        assert!(err.to_string().contains("family library"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mode = fs::metadata(tmp.path().join("a.key")).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn s3_policy_names_only_the_caller_prefix_for_writes() {
        let tmp = Tmp::new();
        let member = create_key(&tmp.path().join("policy.key"))
            .unwrap()
            .member_id()
            .to_string();
        let policy = s3_member_policy("albumen-ingest-test", "family-prefix", &member).unwrap();
        assert!(policy.contains(&format!("members/{member}/*")));
        assert!(policy.contains("s3:PutObject"));
        assert!(policy.contains("s3:GetObject"));
        assert!(!policy.contains("members/ffff"));
    }

    #[test]
    fn s3_namespace_refuses_another_member_and_a_different_body() {
        if std::env::var("ALBUMEN_S3_UNAVAILABLE").ok().as_deref() == Some("1") {
            eprintln!(
                "skipping S3: ALBUMEN_S3_UNAVAILABLE=1. Docker is not used. Local filesystem only."
            );
            return;
        }
        if std::env::var("ALBUMEN_S3_ENDPOINT").is_err() {
            eprintln!("skipping S3: ALBUMEN_S3_ENDPOINT is not set");
            return;
        }
        let tmp = Tmp::new();
        let (library, founder, other) = setup(&tmp);
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let photo = write_file(&src, "note.txt", b"s3-original", fixed_mtime());
        let added = add_object(&library, &founder, &photo, None).unwrap();
        let mut config = S3Config::from_env().unwrap();
        config.prefix = format!("family-{}-{}", std::process::id(), now_ns());
        let store = S3Store::connect(config).unwrap();
        let count = publish_member_s3(&library, founder.member_id(), &store).unwrap();
        assert!(count >= 1);
        let relative = relative_from(&library, &added.object_path).unwrap();
        let first = store.read_hash(&relative).unwrap().unwrap();
        let err = put_family_s3(&store, other.member_id(), &relative, &photo).unwrap_err();
        assert!(err.to_string().contains("outside their namespace"));
        assert_eq!(store.read_hash(&relative).unwrap().unwrap(), first);
        let different = write_file(&src, &added.sha256, b"tampered-s3-body", fixed_mtime());
        let err = put_family_s3(&store, founder.member_id(), &relative, &different).unwrap_err();
        assert!(matches!(err, IngestError::AlreadyExists { .. }));
        assert_eq!(store.read_hash(&relative).unwrap().unwrap().0, first.0);
        publish_member_s3(&library, other.member_id(), &store).unwrap();
        assert_eq!(store.read_hash(&relative).unwrap().unwrap(), first);
    }
}
