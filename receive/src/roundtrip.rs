// SPDX-License-Identifier: Apache-2.0

use std::fs::{self, OpenOptions};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use albumen_ingest::{
    discover, exiftool_command, export_tree, sha256_hex, DirectoryTree, Role, SourceRoot,
};
use russh::client::{self, Handler};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{listen, Library, Settings};

fn fixtures() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

#[tokio::test]
async fn synthetic_live_photo_survives_the_webdav_receiver() {
    let generated = tempfile::tempdir().unwrap();
    let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/synthetic-live-photo.sh");
    let bash = bash_program();
    let output = std::process::Command::new(&bash)
        .arg(&script)
        .arg(generated.path())
        .output()
        .unwrap_or_else(|err| panic!("run {} {}: {err}", bash.display(), script.display()));
    assert!(
        output.status.success(),
        "synthetic live photo failed via {} (status {:?})\nstdout:\n{}\nstderr:\n{}",
        bash.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let heic = generated.path().join("IMG_LIVE.heic");
    let mov = generated.path().join("IMG_LIVE.mov");
    let heic_bytes = std::fs::read(&heic).unwrap();
    let mov_bytes = std::fs::read(&mov).unwrap();
    let heic_sha = sha256_hex(&heic).unwrap();
    let mov_sha = sha256_hex(&mov).unwrap();

    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
            member: None,
        },
        Some("127.0.0.1:0".parse().unwrap()),
        None,
        None,
    )
    .await
    .unwrap();
    let webdav = listening.webdav.unwrap();
    http_put(webdav, "/IMG_LIVE.heic", &heic_bytes, "201", &heic_sha).await;
    http_put(webdav, "/IMG_LIVE.mov", &mov_bytes, "201", &mov_sha).await;

    let stored_heic = root.path().join("IMG_LIVE.heic");
    let stored_mov = root.path().join("IMG_LIVE.mov");
    assert_eq!(sha256_hex(&stored_heic).unwrap(), heic_sha);
    assert_eq!(sha256_hex(&stored_mov).unwrap(), mov_sha);
    assert_eq!(std::fs::read(&heic).unwrap(), heic_bytes);
    let asset = discover(&stored_heic).unwrap();
    assert_eq!(asset.files().len(), 2);
    assert_eq!(asset.files()[1].role, Role::Pair);
    assert_eq!(asset.files()[1].path.file_name().unwrap(), "IMG_LIVE.mov");
    assert_eq!(
        identifier(&stored_heic, "Apple:ContentIdentifier").as_deref(),
        Some("A1B2C3D4-E5F6-4071-8A9B-C0D1E2F30405")
    );
    assert_eq!(
        identifier(&stored_mov, "Keys:ContentIdentifier").as_deref(),
        Some("A1B2C3D4-E5F6-4071-8A9B-C0D1E2F30405")
    );

    let exported = tempfile::tempdir().unwrap();
    export_tree(
        &SourceRoot::open(root.path()).unwrap(),
        &DirectoryTree::new(exported.path()),
    )
    .unwrap();
    assert_eq!(
        sha256_hex(&exported.path().join("IMG_LIVE.heic")).unwrap(),
        heic_sha
    );
    assert_eq!(
        sha256_hex(&exported.path().join("IMG_LIVE.mov")).unwrap(),
        mov_sha
    );
}

fn bash_program() -> std::path::PathBuf {
    if cfg!(windows) {
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
        ] {
            let path = std::path::PathBuf::from(candidate);
            if path.is_file() {
                return path;
            }
        }
    }
    std::path::PathBuf::from("bash")
}

fn identifier(path: &std::path::Path, key: &str) -> Option<String> {
    let output = exiftool_command()
        .args(["-s3", &format!("-{key}")])
        .arg(path)
        .output()
        .expect("exiftool");
    assert!(output.status.success(), "{}", path.display());
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
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
            member: None,
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
    let other_hash = hash_bytes(b"different");
    http_put(webdav, "/IMG_0003.dng", b"different", "201", &other_hash).await;
    http_put(webdav, "/DCIM/IMG_0003.dng", &raw, "409", &raw_hash).await;
    http_request(webdav, "DELETE", "/IMG_0003.dng", b"", "405").await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        kept
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003__2.dng")).unwrap(),
        b"different"
    );

    clear_library(root.path());

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
    sftp_put(sftp, "IMG_0003.dng", b"nope").await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        raw
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003__2.dng")).unwrap(),
        b"nope"
    );

    clear_library(root.path());

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
    assert!(reply.starts_with("226"), "{reply}");
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003.dng")).unwrap(),
        raw
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_0003__2.dng")).unwrap(),
        b"nope"
    );
    assert_eq!(
        sha256_hex(&fixtures().join("raw-jpeg/IMG_0003.dng")).unwrap(),
        raw_hash
    );
    assert!(!root.path().join("DCIM").exists());
    assert!(root.path().join("IMG_0006.mp4").is_file());
}

