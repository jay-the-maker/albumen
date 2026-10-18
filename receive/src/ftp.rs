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
        if buf.len() > 16 * 1024 {
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
                .write_all(b"211-Features:\r\n SIZE\r\n MDTM\r\n PASV\r\n UTF8\r\n211 End\r\n")
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
        if let Err(err) = spool.finish() {
            return self.reply(451, &err).await;
        }
        match self.library.commit(&mut spool, &arrival) {
            Ok(Commit::Stored { .. } | Commit::Deduped { .. }) => {
                self.reply(226, "transfer complete").await
            }
            Err(err) => self.reply(550, &err).await,
        }
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
