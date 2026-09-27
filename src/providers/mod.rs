mod command;
mod git;
#[cfg(test)]
mod tests;
use crate::{
    config::{Collector, CommandOutput, Config, Provider, TokenMapping},
    process::{self, Request},
};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, time::Duration};
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
pub async fn preflight(config: &Config) -> anyhow::Result<()> {
    if !config
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
    let mut env = process::environment();
    let argv = match c.provider {
        Provider::Command => {
            for key in &c.env_allow {
                if let Some(v) = std::env::var_os(key) {
                    env.insert(key.into(), v);
                }
            }
            for (k, v) in &c.env {
                env.insert(k.into(), v.into());
            }
            if let Some(workspace) = workspace {
                env.insert("HERDR_TOKENS_WORKSPACE_ID".into(), workspace.into());
                env.insert("HERDR_TOKENS_WORKSPACE_DIR".into(), cwd.as_os_str().into());
            }
            env.insert("HERDR_TOKENS_COLLECTOR".into(), c.name.clone().into());
            c.command.iter().map(Into::into).collect()
        }
        Provider::Git => {
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            env.insert("LC_ALL".into(), "C".into());
            [
                "git",
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "color.ui=false",
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ]
            .into_iter()
            .map(Into::into)
            .collect()
        }
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
        },
        cancel,
    )
    .await?;
    if !out.status.success() {
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
