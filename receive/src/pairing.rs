// SPDX-License-Identifier: Apache-2.0

//! Pairing for `albumen-receive`.
//!
//! `--pair` generates a self-signed certificate on first run, shows a QR code,
//! and serves WebDAV over TLS. The QR encodes `albumen-pair://v1` with the
//! endpoint, the SHA-256 fingerprint of the certificate DER, a one-time token,
//! and the family member public key when the receiver was started with
//! `--member`. The certificate and token live in a sibling directory of the
//! library, never inside a member namespace. A spent token is marked with a
//! new file. The token file is replaced by a rename when a later run needs a
//! fresh token. Nothing in this module deletes a source or a stored original.
//!
//! The same URI is parsed by the Android and iOS backup apps. Those parsers
//! and this one share the vectors in the tests below.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use albumen_ingest::{dest, IngestError};
use qrcode::{Color, QrCode};
use rand::RngExt;
use rcgen::{CertificateParams, DnType, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use sha2::{Digest, Sha256};

/// Same string the Android and iOS pairing tests parse.
pub const EXAMPLE_WITH_MEMBER: &str = "albumen-pair://v1?endpoint=https%3A%2F%2F127.0.0.1%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&member=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// Same string the Android and iOS pairing tests parse, without a member.
pub const EXAMPLE_WITHOUT_MEMBER: &str = "albumen-pair://v1?endpoint=https%3A%2F%2Fphotos.example%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub struct PairingSession {
    pub fingerprint: String,
    pub token: Arc<PairingToken>,
    server_config: Arc<ServerConfig>,
}

impl PairingSession {
    pub fn acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(Arc::clone(&self.server_config))
    }
}

/// One-time Basic password from the pairing QR. The camera password is separate.
pub struct PairingToken {
    token: String,
    marker: PathBuf,
    spent: AtomicBool,
}

impl std::fmt::Debug for PairingToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingToken")
            .field("spent", &self.spent.load(Ordering::SeqCst))
            .finish()
    }
}

impl PairingToken {
    pub fn one_time_token(&self) -> &str {
        &self.token
    }

    pub fn matches(&self, presented: &str) -> bool {
        constant_eq(presented.as_bytes(), self.token.as_bytes())
    }

