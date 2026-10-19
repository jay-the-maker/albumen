// SPDX-License-Identifier: Apache-2.0

//! Count the printed camera-setup steps and upload a synthetic RAW over the
//! WebDAV and FTP settings on that line. The file is generated here and is
//! not a photograph. This is an ordinary `cargo test`, so the ingest matrix
//! runs it on every OS leg.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use albumen_ingest::sha256_hex;
use albumen_receive::camera_setup::{
    self, setup_steps_exceed_budget, verify_exceeds_budget, PASSWORD_PHRASE, STEPS_PREFIX,
};
use base64::Engine;

const PASSWORD: &str = "camera-secret-not-printed";
const RAW_BYTES: u64 = 100_000_000;
const USER: &str = "camera";

#[test]
fn dry_run_counts_five_camera_setup_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let output = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
        .args([
            "--to",
            library.to_str().unwrap(),
            "--user",
            USER,
            "--password",
            PASSWORD,
            "--webdav",
            "127.0.0.1:8080",
            "--ftp",
            "127.0.0.1:2121",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(text.contains("camera setup"), "{text}");
    assert!(text.contains("dry-run"), "{text}");
    let steps = parse_steps(&text);
    assert_eq!(step_name(&steps, 0), "enter host");
    assert_eq!(step_str(&steps, "enter host", "webdav"), "127.0.0.1");
    assert_eq!(step_str(&steps, "enter port", "ftp"), "2121");
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!err.contains(PASSWORD), "{err}");
    assert!(!library.exists());
}

#[test]
fn printed_settings_upload_a_hundred_megabyte_raw_within_five_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let webdav_source = tmp.path().join("SYNTHETIC-WEBDAV.RAW");
    let ftp_source = tmp.path().join("SYNTHETIC-FTP.RAW");
    write_synthetic(&webdav_source, 1);
    write_synthetic(&ftp_source, 2);
    let webdav_mtime = stamp_mtime(&webdav_source);
    let ftp_mtime = stamp_mtime(&ftp_source);

    let running = Running::start(&[
        "--to",
        library.to_str().unwrap(),
        "--user",
        USER,
        "--password",
        PASSWORD,
        "--webdav",
        "127.0.0.1:0",
        "--ftp",
        "127.0.0.1:0",
        "--apply",
    ]);
    let text = running.stdout.lock().expect("stdout").clone();
    assert!(
        text.lines().next().unwrap_or("").contains("webdav"),
        "{text}"
    );
    assert!(text.contains("camera setup\n"), "{text}");
    let steps = parse_steps(&text);
    let webdav_host = step_str(&steps, "enter host", "webdav");
    let webdav_port: u16 = step_str(&steps, "enter port", "webdav").parse().unwrap();
    let ftp_host = step_str(&steps, "enter host", "ftp");
    let ftp_port: u16 = step_str(&steps, "enter port", "ftp").parse().unwrap();
    let user = step_str(&steps, "enter user", "value");
    let path = step_str(&steps, "enter password or path", "path");
    assert_eq!(
        step_str(&steps, "enter password or path", "password"),
        PASSWORD_PHRASE
    );
    assert_eq!(step_str(&steps, "enable auto-transfer", "value"), "on");
    assert_eq!(user, USER);
    assert!(
        text.contains(&format!("webdav url: http://{webdav_host}:{webdav_port}/")),
        "{text}"
    );
    assert!(text.contains(&format!("ftp host: {ftp_host}")), "{text}");
    assert!(text.contains(&format!("ftp port: {ftp_port}")), "{text}");
    assert!(text.contains(&format!("webdav user: {user}")), "{text}");
    assert!(text.contains(&format!("ftp user: {user}")), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");

    let response = webdav_put(
        &webdav_host,
        webdav_port,
        &path,
        &user,
        PASSWORD,
        &webdav_source,
    );
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    let text = running.wait_for("camera-verify protocol=webdav ");
    let webdav_line = verify_line(&text, "webdav");
    assert_eq!(webdav_line.bytes, RAW_BYTES, "{webdav_line:?}");
    assert!(
        !verify_exceeds_budget(webdav_line.verify_ms),
        "webdav verify_ms={} exceeds {}",
        webdav_line.verify_ms,
        camera_setup::MAX_SHA256_VERIFY_MS
    );
    assert_eq!(
        header(&response, "X-Albumen-Verify-Ms").as_deref(),
        Some(webdav_line.verify_ms.to_string()).as_deref()
    );
    assert_eq!(
        header(&response, "X-Albumen-Sha256").as_deref(),
        Some(webdav_line.sha256.as_str())
    );

    ftp_store(&ftp_host, ftp_port, &user, PASSWORD, &path, &ftp_source);
    let text = running.wait_for("camera-verify protocol=ftp ");
    let ftp_line = verify_line(&text, "ftp");
    assert_eq!(ftp_line.bytes, RAW_BYTES, "{ftp_line:?}");
    assert!(
        !verify_exceeds_budget(ftp_line.verify_ms),
        "ftp verify_ms={} exceeds {}",
        ftp_line.verify_ms,
        camera_setup::MAX_SHA256_VERIFY_MS
    );

    let stored_webdav = library.join("SYNTHETIC-WEBDAV.RAW");
    let stored_ftp = library.join("SYNTHETIC-FTP.RAW");
    assert_files_identical(&webdav_source, &stored_webdav);
    assert_files_identical(&ftp_source, &stored_ftp);
    assert_eq!(sha256_hex(&stored_webdav).unwrap(), webdav_line.sha256);
    assert_eq!(sha256_hex(&stored_ftp).unwrap(), ftp_line.sha256);
    assert_eq!(sha256_hex(&webdav_source).unwrap(), webdav_line.sha256);
    assert_eq!(
        fs::metadata(&webdav_source).unwrap().modified().unwrap(),
        webdav_mtime
    );
    assert_eq!(
        fs::metadata(&ftp_source).unwrap().modified().unwrap(),
        ftp_mtime
    );
    let err = running.stderr.lock().expect("stderr").clone();
    let text = running.stdout.lock().expect("stdout").clone();
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(!err.contains(PASSWORD), "{err}");
    println!(
        "camera-verify-measured webdav_verify_ms={} ftp_verify_ms={}",
        webdav_line.verify_ms, ftp_line.verify_ms
    );
}

