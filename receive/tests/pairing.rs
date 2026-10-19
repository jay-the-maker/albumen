// SPDX-License-Identifier: Apache-2.0

//! Start `albumen-receive --pair`, decode the QR, and pair a client.
//! The first copy has to match SHA-256. A wrong fingerprint is refused
//! before the copy, and the sentence names the fingerprint. A source file
//! and another member's originals stay byte-for-byte.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use albumen_ingest::sha256_hex;
use albumen_receive::pairing;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::Digest;

const PASSWORD: &str = "camera-secret-not-printed";

#[test]
fn dry_run_prints_camera_settings_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let png = tmp.path().join("pair.png");
    let output = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args([
            "--to",
            library.to_str().unwrap(),
            "--user",
            "camera",
            "--password",
            PASSWORD,
            "--pair",
            "--webdav",
            "127.0.0.1:8080",
            "--ftp",
            "127.0.0.1:2121",
            "--qr-png",
            png.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(text.contains("dry-run"));
    assert!(text.contains("webdav url: https://127.0.0.1:8080/"));
    assert!(text.contains("ftp host: 127.0.0.1"));
    assert!(text.contains("ftp port: 2121"));
    assert!(text.contains("ftp encryption: none"));
    assert!(text.contains("webdav password: the password for this process"));
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!err.contains(PASSWORD), "{err}");
    assert!(!library.exists());
    assert!(!png.exists());
    assert!(!pairing::state_directory(&library).exists());
}

#[test]
fn pair_refuses_a_family_library_and_leaves_originals() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let founder = albumen_ingest::family::create_key(&tmp.path().join("founder.key")).unwrap();
    let library = albumen_ingest::family::init_library(&parent, &founder, "ada").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = tmp.path().join("note.txt");
    stamp(&photo, b"synthetic-family-original", mtime);
    let added = albumen_ingest::family::add_object(&library, &founder, &photo, None).unwrap();
    let before = fs::read(&added.object_path).unwrap();
    let before_mtime = fs::metadata(&added.object_path)
        .unwrap()
        .modified()
        .unwrap();
    let before_sha = sha256_hex(&added.object_path).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args([
            "--to",
            library.to_str().unwrap(),
            "--user",
            "camera",
            "--password",
            PASSWORD,
            "--pair",
            "--webdav",
            "127.0.0.1:0",
            "--apply",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("family library"), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!pairing::state_directory(&library).exists(), "{text}");
    assert_eq!(fs::read(&added.object_path).unwrap(), before);
    assert_eq!(
        fs::metadata(&added.object_path)
            .unwrap()
            .modified()
            .unwrap(),
        before_mtime
    );
    assert_eq!(sha256_hex(&added.object_path).unwrap(), before_sha);
}