#[tokio::test]
async fn same_camera_filename_keeps_both_files_and_an_identical_retry_dedupes() {
    let first = b"camera-roll-a";
    let second = b"camera-roll-b";
    let first_hash = hash_bytes(first);
    let second_hash = hash_bytes(second);
    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
            member: None,
        },
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await
    .unwrap();

    let webdav = listening.webdav.unwrap();
    http_put(
        webdav,
        "/DCIM/100CANON/IMG_9999.JPG",
        first,
        "201",
        &first_hash,
    )
    .await;
    http_put(
        webdav,
        "/DCIM/101CANON/IMG_9999.JPG",
        second,
        "201",
        &second_hash,
    )
    .await;
    http_put(
        webdav,
        "/DCIM/100CANON/IMG_9999.JPG",
        first,
        "409",
        &first_hash,
    )
    .await;
    assert_sidecar(
        root.path(),
        "IMG_9999.JPG",
        "IMG_9999.JPG",
        "DCIM/100CANON",
        &first_hash,
    );
    assert_sidecar(
        root.path(),
        "IMG_9999__2.JPG",
        "IMG_9999.JPG",
        "DCIM/101CANON",
        &second_hash,
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999.JPG")).unwrap(),
        first
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999__2.JPG")).unwrap(),
        second
    );
    assert_eq!(copies_of(root.path(), first), 1);

    clear_library(root.path());
    let sftp = listening.sftp.unwrap();
    sftp_put(sftp, "/DCIM/100CANON/IMG_9999.JPG", first).await;
    sftp_put(sftp, "/DCIM/101CANON/IMG_9999.JPG", second).await;
    sftp_put(sftp, "/DCIM/100CANON/IMG_9999.JPG", first).await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999.JPG")).unwrap(),
        first
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999__2.JPG")).unwrap(),
        second
    );
    assert_eq!(copies_of(root.path(), first), 1);
    assert_sidecar(
        root.path(),
        "IMG_9999.JPG",
        "IMG_9999.JPG",
        "DCIM/100CANON",
        &sha256_hex(&root.path().join("IMG_9999.JPG")).unwrap(),
    );

    clear_library(root.path());
    let ftp = listening.ftp.unwrap();
    ftp_put(ftp, "/DCIM/100CANON/IMG_9999.JPG", first).await;
    ftp_put(ftp, "/DCIM/101CANON/IMG_9999.JPG", second).await;
    ftp_put(ftp, "/DCIM/100CANON/IMG_9999.JPG", first).await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999.JPG")).unwrap(),
        first
    );
    assert_eq!(
        std::fs::read(root.path().join("IMG_9999__2.JPG")).unwrap(),
        second
    );
    assert_eq!(copies_of(root.path(), first), 1);
    assert_eq!(
        sha256_hex(&root.path().join("IMG_9999.JPG")).unwrap(),
        first_hash
    );
    assert_eq!(
        sha256_hex(&root.path().join("IMG_9999__2.JPG")).unwrap(),
        second_hash
    );
    assert_sidecar(
        root.path(),
        "IMG_9999__2.JPG",
        "IMG_9999.JPG",
        "DCIM/101CANON",
        &second_hash,
    );
    assert!(!root.path().join("DCIM").exists());
}

const CAMERA_MTIME: u64 = 1_000_000_000;
const CAMERA_FTP_TIME: &str = "20010909014640";

