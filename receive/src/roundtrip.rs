// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use albumen_ingest::sha256_hex;
use russh::client::{self, Handler};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{listen, Settings};

fn fixtures() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

#[tokio::test]
async fn webdav_sftp_and_ftp_store_raw_and_video_without_overwrite() {
    let raw = std::fs::read(fixtures().join("raw-jpeg/IMG_0003.dng")).unwrap();
    let video = std::fs::read(fixtures().join("video/IMG_0006.mp4")).unwrap();
    let raw_hash = sha256_hex(&fixtures().join("raw-jpeg/IMG_0003.dng")).unwrap();
    let video_hash = sha256_hex(&fixtures().join("video/IMG_0006.mp4")).unwrap();

    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
        },
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await
    .unwrap();

    let webdav = listening.webdav.unwrap();
    let sftp = listening.sftp.unwrap();
    let ftp = listening.ftp.unwrap();

    http_put(webdav, "/DCIM/IMG_0003.dng", &raw, "201", &raw_hash).await;
    http_put(webdav, "/IMG_0006.mp4", &video, "201", &video_hash).await;
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0003.dng")).unwrap(),
        raw_hash
    );
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0006.mp4")).unwrap(),
        video_hash
    );
    let kept = std::fs::read(root.path().join("IMG_0003.dng")).unwrap();
    http_put(webdav, "/IMG_0003.dng", b"different", "409", &raw_hash).await;
    http_request(webdav, "DELETE", "/IMG_0003.dng", b"", "405").await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        kept
    );

    std::fs::remove_file(root.path().join("IMG_0003.dng")).unwrap();
    std::fs::remove_file(root.path().join("IMG_0006.mp4")).unwrap();

    sftp_put(sftp, "IMG_0003.dng", &raw).await;
    sftp_put(sftp, "folder/IMG_0006.mp4", &video).await;
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0003.dng")).unwrap(),
        raw_hash
    );
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0006.mp4")).unwrap(),
        video_hash
    );
    let again = sftp_put_result(sftp, "IMG_0003.dng", b"nope").await;
    assert!(again.is_err(), "sftp overwrite was accepted");
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        raw
    );

    std::fs::remove_file(root.path().join("IMG_0003.dng")).unwrap();
    std::fs::remove_file(root.path().join("IMG_0006.mp4")).unwrap();

    ftp_put(ftp, "IMG_0003.dng", &raw).await;
    ftp_put(ftp, "/DCIM/IMG_0006.mp4", &video).await;
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0003.dng")).unwrap(),
        raw_hash
    );
    assert_eq!(
        sha256_hex(&root.path().join("IMG_0006.mp4")).unwrap(),
        video_hash
    );
    let reply = ftp_command(ftp, "DELE IMG_0003.dng").await;
    assert!(reply.starts_with("550"), "{reply}");
    let reply = ftp_store_again(ftp, "IMG_0003.dng", b"nope").await;
    assert!(reply.starts_with("550"), "{reply}");
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        raw
    );
    assert_eq!(
        sha256_hex(&fixtures().join("raw-jpeg/IMG_0003.dng")).unwrap(),
        raw_hash
    );
    assert!(!root.path().join("DCIM").exists());
    assert!(root.path().join("IMG_0006.mp4").is_file());
}

async fn http_put(addr: SocketAddr, path: &str, body: &[u8], status: &str, sha256: &str) {
    let text = http_exchange(addr, "PUT", path, body).await;
    assert!(
        text.starts_with(&format!("HTTP/1.1 {status}")),
        "PUT {path}: {text}"
    );
    assert_eq!(
        header_value(&text, "X-Albumen-Sha256").as_deref(),
        Some(sha256),
        "PUT {path}: {text}"
    );
}

