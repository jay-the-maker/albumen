// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` listens for a camera and stores each finished file with
//! `copy_verify`. It does not overwrite, and it has no delete command.
//! Without `--apply` or `--execute` it prints the plan and opens no sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use std::io::Write;

use albumen_ingest::dest;
use albumen_receive::pairing;
use albumen_receive::secrets;
use albumen_receive::service::{self, ServiceHost, ServiceSpec};
use albumen_receive::{listen, listen_paired, Listening, Settings};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "albumen-receive",
    about = "Receive camera uploads over WebDAV, SFTP, and FTP. Writes only through copy_verify, and only with --apply or --execute."
)]
struct Cli {
    /// Library directory. Existing files are never overwritten.
    #[arg(long)]
    to: PathBuf,
    /// Username the camera sends.
    #[arg(long)]
    user: String,
    /// Password the camera sends. Defaults to ALBUMEN_RECEIVE_PASSWORD.
    #[arg(long)]
    password: Option<String>,
    /// WebDAV listen address, for example 0.0.0.0:8080.
    #[arg(long)]
    webdav: Option<SocketAddr>,
    /// SFTP listen address, for example 0.0.0.0:2222.
    #[arg(long)]
    sftp: Option<SocketAddr>,
    /// FTP listen address, for example 0.0.0.0:2121. FTP is unencrypted.
    #[arg(long)]
    ftp: Option<SocketAddr>,
    /// Ed25519 public key of the joined member. The library must be a family
    /// library, and uploads are stored only under `members/<member>/`.
    /// With `--pair`, the same key is included in the pairing URI.
    #[arg(long)]
    member: Option<String>,
    /// Serve WebDAV over TLS and show a pairing QR. The certificate is
    /// generated on first run and pinned by its SHA-256 fingerprint.
    #[arg(long)]
    pair: bool,
    /// Write the pairing QR as a PNG. Requires `--pair`. The file is written
    /// only with `--apply` or `--execute`.
    #[arg(long)]
    qr_png: Option<PathBuf>,
    /// Host name in the pairing URI when `--webdav` is 0.0.0.0 or ::.
    /// A host name or IP address, without a scheme or port.
    #[arg(long)]
    advertise: Option<String>,
    /// Listen and store uploads. Without this or `--execute`, print the plan and write nothing.
    #[arg(long)]
    apply: bool,
    /// Same as `--apply`.
    #[arg(long)]
    execute: bool,
    /// Read the password from the keyring instead of `--password` or the environment.
    ///
    /// macOS uses the Keychain, Windows uses Credential Manager, and Linux uses
    /// the Secret Service. The entry name is `albumen-receive`.
    #[arg(long)]
    keyring: bool,
    /// Store `--password` or `ALBUMEN_RECEIVE_PASSWORD` in the keyring. Requires
    /// `--apply`. The password is not printed.
    #[arg(long)]
    keyring_set: bool,
    /// Print the service definition for this operating system and write nothing.
    #[arg(long)]
    print_service: bool,
    /// With `--apply`, write the service definition into this directory. The
    /// service is not started.
    #[arg(long)]
    service_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("albumen-receive: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    if cli.print_service || cli.service_dir.is_some() {
        return service_action(&cli);
    }
    if cli.keyring && cli.password.is_some() {
        return Err("pass --password or --keyring, not both".to_string());
    }
    if cli.qr_png.is_some() && !cli.pair {
        return Err("--qr-png requires --pair".to_string());
    }
    if cli.advertise.is_some() && !cli.pair {
        return Err("--advertise requires --pair".to_string());
    }
    if cli.keyring_set && !(cli.apply || cli.execute) {
        println!("dry-run: keyring unchanged. Pass --apply or --execute to store the password.");
        println!("  user: {}", cli.user);
        println!("  password: keyring");
        return Ok(());
    }
    if !(cli.apply || cli.execute) {
        print_plan(&cli);
        return Ok(());
    }
    if cli.user.is_empty() {
        return Err("user must not be empty".to_string());
    }
    if cli.keyring_set {
        let password = password_from_flag_or_env(cli.password.as_deref())?;
        secrets::store_password(&cli.user, &password)?;
        println!("keyring: stored for user {}", cli.user);
        if !cli.keyring {
            return Ok(());
        }
    }
    let password = if cli.keyring {
        secrets::load_password(&cli.user)?
    } else {
        password_from_flag_or_env(cli.password.as_deref())?
    };
    if password.is_empty() {
        return Err("password must not be empty".to_string());
    }
    let user = cli.user.clone();
    let settings = Settings {
        library: cli.to.clone(),
        user: cli.user,
        password,
        member: cli.member.clone(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(async move {
        let (listening, session) = if cli.pair {
            let Some(webdav) = cli.webdav else {
                return Err("--pair requires --webdav".to_string());
            };
            let (listening, session) = listen_paired(
                settings,
                webdav,
                cli.sftp,
                cli.ftp,
                cli.advertise.as_deref(),
            )
            .await
            .map_err(|err| err.to_string())?;
            (listening, Some(session))
        } else {
            let listening = listen(settings, cli.webdav, cli.sftp, cli.ftp)
                .await
                .map_err(|err| err.to_string())?;
            (listening, None)
        };
        if let Some(addr) = listening.webdav {
            let scheme = if cli.pair { "https" } else { "http" };
            println!("webdav {scheme}://{addr}");
        }
        if let Some(addr) = listening.sftp {
            println!("sftp {addr}");
        }
        if let Some(addr) = listening.ftp {
            println!("ftp {addr}");
        }
        if let Some(session) = session {
            let addr = listening.webdav.ok_or("pairing WebDAV did not bind")?;
            let endpoint = pairing::endpoint_url(addr, cli.advertise.as_deref())?;
            let uri = pairing::format_uri(
                &endpoint,
                &session.fingerprint,
                session.token.one_time_token(),
                cli.member.as_deref(),
            )?;
            if let Some(path) = &cli.qr_png {
                pairing::write_qr_png(path, &uri)?;
                println!("qr-png {}", path.display());
            }
            println!("{}", pairing::qr_terminal(&uri)?);
            println!("pairing-uri {uri}");
            println!("pairing-fingerprint {}", session.fingerprint);
        }
        print_camera_settings(&listening, &user, cli.pair);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::future::pending::<()>().await;
        Ok(())
    })
}

fn print_camera_settings(listening: &Listening, user: &str, paired: bool) {
    println!("camera setup");
    if let Some(addr) = listening.webdav {
        let scheme = if paired { "https" } else { "http" };
        println!("  webdav url: {scheme}://{addr}/");
        println!("  webdav user: {user}");
        println!("  webdav password: the password for this process");
        if paired {
            println!("  webdav tls: certificate pinned by the pairing QR");
        } else {
            println!("  webdav tls: off");
        }
    } else {
        println!("  webdav: off");
    }
    if let Some(addr) = listening.ftp {
        println!("  ftp host: {}", addr.ip());
        println!("  ftp port: {}", addr.port());
        println!("  ftp user: {user}");
        println!("  ftp encryption: none");
        println!("  ftp password: the password for this process");
    } else {
        println!("  ftp: off");
    }
    if let Some(addr) = listening.sftp {
        println!("  sftp host: {}", addr.ip());
        println!("  sftp port: {}", addr.port());
        println!("  sftp user: {user}");
        println!("  sftp password: the password for this process");
    }
}

fn print_plan(cli: &Cli) {
    println!(
        "dry-run: no sockets opened and no files written. Pass --apply or --execute to listen."
    );
    println!("receive");
    println!("  to: {}", cli.to.display());
    println!("  user: {}", cli.user);
    if cli.keyring {
        println!("  password: keyring");
    } else {
        match &cli.password {
            Some(password) if !password.is_empty() => println!("  password: set"),
            _ if std::env::var("ALBUMEN_RECEIVE_PASSWORD")
                .ok()
                .is_some_and(|value| !value.is_empty()) =>
            {
                println!("  password: set");
            }
            _ => println!("  password: unset"),
        }
    }
    match &cli.member {
        Some(member) if !member.is_empty() => {
            println!("  member: {member}");
            println!("  namespace: members/{member}/");
        }
        Some(_) => println!("  member: empty"),
        None => println!("  member: unset"),
    }
    println!("  webdav: {}", addr_text(cli.webdav));
    println!("  sftp: {}", addr_text(cli.sftp));
    println!("  ftp: {}", addr_text(cli.ftp));
    if cli.pair {
        println!(
            "  pairing: would generate a self-signed certificate on first run and show a QR code"
        );
        println!("  pairing-uri: albumen-pair://v1 with the endpoint, certificate fingerprint, one-time token, and member when set");
        match &cli.qr_png {
            Some(path) => println!("  qr-png: would write {}", path.display()),
            None => println!("  qr-png: off"),
        }
        match &cli.advertise {
            Some(host) => println!("  advertise: {host}"),
            None => println!("  advertise: the bound address"),
        }
    } else {
        println!("  pairing: off");
    }
    print_planned_camera_settings(cli);
    if cli.pair && cli.webdav.is_none() {
        println!("  would-refuse: --pair requires --webdav");
    } else if cli.webdav.is_none() && cli.sftp.is_none() && cli.ftp.is_none() {
        println!("  would-refuse: pass at least one of --webdav, --sftp, or --ftp");
    } else if cli.pair
        && cli.advertise.is_none()
        && cli.webdav.is_some_and(|addr| addr.ip().is_unspecified())
    {
        println!("  would-refuse: --pair on 0.0.0.0 or :: needs --advertise <host>");
    } else {
        println!("  network: listen on the addresses above. No outbound connection.");
    }
    println!("  writes: copy_verify into the library. Never overwrite. No delete.");
}

fn print_planned_camera_settings(cli: &Cli) {
    println!("camera setup");
    if let Some(addr) = cli.webdav {
        let scheme = if cli.pair { "https" } else { "http" };
        println!("  webdav url: {scheme}://{addr}/");
        println!("  webdav user: {}", cli.user);
        println!("  webdav password: the password for this process");
        if cli.pair {
            println!("  webdav tls: certificate pinned by the pairing QR");
        } else {
            println!("  webdav tls: off");
        }
    } else {
        println!("  webdav: off");
    }
    if let Some(addr) = cli.ftp {
        println!("  ftp host: {}", addr.ip());
        println!("  ftp port: {}", addr.port());
        println!("  ftp user: {}", cli.user);
        println!("  ftp encryption: none");
        println!("  ftp password: the password for this process");
    } else {
        println!("  ftp: off");
    }
}

fn password_from_flag_or_env(password: Option<&str>) -> Result<String, String> {
    match password {
        Some(password) if !password.is_empty() => Ok(password.to_string()),
        Some(_) => Err("password must not be empty".to_string()),
        None => std::env::var("ALBUMEN_RECEIVE_PASSWORD")
            .map_err(|_| "pass --password or set ALBUMEN_RECEIVE_PASSWORD".to_string())
            .and_then(|password| {
                if password.is_empty() {
                    Err("password must not be empty".to_string())
                } else {
                    Ok(password)
                }
            }),
    }
}

fn service_action(cli: &Cli) -> Result<(), String> {
    let binary = std::env::current_exe().map_err(|err| err.to_string())?;
    let spec = ServiceSpec {
        binary: &binary,
        library: &cli.to,
        user: &cli.user,
        webdav: cli.webdav,
        sftp: cli.sftp,
        ftp: cli.ftp,
        member: cli.member.as_deref(),
    };
    let host = ServiceHost::current();
    let text = service::render(host, &spec)?;
    let Some(dir) = &cli.service_dir else {
        println!("{text}");
        println!("dry-run: service file not written and service not started.");
        return Ok(());
    };
    if !(cli.apply || cli.execute) {
        println!("{text}");
        println!(
            "dry-run: would write {} and would not start it. Pass --apply or --execute to write the file.",
            dir.join(host.file_name()).display()
        );
        return Ok(());
    }
    dest::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = dir.join(host.file_name());
    let mut file = dest::open_new(&path).map_err(|err| err.to_string())?;
    file.write_all(text.as_bytes())
        .map_err(|err| err.to_string())?;
    println!("service file {}", path.display());
    println!("service not started. Load it with the steps in docs/platforms.md.");
    Ok(())
}

fn addr_text(addr: Option<SocketAddr>) -> String {
    match addr {
        Some(addr) => addr.to_string(),
        None => "off".to_string(),
    }
}
