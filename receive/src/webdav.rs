// SPDX-License-Identifier: Apache-2.0

//! WebDAV for cameras and card writers. PUT commits through `copy_verify`.
//! A 201 response carries `X-Albumen-Sha256`, the digest `copy_verify` checked.
//! A 409 for an existing file carries the digest of the bytes already stored.
//! DELETE and MOVE are refused. Only the filename is stored.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::store::{stored_name, Library, Spool};

#[derive(Clone)]
pub struct Auth {
    pub user: String,
    pub password: String,
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

async fn handle(
    mut socket: TcpStream,
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
        if header.len() > 64 * 1024 {
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
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<u64>().ok();
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
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
    if leftover.len() as u64 > length {
        return write_status(&mut socket, 400, "bad body", &[]).await;
    }
    let mut body = leftover;
    while (body.len() as u64) < length {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return write_status(&mut socket, 400, "truncated body", &[]).await;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() as u64 > length {
            return write_status(&mut socket, 400, "body longer than Content-Length", &[]).await;
        }
    }
    body.truncate(length as usize);

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
        "PUT" => put(&mut socket, &library, target, &body).await,
        "DELETE" | "MOVE" | "COPY" => {
            write_status(&mut socket, 405, "albumen does not delete or rename", &[]).await
        }
        _ => write_status(&mut socket, 405, "method not allowed", &[]).await,
    }
}

async fn put(
    socket: &mut TcpStream,
    library: &Library,
    target: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let name = match stored_name(&percent_decode(target)) {
        Ok(name) => name,
        Err(err) => return write_status(socket, 400, &err, &[]).await,
    };
    if library.has(&name) {
        return existing(socket, library, &name).await;
    }
    let mut spool = match Spool::create(&name) {
        Ok(spool) => spool,
        Err(err) => return write_status(socket, 400, &err, &[]).await,
    };
    if let Err(err) = spool.write_at(0, body) {
        return write_status(socket, 500, &err, &[]).await;
    }
    if let Err(err) = spool.finish() {
        return write_status(socket, 500, &err, &[]).await;
    }
    match library.commit(&spool) {
        Ok(verified) => created(socket, &verified.sha256).await,
        Err(err) => {
            if err.contains("refusing to overwrite") {
                existing(socket, library, &name).await
            } else {
                write_status(socket, 500, &err, &[]).await
            }
        }
    }
}

async fn created(socket: &mut TcpStream, sha256: &str) -> std::io::Result<()> {
    let sha256 = sha256.replace(['\r', '\n'], "");
    write_status(socket, 201, "Created", &[("X-Albumen-Sha256", &sha256)]).await
}

async fn existing(socket: &mut TcpStream, library: &Library, name: &str) -> std::io::Result<()> {
    let path = library.root().join(name);
    match albumen_ingest::sha256_hex(&path) {
        Ok(sha256) => {
            let sha256 = sha256.replace(['\r', '\n'], "");
            write_status(
                socket,
                409,
                "refusing to overwrite",
                &[("X-Albumen-Sha256", &sha256)],
            )
            .await
        }
        Err(err) => write_status(socket, 500, &err.to_string(), &[]).await,
    }
}

async fn get(
    socket: &mut TcpStream,
    library: &Library,
    target: &str,
    send_body: bool,
) -> std::io::Result<()> {
    let name = match stored_name(&percent_decode(target)) {
        Ok(name) => name,
        Err(_) => return write_status(socket, 404, "not found", &[]).await,
    };
    let path = library.root().join(&name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return write_status(socket, 404, "not found", &[]).await,
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    socket.write_all(header.as_bytes()).await?;
    if send_body {
        socket.write_all(&bytes).await?;
    }
    Ok(())
}

async fn propfind(socket: &mut TcpStream, library: &Library) -> std::io::Result<()> {
    let names = library.files().unwrap_or_default();
    let mut xml =
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:">"#);
    xml.push_str("<d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
    for name in names {
        xml.push_str("<d:response><d:href>/");
        xml.push_str(&xml_escape(&name));
        xml.push_str("</d:href><d:propstat><d:prop><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
    }
    xml.push_str("</d:multistatus>");
    let header = format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        xml.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(xml.as_bytes()).await?;
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
    constant_eq(user.as_bytes(), auth.user.as_bytes())
        && constant_eq(password.as_bytes(), auth.password.as_bytes())
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
    socket: &mut TcpStream,
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
    socket.write_all(text.as_bytes()).await
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
