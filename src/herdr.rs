//! CLI-only Herdr transport. Successful mutations intentionally need no JSON body.
use crate::{
    config::Config,
    process::{self, Request},
    providers::{Patch, Token},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Herdr {
    pub binary: PathBuf,
    pub socket: PathBuf,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Herdr connection failure")]
    Connection,
    #[error("Herdr request timed out (delivery unknown)")]
    Timeout,
    #[error("Herdr malformed response")]
    Malformed,
    #[error("Herdr semantic error")]
    Semantic,
    #[error("workspace not found")]
    WorkspaceNotFound,
}
impl Error {
    pub fn disconnected(&self) -> bool {
        matches!(self, Self::Connection | Self::Timeout)
    }
}
#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(rename = "id")]
    _id: String,
    result: Option<T>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Workspaces {
    workspaces: Vec<Workspace>,
}
#[derive(Deserialize)]
struct Panes {
    panes: Vec<Pane>,
}
#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    worktree: Option<Worktree>,
}
#[derive(Deserialize)]
struct Worktree {
    checkout_path: PathBuf,
}
#[derive(Deserialize)]
struct Pane {
    workspace_id: String,
    cwd: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Directory {
    pub reported: Option<PathBuf>,
    pub canonical: Option<PathBuf>,
    pub reason: String,
}
pub type Discovery = BTreeMap<String, Directory>;
impl Herdr {
    async fn call(&self, args: Vec<String>) -> Result<Vec<u8>, Error> {
        let mut env = process::environment();
        env.insert("HERDR_SOCKET_PATH".into(), self.socket.as_os_str().into());
        let mut argv = vec![self.binary.as_os_str().into()];
        argv.extend(args.into_iter().map(Into::into));
        let out = process::execute(
            Request {
                argv,
                cwd: PathBuf::from("/"),
                env,
                timeout: Duration::from_secs(1),
                stdout_limit: 4 * 1_048_576,
                stderr_limit: 65536,
                capture: process::Capture::Bounded,
            },
            CancellationToken::new(),
        )
        .await
        .map_err(|e| match e {
            process::Error::Timeout => Error::Timeout,
            process::Error::Overflow => Error::Malformed,
            _ => Error::Connection,
        })?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let structured = serde_json::from_slice::<serde_json::Value>(&out.stderr)
            .ok()
            .or_else(|| serde_json::from_slice(&out.stdout).ok());
        let code = structured
            .as_ref()
            .and_then(|v| v.pointer("/error/code"))
            .and_then(|s| s.as_str());
        Err(match code {
            Some("workspace_not_found") => Error::WorkspaceNotFound,
            Some(
                "server_not_running" | "connection_failed" | "connection_error" | "transport_error"
                | "server_unavailable",
            )
            | None => Error::Connection,
            _ => Error::Semantic,
        })
    }
    pub async fn discover(&self, config: &Config) -> Result<Discovery, Error> {
        // Do not use try_join!: early return would drop the other process's cleanup future.
        let (workspaces, panes) = tokio::join!(
            self.call(vec!["workspace".into(), "list".into()]),
            self.call(vec!["pane".into(), "list".into()])
        );
        decode(&workspaces?, &panes?, config)
    }
    pub async fn report(
        &self,
        workspace: &str,
        patch: &Patch,
        seq: u64,
        ttl_ms: u64,
    ) -> Result<(), Error> {
        if patch.is_empty() || patch.len() > 16 {
            return Err(Error::Semantic);
        }
        let mut args = vec![
            "workspace".into(),
            "report-metadata".into(),
            workspace.into(),
            "--source".into(),
            "herdr-tokens".into(),
            "--seq".into(),
            seq.to_string(),
            "--ttl-ms".into(),
            ttl_ms.to_string(),
        ];
        for (key, value) in patch {
            match value {
                Token::Set(v) => {
                    args.push("--token".into());
                    args.push(format!("{key}={v}"));
                }
                Token::Clear => {
                    args.push("--clear-token".into());
                    args.push(key.clone());
                }
            }
        }
        self.call(args).await.map(|_| ())
    }
}
fn envelope<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let e: Envelope<T> = serde_json::from_slice(bytes).map_err(|_| Error::Malformed)?;
    if e.error.is_some() {
        return Err(Error::Semantic);
    }
    e.result.ok_or(Error::Malformed)
}
fn valid_dir(p: &Path) -> Option<PathBuf> {
    if !p.is_absolute() {
        return None;
    }
    let p = p.canonicalize().ok()?;
    if !p.is_dir() || std::fs::read_dir(&p).is_err() {
        return None;
    }
    Some(p)
}
pub fn decode(workspaces: &[u8], panes: &[u8], config: &Config) -> Result<Discovery, Error> {
    let workspaces: Workspaces = envelope(workspaces)?;
    let panes: Panes = envelope(panes)?;
    if workspaces.workspaces.len() > 256 {
        return Err(Error::Malformed);
    }
    let mut result = BTreeMap::new();
    for w in workspaces.workspaces {
        let directory = if let Some(p) = config
            .workspace_dirs
            .get(&w.workspace_id)
            .or_else(|| w.worktree.as_ref().map(|w| &w.checkout_path))
        {
            let canonical = valid_dir(p);
            Directory {
                reported: Some(p.clone()),
                reason: if canonical.is_some() {
                    "override/worktree"
                } else {
                    "override/worktree unavailable; no fallback"
                }
                .into(),
                canonical,
            }
        } else {
            let dirs: BTreeSet<_> = panes
                .panes
                .iter()
                .filter(|p| p.workspace_id == w.workspace_id)
                .filter_map(|p| p.cwd.as_ref())
                .filter_map(|p| valid_dir(p).map(|c| (c, p.clone())))
                .collect();
            let canonical: BTreeSet<_> = dirs.iter().map(|(c, _)| c).collect();
            if canonical.len() == 1 {
                let (c, p) = dirs.iter().next().expect("one directory");
                Directory {
                    reported: Some(p.clone()),
                    canonical: Some(c.clone()),
                    reason: "unambiguous pane cwd".into(),
                }
            } else {
                Directory {
                    reported: None,
                    canonical: None,
                    reason: "missing/ambiguous pane cwd; add workspace_dirs override".into(),
                }
            }
        };
        if w.workspace_id.is_empty() || result.insert(w.workspace_id, directory).is_some() {
            return Err(Error::Malformed);
        }
    }
    Ok(result)
}
