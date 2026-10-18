// SPDX-License-Identifier: Apache-2.0

//! Camera-to-cloud receiver. The user runs it on their own machine.
//! WebDAV, SFTP, and FTP accept a finished upload and then call `copy_verify`.
//! Nothing is overwritten, and nothing already in the library is deleted.

mod ftp;
mod sftp;
mod store;
mod webdav;

#[cfg(test)]
mod roundtrip;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub use store::Library;

#[derive(Clone)]
pub struct Settings {
    pub library: PathBuf,
    pub user: String,
    pub password: String,
}

#[derive(Debug)]
pub struct Listening {
    pub webdav: Option<SocketAddr>,
    pub sftp: Option<SocketAddr>,
    pub ftp: Option<SocketAddr>,
}

pub async fn listen(
    settings: Settings,
    webdav: Option<SocketAddr>,
    sftp: Option<SocketAddr>,
    ftp: Option<SocketAddr>,
) -> std::io::Result<Listening> {
    if webdav.is_none() && sftp.is_none() && ftp.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pass at least one of webdav, sftp, or ftp",
        ));
    }
    let library = Arc::new(Library::open(&settings.library)?);
    let auth = Arc::new(webdav::Auth {
        user: settings.user.clone(),
        password: settings.password.clone(),
    });
    let webdav = match webdav {
        Some(addr) => Some(webdav::spawn(addr, Arc::clone(&library), auth).await?),
        None => None,
    };
    let sftp = match sftp {
        Some(addr) => Some(
            sftp::spawn(
                addr,
                Arc::clone(&library),
                settings.user.clone(),
                settings.password.clone(),
            )
            .await?,
        ),
        None => None,
    };
    let ftp = match ftp {
        Some(addr) => Some(ftp::spawn(addr, library, settings.user, settings.password).await?),
        None => None,
    };
    Ok(Listening { webdav, sftp, ftp })
}
