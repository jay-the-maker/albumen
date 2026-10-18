// SPDX-License-Identifier: Apache-2.0

//! FTP for older camera bodies. STOR commits through `copy_verify`.
//! DELE, RMD, and rename are refused. The transfer is not done until the
//! data connection has closed and the destination hash matches.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::store::{Arrival, Commit, Library, Spool};

pub async fn spawn(
    addr: SocketAddr,
    library: Arc<Library>,
    user: String,
    password: String,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let library = Arc::clone(&library);
                    let user = user.clone();
                    let password = password.clone();
                    tokio::spawn(async move {
                        if let Err(err) = session(socket, library, &user, &password).await {
                            eprintln!("albumen-receive ftp: {err}");
                        }
                    });
                }
                Err(err) => {
                    eprintln!("albumen-receive ftp accept: {err}");
                    break;
                }
            }
        }
    });
    Ok(bound)
}

struct Session {
    control: TcpStream,
    library: Arc<Library>,
    logged_in: bool,
    pending_user: String,
    pasv: Option<TcpListener>,
    port_target: Option<SocketAddr>,
    pending_mtime: Option<(String, String, u64)>,
}

async fn session(
    control: TcpStream,
    library: Arc<Library>,
    user: &str,
    password: &str,
) -> std::io::Result<()> {
    let mut session = Session {
        control,
        library,
        logged_in: false,
        pending_user: String::new(),
        pasv: None,
        port_target: None,
        pending_mtime: None,
    };
    session.reply(220, "albumen ready").await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = session.control.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > max_control_line() {
            session.reply(500, "line too long").await?;
            return Ok(());
        }
        while let Some(index) = buf.windows(2).position(|window| window == b"\r\n") {
            let line = String::from_utf8_lossy(&buf[..index]).into_owned();
            buf.drain(..index + 2);
            if !handle_line(&mut session, &line, user, password).await? {
                return Ok(());
            }
        }
    }
}

/// Control-connection DoS bound. `ALBUMEN_RECEIVE_MAX_LINE` overrides 16 KiB.
fn max_control_line() -> usize {
    match std::env::var("ALBUMEN_RECEIVE_MAX_LINE") {
        Ok(text) => match text.parse::<usize>() {
            Ok(value) if value >= 1 => value,
            _ => 16 * 1024,
        },
        Err(_) => 16 * 1024,
    }
}

async fn handle_line(
    session: &mut Session,
    line: &str,
    user: &str,
    password: &str,
) -> std::io::Result<bool> {
    let (verb, arg) = split_command(line);
    let verb = verb.to_ascii_uppercase();
    match verb.as_str() {
        "USER" => {
            session.logged_in = false;
            session.pending_user = arg.to_string();
            session.reply(331, "password required").await?;
        }
        "PASS" => {
            if session.pending_user == user && arg == password && !user.is_empty() {
                session.logged_in = true;
                session.reply(230, "logged in").await?;
            } else {
                session.logged_in = false;
                session.reply(530, "login incorrect").await?;
            }
        }
        "QUIT" => {
            session.reply(221, "goodbye").await?;
            return Ok(false);
        }
        "NOOP" => session.reply(200, "ok").await?,
        "SYST" => session.reply(215, "UNIX Type: L8").await?,
        "FEAT" => {
            session
                .control
                .write_all(
                    b"211-Features:\r\n SIZE\r\n MDTM\r\n MFMT\r\n PASV\r\n UTF8\r\n211 End\r\n",
                )
                .await?;
        }
        "OPTS" => session.reply(200, "ok").await?,
        "TYPE" => {
            if session.require_login().await? {
                if arg.eq_ignore_ascii_case("A") {
                    session
                        .reply(504, "binary only; ASCII would change the bytes")
                        .await?;
                } else {
                    session.reply(200, "type set").await?;
                }
            }
        }
        "STRU" | "MODE" | "PWD" | "XPWD" => {
            if !session.logged_in {
                session.reply(530, "login required").await?;
            } else if verb == "PWD" || verb == "XPWD" {
                session.reply(257, "\"/\"").await?;
            } else {
                session.reply(200, "ok").await?;
            }
        }
        "CWD" | "XCWD" | "CDUP" => {
            if session.require_login().await? {
                session
                    .reply(250, "directory unchanged; only filenames are stored")
                    .await?;
            }
        }
        "MKD" | "XMKD" => {
            if session.require_login().await? {
                session
                    .reply(257, "\"/\" directory names are not stored")
                    .await?;
            }
        }
        "PASV" => {
            if session.require_login().await? {
                session.passive().await?;
            }
        }
        "PORT" => {
            if session.require_login().await? {
                match parse_port(arg) {
                    Some(addr) => {
                        session.pasv = None;
                        session.port_target = Some(addr);
                        session.reply(200, "port accepted").await?;
                    }
                    None => session.reply(501, "bad PORT").await?,
                }
            }
        }
        "MFMT" | "MDTM" => {
            if session.require_login().await? {
                session.mtime_command(&verb, arg).await?;
            }
        }
        "STOR" => {
            if session.require_login().await? {
                session.store(arg).await?;
            }
        }
        "LIST" | "NLST" => {
            if session.require_login().await? {
                session.list(verb == "NLST").await?;
            }
        }
        "SIZE" => {
            if session.require_login().await? {
                match Arrival::parse(arg).map(|arrival| arrival.camera_filename) {
                    Ok(name) if session.library.has(&name) => {
                        let len = std::fs::metadata(session.library.root().join(&name))
                            .map(|meta| meta.len())
                            .unwrap_or(0);
                        session.reply(213, &len.to_string()).await?;
                    }
                    _ => session.reply(550, "not found").await?,
                }
            }
        }
        "DELE" | "RMD" | "XRMD" | "RNFR" | "RNTO" => {
            if session.require_login().await? {
                session
                    .reply(550, "albumen does not delete or rename")
                    .await?;
            }
        }
        "REST" | "APPE" => {
            if session.require_login().await? {
                session
                    .reply(502, "albumen does not append or resume over an original")
                    .await?;
            }
        }
        _ => session.reply(502, "command not implemented").await?,
    }
    Ok(true)
}

