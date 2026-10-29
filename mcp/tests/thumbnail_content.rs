// SPDX-License-Identifier: Apache-2.0

//! Over stdio, `thumbnail` answers with an MCP image content block
//! `{type: "image", data, mimeType}` and a text block of metadata, not base64
//! inside text.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use base64::Engine;
use serde_json::{json, Value};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/jpeg-gps/IMG_0001.jpg"
);

#[test]
fn thumbnail_is_an_image_content_block() {
    let library = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::copy(FIXTURE, library.path().join("IMG_0001.jpg")).unwrap();
    let original = fs::read(library.path().join("IMG_0001.jpg")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"))
        .arg("--library")
        .arg(library.path())
        .arg("--index-dir")
        .arg(state.path().join("index"))
        .arg("--thumbnail-dir")
        .arg(state.path().join("thumbnails"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
    );
    let reply = ask(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"thumbnail","arguments":{"path":"IMG_0001.jpg","size":128}}}),
    );
    let result = &reply["result"];
    assert_eq!(result["isError"], false, "{reply}");
    let content = result["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["mimeType"], "image/jpeg");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content[0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&bytes[..2], b"\xff\xd8");
    assert_eq!(content[1]["type"], "text");
    let meta: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["path"], "IMG_0001.jpg");
    assert_eq!(meta["width"], 128);
    assert!(
        meta.get("data_base64").is_none(),
        "bytes duplicated in text: {meta}"
    );
    drop(stdin);
    child.wait().unwrap();
    assert_eq!(
        fs::read(library.path().join("IMG_0001.jpg")).unwrap(),
        original
    );
}