struct Running {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl Running {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_albumen-receive"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start albumen-receive");
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stdout_thread = Arc::clone(&stdout_buf);
        let stderr_thread = Arc::clone(&stderr_buf);
        thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&chunk[..n]);
                        *stdout_thread.lock().expect("stdout") =
                            String::from_utf8_lossy(&raw).into_owned();
                    }
                }
            }
        });
        thread::spawn(move || {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&chunk[..n]);
                        *stderr_thread.lock().expect("stderr") =
                            String::from_utf8_lossy(&raw).into_owned();
                    }
                }
            }
        });
        let started = Instant::now();
        loop {
            let text = stdout_buf.lock().expect("stdout").clone();
            if text.contains(STEPS_PREFIX) && text.contains("ftp ") {
                thread::sleep(Duration::from_millis(50));
                return Self {
                    child,
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                };
            }
            if started.elapsed() > Duration::from_secs(20) {
                let err = stderr_buf.lock().expect("stderr").clone();
                let _ = child.kill();
                panic!("receiver did not print camera setup\n{text}\n{err}");
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    thread::sleep(Duration::from_millis(50));
                    let err = stderr_buf.lock().expect("stderr").clone();
                    let text = stdout_buf.lock().expect("stdout").clone();
                    panic!("receiver exited {status}\n{text}\n{err}");
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(err) => panic!("receiver status: {err}"),
            }
        }
    }

    fn wait_for(&self, needle: &str) -> String {
        let started = Instant::now();
        loop {
            let text = self.stdout.lock().expect("stdout").clone();
            if text.contains(needle) {
                return text;
            }
            if started.elapsed() > Duration::from_secs(180) {
                let err = self.stderr.lock().expect("stderr").clone();
                panic!("timed out waiting for {needle}\n{text}\n{err}");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_steps(text: &str) -> Vec<BTreeMap<String, String>> {
    let line = text
        .lines()
        .find(|line| line.starts_with(STEPS_PREFIX))
        .unwrap_or_else(|| panic!("missing {STEPS_PREFIX} in {text}"));
    let json = line.strip_prefix(STEPS_PREFIX).unwrap();
    let count = declared_count(json);
    let steps = step_objects(json);
    assert_eq!(
        steps.len(),
        count,
        "declared count does not match the list\n{json}"
    );
    assert!(
        !setup_steps_exceed_budget(count),
        "camera setup has {count} steps, above {}",
        camera_setup::MAX_CAMERA_SETUP_STEPS
    );
    let names: Vec<&str> = steps.iter().map(|step| step["step"].as_str()).collect();
    assert_eq!(
        names,
        [
            "enter host",
            "enter port",
            "enter user",
            "enter password or path",
            "enable auto-transfer",
        ]
    );
    steps
}

fn declared_count(json: &str) -> usize {
    let key = "\"count\":";
    let at = json
        .find(key)
        .unwrap_or_else(|| panic!("missing count in {json}"));
    let digits: String = json[at + key.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("bad count in {json}"))
}

fn step_objects(json: &str) -> Vec<BTreeMap<String, String>> {
    let marker = "\"steps\":[";
    let start = json
        .find(marker)
        .unwrap_or_else(|| panic!("missing steps in {json}"))
        + marker.len();
    let end = json
        .rfind(']')
        .unwrap_or_else(|| panic!("steps not closed in {json}"));
    let body = &json[start..end];
    let mut objects = Vec::new();
    let mut depth = 0i32;
    let mut began = None;
    for (index, ch) in body.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    began = Some(index);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let start = began.take().expect("object start");
                    objects.push(parse_object(&body[start..=index]));
                }
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unbalanced steps in {json}");
    objects
}

fn parse_object(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut rest = text.trim();
    assert!(rest.starts_with('{') && rest.ends_with('}'), "{text}");
    rest = rest[1..rest.len() - 1].trim();
    while !rest.is_empty() {
        let (key, after_key) = json_string(rest);
        rest = after_key.trim_start();
        assert!(rest.starts_with(':'), "{text}");
        rest = rest[1..].trim_start();
        let (value, after_value) = json_string(rest);
        map.insert(key, value);
        rest = after_value.trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
        }
    }
    map
}

fn json_string(text: &str) -> (String, &str) {
    let mut chars = text.chars();
    assert_eq!(chars.next(), Some('"'), "{text}");
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                let consumed = text.len() - chars.as_str().len();
                return (out, &text[consumed..]);
            }
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => panic!("unsupported escape {other} in {text}"),
                None => panic!("truncated escape in {text}"),
            },
            ch => out.push(ch),
        }
    }
    panic!("unterminated string in {text}");
}