impl Session {
    async fn require_login(&mut self) -> std::io::Result<bool> {
        if self.logged_in {
            Ok(true)
        } else {
            self.reply(530, "login required").await?;
            Ok(false)
        }
    }

    async fn reply(&mut self, code: u16, text: &str) -> std::io::Result<()> {
        let text = text.replace(['\r', '\n'], " ");
        let line = format!("{code} {text}\r\n");
        self.control.write_all(line.as_bytes()).await
    }

    async fn passive(&mut self) -> std::io::Result<()> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        let bound = listener.local_addr()?;
        let port = bound.port();
        self.port_target = None;
        self.pasv = Some(listener);
        let host = match self.control.local_addr()?.ip() {
            IpAddr::V4(ip) if !ip.is_unspecified() => ip,
            _ => Ipv4Addr::LOCALHOST,
        };
        let octets = host.octets();
        self.reply(
            227,
            &format!(
                "Entering Passive Mode ({},{},{},{},{},{})",
                octets[0],
                octets[1],
                octets[2],
                octets[3],
                port / 256,
                port % 256
            ),
        )
        .await
    }

    async fn data_stream(&mut self) -> std::io::Result<TcpStream> {
        if let Some(listener) = self.pasv.take() {
            let (socket, _) = listener.accept().await?;
            return Ok(socket);
        }
        if let Some(addr) = self.port_target.take() {
            return TcpStream::connect(addr).await;
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "no data connection",
        ))
    }

    async fn store(&mut self, arg: &str) -> std::io::Result<()> {
        let arrival = match Arrival::parse(arg) {
            Ok(arrival) => arrival,
            Err(err) => return self.reply(553, &err).await,
        };
        if self.pasv.is_none() && self.port_target.is_none() {
            return self.reply(425, "use PASV or PORT first").await;
        }
        self.reply(150, "opening data connection").await?;
        let mut data = match self.data_stream().await {
            Ok(data) => data,
            Err(err) => return self.reply(425, &err.to_string()).await,
        };
        let mut spool = match Spool::create(&arrival.camera_filename) {
            Ok(spool) => spool,
            Err(err) => return self.reply(451, &err).await,
        };
        let mut buf = [0u8; 64 * 1024];
        let mut offset = 0u64;
        loop {
            let n = data.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if let Err(err) = spool.write_at(offset, &buf[..n]) {
                return self.reply(451, &err).await;
            }
            offset += n as u64;
        }
        drop(data);
        if let Some(seconds) = self.take_pending_mtime(&arrival) {
            if let Err(err) = spool.set_mtime(seconds) {
                return self.reply(451, &err).await;
            }
        }
        if let Err(err) = spool.finish() {
            return self.reply(451, &err).await;
        }
        match self
            .library
            .commit(&mut spool, &arrival, crate::store::MtimeProtocol::Ftp)
        {
            Ok(Commit::Stored { .. } | Commit::Deduped { .. }) => {
                self.reply(226, "transfer complete").await
            }
            Err(err) => self.reply(550, &err).await,
        }
    }

    fn take_pending_mtime(&mut self, arrival: &Arrival) -> Option<u64> {
        let (filename, directory, seconds) = self.pending_mtime.as_ref()?;
        if filename == &arrival.camera_filename && directory == &arrival.directory {
            let seconds = *seconds;
            self.pending_mtime = None;
            Some(seconds)
        } else {
            None
        }
    }

    async fn mtime_command(&mut self, verb: &str, arg: &str) -> std::io::Result<()> {
        let arg = arg.trim();
        if verb == "MDTM" && !arg_starts_with_ftp_time(arg) {
            let arrival = match Arrival::parse(arg) {
                Ok(arrival) => arrival,
                Err(err) => return self.reply(550, &err).await,
            };
            let path = self.library.root().join(&arrival.camera_filename);
            let modified = match std::fs::metadata(&path).and_then(|meta| meta.modified()) {
                Ok(modified) => modified,
                Err(_) => return self.reply(550, "not found").await,
            };
            let secs = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            return self.reply(213, &ftp_time(secs)).await;
        }
        let Some((time, path)) = arg.split_once(' ') else {
            return self.reply(501, "time and path required").await;
        };
        let Some(seconds) = parse_ftp_time(time) else {
            return self.reply(501, "time must be YYYYMMDDHHMMSS").await;
        };
        let arrival = match Arrival::parse(path) {
            Ok(arrival) => arrival,
            Err(err) => return self.reply(550, &err).await,
        };
        if self
            .library
            .note_source_mtime(path, seconds, crate::store::MtimeProtocol::Ftp)
            .is_ok()
        {
            return self.reply(213, time).await;
        }
        self.pending_mtime = Some((arrival.camera_filename, arrival.directory, seconds));
        self.reply(213, time).await
    }

    async fn list(&mut self, names_only: bool) -> std::io::Result<()> {
        if self.pasv.is_none() && self.port_target.is_none() {
            return self.reply(425, "use PASV or PORT first").await;
        }
        self.reply(150, "opening data connection").await?;
        let mut data = match self.data_stream().await {
            Ok(data) => data,
            Err(err) => return self.reply(425, &err.to_string()).await,
        };
        let mut text = String::new();
        for name in self.library.files().unwrap_or_default() {
            if names_only {
                text.push_str(&name);
                text.push_str("\r\n");
            } else {
                let len = std::fs::metadata(self.library.root().join(&name))
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                text.push_str(&format!(
                    "-rw-r--r-- 1 albumen albumen {len} Jan 1 00:00 {name}\r\n"
                ));
            }
        }
        data.write_all(text.as_bytes()).await?;
        drop(data);
        self.reply(226, "transfer complete").await
    }
}

