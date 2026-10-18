// SPDX-License-Identifier: Apache-2.0

//! Drives `albumen-mcp` as an MCP client over stdio.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use albumen_ingest::{parse_manifest, verify_tree, S3Config, S3Store};
use serde_json::{json, Value};

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

fn snapshot(path: &Path) -> (Vec<u8>, SystemTime) {
    let meta = fs::metadata(path).unwrap();
    (fs::read(path).unwrap(), meta.modified().unwrap())
}

#[test]
fn read_only_server_has_no_write_or_delete_tool() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let jpeg = snapshot(&fixtures.join("aae/IMG_0008.jpg"));
    let sidecar = snapshot(&fixtures.join("aae/IMG_0008.aae"));
    let mut client = Client::start(&fixtures, &[]);
    handshake(&mut client);
    let names = tool_names(&client.request(2, "tools/list", json!({})));
    assert_eq!(names, vec!["browse".to_string(), "search".to_string()]);
    for tool in client.request(3, "tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
    {
        let blob = tool.to_string();
        assert!(!blob.to_lowercase().contains("delete"));
    }

    let (is_error, body) = tool_body(&client.request(
        4,
        "tools/call",
        json!({"name": "search", "arguments": {"query": "IMG_0008"}}),
    ));
    assert!(!is_error, "{body}");
    let paths = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"aae/IMG_0008.jpg".to_string()), "{paths:?}");
    assert!(paths.contains(&"aae/IMG_0008.aae".to_string()), "{paths:?}");

    let (is_error, body) = tool_body(&client.request(
        5,
        "tools/call",
        json!({"name": "browse", "arguments": {"prefix": "aae", "limit": 10}}),
    ));
    assert!(!is_error, "{body}");
    assert!(body["files"].as_array().unwrap().len() >= 2);

    let dest = tempfile::tempdir().unwrap();
    let export_to = dest.path().join("out");
    let (is_error, body) = tool_body(&client.request(
        6,
        "tools/call",
        json!({"name": "export", "arguments": {"to": export_to.display().to_string()}}),
    ));
    assert!(is_error, "{body}");
    assert!(body.as_str().unwrap().contains("export is disabled"));
    assert!(!export_to.exists());

    let (is_error, body) = tool_body(&client.request(
        7,
        "tools/call",
        json!({"name": "delete", "arguments": {"path": "aae/IMG_0008.jpg"}}),
    ));
    assert!(is_error, "{body}");
    assert!(body.as_str().unwrap().contains("no delete tool"));
    assert_eq!(snapshot(&fixtures.join("aae/IMG_0008.jpg")), jpeg);
    assert_eq!(snapshot(&fixtures.join("aae/IMG_0008.aae")), sidecar);
    client.close();
}

#[test]
fn enabled_upload_and_export_copy_verify_without_touching_sources() {
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    fs::create_dir(library.join("trip")).unwrap();
    fs::write(library.join("trip/IMG_0001.jpg"), b"jpeg-bytes").unwrap();
    fs::write(library.join("trip/IMG_0001.xmp"), b"<xmp/>").unwrap();
    let kept = snapshot(&library.join("trip/IMG_0001.jpg"));

    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("photo.jpg"), b"still").unwrap();
    fs::write(outside.join("photo.xmp"), b"sidecar").unwrap();
    let source_jpeg = snapshot(&outside.join("photo.jpg"));
    let source_xmp = snapshot(&outside.join("photo.xmp"));

    let mut client = Client::start(&library, &["--enable-export", "--enable-upload"]);
    handshake(&mut client);
    let names = tool_names(&client.request(2, "tools/list", json!({})));
    assert_eq!(
        names,
        vec![
            "browse".to_string(),
            "export".to_string(),
            "search".to_string(),
            "upload".to_string()
        ]
    );
    assert!(!names.iter().any(|name| name == "delete"));

    fs::write(library.join("photo.xmp"), b"occupied").unwrap();
    let (is_error, body) = tool_body(&client.request(
        3,
        "tools/call",
        json!({"name": "upload", "arguments": {"path": outside.join("photo.jpg").display().to_string()}}),
    ));
    assert!(is_error, "{body}");
    assert!(!library.join("photo.jpg").exists());
    assert_eq!(fs::read(library.join("photo.xmp")).unwrap(), b"occupied");
    assert_eq!(snapshot(&outside.join("photo.jpg")), source_jpeg);
    assert_eq!(snapshot(&outside.join("photo.xmp")), source_xmp);
    fs::remove_file(library.join("photo.xmp")).unwrap();

    let (is_error, body) = tool_body(&client.request(
        4,
        "tools/call",
        json!({"name": "upload", "arguments": {"path": outside.join("photo.jpg").display().to_string()}}),
    ));
    assert!(!is_error, "{body}");
    let uploaded: Vec<_> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["filename"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        uploaded,
        vec!["photo.jpg".to_string(), "photo.xmp".to_string()]
    );
    assert_eq!(fs::read(library.join("photo.jpg")).unwrap(), b"still");
    assert_eq!(fs::read(library.join("photo.xmp")).unwrap(), b"sidecar");
    assert_eq!(snapshot(&outside.join("photo.jpg")), source_jpeg);
    assert_eq!(snapshot(&outside.join("photo.xmp")), source_xmp);
    assert_eq!(snapshot(&library.join("trip/IMG_0001.jpg")), kept);

    let (is_error, body) = tool_body(&client.request(
        5,
        "tools/call",
        json!({"name": "upload", "arguments": {"path": outside.join("photo.jpg").display().to_string()}}),
    ));
    assert!(is_error, "{body}");
    assert!(body.as_str().unwrap().contains("refusing to overwrite"));
    assert_eq!(fs::read(library.join("photo.jpg")).unwrap(), b"still");
    assert_eq!(snapshot(&outside.join("photo.jpg")), source_jpeg);

    let dest = root.path().join("export");
    let (is_error, body) = tool_body(&client.request(
        6,
        "tools/call",
        json!({"name": "export", "arguments": {"to": dest.display().to_string()}}),
    ));
    assert!(!is_error, "{body}");
    assert_eq!(
        fs::read(dest.join("trip/IMG_0001.jpg")).unwrap(),
        b"jpeg-bytes"
    );
    assert_eq!(fs::read(dest.join("photo.xmp")).unwrap(), b"sidecar");
    assert!(dest.join("ALBUMEN-MANIFEST.txt").is_file());
    assert_eq!(snapshot(&library.join("trip/IMG_0001.jpg")), kept);
    assert_eq!(snapshot(&outside.join("photo.jpg")), source_jpeg);

    let exported_mtime = fs::metadata(dest.join("photo.jpg"))
        .unwrap()
        .modified()
        .unwrap();
    let (is_error, body) = tool_body(&client.request(
        7,
        "tools/call",
        json!({"name": "export", "arguments": {"to": dest.display().to_string()}}),
    ));
    assert!(!is_error, "{body}");
    assert_eq!(
        fs::metadata(dest.join("photo.jpg"))
            .unwrap()
            .modified()
            .unwrap(),
        exported_mtime
    );

    let mut tampered = fs::read(dest.join("photo.jpg")).unwrap();
    tampered[0] ^= 0xff;
    fs::write(dest.join("photo.jpg"), &tampered).unwrap();
    let (is_error, body) = tool_body(&client.request(
        8,
        "tools/call",
        json!({"name": "export", "arguments": {"to": dest.display().to_string()}}),
    ));
    assert!(is_error, "{body}");
    assert!(body.as_str().unwrap().contains("refusing to overwrite"));
    assert_eq!(fs::read(dest.join("photo.jpg")).unwrap(), tampered);
    assert_eq!(snapshot(&library.join("trip/IMG_0001.jpg")), kept);
    client.close();
}

