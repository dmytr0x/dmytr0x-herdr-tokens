//! Endpoint identity, singleton ownership, durable ordering and bounded local IPC.
use crate::config::hash;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
use tokio::net::UnixListener;

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normal = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            x => normal.push(x.as_os_str()),
        }
    }
    // Resolve the existing parent; the socket itself need not exist.
    if let Some(parent) = normal.parent()
        && let Ok(parent) = parent.canonicalize()
    {
        return Ok(parent.join(normal.file_name().context("path needs a name")?));
    }
    Ok(normal)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub endpoint: String,
    pub socket: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
}
#[derive(Clone)]
pub struct Endpoint {
    pub hash: String,
    pub socket: PathBuf,
    pub runtime: PathBuf,
    pub control: PathBuf,
}
impl Endpoint {
    pub fn new(socket: &Path, runtime: Option<&Path>) -> Result<Self> {
        let socket = absolute(socket)?;
        let hash = hash(socket.as_os_str().as_encoded_bytes());
        let runtime = runtime.map(absolute).transpose()?.unwrap_or_else(|| {
            PathBuf::from(format!("/tmp/herdr-tokens-{}", nix::unistd::getuid()))
        });
        private_dir(&runtime)?;
        let control = runtime.join(format!("{}.sock", &hash[..32]));
        let max = if cfg!(target_os = "macos") { 103 } else { 107 };
        ensure!(
            control.as_os_str().as_encoded_bytes().len() <= max,
            "control socket path too long; choose a shorter --runtime-dir"
        );
        Ok(Self {
            hash,
            socket,
            runtime,
            control,
        })
    }
    pub fn identity(&self, config: PathBuf, state: PathBuf) -> Identity {
        Identity {
            endpoint: self.hash.clone(),
            socket: self.socket.clone(),
            config,
            state,
        }
    }
    pub fn acquire(&self) -> Result<Option<Lock>> {
        let path = self.runtime.join(format!("{}.lock", &self.hash[..32]));
        managed(&path, false)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)?;
        managed_file(&file)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Lock {
                file,
                control: self.control.clone(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn bind(&self, lock: &Lock) -> Result<UnixListener> {
        ensure!(lock.control == self.control, "endpoint lock mismatch");
        managed(&self.control, true)?;
        if self.control.exists() {
            fs::remove_file(&self.control)?;
        }
        let listener = UnixListener::bind(&self.control)?;
        fs::set_permissions(&self.control, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
}
pub struct Lock {
    file: File,
    control: PathBuf,
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.control);
        let _ = FileExt::unlock(&self.file);
    }
}
pub(crate) fn managed_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == nix::unistd::getuid().as_raw()
            && metadata.mode() & 0o077 == 0,
        "unsafe opened runtime/state file"
    );
    Ok(())
}
fn managed(path: &Path, socket: bool) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.uid() == nix::unistd::getuid().as_raw()
                    && m.mode() & 0o077 == 0
                    && !m.file_type().is_symlink(),
                "unsafe runtime/state entry"
            );
            ensure!(
                if socket {
                    m.file_type().is_socket()
                } else {
                    m.is_file()
                },
                "unexpected runtime/state entry type"
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && !m.file_type().is_symlink()
            && m.uid() == nix::unistd::getuid().as_raw()
            && m.mode() & 0o077 == 0,
        "directory must be owned by this user, non-symlink, mode 0700"
    );
    Ok(())
}
pub fn state_dir(identity: &Identity) -> Result<PathBuf> {
    // Herdr creates the injected plugin state root with mode 0755. Only our
    // managed descendants must be private; never chmod a caller-owned root.
    match fs::symlink_metadata(&identity.state) {
        Ok(m) => ensure!(
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == nix::unistd::getuid().as_raw()
                && m.mode() & 0o022 == 0,
            "state root must be owned by this user, non-symlink and not group/world writable"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => private_dir(&identity.state)?,
        Err(e) => return Err(e.into()),
    }
    let endpoints = identity.state.join("endpoints");
    private_dir(&endpoints)?;
    let dir = endpoints.join(&identity.endpoint);
    private_dir(&dir)?;
    Ok(dir)
}
mod sequences;
pub use sequences::Sequences;
mod protocol;
pub use protocol::{Command, Control, ProtocolError, Request, Response, matching, request, serve};
