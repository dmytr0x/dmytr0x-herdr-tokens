//! Bounded Unix process-group supervision. No caller can inherit terminal IO.
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

/// Shared upper bounds used by transport and publication uncertainty accounting.
pub const TERMINATION_GRACE: Duration = Duration::from_millis(100);
pub const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(1);
pub const DELIVERY_BOUND: Duration = TRANSPORT_TIMEOUT.saturating_add(TERMINATION_GRACE);

struct ProcessGroup(Pid);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("process launch/io failure")]
    Io,
    #[error("process deadline exceeded")]
    Timeout,
    #[error("process cancelled")]
    Cancelled,
    #[error("process output limit exceeded")]
    Overflow,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Capture {
    /// Keep output up to the request limits; exceeding a limit is `Error::Overflow`.
    #[default]
    Bounded,
    /// Drain and count output without keeping it; limits are ignored.
    Discard,
}
#[derive(Clone)]
pub struct Request {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub capture: Capture,
}
/// `stdout`/`stderr` are empty under `Capture::Discard`; the byte counts are always set.
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}
pub fn environment() -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for key in [
        "PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TMP", "TEMP", "LANG", "LC_ALL",
    ] {
        if let Some(v) = std::env::var_os(key) {
            env.insert(key.into(), v);
        }
    }
    env.entry("PATH".into())
        .or_insert_with(|| "/usr/bin:/bin".into());
    env
}
pub fn resolve(
    exe: &std::ffi::OsStr,
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let p = Path::new(exe);
    if p.is_absolute() {
        return Some(p.into());
    }
    if exe.as_encoded_bytes().contains(&b'/') {
        return Some(cwd.join(p));
    }
    std::env::split_paths(env.get(std::ffi::OsStr::new("PATH"))?)
        .map(|p| {
            if p.is_absolute() {
                p.join(exe)
            } else {
                cwd.join(p).join(exe)
            }
        })
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
async fn drain(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
    capture: Capture,
) -> Result<(Vec<u8>, u64), Error> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut buf = [0; 8192];
    loop {
        let n = pipe.read(&mut buf).await.map_err(|_| Error::Io)?;
        if n == 0 {
            return Ok((out, total));
        }
        total += n as u64;
        if capture == Capture::Discard {
            continue;
        }
        if n > limit.saturating_sub(out.len()) {
            return Err(Error::Overflow);
        }
        out.extend_from_slice(&buf[..n]);
    }
}
pub async fn execute(req: Request, cancel: CancellationToken) -> Result<Output, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let exe = req
        .argv
        .first()
        .and_then(|s| resolve(s, &req.cwd, &req.env))
        .ok_or(Error::Io)?;
    let mut cmd = Command::new(exe);
    cmd.args(&req.argv[1..])
        .current_dir(&req.cwd)
        .env_clear()
        .envs(req.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|_| Error::Io)?;
    let pid = Pid::from_raw(child.id().ok_or(Error::Io)? as i32);
    let group = ProcessGroup(pid);
    let stdout = child.stdout.take().ok_or(Error::Io)?;
    let stderr = child.stderr.take().ok_or(Error::Io)?;
    let operation = async {
        let (status, (stdout, stdout_bytes), (stderr, stderr_bytes)) = tokio::try_join!(
            async { child.wait().await.map_err(|_| Error::Io) },
            drain(stdout, req.stdout_limit, req.capture),
            drain(stderr, req.stderr_limit, req.capture)
        )?;
        Ok(Output {
            status,
            stdout,
            stderr,
            stdout_bytes,
            stderr_bytes,
        })
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        r = tokio::time::timeout(req.timeout, operation) => r.unwrap_or(Err(Error::Timeout)),
    };
    // Even a successful direct child may leave descendants that closed their pipes.
    // Never recycle a permit before its group has been terminated.
    if killpg(pid, Signal::SIGTERM).is_ok() {
        tokio::time::sleep(TERMINATION_GRACE).await;
        let _ = killpg(pid, Signal::SIGKILL);
    }
    let _ = child.wait().await;
    std::mem::forget(group); // Explicit termination and reaping completed.
    result
}