#[test]
fn qr_pairs_a_client_and_a_wrong_fingerprint_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let png = tmp.path().join("pair.png");
    let source = tmp.path().join("source.jpg");
    let other = tmp.path().join("other-member");
    fs::create_dir(&other).unwrap();
    let other_file = other.join("kept.jpg");
    let mtime = UNIX_EPOCH + Duration::new(1_500_000_000, 123_456_789);
    let body = b"synthetic-paired-original";
    stamp(&source, body, mtime);
    stamp(&other_file, b"other-member-original", mtime);
    let source_sha = sha256_hex(&source).unwrap();
    let source_bytes = fs::read(&source).unwrap();
    let source_mtime = fs::metadata(&source).unwrap().modified().unwrap();
    let other_bytes = fs::read(&other_file).unwrap();
    let other_mtime = fs::metadata(&other_file).unwrap().modified().unwrap();

    let running = Receiver::start(&[
        "--to",
        library.to_str().unwrap(),
        "--user",
        "camera",
        "--password",
        PASSWORD,
        "--pair",
        "--webdav",
        "127.0.0.1:0",
        "--ftp",
        "127.0.0.1:0",
        "--qr-png",
        png.to_str().unwrap(),
        "--apply",
    ]);
    let text = running.output.clone();
    assert!(text.contains('█'), "{text}");
    assert!(text.contains("ftp encryption: none"), "{text}");
    assert!(
        text.contains("webdav tls: certificate pinned by the pairing QR"),
        "{text}"
    );
    assert!(!text.contains(PASSWORD), "{text}");
    let uri = pairing_uri(&text);
    let decoded = decode_png_qr(&png);
    assert_eq!(decoded, uri, "the PNG QR does not match the printed URI");
    let parsed = pairing::parse_uri(&decoded).unwrap();
    assert!(parsed.member.is_none(), "{uri}");
    let addr = webdav_addr(&text);
    assert!(
        text.contains(&format!("webdav url: https://{addr}/")),
        "{text}"
    );

    let wrong = flipped_fingerprint(&parsed.fingerprint);
    let refused = tls_exchange(
        addr,
        &wrong,
        "camera",
        &parsed.token,
        "PUT",
        "/source.jpg",
        body,
    );
    let refused = refused.expect_err("a wrong fingerprint must not connect");
    assert!(refused.contains("fingerprint"), "{refused}");
    assert!(refused.contains("The copy did not start."), "{refused}");
    assert!(!refused.contains(PASSWORD), "{refused}");
    assert!(!library.join("source.jpg").exists());

    let created = tls_exchange(
        addr,
        &parsed.fingerprint,
        "camera",
        &parsed.token,
        "PUT",
        "/source.jpg",
        body,
    )
    .expect("paired PUT");
    assert!(created.starts_with("HTTP/1.1 201"), "{created}");
    assert_eq!(
        header(&created, "X-Albumen-Sha256").as_deref(),
        Some(source_sha.as_str())
    );
    let stored = library.join("source.jpg");
    assert_eq!(fs::read(&stored).unwrap(), body);
    assert_eq!(sha256_hex(&stored).unwrap(), source_sha);

    let again = tls_exchange(
        addr,
        &parsed.fingerprint,
        "camera",
        &parsed.token,
        "PUT",
        "/again.jpg",
        b"second",
    )
    .expect("second token attempt reaches the receiver");
    assert!(again.starts_with("HTTP/1.1 401"), "{again}");
    assert!(!library.join("again.jpg").exists());
    assert_eq!(fs::read(&stored).unwrap(), body);

    let with_password = tls_exchange(
        addr,
        &parsed.fingerprint,
        "camera",
        PASSWORD,
        "PUT",
        "/camera.jpg",
        b"from-the-camera",
    )
    .expect("camera password");
    assert!(with_password.starts_with("HTTP/1.1 201"), "{with_password}");
    let camera_sha = sha256_hex(&library.join("camera.jpg")).unwrap();
    assert_eq!(
        header(&with_password, "X-Albumen-Sha256").as_deref(),
        Some(camera_sha.as_str())
    );

    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(
        fs::metadata(&source).unwrap().modified().unwrap(),
        source_mtime
    );
    assert_eq!(fs::read(&other_file).unwrap(), other_bytes);
    assert_eq!(
        fs::metadata(&other_file).unwrap().modified().unwrap(),
        other_mtime
    );
    assert!(!library.join("members").exists());
    assert!(!library.join("cert.der").exists());
    let state = pairing::state_directory(&library);
    assert!(state.join("cert.der").is_file());
    assert!(state.join("key.der").is_file());
    assert!(!state.starts_with(&library));
}