#[tokio::test]
async fn mtime_round_trips_on_webdav_ftp_and_sftp() {
    let web_body = b"mtime-web";
    let mfmt_body = b"mtime-mfmt";
    let mdtm_body = b"mtime-mdtm";
    let sftp_body = b"mtime-sftp";
    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
            member: None,
        },
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await
    .unwrap();

    let webdav = listening.webdav.unwrap();
    let hash = hash_bytes(web_body);
    http_put_with_mtime(
        webdav,
        "/DCIM/100CANON/IMG_WEB.JPG",
        web_body,
        CAMERA_MTIME,
        "201",
        &hash,
    )
    .await;
    assert_eq!(
        std::fs::read(root.path().join("IMG_WEB.JPG")).unwrap(),
        web_body
    );
    assert_mtime(root.path(), "IMG_WEB.JPG", CAMERA_MTIME);

    let ftp = listening.ftp.unwrap();
    ftp_put_with_time(ftp, "MFMT", "/DCIM/100CANON/IMG_MFMT.JPG", mfmt_body).await;
    let mdtm = ftp_command(ftp, "MDTM IMG_MFMT.JPG").await;
    assert_eq!(mdtm, format!("213 {CAMERA_FTP_TIME}"));
    assert_eq!(
        std::fs::read(root.path().join("IMG_MFMT.JPG")).unwrap(),
        mfmt_body
    );
    assert_mtime(root.path(), "IMG_MFMT.JPG", CAMERA_MTIME);

    ftp_put_with_time(ftp, "MDTM", "IMG_MDTM.JPG", mdtm_body).await;
    let mdtm = ftp_command(ftp, "MDTM IMG_MDTM.JPG").await;
    assert_eq!(mdtm, format!("213 {CAMERA_FTP_TIME}"));
    assert_mtime(root.path(), "IMG_MDTM.JPG", CAMERA_MTIME);

    let sftp = listening.sftp.unwrap();
    let session = sftp_session(sftp).await;
    let mut file = session
        .open_with_flags_and_attributes(
            "/DCIM/100CANON/IMG_SFTP.JPG",
            russh_sftp::protocol::OpenFlags::WRITE,
            russh_sftp::client::fs::Metadata {
                mtime: Some(u32::try_from(CAMERA_MTIME).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    file.write_all(sftp_body).await.unwrap();
    file.close().await.unwrap();
    let remote = session
        .metadata("/DCIM/100CANON/IMG_SFTP.JPG")
        .await
        .unwrap();
    assert_eq!(remote.mtime, Some(u32::try_from(CAMERA_MTIME).unwrap()));
    assert_eq!(
        std::fs::read(root.path().join("IMG_SFTP.JPG")).unwrap(),
        sftp_body
    );
    assert_mtime(root.path(), "IMG_SFTP.JPG", CAMERA_MTIME);
}

const LATER_MTIME: u64 = CAMERA_MTIME + 60;
const LATER_FTP_TIME: &str = "20010909014740";

#[tokio::test]
async fn a_later_mtime_is_appended_and_does_not_replace_the_first() {
    let body = b"mtime-conflict";
    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
            member: None,
        },
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await
    .unwrap();

    let webdav = listening.webdav.unwrap();
    let hash = hash_bytes(body);
    http_put_with_mtime(
        webdav,
        "/DCIM/100CANON/IMG_WEB.JPG",
        body,
        CAMERA_MTIME,
        "201",
        &hash,
    )
    .await;
    http_put_with_mtime(
        webdav,
        "/DCIM/100CANON/IMG_WEB.JPG",
        body,
        LATER_MTIME,
        "409",
        &hash,
    )
    .await;
    assert_mtime(root.path(), "IMG_WEB.JPG", CAMERA_MTIME);
    assert_conflict(root.path(), "IMG_WEB.JPG", "webdav", LATER_MTIME, 1);

    let ftp = listening.ftp.unwrap();
    ftp_put_with_time(
        ftp,
        "MFMT",
        "/DCIM/100CANON/IMG_MFMT.JPG",
        b"mtime-late-mfmt",
    )
    .await;
    assert_mtime(root.path(), "IMG_MFMT.JPG", CAMERA_MTIME);
    let late = ftp_command(ftp, &format!("MFMT {LATER_FTP_TIME} IMG_MFMT.JPG")).await;
    assert!(late.starts_with("213"), "{late}");
    let again = ftp_command(ftp, &format!("MDTM {LATER_FTP_TIME} IMG_MFMT.JPG")).await;
    assert!(again.starts_with("213"), "{again}");
    assert_mtime(root.path(), "IMG_MFMT.JPG", CAMERA_MTIME);
    let read_back = ftp_command(ftp, "MDTM IMG_MFMT.JPG").await;
    assert_eq!(read_back, format!("213 {CAMERA_FTP_TIME}"));
    assert_conflict(root.path(), "IMG_MFMT.JPG", "ftp", LATER_MTIME, 2);

    let sftp = listening.sftp.unwrap();
    let session = sftp_session(sftp).await;
    let mut file = session
        .open_with_flags_and_attributes(
            "/DCIM/100CANON/IMG_SFTP.JPG",
            russh_sftp::protocol::OpenFlags::WRITE,
            russh_sftp::client::fs::Metadata {
                mtime: Some(u32::try_from(CAMERA_MTIME).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    file.write_all(b"mtime-late-sftp").await.unwrap();
    file.close().await.unwrap();
    assert_mtime(root.path(), "IMG_SFTP.JPG", CAMERA_MTIME);
    session
        .set_metadata(
            "/DCIM/100CANON/IMG_SFTP.JPG",
            russh_sftp::client::fs::Metadata {
                mtime: Some(u32::try_from(LATER_MTIME).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut retry = session
        .open_with_flags_and_attributes(
            "/DCIM/100CANON/IMG_SFTP.JPG",
            russh_sftp::protocol::OpenFlags::WRITE,
            russh_sftp::client::fs::Metadata {
                mtime: Some(u32::try_from(LATER_MTIME + 60).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    retry.write_all(b"mtime-late-sftp").await.unwrap();
    retry.close().await.unwrap();
    let remote = session
        .metadata("/DCIM/100CANON/IMG_SFTP.JPG")
        .await
        .unwrap();
    assert_eq!(remote.mtime, Some(u32::try_from(CAMERA_MTIME).unwrap()));
    assert_mtime(root.path(), "IMG_SFTP.JPG", CAMERA_MTIME);
    assert_conflict(root.path(), "IMG_SFTP.JPG", "sftp", LATER_MTIME, 1);
    assert_conflict(root.path(), "IMG_SFTP.JPG", "sftp", LATER_MTIME + 60, 1);
}

fn assert_mtime(root: &std::path::Path, stored_name: &str, seconds: u64) {
    let modified = std::fs::metadata(root.join(stored_name))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(modified, seconds);
    let text = std::fs::read_to_string(root.join(format!("{stored_name}.albumen.json"))).unwrap();
    assert!(
        text.contains(&format!("\"source_mtime\":\"{seconds}\"")),
        "{text}"
    );
    let received = text
        .split("\"received_at\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("received_at missing in {text}"));
    assert_ne!(received, seconds, "{text}");
}

fn assert_conflict(
    root: &std::path::Path,
    stored_name: &str,
    protocol: &str,
    seconds: u64,
    count: usize,
) {
    let text = std::fs::read_to_string(root.join(format!("{stored_name}.albumen.json"))).unwrap();
    assert!(
        text.contains(&format!("\"source_mtime\":\"{CAMERA_MTIME}\"")),
        "{text}"
    );
    let conflicts = text
        .split("\"mtime_conflicts\":")
        .nth(1)
        .unwrap_or_else(|| panic!("mtime_conflicts missing in {text}"));
    let entry = format!("\"protocol\":\"{protocol}\"");
    let value = format!("\"value\":\"{seconds}\"");
    assert!(conflicts.contains(&entry), "{text}");
    assert_eq!(conflicts.matches(&value).count(), count, "{text}");
}

fn assert_sidecar(
    root: &std::path::Path,
    stored_name: &str,
    camera_filename: &str,
    directory: &str,
    sha256: &str,
) {
    let text = std::fs::read_to_string(root.join(format!("{stored_name}.albumen.json"))).unwrap();
    assert!(
        text.contains(&format!("\"camera_filename\":\"{camera_filename}\"")),
        "{text}"
    );
    assert!(
        text.contains(&format!("\"directory\":\"{directory}\"")),
        "{text}"
    );
    assert!(text.contains(&format!("\"sha256\":\"{sha256}\"")), "{text}");
    assert!(
        text.contains(&format!("\"stored_name\":\"{stored_name}\"")),
        "{text}"
    );
}

fn copies_of(root: &std::path::Path, bytes: &[u8]) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| std::fs::read(entry.path()).ok().as_deref() == Some(bytes))
        .count()
}

fn clear_library(root: &std::path::Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::remove_file(path).unwrap();
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob");
    std::fs::write(&path, bytes).unwrap();
    sha256_hex(&path).unwrap()
}

#[tokio::test]
async fn webdav_answers_100_continue_before_the_body() {
    let body = b"synthetic-continue";
    let hash = hash_bytes(body);
    let root = tempfile::tempdir().unwrap();
    let listening = listen(
        Settings {
            library: root.path().to_path_buf(),
            user: "camera".into(),
            password: "roll".into(),
            member: None,
        },
        Some("127.0.0.1:0".parse().unwrap()),
        None,
        None,
    )
    .await
    .unwrap();
    let addr = listening.webdav.unwrap();
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let token = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"camera:roll");
    let request = format!(
        "PUT /continue.jpg HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {token}\r\nExpect: 100-continue\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let interim = tokio::time::timeout(Duration::from_secs(5), read_until_header_end(&mut socket))
        .await
        .expect("timed out waiting for 100 Continue")
        .unwrap();
    assert!(
        interim.starts_with("HTTP/1.1 100"),
        "expected 100 Continue, got {interim}"
    );
    socket.write_all(body).await.unwrap();
    let stored = tokio::time::timeout(Duration::from_secs(5), read_until_close(&mut socket))
        .await
        .expect("timed out waiting for the stored response")
        .unwrap();
    assert!(
        stored.starts_with("HTTP/1.1 201"),
        "PUT continue.jpg: interim={interim} stored={stored}"
    );
    assert_eq!(
        header_value(&stored, "X-Albumen-Sha256").as_deref(),
        Some(hash.as_str()),
        "{stored}"
    );
    assert_eq!(
        std::fs::read(root.path().join("continue.jpg")).unwrap(),
        body
    );
}

async fn read_until_header_end(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 256];
    while buf.len() < 4 || &buf[buf.len() - 4..] != b"\r\n\r\n" {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

async fn read_until_close(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    socket.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

async fn http_put(addr: SocketAddr, path: &str, body: &[u8], status: &str, sha256: &str) {
    let text = http_exchange(addr, "PUT", path, body, None).await;
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

async fn http_put_with_mtime(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    mtime: u64,
    status: &str,
    sha256: &str,
) {
    let text = http_exchange(addr, "PUT", path, body, Some(mtime)).await;
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

async fn http_request(addr: SocketAddr, method: &str, path: &str, body: &[u8], status: &str) {
    let text = http_exchange(addr, method, path, body, None).await;
    assert!(
        text.starts_with(&format!("HTTP/1.1 {status}")),
        "{method} {path}: {text}"
    );
}

async fn http_exchange(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
    mtime: Option<u64>,
) -> String {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let token = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"camera:roll");
    let mtime_header = mtime
        .map(|seconds| format!("X-Albumen-Mtime: {seconds}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {token}\r\n{mtime_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    let mut response = Vec::new();
    // macOS loopback can reset the socket after the response has arrived.
    // A response that already starts is the reply. An empty reset still fails.
    match socket.read_to_end(&mut response).await {
        Ok(_) => {}
        Err(err)
            if err.kind() == std::io::ErrorKind::ConnectionReset
                && response.starts_with(b"HTTP/1.1") => {}
        Err(err) => panic!("read response: {err}"),
    }
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

async fn ftp_put_with_time(addr: SocketAddr, verb: &str, name: &str, body: &[u8]) {
    let mut control = ftp_login(addr).await;
    let timed = ftp_cmd(&mut control, &format!("{verb} {CAMERA_FTP_TIME} {name}")).await;
    assert!(timed.starts_with("213"), "{timed}");
    let pasv = ftp_cmd(&mut control, "PASV").await;
    assert!(pasv.starts_with("227"), "{pasv}");
    let data_addr = parse_pasv(&pasv);
    let early = ftp_cmd(&mut control, &format!("STOR {name}")).await;
    assert!(early.starts_with("150"), "{early}");
    let mut data = TcpStream::connect(data_addr).await.unwrap();
    data.write_all(body).await.unwrap();
    drop(data);
    let done = ftp_read_line(&mut control).await;
    assert!(done.starts_with("226"), "{done}");
    let _ = ftp_cmd(&mut control, "QUIT").await;
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

#[tokio::test]
async fn phone_backup_writes_only_the_joined_member_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let founder = albumen_ingest::family::create_key(&tmp.path().join("founder.key")).unwrap();
    let library = albumen_ingest::family::init_library(&parent, &founder, "ada").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = tmp.path().join("note.txt");
    let sidecar = tmp.path().join("note.xmp");
    stamp_file(&photo, b"synthetic-family-original", mtime);
    stamp_file(&sidecar, b"<x:xmpmeta synthetic/>", mtime);
    let added =
        albumen_ingest::family::add_object(&library, &founder, &photo, Some(&sidecar)).unwrap();

    let phone = albumen_ingest::family::create_key(&tmp.path().join("phone.key")).unwrap();
    let token = albumen_ingest::family::invite(&library, &founder, phone.member_id()).unwrap();
    albumen_ingest::family::join(&library, &phone, &token, "phone").unwrap();
    albumen_ingest::family::admit(&library, &founder, phone.member_id(), &token).unwrap();

    let stranger = albumen_ingest::family::create_key(&tmp.path().join("stranger.key")).unwrap();
    let stranger_err = Library::open_member_namespace(&library, stranger.member_id()).unwrap_err();
    assert!(
        stranger_err.to_string().contains("has not joined"),
        "{stranger_err}"
    );
    let open_err = Library::open(&library).unwrap_err();
    assert!(
        open_err.to_string().contains("family library"),
        "{open_err}"
    );

    let before = snapshot_tree(&library.join("members").join(founder.member_id()));
    assert!(!before.is_empty());

    let listening = listen(
        Settings {
            library: library.clone(),
            user: "camera".into(),
            password: "roll".into(),
            member: Some(phone.member_id().to_string()),
        },
        Some("127.0.0.1:0".parse().unwrap()),
        None,
        None,
    )
    .await
    .unwrap();
    let webdav = listening.webdav.unwrap();
    let body = b"phone-backup-original";
    let phone_file = tmp.path().join("phone.jpg");
    fs::write(&phone_file, body).unwrap();
    let phone_sha = albumen_ingest::sha256_hex(&phone_file).unwrap();
    let member = phone.member_id();
    http_put(
        webdav,
        &format!("/members/{member}/phone.jpg"),
        body,
        "201",
        &phone_sha,
    )
    .await;
    let stored = library.join("members").join(member).join("phone.jpg");
    assert_eq!(fs::read(&stored).unwrap(), body);
    assert_eq!(albumen_ingest::sha256_hex(&stored).unwrap(), phone_sha);

    http_request(
        webdav,
        "PUT",
        &format!("/members/{}/evil.jpg", founder.member_id()),
        b"not-b",
        "403",
    )
    .await;
    http_request(webdav, "PUT", "/evil.jpg", b"not-root", "403").await;
    http_request(
        webdav,
        "PUT",
        &format!(
            "/members/{member}/../../members/{}/evil.jpg",
            founder.member_id()
        ),
        b"climb",
        "400",
    )
    .await;

    let after = snapshot_tree(&library.join("members").join(founder.member_id()));
    assert_eq!(
        before, after,
        "member B originals, mtime, or sidecars changed"
    );
    assert!(added.object_path.is_file());
    assert!(added.sidecar_path.unwrap().is_file());
    assert!(!library
        .join("members")
        .join(founder.member_id())
        .join("evil.jpg")
        .exists());
    assert!(!library.join("evil.jpg").exists());
}

fn stamp_file(path: &Path, bytes: &[u8], mtime: SystemTime) {
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn snapshot_tree(root: &Path) -> Vec<(String, String, SystemTime, u64)> {
    let mut out = Vec::new();
    snapshot_into(root, root, &mut out);
    out.sort();
    out
}

fn snapshot_into(root: &Path, dir: &Path, out: &mut Vec<(String, String, SystemTime, u64)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            snapshot_into(root, &path, out);
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let meta = fs::metadata(&path).unwrap();
        out.push((
            rel,
            albumen_ingest::sha256_hex(&path).unwrap(),
            meta.modified().unwrap(),
            meta.len(),
        ));
    }
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