fn header_value(response: &str, name: &str) -> Option<String> {
    let head = response.split("\r\n\r\n").next().unwrap_or(response);
    for line in head.split("\r\n").skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

async fn http_request(addr: SocketAddr, method: &str, path: &str, body: &[u8], status: &str) {
    let text = http_exchange(addr, method, path, body).await;
    assert!(
        text.starts_with(&format!("HTTP/1.1 {status}")),
        "{method} {path}: {text}"
    );
}

async fn http_exchange(addr: SocketAddr, method: &str, path: &str, body: &[u8]) -> String {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let token = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"camera:roll");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

struct AcceptKey;

impl Handler for AcceptKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn sftp_session(addr: SocketAddr) -> SftpSession {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let mut handle = client::connect(config, addr, AcceptKey).await.unwrap();
    let auth = handle
        .authenticate_password("camera", "roll")
        .await
        .unwrap();
    assert!(matches!(auth, client::AuthResult::Success), "{auth:?}");
    let channel = handle.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    SftpSession::new(channel.into_stream()).await.unwrap()
}

async fn sftp_put(addr: SocketAddr, path: &str, body: &[u8]) {
    sftp_put_result(addr, path, body).await.unwrap();
}

async fn sftp_put_result(addr: SocketAddr, path: &str, body: &[u8]) -> Result<(), String> {
    let session = sftp_session(addr).await;
    session
        .write(path, body)
        .await
        .map_err(|err| err.to_string())
}

async fn ftp_put(addr: SocketAddr, name: &str, body: &[u8]) {
    let reply = ftp_transfer(addr, name, body, true).await;
    assert!(reply.starts_with("226"), "{reply}");
}

async fn ftp_store_again(addr: SocketAddr, name: &str, body: &[u8]) -> String {
    ftp_transfer(addr, name, body, false).await
}

async fn ftp_command(addr: SocketAddr, command: &str) -> String {
    let mut control = ftp_login(addr).await;
    let reply = ftp_cmd(&mut control, command).await;
    let _ = ftp_cmd(&mut control, "QUIT").await;
    reply
}

async fn ftp_transfer(addr: SocketAddr, name: &str, body: &[u8], expect_data: bool) -> String {
    let mut control = ftp_login(addr).await;
    let pasv = ftp_cmd(&mut control, "PASV").await;
    assert!(pasv.starts_with("227"), "{pasv}");
    let data_addr = parse_pasv(&pasv);
    let early = ftp_cmd(&mut control, &format!("STOR {name}")).await;
    if !early.starts_with("150") {
        return early;
    }
    if expect_data || early.starts_with("150") {
        let mut data = TcpStream::connect(data_addr).await.unwrap();
        data.write_all(body).await.unwrap();
        drop(data);
    }
    let mut reply = String::new();
    let mut buf = [0u8; 256];
    let n = control.read(&mut buf).await.unwrap();
    reply.push_str(&String::from_utf8_lossy(&buf[..n]));
    let _ = ftp_cmd(&mut control, "QUIT").await;
    reply
}

async fn ftp_login(addr: SocketAddr) -> TcpStream {
    let mut control = TcpStream::connect(addr).await.unwrap();
    let greeting = ftp_read_line(&mut control).await;
    assert!(greeting.starts_with("220"), "{greeting}");
    let user = ftp_cmd(&mut control, "USER camera").await;
    assert!(user.starts_with("331"), "{user}");
    let pass = ftp_cmd(&mut control, "PASS roll").await;
    assert!(pass.starts_with("230"), "{pass}");
    control
}

async fn ftp_cmd(control: &mut TcpStream, command: &str) -> String {
    control
        .write_all(format!("{command}\r\n").as_bytes())
        .await
        .unwrap();
    ftp_read_line(control).await
}

async fn ftp_read_line(control: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = control.read(&mut byte).await.unwrap();
        assert_eq!(n, 1);
        buf.push(byte[0]);
        if buf.ends_with(b"\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).trim().to_string()
}

fn parse_pasv(line: &str) -> SocketAddr {
    let start = line.find('(').unwrap();
    let end = line.find(')').unwrap();
    let nums: Vec<u8> = line[start + 1..end]
        .split(',')
        .map(|part| part.trim().parse().unwrap())
        .collect();
    let port = u16::from(nums[4]) * 256 + u16::from(nums[5]);
    SocketAddr::from(([nums[0], nums[1], nums[2], nums[3]], port))
}