fn step_name(steps: &[BTreeMap<String, String>], index: usize) -> String {
    steps[index]["step"].clone()
}

fn step_str(steps: &[BTreeMap<String, String>], name: &str, field: &str) -> String {
    let step = steps
        .iter()
        .find(|step| step.get("step").map(String::as_str) == Some(name))
        .unwrap_or_else(|| panic!("missing step {name}"));
    step.get(field)
        .unwrap_or_else(|| panic!("missing {field} on {name}"))
        .clone()
}

#[derive(Debug)]
struct Verify {
    bytes: u64,
    sha256: String,
    verify_ms: u128,
}

fn verify_line(text: &str, protocol: &str) -> Verify {
    let prefix = format!("camera-verify protocol={protocol} ");
    let line = text
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {prefix} in {text}"));
    Verify {
        bytes: field(line, "bytes").parse().unwrap(),
        sha256: field(line, "sha256"),
        verify_ms: field(line, "verify_ms").parse().unwrap(),
    }
}

fn field(line: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing {key} in {line}"))
        .to_string()
}

fn write_synthetic(path: &Path, seed: u8) {
    let mut file = File::create(path).unwrap();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut offset = 0u64;
    while offset < RAW_BYTES {
        let n = ((RAW_BYTES - offset) as usize).min(buf.len());
        for (index, byte) in buf[..n].iter_mut().enumerate() {
            let pos = offset + index as u64;
            *byte = seed.wrapping_add(pos as u8).wrapping_add((pos >> 8) as u8);
        }
        if offset == 0 {
            let prefix = b"ALBUMEN-SYNTHETIC-RAW\n";
            buf[..prefix.len()].copy_from_slice(prefix);
            buf[prefix.len()] = seed;
        }
        file.write_all(&buf[..n]).unwrap();
        offset += n as u64;
    }
    file.sync_all().unwrap();
}

fn stamp_mtime(path: &Path) -> SystemTime {
    let mtime = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    fs::metadata(path).unwrap().modified().unwrap()
}

