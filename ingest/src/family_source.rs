// SPDX-License-Identifier: Apache-2.0

//! A family library opened so this process cannot change it.
//!
//! Every member namespace is a [`crate::source::SourceRoot`]. This type has
//! no create, write, rename, unlink, or utimens method. A destination that
//! resolves inside the library is refused. The contract is
//! [DESIGN_PHILOSOPHY.md](../../DESIGN_PHILOSOPHY.md).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::IngestError;
use crate::source::SourceRoot;

/// The family library and one read-only root per member namespace.
#[derive(Debug, Clone)]
pub struct FamilySource {
    library: SourceRoot,
    members: BTreeMap<String, SourceRoot>,
}

impl FamilySource {
    /// Open `library` and every `members/<member-id>/` directory under it.
    ///
    /// `library` must already contain a family genesis record. Each member
    /// directory is opened with [`SourceRoot`], which is `O_RDONLY`.
    pub fn open(library: &Path) -> Result<Self, IngestError> {
        let root = SourceRoot::open(library)?;
        require_genesis(&root)?;
        let mut members = BTreeMap::new();
        let members_dir = root.canonical().join("members");
        match root.metadata_nofollow(&members_dir) {
            Ok(meta) if meta.is_dir() => {
                let read = root.read_dir(&members_dir)?;
                for entry in read {
                    let entry = entry.map_err(|err| IngestError::io(&members_dir, err))?;
                    let path = entry.path();
                    let meta = root.metadata_nofollow(&path)?;
                    if !meta.is_dir() {
                        continue;
                    }
                    let file_name = entry.file_name();
                    let Some(name) = file_name.to_str() else {
                        return Err(IngestError::NonUtf8Filename { path });
                    };
                    if !is_member_id(name) {
                        continue;
                    }
                    let namespace = SourceRoot::open(&path)?;
                    members.insert(name.to_string(), namespace);
                }
            }
            Ok(_) => {
                return Err(family(format!(
                    "{} is not a member directory",
                    members_dir.display()
                )));
            }
            Err(IngestError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        Ok(Self {
            library: root,
            members,
        })
    }

    pub fn library(&self) -> &SourceRoot {
        &self.library
    }

    pub fn canonical(&self) -> &Path {
        self.library.canonical()
    }

    /// Member public keys, in order. Each one was opened read-only.
    pub fn member_ids(&self) -> impl Iterator<Item = &str> {
        self.members.keys().map(String::as_str)
    }

    /// The read-only root for `member`.
    pub fn member(&self, member: &str) -> Result<&SourceRoot, IngestError> {
        self.members.get(member).ok_or_else(|| {
            family(format!(
                "member {member} has no namespace under {}",
                self.library.canonical().display()
            ))
        })
    }

    /// Refuse `dest` when it is the library or anything inside it.
    pub fn refuse_destination(&self, dest: &Path) -> Result<(), IngestError> {
        self.library.refuse_destination(dest)
    }

    /// Read one file inside a member namespace. `O_RDONLY` only.
    pub fn read_member(&self, member: &str, path: &Path) -> Result<Vec<u8>, IngestError> {
        let root = self.member(member)?;
        let mut file = root.open_read(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| IngestError::io(path, err))?;
        Ok(bytes)
    }
}

fn require_genesis(root: &SourceRoot) -> Result<(), IngestError> {
    let path = root.canonical().join("genesis.json");
    let mut file = root.open_read(&path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| IngestError::io(&path, err))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
        family(format!(
            "family genesis {} is not json: {err}",
            path.display()
        ))
    })?;
    let payload = value
        .get("payload")
        .ok_or_else(|| family(format!("family genesis {} has no payload", path.display())))?;
    let version = payload.get("albumen_family").and_then(Value::as_u64);
    let kind = payload.get("kind").and_then(Value::as_str);
    if version != Some(1) || kind != Some("genesis") {
        return Err(family(format!(
            "{} is not a family library",
            root.canonical().display()
        )));
    }
    Ok(())
}

fn is_member_id(name: &str) -> bool {
    name.len() == 64 && name.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn family(message: impl Into<String>) -> IngestError {
    IngestError::Family {
        message: message.into(),
    }
}

pub(crate) fn family_object(library: &Path, member: &str, sha: &str) -> PathBuf {
    library
        .join("members")
        .join(member)
        .join("objects")
        .join("sha256")
        .join(&sha[..2])
        .join(sha)
}

pub(crate) fn family_sidecar(library: &Path, member: &str, sha: &str) -> PathBuf {
    let object = family_object(library, member, sha);
    let mut name = object.into_os_string();
    name.push(".sidecar");
    PathBuf::from(name)
}