#[test]
fn enabled_export_to_s3_verifies_and_leaves_the_library() {
    if std::env::var("ALBUMEN_S3_ENDPOINT").is_err() {
        panic!("S3 export requires MinIO. Run scripts/with-minio.sh cargo test --locked");
    }
    let bucket = std::env::var("ALBUMEN_S3_BUCKET").expect("ALBUMEN_S3_BUCKET");
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    fs::write(library.join("frame.jpg"), b"frame").unwrap();
    let kept = snapshot(&library.join("frame.jpg"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("mcp-{nanos}-{}", std::process::id());
    let to = format!("s3://{bucket}/{prefix}");
    let manifest = root.path().join("manifest.txt");

    let mut client = Client::start(&library, &["--enable-export"]);
    handshake(&mut client);
    let names = tool_names(&client.request(2, "tools/list", json!({})));
    assert_eq!(
        names,
        vec![
            "browse".to_string(),
            "export".to_string(),
            "search".to_string()
        ]
    );
    let (is_error, body) = tool_body(&client.request(
        3,
        "tools/call",
        json!({
            "name": "export",
            "arguments": {"to": to, "manifest": manifest.display().to_string()}
        }),
    ));
    assert!(!is_error, "{body}");
    assert_eq!(body["files"], 1);
    let entries = parse_manifest(&fs::read_to_string(&manifest).unwrap()).unwrap();
    let mut config = S3Config::from_env().unwrap();
    config.bucket = bucket;
    config.prefix = prefix;
    let store = S3Store::connect(config).unwrap();
    verify_tree(&entries, &store, &[]).unwrap();
    assert_eq!(snapshot(&library.join("frame.jpg")), kept);

    let (is_error, body) = tool_body(&client.request(
        4,
        "tools/call",
        json!({"name": "upload", "arguments": {"path": library.join("frame.jpg").display().to_string()}}),
    ));
    assert!(is_error, "{body}");
    assert!(body.as_str().unwrap().contains("upload is disabled"));
    assert_eq!(snapshot(&library.join("frame.jpg")), kept);
    client.close();
}

#[test]
fn quarantine_is_unreachable_from_the_mcp_server() {
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    fs::create_dir(&library).unwrap();
    let source = library.join("IMG_0001.jpg");
    fs::write(&source, b"jpeg-bytes").unwrap();
    let kept = snapshot(&source);
    let trash = root.path().join("trash");

    for extra in [&[][..], &["--enable-export", "--enable-upload"][..]] {
        let mut client = Client::start(&library, extra);
        handshake(&mut client);
        let names = tool_names(&client.request(2, "tools/list", json!({})));
        assert!(
            !names.iter().any(|name| name.contains("quarantine")),
            "{extra:?} offers {names:?}"
        );
        for (id, tool) in [(3, "quarantine"), (4, "copy_verify_quarantine")] {
            let (is_error, body) = tool_body(&client.request(
                id,
                "tools/call",
                json!({
                    "name": tool,
                    "arguments": {
                        "path": source.display().to_string(),
                        "trash": trash.display().to_string()
                    }
                }),
            ));
            assert!(is_error, "{extra:?} {tool}: {body}");
            assert!(
                body.as_str()
                    .unwrap()
                    .contains("copy_verify_quarantine is not a default path"),
                "{extra:?} {tool}: {body}"
            );
        }
        client.close();
        assert_eq!(snapshot(&source), kept);
        assert!(!trash.exists());
        assert!(source.is_file());
    }
}
