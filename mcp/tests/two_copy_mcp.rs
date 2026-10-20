// SPDX-License-Identifier: Apache-2.0

//! Sync is not backup, MCP surface: hide- and remove-style tool calls made
//! against copy A through `albumen-mcp` never delete or change anything on
//! copy B, and A's originals stay bit-exact. The server has no delete,
//! remove, hide or move tool; every such call is refused. Synthetic bytes
//! only; no network.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

type Snapshot = BTreeMap<String, (Vec<u8>, SystemTime)>;

struct Client {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Client {
    fn start(library: &Path, extra: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_albumen-mcp"));
        command
            .arg("--library")
            .arg(library)
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("spawn albumen-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        write_message(
            &mut self.stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        let message = read_message(&mut self.stdout);
        assert_eq!(message["id"], id, "{message}");
        assert!(message.get("error").is_none(), "rpc error: {message}");
        message["result"].clone()
    }

    fn notify(&mut self, method: &str) {
        write_message(
            &mut self.stdin,
            &json!({"jsonrpc": "2.0", "method": method}),
        );
    }

    fn close(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait for albumen-mcp");
        assert!(status.success(), "server exited {status}");
    }
}

fn write_message(out: &mut impl Write, message: &Value) {
    let body = serde_json::to_vec(message).unwrap();
    write!(out, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    out.write_all(&body).unwrap();
    out.flush().unwrap();
}

fn read_message(input: &mut impl BufRead) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line).unwrap();
        assert!(read > 0, "server closed stdout");
        if line == "\n" || line == "\r\n" {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = length.expect("Content-Length");
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn handshake(client: &mut Client) {
    let early = {
        write_message(
            &mut client.stdin,
            &json!({"jsonrpc": "2.0", "id": 0, "method": "tools/list"}),
        );
        read_message(&mut client.stdout)
    };
    assert_eq!(early["error"]["code"], -32600, "{early}");

    let init = client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "albumen-test", "version": "0.0.1"}
        }),
    );
    assert_eq!(init["protocolVersion"], "2024-11-05");
    assert_eq!(init["serverInfo"]["name"], "albumen-mcp");
    client.notify("notifications/initialized");
}

fn tool_names(result: &Value) -> Vec<String> {
    result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

fn tool_body(result: &Value) -> (bool, Value) {
    let is_error = result["isError"].as_bool().unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed = if is_error {
        Value::String(text.to_string())
    } else {
        serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
    };
    (is_error, parsed)
}

fn stamp(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    // 100 ns aligned so NTFS keeps this exact tick.
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_700);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }
    }
}

fn tree(root: &Path) -> Snapshot {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            out.insert(relative, (fs::read(&path).unwrap(), modified));
        }
    }
    out
}

const ORIGINALS: [(&str, &[u8]); 3] = [
    ("DCIM/IMG_0001.jpg", b"synthetic-mcp-original-1"),
    ("DCIM/IMG_0002.jpg", b"synthetic-mcp-original-2"),
    ("DCIM/IMG_0002.xmp", b"<x:xmpmeta synthetic/>"),
];

#[test]
fn mcp_hide_and_remove_calls_on_copy_a_never_touch_copy_b() {
    let work = tempfile::tempdir().unwrap();
    let copy_a = work.path().join("copy-a");
    let copy_b = work.path().join("copy-b");
    for (name, bytes) in ORIGINALS {
        stamp(&copy_a.join(name), bytes);
    }
    copy_tree(&copy_a, &copy_b);
    let a_originals: Snapshot = tree(&copy_a);
    let b_before = tree(&copy_b);
    assert_eq!(b_before, a_originals);

    let mut client = Client::start(&copy_a, &[]);
    handshake(&mut client);
    let names = tool_names(&client.request(2, "tools/list", json!({})));
    for refused in ["delete", "remove", "hide", "tombstone", "unlink", "move"] {
        assert!(!names.iter().any(|name| name == refused), "{names:?}");
    }

    let mut id = 10;
    let b_target = copy_b.join("DCIM/IMG_0001.jpg").display().to_string();
    for tool in [
        "delete",
        "remove",
        "unlink",
        "move",
        "move_verified",
        "hide",
        "tombstone",
        "quarantine",
        "copy_verify_quarantine",
    ] {
        for target in ["DCIM/IMG_0001.jpg", "DCIM/IMG_0002.xmp", b_target.as_str()] {
            id += 1;
            let (is_error, body) = tool_body(&client.request(
                id,
                "tools/call",
                json!({"name": tool, "arguments": {"path": target, "to": b_target}}),
            ));
            assert!(is_error, "{tool} {target} was not refused: {body}");
        }
    }
    // Read tools on A still work and see every original.
    id += 1;
    let (is_error, body) = tool_body(&client.request(
        id,
        "tools/call",
        json!({"name": "browse", "arguments": {"prefix": "DCIM", "limit": 10}}),
    ));
    assert!(!is_error, "{body}");
    assert_eq!(
        body["files"].as_array().unwrap().len(),
        ORIGINALS.len(),
        "{body}"
    );
    client.close();

    // Copy B: same file set, bytes and mtimes.
    assert_eq!(tree(&copy_b), b_before, "copy B changed");
    // Copy A: every original bit-exact with its mtime, and none removed.
    let a_now = tree(&copy_a);
    for (name, (bytes, modified)) in &a_originals {
        let (now_bytes, now_modified) = a_now
            .get(name)
            .unwrap_or_else(|| panic!("copy A lost {name}"));
        assert!(now_bytes == bytes, "copy A {name} bytes changed");
        assert_eq!(now_modified, modified, "copy A {name} mtime changed");
    }
}
