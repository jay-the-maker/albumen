// SPDX-License-Identifier: Apache-2.0

//! MCP stdio transport conformance, spec 2026-07-28 (current) and 2025-06-18:
//! https://modelcontextprotocol.io/specification/latest/basic/transports/stdio
//! "Messages are delimited by newlines, and MUST NOT contain embedded
//! newlines." and "The server MUST NOT write anything to its stdout that is
//! not a valid MCP message." This speaks exactly that framing: initialize,
//! notifications/initialized, tools/list, and one read-only tools/call
//! (browse) against a synthetic library, then checks that every stdout byte
//! is one JSON-RPC message per line and that the library is unchanged.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        out.insert(
            entry.file_name().to_string_lossy().into_owned(),
            (fs::read(entry.path()).unwrap(), meta.modified().unwrap()),
        );
    }
    out
}

#[test]
fn spec_stdio_handshake_list_and_read_only_call() {
    let library = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(library.path().join("2026-10-01 a.jpg"), b"synthetic a").unwrap();
    fs::write(library.path().join("2026-10-02 b.jpg"), b"synthetic b").unwrap();
    let before = snapshot(library.path());

    let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"))
        .arg("--library")
        .arg(library.path())
        .arg("--index-dir")
        .arg(state.path().join("index"))
        .arg("--thumbnail-dir")
        .arg(state.path().join("thumbnails"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn albumen-mcp");
    let mut stdin = child.stdin.take().unwrap();
    // Each client message is one line of compact JSON, as the spec requires.
    for msg in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"spec-stdio","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browse","arguments":{}}}"#,
    ] {
        assert!(!msg.contains('\n'));
        stdin.write_all(msg.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    stdin.flush().unwrap();
    drop(stdin); // spec shutdown: close stdin, server exits
    let mut raw = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut raw).unwrap();
    assert!(child.wait().unwrap().success());

    let text = String::from_utf8(raw).expect("stdout is UTF-8");
    assert!(text.ends_with('\n'), "last message not newline-terminated");
    let replies: Vec<Value> = text
        .lines()
        .map(|line| {
            assert!(!line.is_empty() && !line.contains('\r'), "{line:?}");
            let v: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout line is not one JSON-RPC message: {e}: {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "{line}");
            v
        })
        .collect();
    // Exactly one reply per request; nothing for the notification.
    assert_eq!(replies.len(), 3, "{text}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "albumen-mcp");
    assert_eq!(replies[1]["id"], 2);
    let names: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"browse"), "{names:?}");
    assert_eq!(replies[2]["id"], 3);
    assert!(replies[2].get("error").is_none(), "{}", replies[2]);
    let call = replies[2]["result"].to_string();
    assert!(
        call.contains("2026-10-01 a.jpg") && call.contains("2026-10-02 b.jpg"),
        "{call}"
    );

    assert_eq!(snapshot(library.path()), before, "library changed");
}
