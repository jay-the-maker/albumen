// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` listens for a camera and stores each finished file with
//! `copy_verify`. It does not overwrite, and it has no delete command.
//! Without `--apply` or `--execute` it prints the plan and opens no sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

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
    if !(cli.apply || cli.execute) {
        print_plan(&cli);
        return Ok(());
    }
    let password = match cli.password {
        Some(password) => password,
        None => std::env::var("ALBUMEN_RECEIVE_PASSWORD")
            .map_err(|_| "pass --password or set ALBUMEN_RECEIVE_PASSWORD".to_string())?,
    };
    if password.is_empty() || cli.user.is_empty() {
        return Err("user and password must not be empty".to_string());
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

fn addr_text(addr: Option<SocketAddr>) -> String {
    match addr {
        Some(addr) => addr.to_string(),
        None => "off".to_string(),
    }
}
