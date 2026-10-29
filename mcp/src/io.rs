// SPDX-License-Identifier: Apache-2.0

//! MCP stdio framing: one JSON-RPC message per line, newline-delimited
//! (MCP spec, stdio transport). Messages contain no embedded newlines.

use std::io::{self, BufRead, Read, Write};

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
    // serde_json escapes newlines inside strings, so the body is one line.
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    out.write_all(&body)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// Reads the next non-blank line as one JSON-RPC message. `Ok(None)` at EOF.
/// A line longer than [`max_message_bytes`] is refused without buffering it all.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let max_message = max_message_bytes();
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(max_message)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let read = input.by_ref().take(limit).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        let ended = line.last() == Some(&b'\n');
        if ended {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        if line.len() > max_message || (!ended && read as u64 == limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message exceeds {max_message} bytes"),
            ));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&line).map(Some).map_err(|err| {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid JSON: {err}"))
        });
    }
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
    fn messages_are_newline_delimited_without_headers() {
        let message = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"t": "a\nb"}});
        let mut buf = Vec::new();
        write_message(&mut buf, &message).unwrap();
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(buf.last(), Some(&b'\n'));
        assert!(!buf.starts_with(b"Content-Length"));
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n\n{\"jsonrpc\":\"2.0\",\"method\":\"x\"}";
        let mut cursor = Cursor::new(&input[..]);
        assert_eq!(read_message(&mut cursor).unwrap().unwrap()["id"], 1);
        assert_eq!(read_message(&mut cursor).unwrap().unwrap()["method"], "x");
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn an_inbound_message_over_the_default_is_refused_before_the_body() {
        let line = vec![b'x'; 8 * 1024 * 1024 + 2];
        let mut cursor = Cursor::new(line);
        let err = read_message(&mut cursor).unwrap_err();
        assert!(err.to_string().contains("message exceeds"));
    }
}
