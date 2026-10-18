// SPDX-License-Identifier: Apache-2.0

//! SFTP for cameras that can use SSH. Writes commit through `copy_verify`
//! when the handle is closed. Remove and rename are refused.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::sync::Arc;

use russh::server::{Auth, Handler, Msg, Server as _, Session};
use russh::Channel;
use russh::ChannelId;
use russh_sftp::protocol::{
    Attrs, Data, File as SftpFile, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
    Version,
};
use tokio::net::TcpListener;

use crate::store::{Arrival, Commit, Library, Spool};

pub async fn spawn(
    addr: SocketAddr,
    library: Arc<Library>,
    user: String,
    password: String,
) -> std::io::Result<SocketAddr> {
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let config = Arc::new(russh::server::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(600)),
        auth_rejection_time: std::time::Duration::from_millis(200),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        keys: vec![key],
        ..Default::default()
    });
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let mut server = SshServer {
        library,
        user,
        password,
    };
    tokio::spawn(async move {
        if let Err(err) = server.run_on_socket(config, &listener).await {
            eprintln!("albumen-receive sftp: {err}");
        }
    });
    Ok(bound)
}

struct SshServer {
    library: Arc<Library>,
    user: String,
    password: String,
}

impl russh::server::Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshSession {
            library: Arc::clone(&self.library),
            user: self.user.clone(),
            password: self.password.clone(),
            channels: HashMap::new(),
        }
    }
}

struct SshSession {
    library: Arc<Library>,
    user: String,
    password: String,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl Handler for SshSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.user && password == self.password {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            let _ = session.channel_failure(channel);
            return Ok(());
        }
        let Some(opened) = self.channels.remove(&channel) else {
            let _ = session.channel_failure(channel);
            return Ok(());
        };
        session.channel_success(channel)?;
        let sftp = SftpSession {
            library: Arc::clone(&self.library),
            handles: HashMap::new(),
            next_handle: 1,
            dirs: HashMap::new(),
        };
        russh_sftp::server::run(opened.into_stream(), sftp).await;
        Ok(())
    }
}

struct SftpSession {
    library: Arc<Library>,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
    dirs: HashMap<String, bool>,
}

enum OpenHandle {
    Writing { spool: Spool, arrival: Arrival },
    Reading(File),
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let arrival = Arrival::parse(&filename).map_err(|_| StatusCode::Failure)?;
        let writing = pflags.contains(OpenFlags::WRITE)
            || pflags.contains(OpenFlags::CREATE)
            || pflags.contains(OpenFlags::APPEND)
            || pflags.contains(OpenFlags::TRUNCATE);
        let handle = self.next_handle.to_string();
        self.next_handle += 1;
        if writing {
            let spool = Spool::create(&arrival.camera_filename).map_err(|_| StatusCode::Failure)?;
            if let Some(mtime) = attrs.mtime {
                let _ = spool.set_mtime(u64::from(mtime));
            }
            self.handles
                .insert(handle.clone(), OpenHandle::Writing { spool, arrival });
        } else if pflags.contains(OpenFlags::READ) {
            let file = File::open(self.library.root().join(&arrival.camera_filename))
                .map_err(|_| StatusCode::NoSuchFile)?;
            self.handles
                .insert(handle.clone(), OpenHandle::Reading(file));
        } else {
            return Err(StatusCode::OpUnsupported);
        }
        Ok(Handle { id, handle })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(OpenHandle::Writing { spool, .. }) => {
                spool
                    .write_at(offset, &data)
                    .map_err(|_| StatusCode::Failure)?;
                Ok(status_ok(id))
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(OpenHandle::Reading(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::Failure)?;
        let mut buf = vec![0u8; len as usize];
        let n = file.read(&mut buf).map_err(|_| StatusCode::Failure)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        match self.handles.remove(&handle) {
            Some(OpenHandle::Writing { mut spool, arrival }) => {
                match self.library.commit(&mut spool, &arrival) {
                    Ok(Commit::Stored { .. } | Commit::Deduped { .. }) => Ok(status_ok(id)),
                    Err(_) => Err(StatusCode::Failure),
                }
            }
            Some(OpenHandle::Reading(_)) => Ok(status_ok(id)),
            None => {
                self.dirs.remove(&handle);
                Ok(status_ok(id))
            }
        }
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let _ = path;
        let handle = self.next_handle.to_string();
        self.next_handle += 1;
        self.dirs.insert(handle.clone(), false);
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(listed) = self.dirs.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *listed {
            return Err(StatusCode::Eof);
        }
        *listed = true;
        let mut files = Vec::new();
        for name in self.library.files().map_err(|_| StatusCode::Failure)? {
            let meta = std::fs::metadata(self.library.root().join(&name))
                .map_err(|_| StatusCode::Failure)?;
            files.push(SftpFile::new(&name, FileAttributes::from(&meta)));
        }
        Ok(Name { id, files })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let shown = if path.is_empty() || path == "." || path == "/" {
            "/".to_string()
        } else {
            format!(
                "/{}",
                Arrival::parse(&path)
                    .map_err(|_| StatusCode::Failure)?
                    .camera_filename
            )
        };
        Ok(Name {
            id,
            files: vec![SftpFile::dummy(shown)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.attrs(id, &path)
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.attrs(id, &path)
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(OpenHandle::Reading(file)) => {
                let meta = file.metadata().map_err(|_| StatusCode::Failure)?;
                Ok(Attrs {
                    id,
                    attrs: FileAttributes::from(&meta),
                })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn mkdir(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(status_ok(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(status_ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if let (Some(OpenHandle::Writing { spool, .. }), Some(mtime)) =
            (self.handles.get(&handle), attrs.mtime)
        {
            let _ = spool.set_mtime(u64::from(mtime));
        }
        Ok(status_ok(id))
    }

    async fn remove(&mut self, _id: u32, _filename: String) -> Result<Status, Self::Error> {
        Err(StatusCode::PermissionDenied)
    }

    async fn rename(
        &mut self,
        _id: u32,
        _oldpath: String,
        _newpath: String,
    ) -> Result<Status, Self::Error> {
        Err(StatusCode::PermissionDenied)
    }

    async fn rmdir(&mut self, _id: u32, _path: String) -> Result<Status, Self::Error> {
        Err(StatusCode::PermissionDenied)
    }
}

impl SftpSession {
    fn attrs(&self, id: u32, path: &str) -> Result<Attrs, StatusCode> {
        if path.is_empty() || path == "/" || path == "." {
            return Ok(Attrs {
                id,
                attrs: FileAttributes::dummy(),
            });
        }
        let name = Arrival::parse(path)
            .map_err(|_| StatusCode::NoSuchFile)?
            .camera_filename;
        let meta = std::fs::metadata(self.library.root().join(name))
            .map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }
}

fn status_ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "ok".to_string(),
        language_tag: "en".to_string(),
    }
}