fn arg_starts_with_ftp_time(arg: &str) -> bool {
    let Some(time) = arg.get(..14) else {
        return false;
    };
    time.bytes().all(|byte| byte.is_ascii_digit()) && arg.as_bytes().get(14) == Some(&b' ')
}

fn parse_ftp_time(text: &str) -> Option<u64> {
    if text.len() != 14 || !text.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let year: i32 = text[0..4].parse().ok()?;
    let month: u32 = text[4..6].parse().ok()?;
    let day: u32 = text[6..8].parse().ok()?;
    let hour: u32 = text[8..10].parse().ok()?;
    let minute: u32 = text[10..12].parse().ok()?;
    let second: u32 = text[12..14].parse().ok()?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_before_month(year, month)? + day - 1;
    let mut year_days = 0i64;
    for y in 1970..year {
        year_days += if is_leap(y) { 366 } else { 365 };
    }
    let stamp = (year_days + i64::from(days)) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);
    u64::try_from(stamp).ok()
}

fn ftp_time(seconds: u64) -> String {
    let days = seconds / 86_400;
    let time = seconds % 86_400;
    let hour = time / 3600;
    let minute = (time % 3600) / 60;
    let second = time % 60;
    let mut remaining = days as i64;
    let mut year = 1970i32;
    loop {
        let length = if is_leap(year) { 366 } else { 365 };
        if remaining < i64::from(length) {
            break;
        }
        remaining -= i64::from(length);
        year += 1;
    }
    let mut month = 1u32;
    while month < 12 {
        let length = days_before_month(year, month + 1).unwrap_or(0)
            - days_before_month(year, month).unwrap_or(0);
        if remaining < i64::from(length) {
            break;
        }
        remaining -= i64::from(length);
        month += 1;
    }
    let day = remaining + 1;
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_before_month(year: i32, month: u32) -> Option<u32> {
    const COMMON: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let base = *COMMON.get(month.checked_sub(1)? as usize)?;
    let leap = u32::from(month > 2 && is_leap(year));
    Some(base + leap)
}

fn split_command(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.split_once(' ') {
        Some((verb, arg)) => (verb, arg.trim()),
        None => (line, ""),
    }
}

fn parse_port(arg: &str) -> Option<SocketAddr> {
    let nums: Vec<u8> = arg
        .split(',')
        .map(|part| part.trim().parse().ok())
        .collect::<Option<_>>()?;
    if nums.len() != 6 {
        return None;
    }
    let ip = Ipv4Addr::new(nums[0], nums[1], nums[2], nums[3]);
    let port = u16::from(nums[4]) * 256 + u16::from(nums[5]);
    Some(SocketAddr::from((ip, port)))
}
