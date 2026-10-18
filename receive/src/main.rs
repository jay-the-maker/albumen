// SPDX-License-Identifier: Apache-2.0

//! `albumen-receive` listens for a camera and stores each finished file with
//! `copy_verify`. It does not overwrite, and it has no delete command.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use albumen_receive::{listen, Settings};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "albumen-receive",
    about = "Receive camera uploads over WebDAV, SFTP, and FTP. Writes only through copy_verify."
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
