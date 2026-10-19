// SPDX-License-Identifier: Apache-2.0

//! MCP upload refuses a family library and leaves every member's originals.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::family;
use albumen_ingest::sha256_hex;
use albumen_mcp::Library;
use serde_json::json;

fn stamp(path: &Path, bytes: &[u8], mtime: SystemTime) {
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    bytes: Vec<u8>,
    mtime: SystemTime,
    sha256: String,
}

fn kept(path: &Path) -> Kept {
    Kept {
        bytes: fs::read(path).unwrap(),
        mtime: fs::metadata(path).unwrap().modified().unwrap(),
        sha256: sha256_hex(path).unwrap(),
    }
}

struct Planted {
    library: PathBuf,
    object: Kept,
    sidecar: Kept,
}

fn library_with_original(root: &Path) -> Planted {
    let parent = root.join("parent");
    fs::create_dir_all(&parent).unwrap();
    let founder = family::create_key(&root.join("founder.key")).unwrap();
    let library = family::init_library(&parent, &founder, "ada").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = root.join("note.txt");
    let sidecar = root.join("note.xmp");
    stamp(&photo, b"synthetic-family-original", mtime);
    stamp(&sidecar, b"<x:xmpmeta synthetic/>", mtime);
    let added = family::add_object(&library, &founder, &photo, Some(&sidecar)).unwrap();
    let sidecar_path = added.sidecar_path.clone().unwrap();
    Planted {
        library,
        object: kept(&added.object_path),
        sidecar: kept(&sidecar_path),
    }
}

#[test]
fn upload_refuses_a_family_library_and_keeps_originals() {
    let tmp = tempfile::tempdir().unwrap();
    let planted = library_with_original(tmp.path());
    let library = &planted.library;
    let incoming = tmp.path().join("incoming.txt");
    fs::write(&incoming, b"mcp-upload").unwrap();
    let tools = Library::new(library.clone(), true, true, true).unwrap();
    let err = tools
        .call("upload", &json!({"path": incoming.display().to_string()}))
        .unwrap_err();
    assert!(err.contains("family library"), "{err}");
    let dry = Library::new(library.clone(), true, true, false).unwrap();
    let err = dry
        .call("upload", &json!({"path": incoming.display().to_string()}))
        .unwrap_err();
    assert!(err.contains("family library"), "{err}");
    let nested = library.join("members");
    let err = Library::new(nested, false, true, true)
        .unwrap()
        .call("upload", &json!({"path": incoming.display().to_string()}))
        .unwrap_err();
    assert!(err.contains("family library"), "{err}");
    let member = family::load_key(&tmp.path().join("founder.key"))
        .unwrap()
        .member_id()
        .to_string();
    let mut files = Vec::new();
    walk(
        &library.join("members").join(member).join("objects"),
        &mut files,
    );
    assert!(files.iter().any(|path| kept(path) == planted.object));
    assert!(files.iter().any(|path| kept(path) == planted.sidecar));
}

fn walk(path: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}
