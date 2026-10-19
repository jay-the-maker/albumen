// SPDX-License-Identifier: Apache-2.0

//! WebDAV for cameras and card writers. PUT commits through `copy_verify`.
//! A 201 response carries `X-Albumen-Sha256`, the digest `copy_verify` checked,
//! and `X-Albumen-Verify-Ms`, the milliseconds of that re-read.
//! A different file with the same camera filename is stored under another name.
//! A 409 means those exact bytes are already stored. DELETE and MOVE are refused.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::pairing::PairingToken;

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

use crate::store::{Arrival, Commit, Library, Spool};

#[derive(Clone)]
pub struct Auth {
    pub user: String,
    pub password: String,
    /// One-time token from the pairing QR. The camera password stays valid.
    pub pairing_token: Option<Arc<PairingToken>>,
}

pub async fn spawn(
    addr: SocketAddr,
    library: Arc<Library>,
    auth: Arc<Auth>,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let library = Arc::clone(&library);
                    let auth = Arc::clone(&auth);
                    tokio::spawn(async move {
                        if let Err(err) = handle(socket, library, auth).await {
                            eprintln!("albumen-receive webdav: {err}");
                        }
                    });
                }
                Err(err) => {
                    eprintln!("albumen-receive webdav accept: {err}");
                    break;
                }
            }
        }
    });
    Ok(bound)
}

pub async fn spawn_tls(
    addr: SocketAddr,
    library: Arc<Library>,
    auth: Arc<Auth>,
    acceptor: tokio_rustls::TlsAcceptor,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let library = Arc::clone(&library);
                    let auth = Arc::clone(&auth);
                    let acceptor = acceptor.clone();
                    tokio::spawn(async move {
                        match acceptor.accept(socket).await {
                            Ok(tls) => {
                                if let Err(err) = handle(tls, library, auth).await {
                                    eprintln!("albumen-receive webdav: {err}");
                                }
                            }
                            Err(err) => {
                                eprintln!("albumen-receive webdav tls: {err}");
                            }
                        }
                    });
                }
                Err(err) => {
                    eprintln!("albumen-receive webdav accept: {err}");
                    break;
                }
            }
        }
    });
    Ok(bound)
}

async fn handle(
    mut socket: impl Stream,
    library: Arc<Library>,
    auth: Arc<Auth>,
) -> std::io::Result<()> {
    let mut header = Vec::new();
    let mut buf = [0u8; 1024];
    let header_end = loop {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        header.extend_from_slice(&buf[..n]);
        if header.len() > max_header_bytes() {
            return write_status(&mut socket, 431, "headers too large", &[]).await;
        }
        if let Some(index) = find_header_end(&header) {
            break index;
        }
    };
    let head = String::from_utf8_lossy(&header[..header_end]).into_owned();
    let leftover = header[header_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let Some(request) = lines.next() else {
        return Ok(());
    };
    let mut parts = request.split(' ');
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let mut content_length = None;
    let mut authorization = None;
    let mut source_mtime = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<u64>().ok();
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("x-albumen-mtime") {
                source_mtime = value.trim().parse::<u64>().ok();
            }
        }
    }
    if !authorized(authorization.as_deref(), &auth) {
        return write_status(
            &mut socket,
            401,
            "Unauthorized",
            &[("WWW-Authenticate", "Basic realm=\"albumen\"")],
        )
        .await;
    }
    let length = content_length.unwrap_or(0);
    // URLSession waits for this before it sends the body. A client that already
    // sent the bytes is left alone.
    if method == "PUT" && expects_continue(&head) && (leftover.len() as u64) < length {
        socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    if leftover.len() as u64 > length {
        return write_status(&mut socket, 400, "bad body", &[]).await;
    }

    if method == "PUT" {
        return put(
            &mut socket,
            &library,
            target,
            &leftover,
            length,
            source_mtime,
        )
        .await;
    }
    if let Err(err) = discard_body(&mut socket, &leftover, length).await {
        return write_status(&mut socket, 400, &err, &[]).await;
    }

    match method {
        "OPTIONS" => {
            write_status(
                &mut socket,
                200,
                "OK",
                &[("Allow", "OPTIONS, GET, HEAD, PUT, PROPFIND, MKCOL")],
            )
            .await
        }
        "MKCOL" => write_status(&mut socket, 201, "Created", &[]).await,
        "PROPFIND" => propfind(&mut socket, &library).await,
        "HEAD" | "GET" => get(&mut socket, &library, target, method == "GET").await,
        "DELETE" | "MOVE" | "COPY" => {
            write_status(&mut socket, 405, "albumen does not delete or rename", &[]).await
        }
        _ => write_status(&mut socket, 405, "method not allowed", &[]).await,
    }
}

