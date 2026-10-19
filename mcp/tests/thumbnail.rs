// SPDX-License-Identifier: Apache-2.0

//! The thumbnail tool decodes a library JPEG, returns a smaller JPEG, caches
//! it outside the library, and leaves the original byte-for-byte unchanged.

use std::fs;
use std::path::Path;

use albumen_mcp::Library;
use base64::Engine;
use serde_json::json;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/jpeg-gps/IMG_0001.jpg"
);

fn open(root: &Path, cache: &Path) -> Library {
    Library::new(root.to_path_buf(), false, false, false, None)
        .unwrap()
        .with_index_dir(None)
        .with_thumbnail_dir(Some(cache.to_path_buf()))
}

fn listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.display().to_string(), fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_thumbnail_is_a_smaller_jpeg_cached_outside_the_library() {
    let library_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let root = library_dir.path();
    fs::create_dir_all(root.join("2020/01")).unwrap();
    fs::copy(FIXTURE, root.join("2020/01/IMG_0001.jpg")).unwrap();
    fs::write(root.join("2020/01/stub.jpg"), b"\xff\xd8\xff\xd9").unwrap();
    let before = listing(root);

    let library = open(root, cache_dir.path());
    let first = library
        .call(
            "thumbnail",
            &json!({"path": "2020/01/IMG_0001.jpg", "size": 128}),
        )
        .unwrap();
    assert_eq!(first["cached"], false);
    assert_eq!(first["mime_type"], "image/jpeg");
    assert_eq!(first["width"], 128);
    assert_eq!(first["height"], 96);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(first["data_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(&bytes[..2], b"\xff\xd8");
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (128, 96));

    let second = library
        .call(
            "thumbnail",
            &json!({"path": "2020/01/IMG_0001.jpg", "size": 128}),
        )
        .unwrap();
    assert_eq!(second["cached"], true);
    assert_eq!(second["data_base64"], first["data_base64"]);

    let err = library
        .call("thumbnail", &json!({"path": "2020/01/stub.jpg"}))
        .unwrap_err();
    assert!(err.contains("no thumbnail"), "{err}");
    let err = library
        .call("thumbnail", &json!({"path": "../outside.jpg"}))
        .unwrap_err();
    assert!(err.contains("relative"), "{err}");

    assert_eq!(listing(root), before, "the library changed");
    assert!(!listing(cache_dir.path()).is_empty(), "nothing was cached");

    let inside = root.join("cache");
    let refused = open(root, &inside);
    let err = refused
        .call("thumbnail", &json!({"path": "2020/01/IMG_0001.jpg"}))
        .unwrap_err();
    assert!(err.contains("inside the library"), "{err}");
    assert!(!inside.exists());
    assert_eq!(listing(root), before);
}
