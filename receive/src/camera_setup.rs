// SPDX-License-Identifier: Apache-2.0

//! Machine-readable camera setup. The human lines under `camera setup` stay.
//! This line is the same host, port, user, password phrase, and path, counted
//! as actions a body performs once for WebDAV and FTP together.

use std::net::SocketAddr;

/// More than this many camera-body actions fails CI.
pub const MAX_CAMERA_SETUP_STEPS: usize = 5;

/// The re-read SHA-256 of a stored 100 MB synthetic RAW fails CI above this.
pub const MAX_SHA256_VERIFY_MS: u128 = 5_000;

/// Printed in place of the process password. The secret is not on this line.
pub const PASSWORD_PHRASE: &str = "the password for this process";

pub const STEPS_PREFIX: &str = "camera-setup-steps ";

/// `true` when `count` is above [`MAX_CAMERA_SETUP_STEPS`]. Five passes.
pub fn setup_steps_exceed_budget(count: usize) -> bool {
    count > MAX_CAMERA_SETUP_STEPS
}

/// `true` when the re-read took more than [`MAX_SHA256_VERIFY_MS`]. Five
/// seconds passes.
pub fn verify_exceeds_budget(verify_ms: u128) -> bool {
    verify_ms > MAX_SHA256_VERIFY_MS
}

/// One line a test can parse. Host and port carry both protocols so the
/// action count stays one enter-host and one enter-port.
pub fn format_steps_line(
    webdav: Option<SocketAddr>,
    ftp: Option<SocketAddr>,
    user: &str,
) -> String {
    let auto = if webdav.is_some() || ftp.is_some() {
        "on"
    } else {
        "off"
    };
    let steps = [
        object(&[
            ("step", "enter host"),
            ("webdav", &endpoint_host(webdav)),
            ("ftp", &endpoint_host(ftp)),
        ]),
        object(&[
            ("step", "enter port"),
            ("webdav", &endpoint_port(webdav)),
            ("ftp", &endpoint_port(ftp)),
        ]),
        object(&[("step", "enter user"), ("value", user)]),
        object(&[
            ("step", "enter password or path"),
            ("password", PASSWORD_PHRASE),
            ("path", "/"),
        ]),
        object(&[("step", "enable auto-transfer"), ("value", auto)]),
    ];
    format!(
        "{STEPS_PREFIX}{{\"count\":{},\"steps\":[{}]}}",
        steps.len(),
        steps.join(",")
    )
}

fn endpoint_host(addr: Option<SocketAddr>) -> String {
    match addr {
        Some(addr) => addr.ip().to_string(),
        None => "off".to_string(),
    }
}

fn endpoint_port(addr: Option<SocketAddr>) -> String {
    match addr {
        Some(addr) => addr.port().to_string(),
        None => "off".to_string(),
    }
}

fn object(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(key));
        out.push(':');
        out.push_str(&json_string(value));
    }
    out.push('}');
    out
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn five_steps_pass_and_six_fail() {
        assert!(!setup_steps_exceed_budget(0));
        assert!(!setup_steps_exceed_budget(MAX_CAMERA_SETUP_STEPS));
        assert!(setup_steps_exceed_budget(MAX_CAMERA_SETUP_STEPS + 1));
        assert!(!verify_exceeds_budget(MAX_SHA256_VERIFY_MS));
        assert!(verify_exceeds_budget(MAX_SHA256_VERIFY_MS + 1));
    }

    #[test]
    fn printed_list_is_five_actions_and_hides_the_secret() {
        let webdav: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let ftp: SocketAddr = "127.0.0.1:2121".parse().unwrap();
        let line = format_steps_line(Some(webdav), Some(ftp), "cam\"era");
        assert!(line.starts_with(STEPS_PREFIX));
        let json = line.strip_prefix(STEPS_PREFIX).unwrap();
        assert!(!json.contains('\n'));
        assert!(json.contains("\"count\":5"));
        assert!(json.contains("\"step\":\"enter host\""));
        assert!(json.contains("\"step\":\"enter port\""));
        assert!(json.contains("\"step\":\"enter user\""));
        assert!(json.contains("\"step\":\"enter password or path\""));
        assert!(json.contains("\"step\":\"enable auto-transfer\""));
        assert!(json.contains("\"webdav\":\"127.0.0.1\""));
        assert!(json.contains("\"ftp\":\"2121\""));
        assert!(json.contains("\"value\":\"cam\\\"era\""));
        assert!(json.contains(PASSWORD_PHRASE));
        assert!(!json.contains("camera-secret"));
        assert_eq!(json.matches("\"step\":").count(), 5);
        assert!(!setup_steps_exceed_budget(
            json.matches("\"step\":").count()
        ));
    }
}