/// Header DoS bound. `ALBUMEN_RECEIVE_MAX_HEADER` overrides 64 KiB.
fn max_header_bytes() -> usize {
    match std::env::var("ALBUMEN_RECEIVE_MAX_HEADER") {
        Ok(text) => match text.parse::<usize>() {
            Ok(value) if value >= 1 => value,
            _ => 64 * 1024,
        },
        Err(_) => 64 * 1024,
    }
}

async fn discard_body(
    socket: &mut impl Stream,
    leftover: &[u8],
    length: u64,
) -> Result<(), String> {
    let mut written = leftover.len() as u64;
    let mut buf = [0u8; 64 * 1024];
    while written < length {
        let n = socket.read(&mut buf).await.map_err(|err| err.to_string())?;
        if n == 0 {
            return Err("truncated body".to_string());
        }
        let room = (length - written) as usize;
        if n > room {
            return Err("body longer than Content-Length".to_string());
        }
        written += n as u64;
    }
    Ok(())
}

async fn put(
    socket: &mut impl Stream,
    library: &Library,
    target: &str,
    leftover: &[u8],
    length: u64,
    source_mtime: Option<u64>,
) -> std::io::Result<()> {
    let arrival = match Arrival::parse(&percent_decode(target)) {
        Ok(arrival) => arrival,
        Err(err) => return write_status(socket, 400, &err, &[]).await,
    };
    if let Some(message) = library.namespace_refusal(&arrival) {
        if let Err(err) = discard_body(socket, leftover, length).await {
            return write_status(socket, 400, &err, &[]).await;
        }
        return write_status(socket, 403, &message, &[]).await;
    }
    let mut spool = match Spool::create(&arrival.camera_filename) {
        Ok(spool) => spool,
        Err(err) => return write_status(socket, 400, &err, &[]).await,
    };
    if let Err(err) = write_body_to_spool(socket, &mut spool, leftover, length).await {
        return write_status(socket, 400, &err, &[]).await;
    }
    if let Some(seconds) = source_mtime {
        if let Err(err) = spool.set_mtime(seconds) {
            return write_status(socket, 500, &err, &[]).await;
        }
    }
    match library.commit(&mut spool, &arrival, crate::store::MtimeProtocol::WebDav) {
        Ok(Commit::Stored {
            sha256, verify_ms, ..
        }) => created(socket, &sha256, verify_ms).await,
        Ok(Commit::Deduped { sha256, .. }) => deduped(socket, &sha256).await,
        Err(err) => write_status(socket, 500, &err, &[]).await,
    }
}

async fn write_body_to_spool(
    socket: &mut impl Stream,
    spool: &mut Spool,
    leftover: &[u8],
    length: u64,
) -> Result<(), String> {
    if !leftover.is_empty() {
        spool.write_at(0, leftover)?;
    }
    let mut written = leftover.len() as u64;
    let mut buf = [0u8; 64 * 1024];
    while written < length {
        let n = socket.read(&mut buf).await.map_err(|err| err.to_string())?;
        if n == 0 {
            return Err("truncated body".to_string());
        }
        let room = (length - written) as usize;
        let take = n.min(room);
        spool.write_at(written, &buf[..take])?;
        written += take as u64;
        if n > room {
            return Err("body longer than Content-Length".to_string());
        }
    }
    Ok(())
}