#[test]
fn paired_member_upload_leaves_the_other_member() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let founder = albumen_ingest::family::create_key(&tmp.path().join("founder.key")).unwrap();
    let library = albumen_ingest::family::init_library(&parent, &founder, "ada").unwrap();
    let mtime = UNIX_EPOCH + Duration::new(1_400_000_111, 123_456_789);
    let photo = tmp.path().join("note.txt");
    stamp(&photo, b"synthetic-family-original", mtime);
    let added = albumen_ingest::family::add_object(&library, &founder, &photo, None).unwrap();
    let phone = albumen_ingest::family::create_key(&tmp.path().join("phone.key")).unwrap();
    let invite = albumen_ingest::family::invite(&library, &founder, phone.member_id()).unwrap();
    albumen_ingest::family::join(&library, &phone, &invite, "phone").unwrap();
    albumen_ingest::family::admit(&library, &founder, phone.member_id(), &invite).unwrap();
    let before = fs::read(&added.object_path).unwrap();
    let before_mtime = fs::metadata(&added.object_path)
        .unwrap()
        .modified()
        .unwrap();
    let before_sha = sha256_hex(&added.object_path).unwrap();

    let png = tmp.path().join("pair.png");
    let running = Receiver::start(&[
        "--to",
        library.to_str().unwrap(),
        "--user",
        "camera",
        "--password",
        PASSWORD,
        "--member",
        phone.member_id(),
        "--pair",
        "--webdav",
        "127.0.0.1:0",
        "--qr-png",
        png.to_str().unwrap(),
        "--apply",
    ]);
    let uri = pairing_uri(&running.output);
    assert_eq!(decode_png_qr(&png), uri);
    let parsed = pairing::parse_uri(&uri).unwrap();
    assert_eq!(parsed.member.as_deref(), Some(phone.member_id()));
    let addr = webdav_addr(&running.output);
    let body = b"phone-paired-original";
    let source = tmp.path().join("phone.jpg");
    stamp(&source, body, mtime);
    let source_bytes = fs::read(&source).unwrap();
    let source_mtime = fs::metadata(&source).unwrap().modified().unwrap();
    let source_sha = sha256_hex(&source).unwrap();

    let wrong = tls_exchange(
        addr,
        &flipped_fingerprint(&parsed.fingerprint),
        "camera",
        &parsed.token,
        "PUT",
        &format!("/members/{}/phone.jpg", phone.member_id()),
        body,
    );
    let wrong = wrong.expect_err("wrong fingerprint");
    assert!(wrong.contains("fingerprint"), "{wrong}");
    assert!(!library
        .join("members")
        .join(phone.member_id())
        .join("phone.jpg")
        .exists());

    let created = tls_exchange(
        addr,
        &parsed.fingerprint,
        "camera",
        &parsed.token,
        "PUT",
        &format!("/members/{}/phone.jpg", phone.member_id()),
        body,
    )
    .unwrap();
    assert!(created.starts_with("HTTP/1.1 201"), "{created}");
    assert_eq!(
        header(&created, "X-Albumen-Sha256").as_deref(),
        Some(source_sha.as_str())
    );
    let stored = library
        .join("members")
        .join(phone.member_id())
        .join("phone.jpg");
    assert_eq!(fs::read(&stored).unwrap(), body);
    assert_eq!(sha256_hex(&stored).unwrap(), source_sha);

    let refused = tls_exchange(
        addr,
        &parsed.fingerprint,
        "camera",
        PASSWORD,
        "PUT",
        &format!("/members/{}/evil.jpg", founder.member_id()),
        b"not-theirs",
    )
    .unwrap();
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");

    assert_eq!(fs::read(&added.object_path).unwrap(), before);
    assert_eq!(
        fs::metadata(&added.object_path)
            .unwrap()
            .modified()
            .unwrap(),
        before_mtime
    );
    assert_eq!(sha256_hex(&added.object_path).unwrap(), before_sha);
    assert!(!library
        .join("members")
        .join(founder.member_id())
        .join("evil.jpg")
        .exists());
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(
        fs::metadata(&source).unwrap().modified().unwrap(),
        source_mtime
    );
    assert!(!pairing::state_directory(&library).starts_with(&library));
    assert!(!library.join("cert.der").exists());
}

struct Receiver {
    child: Child,
    output: String,
}