    /// Accept this token once. A second call, including from a later process,
    /// returns false. The marker file is created and left in place.
    pub fn try_consume(&self) -> bool {
        if self.spent.load(Ordering::SeqCst) {
            return false;
        }
        match dest::write_private(&self.marker, b"consumed\n") {
            Ok(()) => {
                self.spent.store(true, Ordering::SeqCst);
                true
            }
            Err(IngestError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                self.spent.store(true, Ordering::SeqCst);
                false
            }
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPairing {
    pub endpoint: String,
    pub fingerprint: String,
    pub token: String,
    pub member: Option<String>,
}

/// Directory beside the library. Certificates are not library files and are
/// not member objects.
pub fn state_directory(library: &Path) -> PathBuf {
    let file_name = library
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("library"));
    let mut name = file_name.to_os_string();
    name.push(".receive");
    match library.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

pub fn load_or_create(
    library: &Path,
    bind: Option<SocketAddr>,
    advertise: Option<&str>,
) -> io::Result<PairingSession> {
    let _names = san_list(bind, advertise)?;
    let dir = state_directory(library);
    dest::create_dir_all(&dir).map_err(io_other)?;
    let cert_der = load_or_generate_cert(&dir, bind, advertise)?;
    let key_der = fs::read(dir.join("key.der"))?;
    let server_config = server_config(&cert_der, &key_der)?;
    let token = load_or_generate_token(&dir)?;
    let marker = dir.join("consumed").join(sha256_hex(token.as_bytes()));
    Ok(PairingSession {
        fingerprint: sha256_hex(&cert_der),
        token: Arc::new(PairingToken {
            token,
            marker,
            spent: AtomicBool::new(false),
        }),
        server_config,
    })
}

pub fn endpoint_url(addr: SocketAddr, advertise: Option<&str>) -> Result<String, String> {
    let host = if let Some(name) = advertise {
        bracket_host(&normalize_advertise(name)?)?
    } else if addr.ip().is_unspecified() {
        return Err("--pair on 0.0.0.0 or :: needs --advertise <host>".to_string());
    } else {
        match addr.ip() {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        }
    };
    Ok(format!("https://{host}:{}/", addr.port()))
}

pub fn format_uri(
    endpoint: &str,
    fingerprint: &str,
    token: &str,
    member: Option<&str>,
) -> Result<String, String> {
    valid_endpoint(endpoint)?;
    let fingerprint = normalize_fingerprint(fingerprint)
        .ok_or_else(|| "fingerprint must be 64 hex characters".to_string())?;
    if !is_hex_64(token) {
        return Err("pairing token must be 64 hex characters".to_string());
    }
    let mut uri = format!(
        "albumen-pair://v1?endpoint={}&fingerprint={}&token={}",
        percent_encode(endpoint),
        fingerprint,
        token.to_ascii_lowercase(),
    );
    if let Some(member) = member {
        if !is_hex_64(member) {
            return Err("member must be a 32-byte Ed25519 public key".to_string());
        }
        uri.push_str("&member=");
        uri.push_str(&member.to_ascii_lowercase());
    }
    Ok(uri)
}

pub fn parse_uri(input: &str) -> Result<ParsedPairing, String> {
    let text = input.trim();
    if text.len() > 2048 {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    }
    let Some(rest) = text.strip_prefix("albumen-pair://") else {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    };
    let Some((version, query)) = rest.split_once('?') else {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    };
    if version.trim_end_matches('/') != "v1" {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    }
    let pairs = query_pairs(query)?;
    let endpoint = required(&pairs, "endpoint")?;
    let fingerprint = required(&pairs, "fingerprint")?;
    let token = required(&pairs, "token")?;
    let member = optional(&pairs, "member")?;
    valid_endpoint(&endpoint).map_err(|_| {
        "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string()
    })?;
    let Some(fingerprint) = normalize_fingerprint(&fingerprint) else {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    };
    if !is_hex_64(&token) {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    }
    let member = match member {
        None => None,
        Some(member) if is_hex_64(&member) => Some(member.to_ascii_lowercase()),
        Some(_) => {
            return Err(
                "The pairing code is not an albumen-pair v1 URI. The copy did not start."
                    .to_string(),
            );
        }
    };
    Ok(ParsedPairing {
        endpoint,
        fingerprint,
        token: token.to_ascii_lowercase(),
        member,
    })
}

/// `None` when the presented certificate matches the pairing code.
/// A mismatch is the sentence the phone shows. The receiver password is not
/// part of that sentence.
pub fn fingerprint_mismatch(expected: &str, presented: &str) -> Option<String> {
    let presented_clean = presented.trim().replace(['\r', '\n'], " ");
    match (
        normalize_fingerprint(expected),
        normalize_fingerprint(&presented_clean),
    ) {
        (Some(expected), Some(presented)) if expected == presented => None,
        (Some(expected), _) => Some(format!(
            "Certificate fingerprint does not match the pairing code. Expected {expected}. Received {presented_clean}. The copy did not start."
        )),
        (None, _) => Some(
            "Certificate fingerprint does not match the pairing code. The pairing code has no fingerprint. The copy did not start."
                .to_string(),
        ),
    }
}

pub fn qr_terminal(uri: &str) -> Result<String, String> {
    let code = qr_code(uri)?;
    Ok(code
        .render::<char>()
        .quiet_zone(true)
        .module_dimensions(2, 1)
        .dark_color('█')
        .light_color(' ')
        .build())
}

pub fn write_qr_png(path: &Path, uri: &str) -> Result<(), String> {
    let bytes = qr_png(uri)?;
    dest::write_private(path, &bytes).map_err(|err| err.to_string())
}

fn qr_png(uri: &str) -> Result<Vec<u8>, String> {
    let code = qr_code(uri)?;
    Ok(png_from_qr(&code))
}

fn qr_code(uri: &str) -> Result<QrCode, String> {
    QrCode::new(uri.as_bytes()).map_err(|err| format!("pairing QR was not built: {err}"))
}

fn load_or_generate_cert(
    dir: &Path,
    bind: Option<SocketAddr>,
    advertise: Option<&str>,
) -> io::Result<Vec<u8>> {
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");
    match (cert_path.is_file(), key_path.is_file()) {
        (true, true) => Ok(fs::read(cert_path)?),
        (false, false) => generate_cert(dir, bind, advertise),
        _ => Err(io::Error::other(
            "pairing certificate is incomplete and was left in place",
        )),
    }
}

fn generate_cert(
    dir: &Path,
    bind: Option<SocketAddr>,
    advertise: Option<&str>,
) -> io::Result<Vec<u8>> {
    let (dns, ips) = san_list(bind, advertise)?;
    let mut params = CertificateParams::new(dns).map_err(io_other)?;
    for ip in ips {
        params.subject_alt_names.push(SanType::IpAddress(ip));
    }
    params
        .distinguished_name
        .push(DnType::CommonName, "albumen-receive");
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2036, 1, 1);
    let key = KeyPair::generate().map_err(io_other)?;
    let cert = params.self_signed(&key).map_err(io_other)?;
    let cert_der = cert.der().to_vec();
    let key_der = key.serialize_der();
    dest::write_private(&dir.join("cert.der"), &cert_der).map_err(io_other)?;
    dest::write_private(&dir.join("key.der"), &key_der).map_err(io_other)?;
    Ok(cert_der)
}

fn load_or_generate_token(dir: &Path) -> io::Result<String> {
    let path = dir.join("token");
    if let Some(existing) = read_token(&path)? {
        if !token_spent(dir, &existing) {
            return Ok(existing);
        }
    }
    let token = generate_token();
    store_token(dir, &token)?;
    Ok(token)
}

fn read_token(path: &Path) -> io::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    let token = text.trim();
    if !is_hex_64(token) {
        return Err(io::Error::other(
            "pairing token file is not 64 hex characters and was left in place",
        ));
    }
    Ok(Some(token.to_ascii_lowercase()))
}

fn store_token(dir: &Path, token: &str) -> io::Result<()> {
    let path = dir.join("token");
    if !path.exists() {
        dest::write_private(&path, token.as_bytes()).map_err(io_other)?;
        return Ok(());
    }
    let mut nonce = [0u8; 8];
    rand::rng().fill(&mut nonce);
    let tmp = dir.join(format!("token.{}.tmp", hex_encode(&nonce)));
    dest::write_private(&tmp, token.as_bytes()).map_err(io_other)?;
    dest::rename(&tmp, &path).map_err(io_other)?;
    Ok(())
}

fn token_spent(dir: &Path, token: &str) -> bool {
    dir.join("consumed")
        .join(sha256_hex(token.as_bytes()))
        .is_file()
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex_encode(&bytes)
}

fn server_config(cert_der: &[u8], key_der: &[u8]) -> io::Result<Arc<ServerConfig>> {
    let cert = CertificateDer::from(cert_der.to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der.to_vec()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(io_other)?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(io_other)?;
    Ok(Arc::new(config))
}

fn san_list(
    bind: Option<SocketAddr>,
    advertise: Option<&str>,
) -> io::Result<(Vec<String>, Vec<IpAddr>)> {
    let mut dns = vec!["localhost".to_string()];
    let mut ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    if let Some(addr) = bind {
        if !addr.ip().is_unspecified() && !ips.contains(&addr.ip()) {
            ips.push(addr.ip());
        }
    }
    if let Some(name) = advertise {
        let name = normalize_advertise(name).map_err(io::Error::other)?;
        if let Ok(ip) = name.parse::<IpAddr>() {
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        } else if !dns.iter().any(|item| item == &name) {
            dns.push(name);
        }
    }
    Ok((dns, ips))
}

fn normalize_advertise(value: &str) -> Result<String, String> {
    let value = value.trim().trim_matches(|ch| ch == '[' || ch == ']');
    if value.is_empty() || value.contains(['/', '?', '#', ' ', '@']) || value.contains("://") {
        return Err("--advertise is a host name or IP address, not a URL".to_string());
    }
    Ok(value.to_string())
}

fn bracket_host(name: &str) -> Result<String, String> {
    if name.parse::<std::net::Ipv6Addr>().is_ok() {
        return Ok(format!("[{name}]"));
    }
    if name.contains(':') {
        return Err("--advertise is a host name or IP address without a port".to_string());
    }
    Ok(name.to_string())
}

fn valid_endpoint(endpoint: &str) -> Result<(), String> {
    let Some(rest) = endpoint.strip_prefix("https://") else {
        return Err("pairing endpoint is https".to_string());
    };
    if rest.is_empty() || rest.contains([' ', '\n', '\r', '@']) {
        return Err("pairing endpoint is missing a host".to_string());
    }
    let hostport = rest.split('/').next().unwrap_or("");
    if hostport.is_empty() {
        return Err("pairing endpoint is missing a host".to_string());
    }
    Ok(())
}

fn required(pairs: &[(String, String)], key: &str) -> Result<String, String> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .ok_or_else(|| {
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string()
        })
}

fn optional(pairs: &[(String, String)], key: &str) -> Result<Option<String>, String> {
    let mut found = None;
    for (name, value) in pairs {
        if name == key {
            if found.is_some() {
                return Err(
                    "The pairing code is not an albumen-pair v1 URI. The copy did not start."
                        .to_string(),
                );
            }
            found = Some(value.clone());
        }
    }
    Ok(found)
}

fn query_pairs(query: &str) -> Result<Vec<(String, String)>, String> {
    if query.is_empty() {
        return Err(
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string(),
        );
    }
    let mut pairs = Vec::new();
    for part in query.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            return Err(
                "The pairing code is not an albumen-pair v1 URI. The copy did not start."
                    .to_string(),
            );
        };
        if key.is_empty() {
            return Err(
                "The pairing code is not an albumen-pair v1 URI. The copy did not start."
                    .to_string(),
            );
        }
        let key = percent_decode(key).map_err(|_| {
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string()
        })?;
        let value = percent_decode(value).map_err(|_| {
            "The pairing code is not an albumen-pair v1 URI. The copy did not start.".to_string()
        })?;
        if pairs.iter().any(|(existing, _)| existing == &key) {
            return Err(
                "The pairing code is not an albumen-pair v1 URI. The copy did not start."
                    .to_string(),
            );
        }
        pairs.push((key, value));
    }
    Ok(pairs)
}

