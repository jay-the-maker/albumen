// SPDX-License-Identifier: Apache-2.0

//! Camera-to-cloud receiver. The user runs it on their own machine.
//! WebDAV, SFTP, and FTP accept a finished upload and then call `copy_verify`.
//! Nothing is overwritten, and nothing already in the library is deleted.
//! A hide or a failed receipt writes a tombstone and leaves the original.

pub mod camera_setup;
mod ftp;
pub mod pairing;
pub mod secrets;
pub mod service;
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
    /// Ed25519 public key of the joined member. When set, uploads are stored
    /// only under `members/<member>/` in a family library.
    pub member: Option<String>,
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
    let (listening, _) = listen_inner(settings, webdav, sftp, ftp, None).await?;
    Ok(listening)
}

/// Listen with WebDAV on TLS. The certificate was generated for this library
/// and is pinned by the pairing URI. SFTP and FTP stay as they were.
pub async fn listen_paired(
    settings: Settings,
    webdav: SocketAddr,
    sftp: Option<SocketAddr>,
    ftp: Option<SocketAddr>,
    advertise: Option<&str>,
) -> std::io::Result<(Listening, pairing::PairingSession)> {
    let (listening, session) =
        listen_inner(settings, Some(webdav), sftp, ftp, Some(advertise)).await?;
    let session = session.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pairing session was not created",
        )
    })?;
    Ok((listening, session))
}

async fn listen_inner(
    settings: Settings,
    webdav: Option<SocketAddr>,
    sftp: Option<SocketAddr>,
    ftp: Option<SocketAddr>,
    pair: Option<Option<&str>>,
) -> std::io::Result<(Listening, Option<pairing::PairingSession>)> {
    if webdav.is_none() && sftp.is_none() && ftp.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pass at least one of webdav, sftp, or ftp",
        ));
    }
    if pair.is_some() && webdav.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--pair requires --webdav",
        ));
    }
    // Open the library before any pairing file is written. A refused family
    // root leaves the certificate directory uncreated.
    let library = Arc::new(match &settings.member {
        Some(member) => Library::open_member_namespace(&settings.library, member)?,
        None => Library::open(&settings.library)?,
    });
    let session = match pair {
        Some(advertise) => Some(pairing::load_or_create(
            &settings.library,
            webdav,
            advertise,
        )?),
        None => None,
    };
    let auth = Arc::new(webdav::Auth {
        user: settings.user.clone(),
        password: settings.password.clone(),
        pairing_token: session.as_ref().map(|session| Arc::clone(&session.token)),
    });
    let webdav = match (webdav, session.as_ref()) {
        (Some(addr), Some(session)) => {
            Some(webdav::spawn_tls(addr, Arc::clone(&library), auth, session.acceptor()).await?)
        }
        (Some(addr), None) => Some(webdav::spawn(addr, Arc::clone(&library), auth).await?),
        (None, _) => None,
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
    Ok((Listening { webdav, sftp, ftp }, session))
}