fn webdav_put(
    host: &str,
    port: u16,
    path: &str,
    user: &str,
    password: &str,
    file: &Path,
) -> String {
    let filename = file.file_name().unwrap().to_str().unwrap();
    let target = if path.ends_with('/') {
        format!("{path}{filename}")
    } else {
        format!("{path}/{filename}")
    };
    let len = fs::metadata(file).unwrap().len();
    let mut stream = TcpStream::connect(format!("{host}:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .unwrap();
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    let header = format!(
        "PUT {target} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Basic {token}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes()).unwrap();
    let mut input = File::open(file).unwrap();
    std::io::copy(&mut input, &mut stream).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn ftp_store(host: &str, port: u16, user: &str, password: &str, path: &str, file: &Path) {
    let filename = file.file_name().unwrap().to_str().unwrap();
    let mut control = TcpStream::connect(format!("{host}:{port}")).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(180)))
        .unwrap();
    let greeting = ftp_line(&mut control);
    assert!(greeting.starts_with("220"), "{greeting}");
    let user_reply = ftp_cmd(&mut control, &format!("USER {user}"));
    assert!(user_reply.starts_with("331"), "{user_reply}");
    let pass_reply = ftp_cmd(&mut control, &format!("PASS {password}"));
    assert!(pass_reply.starts_with("230"), "{pass_reply}");
    let typed = ftp_cmd(&mut control, "TYPE I");
    assert!(typed.starts_with("200"), "{typed}");
    if path != "/" {
        let cwd = ftp_cmd(&mut control, &format!("CWD {path}"));
        assert!(cwd.starts_with("250") || cwd.starts_with("200"), "{cwd}");
    }
    let pasv = ftp_cmd(&mut control, "PASV");
    assert!(pasv.starts_with("227"), "{pasv}");
    let data_addr = parse_pasv(&pasv);
    let early = ftp_cmd(&mut control, &format!("STOR {filename}"));
    assert!(early.starts_with("150"), "{early}");
    let mut data = TcpStream::connect(data_addr).unwrap();
    let mut input = File::open(file).unwrap();
    std::io::copy(&mut input, &mut data).unwrap();
    drop(data);
    let done = ftp_line(&mut control);
    assert!(done.starts_with("226"), "{done}");
    let _ = ftp_cmd(&mut control, "QUIT");
}

fn ftp_cmd(control: &mut TcpStream, command: &str) -> String {
    control
        .write_all(format!("{command}\r\n").as_bytes())
        .unwrap();
    ftp_line(control)
}

fn ftp_line(control: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = control.read(&mut byte).expect("ftp read");
        assert_eq!(n, 1, "ftp closed");
        buf.push(byte[0]);
        if byte[0] == b'\n' || buf.len() > 4096 {
            break;
        }
    }
    String::from_utf8_lossy(&buf).trim().to_string()
}

fn parse_pasv(line: &str) -> std::net::SocketAddr {
    let start = line.find('(').expect("pasv");
    let end = line.find(')').expect("pasv");
    let nums: Vec<u8> = line[start + 1..end]
        .split(',')
        .map(|part| part.trim().parse().unwrap())
        .collect();
    let port = u16::from(nums[4]) * 256 + u16::from(nums[5]);
    std::net::SocketAddr::from(([nums[0], nums[1], nums[2], nums[3]], port))
}

fn header(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn assert_files_identical(left: &Path, right: &Path) {
    let mut a = File::open(left).unwrap();
    let mut b = File::open(right).unwrap();
    assert_eq!(
        a.metadata().unwrap().len(),
        b.metadata().unwrap().len(),
        "{}",
        right.display()
    );
    let mut buf_a = vec![0u8; 1024 * 1024];
    let mut buf_b = vec![0u8; 1024 * 1024];
    let mut offset = 0u64;
    loop {
        let n = a.read(&mut buf_a).unwrap();
        if n == 0 {
            assert_eq!(b.read(&mut buf_b[..1]).unwrap(), 0);
            break;
        }
        b.read_exact(&mut buf_b[..n])
            .unwrap_or_else(|err| panic!("{err} at {offset}"));
        if buf_a[..n] != buf_b[..n] {
            panic!("stored copy differs at byte {offset}");
        }
        offset += n as u64;
    }
}
