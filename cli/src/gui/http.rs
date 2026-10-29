// SPDX-License-Identifier: Apache-2.0

//! The smallest HTTP/1.1 the GUI needs: one request per connection.

use std::io::{Read, Write};
use std::net::TcpStream;

pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse a request head (request line and headers, CRLF separated).
pub fn parse_request(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    if !first.next()?.starts_with("HTTP/1.") || !target.starts_with('/') {
        return None;
    }
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Request {
        method,
        target,
        headers,
    })
}

/// Read one request head, at most 16 KiB. Bodies are not used.
pub fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk)?;
        if n == 0 || buf.len() > 16 * 1024 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(parse_request(&String::from_utf8_lossy(&buf)))
}

/// Percent-decode one query value.
pub fn query_param(target: &str, key: &str) -> Option<String> {
    let (_, query) = target.split_once('?')?;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(decode(v));
        }
    }
    None
}

fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn respond(stream: &mut TcpStream, code: u16, mime: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_line_and_headers() {
        let r =
            parse_request("GET /img?p=a HTTP/1.1\r\nHost: 127.0.0.1:9\r\nX-Albumen:  1 \r\n\r\n")
                .unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.target, "/img?p=a");
        assert_eq!(r.header("host"), Some("127.0.0.1:9"));
        assert_eq!(r.header("x-albumen"), Some("1"));
        assert!(parse_request("GET http://evil/ HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request("garbage").is_none());
    }

    #[test]
    fn decodes_query_values() {
        let t = "/img?x=1&p=2026-10-01%20harbour.png&q=a+b&bad=%zz&end=%4";
        assert_eq!(query_param(t, "p").unwrap(), "2026-10-01 harbour.png");
        assert_eq!(query_param(t, "q").unwrap(), "a b");
        assert_eq!(query_param(t, "bad").unwrap(), "%zz");
        assert_eq!(query_param(t, "end").unwrap(), "%4");
        assert!(query_param(t, "none").is_none());
    }
}
