mod command;
mod git;
#[cfg(test)]
mod tests;
use crate::{
    config::{Collector, CommandOutput, Config, Job, Provider, TokenMapping},
    process::{self, Request},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Token {
    Set(String),
    Clear,
}
pub type Patch = BTreeMap<String, Token>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Process(#[from] process::Error),
    #[error("collector exited unsuccessfully (code {code:?}, stderr bytes {stderr_bytes})")]
    Exit {
        code: Option<i32>,
        stderr_bytes: usize,
    },
    #[error("invalid collector output")]
    InvalidOutput,
    #[error("collector task panicked")]
    Panic,
}
pub struct Collected {
    pub patch: Patch,
    pub truncated: bool,
}
fn render(value: &str, mapping: &TokenMapping) -> (Token, bool) {
    let (value, truncated) = normalize(value);
    match value {
        Token::Clear => (Token::Clear, truncated),
        Token::Set(value) => {
            if !mapping.show_zero && matches!(value.as_str(), "0" | "0.0") {
                return (Token::Clear, truncated);
            }
            let (value, clipped) =
                normalize(&format!("{}{}{}", mapping.prefix, value, mapping.suffix));
            (value, truncated || clipped)
        }
    }
}
fn normalize(s: &str) -> (Token, bool) {
    let mut chars = s.trim().chars().filter(|c| !c.is_control());
    let value: String = chars.by_ref().take(80).collect();
    let truncated = chars.next().is_some();
    let value = value.trim();
    (
        if value.is_empty() {
            Token::Clear
        } else {
            Token::Set(value.into())
        },
        truncated,
    )
}
const GIT: [&str; 6] = [
    "git",
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "color.ui=false",
];
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
fn git_env() -> BTreeMap<OsString, OsString> {
    let mut env = process::environment();
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("LC_ALL".into(), "C".into());
    env
}
fn command_env(allow: &[String], vars: &BTreeMap<String, String>) -> BTreeMap<OsString, OsString> {
    let mut env = process::environment();
    for key in allow {
        if let Some(v) = std::env::var_os(key) {
            env.insert(key.into(), v);
        }
    }
    for (k, v) in vars {
        env.insert(k.into(), v.into());
    }
    env
}
/// Safe categories only: Git stderr and command arguments never leave this boundary.
#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    #[error(transparent)]
    Process(#[from] process::Error),
    #[error("Git invocation failed")]
    Git,
    #[error("malformed Git response")]
    Malformed,
    #[error("repository path unavailable")]
    UnavailablePath,
}
#[derive(Debug, PartialEq, Eq)]
pub enum WorktreeResolution {
    NotRepository,
    Repository { toplevel: PathBuf, common: PathBuf },
}
async fn git(
    args: &[&str],
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<process::Output, ResolutionError> {
    Ok(process::execute(
        Request {
            argv: GIT.iter().chain(args).map(OsString::from).collect(),
            cwd: cwd.into(),
            env: git_env(),
            timeout: GIT_TIMEOUT,
            stdout_limit: 65536,
            stderr_limit: 16384,
            capture: process::Capture::Bounded,
        },
        cancel,
    )
    .await?)
}
fn successful(out: process::Output) -> Result<Vec<u8>, ResolutionError> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(ResolutionError::Git)
    }
}
fn line_path(line: &[u8], base: &Path) -> Result<PathBuf, ResolutionError> {
    if line.is_empty() || line.contains(&0) {
        return Err(ResolutionError::Malformed);
    }
    let path = base
        .join(OsStr::from_bytes(line))
        .canonicalize()
        .map_err(|_| ResolutionError::UnavailablePath)?;
    if !path.is_dir() {
        return Err(ResolutionError::UnavailablePath);
    }
    Ok(path)
}
pub async fn resolve_worktree(
    dir: &Path,
    cancel: CancellationToken,
) -> Result<WorktreeResolution, ResolutionError> {
    if cancel.is_cancelled() {
        return Err(process::Error::Cancelled.into());
    }
    if !dir.is_dir() {
        return Err(ResolutionError::UnavailablePath);
    }
    let out = git(&["rev-parse", "--show-toplevel"], dir, cancel.clone()).await?;
    if !out.status.success() && out.stderr.starts_with(b"fatal: not a git repository") {
        return Ok(WorktreeResolution::NotRepository);
    }
    let toplevel = successful(out)?;
    let common = successful(git(&["rev-parse", "--git-common-dir"], dir, cancel).await?)?;
    Ok(WorktreeResolution::Repository {
        toplevel: line_path(
            toplevel
                .strip_suffix(b"\n")
                .ok_or(ResolutionError::Malformed)?,
            dir,
        )?,
        common: line_path(
            common
                .strip_suffix(b"\n")
                .ok_or(ResolutionError::Malformed)?,
            dir,
        )?,
    })
}
pub async fn main_worktree(
    common_dir: &Path,
    cancel: CancellationToken,
) -> Result<PathBuf, ResolutionError> {
    let out = git(
        &["worktree", "list", "--porcelain", "-z"],
        common_dir,
        cancel.clone(),
    )
    .await?;
    let first = if out.status.success() {
        out.stdout
            .split(|&b| b == 0)
            .next()
            .ok_or(ResolutionError::Malformed)?
            .to_vec()
    } else {
        let out = successful(git(&["worktree", "list", "--porcelain"], common_dir, cancel).await?)?;
        let end = out
            .windows(b"\nHEAD ".len())
            .position(|w| w == b"\nHEAD ")
            .ok_or(ResolutionError::Malformed)?;
        out[..end].to_vec()
    };
    line_path(
        first
            .strip_prefix(b"worktree ")
            .ok_or(ResolutionError::Malformed)?,
        common_dir,
    )
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobOutcome {
    pub code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    /// `None` exactly when the process exited with code 0.
    pub error: Option<String>,
}
/// Runs one background job process in `dir`; output is counted and discarded.
pub async fn run_job(
    job: &Job,
    dir: &Path,
    workspace_ids: &[String],
    cancel: CancellationToken,
) -> JobOutcome {
    let mut env = command_env(&job.env_allow, &job.env);
    env.insert("HERDR_TOKENS_JOB".into(), job.name.clone().into());
    env.insert("HERDR_TOKENS_WORKTREE_DIR".into(), dir.as_os_str().into());
    env.insert(
        "HERDR_TOKENS_WORKSPACE_IDS".into(),
        workspace_ids.join(",").into(),
    );
    let started = Instant::now();
    let result = process::execute(
        Request {
            argv: job.command.iter().map(Into::into).collect(),
            cwd: dir.into(),
            env,
            timeout: Duration::from_millis(job.timeout_ms),
            stdout_limit: 0,
            stderr_limit: 0,
            capture: process::Capture::Discard,
        },
        cancel,
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(out) => JobOutcome {
            code: out.status.code(),
            duration_ms,
            stdout_bytes: out.stdout_bytes,
            stderr_bytes: out.stderr_bytes,
            error: match out.status.code() {
                Some(0) => None,
                Some(code) => Some(format!("exited with code {code}")),
                None => Some("terminated by signal".into()),
            },
        },
        Err(e) => JobOutcome {
            duration_ms,
            error: Some(e.to_string()),
            ..JobOutcome::default()
        },
    }
}
pub async fn preflight(config: &Config) -> anyhow::Result<()> {
    if config.jobs.is_empty()
        && !config
            .collectors
            .iter()
            .any(|c| c.provider == Provider::Git)
    {
        return Ok(());
    }
    let result = process::execute(
        Request {
            argv: vec!["git".into(), "--version".into()],
            cwd: std::env::current_dir()?,
            env: process::environment(),
            timeout: Duration::from_secs(1),
            stdout_limit: 1024,
            stderr_limit: 16384,
            capture: process::Capture::Bounded,
        },
        CancellationToken::new(),
    )
    .await?;
    let text = std::str::from_utf8(&result.stdout).unwrap_or("");
    let version = text.strip_prefix("git version ").unwrap_or("");
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    anyhow::ensure!(
        result.status.success() && (major > 2 || major == 2 && minor >= 20),
        "Git >= 2.20 is required"
    );
    Ok(())
}
pub async fn collect(
    c: &Collector,
    workspace: &str,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<Collected, Error> {
    collect_with_context(c, Some(workspace), cwd, cancel).await
}

pub async fn collect_global(
    c: &Collector,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<Collected, Error> {
    collect_with_context(c, None, cwd, cancel).await
}

async fn collect_with_context(
    c: &Collector,
    workspace: Option<&str>,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<Collected, Error> {
    let (env, argv) = match c.provider {
        Provider::Command => {
            let mut env = command_env(&c.env_allow, &c.env);
            if let Some(workspace) = workspace {
                env.insert("HERDR_TOKENS_WORKSPACE_ID".into(), workspace.into());
                env.insert("HERDR_TOKENS_WORKSPACE_DIR".into(), cwd.as_os_str().into());
            }
            env.insert("HERDR_TOKENS_COLLECTOR".into(), c.name.clone().into());
            (env, c.command.iter().map(Into::into).collect())
        }
        Provider::Git => (
            git_env(),
            GIT.into_iter()
                .chain([
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=all",
                ])
                .map(Into::into)
                .collect(),
        ),
    };
    let out = process::execute(
        Request {
            argv,
            cwd: cwd.into(),
            env,
            timeout: Duration::from_millis(c.timeout_ms),
            stdout_limit: if c.provider == Provider::Git {
                1_048_576
            } else {
                65536
            },
            stderr_limit: 16384,
            capture: process::Capture::Bounded,
        },
        cancel,
    )
    .await?;
    if !out.status.success() {
        // `LC_ALL=C` keeps this message stable; unsafe or broken checkouts still fail.
        if c.provider == Provider::Git && out.stderr.starts_with(b"fatal: not a git repository") {
            return Ok(Collected {
                patch: c.tokens.keys().map(|t| (t.clone(), Token::Clear)).collect(),
                truncated: false,
            });
        }
        return Err(Error::Exit {
            code: out.status.code(),
            stderr_bytes: out.stderr.len(),
        });
    }
    let (patch, truncated) = match c.provider {
        Provider::Command => match c.output {
            CommandOutput::Json => command::parse_json(&out.stdout, &c.tokens)?,
            CommandOutput::Text => command::parse_text(&out.stdout, &c.tokens)?,
        },
        Provider::Git => {
            let fields = git::parse(&out.stdout)?;
            let mut patch = Patch::new();
            let mut truncated = false;
            for (token, mapping) in &c.tokens {
                let (value, clipped) = render(&fields[&mapping.field], mapping);
                patch.insert(token.clone(), value);
                truncated |= clipped;
            }
            (patch, truncated)
        }
    };
    Ok(Collected { patch, truncated })
}
