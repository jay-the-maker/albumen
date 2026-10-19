// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` listens for a camera and stores each finished file with
//! `copy_verify`. It does not overwrite, and it has no delete command.
//! Without `--apply` or `--execute` it prints the plan and opens no sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use std::io::Write;

use albumen_ingest::dest;
use albumen_receive::secrets;
use albumen_receive::service::{self, ServiceHost, ServiceSpec};
use albumen_receive::{listen, Settings};
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
    let settings = Settings {
        library: cli.to,
        user: cli.user,
        password,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(async move {
        let listening = listen(settings, cli.webdav, cli.sftp, cli.ftp)
            .await
            .map_err(|err| err.to_string())?;
        if let Some(addr) = listening.webdav {
            println!("webdav http://{addr}");
        }
        if let Some(addr) = listening.sftp {
            println!("sftp {addr}");
        }
        if let Some(addr) = listening.ftp {
            println!("ftp {addr}");
        }
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::future::pending::<()>().await;
        Ok(())
    })
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
    println!("  webdav: {}", addr_text(cli.webdav));
    println!("  sftp: {}", addr_text(cli.sftp));
    println!("  ftp: {}", addr_text(cli.ftp));
    if cli.webdav.is_none() && cli.sftp.is_none() && cli.ftp.is_none() {
        println!("  would-refuse: pass at least one of --webdav, --sftp, or --ftp");
    } else {
        println!("  network: listen on the addresses above. No outbound connection.");
    }
    println!("  writes: copy_verify into the library. Never overwrite. No delete.");
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
