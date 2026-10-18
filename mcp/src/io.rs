// SPDX-License-Identifier: Apache-2.0

//! MCP stdio framing: `Content-Length` headers, then a JSON body.

use std::io::{self, BufRead, Write};

use serde_json::Value;

const DEFAULT_MAX_MESSAGE: usize = 8 * 1024 * 1024;

/// Protocol bound on one inbound MCP message.
///
/// This stays so an unauthenticated client cannot make the process allocate
/// an unbounded body. `ALBUMEN_MCP_MAX_MESSAGE` overrides the 8 MiB default.
/// There is no library-size ceiling here. Outbound pages are not cut to fit.
pub fn max_message_bytes() -> usize {
    match std::env::var("ALBUMEN_MCP_MAX_MESSAGE") {
        Ok(text) => match text.parse::<usize>() {
            Ok(value) if value >= 1 => value,
            _ => DEFAULT_MAX_MESSAGE,
        },
        Err(_) => DEFAULT_MAX_MESSAGE,
    }
}

pub fn write_message(out: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()?;
    Ok(())
}

pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return if length.is_none() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof in message headers",
                ))
            };
        }
        if line == "\n" || line == "\r\n" {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse().map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("bad Content-Length: {err}"),
                    )
                })?);
            }
        }
    }
    let Some(length) = length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Content-Length",
        ));
    };
    let max_message = max_message_bytes();
    if length > max_message {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message exceeds {max_message} bytes"),
        ));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("invalid JSON: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_one_message() {
        let message = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let mut buf = Vec::new();
        write_message(&mut buf, &message).unwrap();
        let mut cursor = Cursor::new(buf);
        let read = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(read, message);
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn an_inbound_message_over_the_default_is_refused_before_the_body() {
        let mut header = format!("Content-Length: {}\r\n\r\n", 8 * 1024 * 1024 + 1);
        header.push('x');
        let mut cursor = Cursor::new(header.into_bytes());
        let err = read_message(&mut cursor).unwrap_err();
        assert!(err.to_string().contains("message exceeds"));
    }
}
