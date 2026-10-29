// SPDX-License-Identifier: Apache-2.0

//! Regression: the stdio transport is newline-delimited JSON-RPC, as in the
//! MCP spec and the official SDK clients. A `Content-Length` header server
//! never answers those clients. This sends the exact bytes an SDK client
//! sends for initialize, initialized, and tools/list.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

#[test]
fn sdk_style_handshake_gets_newline_delimited_replies() {
    let library = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
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
        .expect("spawn albumen-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, replies) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            if lines
                .send(String::from_utf8_lossy(&line).into_owned())
                .is_err()
            {
                break;
            }
        }
    });
    let next = |what: &str| -> Value {
        let line = replies
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no newline-delimited reply to {what}"));
        assert!(!line.starts_with("Content-Length"), "{what}: {line}");
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("{what}: {err}: {line}"))
    };

    stdin
        .write_all(
            b"{\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\
\"capabilities\":{},\"clientInfo\":{\"name\":\"sdk-shape\",\"version\":\"0\"}},\
\"jsonrpc\":\"2.0\",\"id\":0}\n",
        )
        .unwrap();
    stdin.flush().unwrap();
    let init = next("initialize");
    assert_eq!(init["id"], 0, "{init}");
    assert_eq!(
        init["result"]["serverInfo"]["name"], "albumen-mcp",
        "{init}"
    );
    assert!(init["result"]["protocolVersion"].is_string(), "{init}");

    stdin
        .write_all(b"{\"method\":\"notifications/initialized\",\"jsonrpc\":\"2.0\"}\n")
        .unwrap();
    stdin
        .write_all(b"{\"method\":\"tools/list\",\"jsonrpc\":\"2.0\",\"id\":1}\n")
        .unwrap();
    stdin.flush().unwrap();
    let tools = next("tools/list");
    assert_eq!(tools["id"], 1, "{tools}");
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{tools}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"search"), "{names:?}");

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