fn percent_encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("truncated percent escape".to_string());
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| "bad percent escape".to_string())?;
            let value =
                u8::from_str_radix(hex, 16).map_err(|_| "bad percent escape".to_string())?;
            out.push(value);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).map_err(|_| "pairing URI is not UTF-8".to_string())
}

fn normalize_fingerprint(value: &str) -> Option<String> {
    let value = value.trim();
    if is_hex_64(value) {
        Some(value.to_ascii_lowercase())
    } else {
        None
    }
}

fn is_hex_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let len = left.len().max(right.len());
    for index in 0..len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= (left_byte ^ right_byte) as usize;
    }
    diff == 0
}

fn io_other(err: impl ToString) -> io::Error {
    io::Error::other(err.to_string())
}

fn png_from_qr(code: &QrCode) -> Vec<u8> {
    const QUIET: usize = 4;
    const SCALE: usize = 8;
    let modules = code.width();
    let side = modules + QUIET * 2;
    let pixels = side * SCALE;
    let mut gray = vec![255u8; pixels * pixels];
    let colors = code.to_colors();
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] != Color::Dark {
                continue;
            }
            let origin_x = (x + QUIET) * SCALE;
            let origin_y = (y + QUIET) * SCALE;
            for row in 0..SCALE {
                let start = (origin_y + row) * pixels + origin_x;
                gray[start..start + SCALE].fill(0);
            }
        }
    }
    png_gray(&gray, pixels as u32, pixels as u32)
}