async fn created(socket: &mut impl Stream, sha256: &str, verify_ms: u128) -> std::io::Result<()> {
    let sha256 = sha256.replace(['\r', '\n'], "");
    let verify_ms = verify_ms.to_string();
    write_status(
        socket,
        201,
        "Created",
        &[
            ("X-Albumen-Sha256", &sha256),
            ("X-Albumen-Verify-Ms", &verify_ms),
        ],
    )
    .await
}

async fn deduped(socket: &mut impl Stream, sha256: &str) -> std::io::Result<()> {
    let sha256 = sha256.replace(['\r', '\n'], "");
    write_status(
        socket,
        409,
        "identical bytes already stored",
        &[("X-Albumen-Sha256", &sha256)],
    )
    .await
}

async fn get(
    socket: &mut impl Stream,
    library: &Library,
    target: &str,
    send_body: bool,
) -> std::io::Result<()> {
    let name = match Arrival::parse(&percent_decode(target)).map(|arrival| arrival.camera_filename)
    {
        Ok(name) => name,
        Err(_) => return write_status(socket, 404, "not found", &[]).await,
    };
    let path = library.root().join(&name);
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => return write_status(socket, 404, "not found", &[]).await,
    };
    let len = match file.metadata().await {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return write_status(socket, 404, "not found", &[]).await,
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
    );
    socket.write_all(header.as_bytes()).await?;
    if send_body {
        tokio::io::copy(&mut file, socket).await?;
    }
    socket.shutdown().await
}

async fn propfind(socket: &mut impl Stream, library: &Library) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(library.root()) {
        Ok(entries) => entries,
        Err(err) => return write_status(socket, 500, &err.to_string(), &[]).await,
    };
    socket
        .write_all(
            b"HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .await?;
    write_chunk(
        socket,
        r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#,
    )
    .await?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !Library::is_listed_file(&name) {
            continue;
        }
        let mut xml = String::from("<d:response><d:href>/");
        xml.push_str(&xml_escape(&name));
        xml.push_str("</d:href><d:propstat><d:prop><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
        write_chunk(socket, &xml).await?;
    }
    write_chunk(socket, "</d:multistatus>").await?;
    socket.write_all(b"0\r\n\r\n").await?;
    socket.shutdown().await
}

async fn write_chunk(socket: &mut impl Stream, data: &str) -> std::io::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    socket
        .write_all(format!("{:X}\r\n", data.len()).as_bytes())
        .await?;
    socket.write_all(data.as_bytes()).await?;
    socket.write_all(b"\r\n").await?;
    Ok(())
}

fn authorized(header: Option<&str>, auth: &Auth) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some(encoded) = header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))
    else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((user, password)) = text.split_once(':') else {
        return false;
    };
    if !constant_eq(user.as_bytes(), auth.user.as_bytes()) {
        return false;
    }
    if constant_eq(password.as_bytes(), auth.password.as_bytes()) {
        return true;
    }
    match &auth.pairing_token {
        Some(token) if token.matches(password) => token.try_consume(),
        _ => false,
    }
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let len = left.len().max(right.len());
    for index in 0..len {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }
    diff == 0
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_status(
    socket: &mut impl Stream,
    status: u16,
    reason: &str,
    extra: &[(&str, &str)],
) -> std::io::Result<()> {
    let reason = reason.replace(['\r', '\n'], " ");
    let mut text =
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n");
    for (name, value) in extra {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push_str("\r\n");
    }
    text.push_str("\r\n");
    socket.write_all(text.as_bytes()).await?;
    // A clean TLS close lets a pinned client finish the response. The library
    // file is already committed when this is a 201.
    socket.shutdown().await
}

fn expects_continue(head: &str) -> bool {
    head.split("\r\n").any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("expect")
                && value.to_ascii_lowercase().contains("100-continue")
        })
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
