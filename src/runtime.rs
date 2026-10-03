//! Endpoint identity, singleton ownership, durable ordering and bounded local IPC.
use crate::config::hash;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

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
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Lock {
                file,
                control: self.control.clone(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn bind(&self, _lock: &Lock) -> Result<UnixListener> {
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Watermark {
    schema_version: u32,
    reserved_through: u64,
}
pub struct Sequences {
    dir: PathBuf,
    next: u64,
    through: u64,
}
impl Sequences {
    /// Caller must hold the endpoint lock for the allocator's entire lifetime.
    pub fn open(dir: PathBuf, _lock: &Lock) -> Result<Self> {
        let path = dir.join("sequence.toml");
        managed(&path, false)?;
        let through = match File::open(path) {
            Ok(f) => {
                let mut text = String::new();
                f.take(4097).read_to_string(&mut text)?;
                ensure!(text.len() <= 4096, "sequence state oversized");
                let w: Watermark = toml::from_str(&text)
                    .context("corrupt sequence state; do not reset while Herdr retains history")?;
                ensure!(w.schema_version == 1, "unsupported sequence schema");
                w.reserved_through
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        let mut result = Self {
            dir,
            next: through,
            through,
        };
        result.reserve()?;
        Ok(result)
    }
    fn reserve(&mut self) -> Result<()> {
        let end = self
            .through
            .checked_add(1024)
            .context("sequence range exhausted")?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
        write!(temp, "schema_version = 1\nreserved_through = {end}\n")?;
        temp.as_file().sync_all()?;
        temp.persist(self.dir.join("sequence.toml"))?;
        File::open(&self.dir)?.sync_all()?;
        self.next = self.through + 1;
        self.through = end;
        Ok(())
    }
    pub fn allocate(&mut self) -> Result<u64> {
        if self.next > self.through {
            self.reserve()?;
        }
        let n = self.next;
        self.next = n.checked_add(1).context("sequence range exhausted")?;
        Ok(n)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub endpoint: String,
    pub command: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub include_values: bool,
    #[serde(default)]
    pub job: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub ok: bool,
    pub identity: Identity,
    pub ready: bool,
    pub result: serde_json::Value,
}
pub struct Control {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}
pub async fn serve(
    listener: UnixListener,
    tx: mpsc::Sender<Control>,
    identity: Identity,
    cancel: tokio_util::sync::CancellationToken,
) {
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((mut stream,_)) = accepted else { break; };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    // Wait briefly for write readiness without spawning an unbounded
                    // rejection task. try_write alone can lose the busy response.
                    let response = Response { version: 1, ok: false, ready: true, identity: identity.clone(), result: serde_json::json!({"error":"busy"}) };
                    if let Ok(mut bytes) = serde_json::to_vec(&response) {
                        bytes.push(b'\n');
                        let _ = tokio::time::timeout(Duration::from_millis(50), stream.write_all(&bytes)).await;
                    }
                    continue;
                };
                let tx = tx.clone(); let identity = identity.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        let mut reader = BufReader::new(&mut stream);
                        let mut bytes = Vec::new();
                        loop {
                            let buf = reader.fill_buf().await?;
                            if buf.is_empty() { return Err(std::io::Error::other("incomplete request")); }
                            let n = buf.iter().position(|b| *b == b'\n').map_or(buf.len(), |p| p+1);
                            if bytes.len() + n > 8192 { return Err(std::io::Error::other("oversized request")); }
                            bytes.extend_from_slice(&buf[..n]); reader.consume(n);
                            if bytes.last() == Some(&b'\n') { break; }
                        }
                        let req = serde_json::from_slice::<Request>(&bytes);
                        let response = match req {
                            Ok(request) if request.version == 1 && request.endpoint == identity.endpoint => {
                                let (reply, rx) = oneshot::channel();
                                if tx.try_send(Control { request, reply }).is_ok() { rx.await.map_err(|_| std::io::Error::other("runner stopped"))? }
                                else { Response { version: 1, ok: false, ready: true, identity: identity.clone(), result: serde_json::json!({"error":"busy"}) } }
                            }
                            _ => Response { version: 1, ok: false, ready: true, identity, result: serde_json::json!({"error":"invalid protocol/endpoint"}) },
                        };
                        let mut bytes = serde_json::to_vec(&response)?; bytes.push(b'\n'); stream.write_all(&bytes).await
                    }).await;
                });
            }
        }
    }
    // Let the stop acknowledgement reach its caller before closing control.
    let _ = tokio::time::timeout(Duration::from_millis(100), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    // Connections contain only IPC, never process supervision; aborting is safe here.
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}
pub async fn request(
    endpoint: &Endpoint,
    command: &str,
    workspace: Option<String>,
    include_values: bool,
    job: Option<String>,
) -> Result<Response> {
    managed(&endpoint.control, true)?;
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(&endpoint.control)
            .await
            .context("runner unavailable")?;
        let req = Request {
            version: 1,
            endpoint: endpoint.hash.clone(),
            command: command.into(),
            workspace,
            include_values,
            job,
        };
        let mut bytes = serde_json::to_vec(&req)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= 8192, "control request too large");
        // A saturated server may send busy and close before reading the request.
        // Read that bounded response even if our write raced with its close.
        let _ = stream.write_all(&bytes).await;
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        loop {
            let buf = reader.fill_buf().await?;
            ensure!(!buf.is_empty(), "incomplete control response");
            let n = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |p| p + 1);
            ensure!(
                bytes.len() + n <= 32 * 1_048_576,
                "control response too large"
            );
            bytes.extend_from_slice(&buf[..n]);
            reader.consume(n);
            if bytes.last() == Some(&b'\n') {
                break;
            }
        }
        let response: Response = serde_json::from_slice(&bytes)?;
        ensure!(
            response.version == 1
                && response.identity.endpoint == endpoint.hash
                && response.identity.socket == endpoint.socket,
            "control identity mismatch"
        );
        Ok::<_, anyhow::Error>(response)
    })
    .await
    .context("control deadline exceeded")??;
    Ok(response)
}
pub async fn matching(endpoint: &Endpoint, identity: &Identity) -> Result<()> {
    let r = request(endpoint, "ping", None, false, None).await?;
    ensure!(
        &r.identity == identity,
        "runner conflict: endpoint uses different config/state locations"
    );
    ensure!(r.ok && r.ready, "runner is not ready");
    Ok(())
}