fn png_gray(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((width as usize) + 1) * height as usize);
    for row in pixels.chunks(width as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib_store(&raw));
    png_chunk(&mut png, b"IEND", &[]);
    png
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_bytes = Vec::with_capacity(4 + data.len());
    crc_bytes.extend_from_slice(kind);
    crc_bytes.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_bytes).to_be_bytes());
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut index = 0;
    loop {
        let end = (index + 65535).min(data.len());
        let block = &data[index..end];
        let last = end == data.len();
        out.push(if last { 0x01 } else { 0x00 });
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
        index = end;
        if last {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors_parse_and_a_mismatch_is_the_visible_sentence() {
        let with_member = parse_uri(EXAMPLE_WITH_MEMBER).unwrap();
        assert_eq!(with_member.endpoint, "https://127.0.0.1:8443/");
        assert_eq!(
            with_member.fingerprint,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(with_member.token, "a".repeat(64));
        assert_eq!(with_member.member.as_deref(), Some("b".repeat(64).as_str()));
        let plain = parse_uri(EXAMPLE_WITHOUT_MEMBER).unwrap();
        assert_eq!(plain.member, None);
        assert_eq!(plain.endpoint, "https://photos.example:8443/");

        let rebuilt = format_uri(
            &with_member.endpoint,
            &with_member.fingerprint,
            &with_member.token,
            with_member.member.as_deref(),
        )
        .unwrap();
        assert_eq!(parse_uri(&rebuilt).unwrap(), with_member);

        assert!(fingerprint_mismatch(&with_member.fingerprint, &with_member.fingerprint).is_none());
        let upper = with_member.fingerprint.to_ascii_uppercase();
        assert!(fingerprint_mismatch(&upper, &with_member.fingerprint).is_none());
        let wrong = "f".repeat(64);
        let sentence = fingerprint_mismatch(&with_member.fingerprint, &wrong).unwrap();
        assert!(sentence.contains("fingerprint"));
        assert!(sentence.contains("The copy did not start."));
        assert!(sentence.contains(&with_member.fingerprint));
        assert!(sentence.contains(&wrong));
        assert!(!sentence.contains("camera-secret"));

        let rejected = parse_uri(
            "albumen-pair://v2?endpoint=https%3A%2F%2F127.0.0.1%2F&fingerprint=ab&token=cd",
        )
        .unwrap_err();
        assert!(rejected.contains("did not start"));
        assert!(!rejected.contains("aaaaaaaa"));
    }

    #[test]
    fn certificate_is_reused_and_a_spent_token_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("library");
        fs::create_dir(&library).unwrap();
        let bind = "127.0.0.1:8443".parse().unwrap();
        let first = load_or_create(&library, Some(bind), None).unwrap();
        let fingerprint = first.fingerprint.clone();
        let token = first.token.one_time_token().to_string();
        assert!(first.token.try_consume());
        assert!(!first.token.try_consume());
        let marker = state_directory(&library)
            .join("consumed")
            .join(sha256_hex(token.as_bytes()));
        assert!(marker.is_file());
        let second = load_or_create(&library, Some(bind), None).unwrap();
        assert_eq!(second.fingerprint, fingerprint);
        assert_ne!(second.token.one_time_token(), token);
        assert!(marker.is_file());
        assert!(!library.join("cert.der").exists());
        assert!(state_directory(&library).join("cert.der").is_file());
        assert!(state_directory(&library).join("key.der").is_file());
    }

    #[test]
    fn qr_png_round_trips_the_uri() {
        let uri = EXAMPLE_WITHOUT_MEMBER;
        let png_bytes = qr_png(uri).unwrap();
        let terminal = qr_terminal(uri).unwrap();
        assert!(terminal.contains('█'));
        let decoded = decode_png_qr(&png_bytes);
        assert_eq!(decoded, uri);
    }

    fn decode_png_qr(bytes: &[u8]) -> String {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        let width = info.width as usize;
        let height = info.height as usize;
        let pixels = &buf[..info.buffer_size()];
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            pixels[y * width + x]
        });
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1, "QR grid was not found in the PNG");
        let (_meta, content) = grids[0].decode().unwrap();
        content
    }
}