impl Receiver {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start albumen-receive");
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        let errors = Arc::new(Mutex::new(String::new()));
        let output_thread = Arc::clone(&output);
        let errors_thread = Arc::clone(&errors);
        std::thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&chunk[..n]);
                        *output_thread.lock().expect("stdout lock") =
                            String::from_utf8_lossy(&raw).into_owned();
                    }
                }
            }
        });
        std::thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&chunk[..n]);
                        *errors_thread.lock().expect("stderr lock") =
                            String::from_utf8_lossy(&raw).into_owned();
                    }
                }
            }
        });
        let started = Instant::now();
        loop {
            let text = output.lock().expect("stdout lock").clone();
            if text.contains("webdav https://") && text.contains("pairing-uri ") {
                std::thread::sleep(Duration::from_millis(50));
                let text = output.lock().expect("stdout lock").clone();
                return Self {
                    child,
                    output: text,
                };
            }
            if started.elapsed() > Duration::from_secs(20) {
                let err = errors.lock().expect("stderr lock").clone();
                let _ = child.kill();
                panic!("receiver did not print the pairing URI\n{text}\n{err}");
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    std::thread::sleep(Duration::from_millis(50));
                    let err = errors.lock().expect("stderr lock").clone();
                    let text = output.lock().expect("stdout lock").clone();
                    panic!("receiver exited {status}\n{text}\n{err}");
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(err) => panic!("receiver status: {err}"),
            }
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pairing_uri(text: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("pairing-uri "))
        .unwrap_or_else(|| panic!("missing pairing-uri in {text}"))
        .trim()
        .to_string()
}

fn webdav_addr(text: &str) -> SocketAddr {
    let line = text
        .lines()
        .find(|line| line.starts_with("webdav https://"))
        .unwrap_or_else(|| panic!("missing webdav line in {text}"));
    line.trim_start_matches("webdav https://")
        .parse()
        .unwrap_or_else(|err| panic!("webdav address in {line}: {err}"))
}

fn flipped_fingerprint(fingerprint: &str) -> String {
    let mut chars: Vec<char> = fingerprint.chars().collect();
    chars[0] = if chars[0] == '0' { '1' } else { '0' };
    chars.into_iter().collect()
}

fn header(response: &str, name: &str) -> Option<String> {
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

fn decode_png_qr(path: &Path) -> String {
    let file = fs::File::open(path).unwrap();
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let width = info.width as usize;
    let height = info.height as usize;
    let pixels = &buf[..info.buffer_size()];
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    let grids = image.detect_grids();
    assert_eq!(
        grids.len(),
        1,
        "QR grid was not found in {}",
        path.display()
    );
    grids[0].decode().unwrap().1
}

fn stamp(path: &Path, bytes: &[u8], mtime: SystemTime) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

fn tls_exchange(
    addr: SocketAddr,
    fingerprint: &str,
    user: &str,
    password: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<String, String> {
    let mut last = String::from("receiver did not accept a connection");
    for _ in 0..40 {
        match tls_once(addr, fingerprint, user, password, method, path, body) {
            Ok(response) => return Ok(response),
            Err(err) if err.contains("fingerprint") || err.contains("401") => return Err(err),
            Err(err) => {
                last = err;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Err(last)
}

fn tls_once(
    addr: SocketAddr,
    fingerprint: &str,
    user: &str,
    password: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<String, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|err| err.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier {
            fingerprint: fingerprint.to_string(),
        }))
        .with_no_client_auth();
    let server_name = ServerName::try_from("localhost").map_err(|err| err.to_string())?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|err| error_chain(&err))?;
    let mut tcp = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|err| err.to_string())?;
    tcp.set_read_timeout(Some(Duration::from_secs(10))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let mut tls = rustls::Stream::new(&mut connection, &mut tcp);
    let basic = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{user}:{password}"),
    );
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {basic}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tls.write_all(request.as_bytes())
        .map_err(|err| error_chain(&err))?;
    tls.write_all(body).map_err(|err| error_chain(&err))?;
    let mut response = Vec::new();
    tls.read_to_end(&mut response)
        .map_err(|err| error_chain(&err))?;
    String::from_utf8(response).map_err(|err| err.to_string())
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text
}

#[derive(Debug)]
struct PinVerifier {
    fingerprint: String,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let presented = hex_encode(&sha2::Sha256::digest(end_entity.as_ref()));
        match pairing::fingerprint_mismatch(&self.fingerprint, &presented) {
            None => Ok(ServerCertVerified::assertion()),
            Some(sentence) => Err(Error::General(sentence)),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
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
