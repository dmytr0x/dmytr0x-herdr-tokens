#![cfg(unix)]
pub mod config;
pub mod diagnostics;
pub mod herdr;
pub mod process;
pub mod providers;
pub mod publisher;
pub mod runner;
pub mod runtime;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::Stdio, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Publish display-only workspace tokens to one explicit Herdr endpoint"
)]
pub struct Cli {
    #[arg(long, env = "HERDR_PLUGIN_CONFIG_DIR", global = true)]
    config_dir: Option<PathBuf>,
    #[arg(long, env = "HERDR_PLUGIN_STATE_DIR", global = true)]
    state_dir: Option<PathBuf>,
    #[arg(long, env = "HERDR_SOCKET_PATH", global = true)]
    socket: Option<PathBuf>,
    #[arg(long, env = "HERDR_BIN_PATH", global = true)]
    herdr_bin: Option<PathBuf>,
    #[arg(long, global = true)]
    runtime_dir: Option<PathBuf>,
    #[arg(long, hide = true, global = true)]
    detached: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Run,
    Start,
    Stop,
    Validate,
    Reload,
    Refresh {
        #[arg(long)]
        workspace: Option<String>,
    },
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        include_values: bool,
    },
}
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);
fn invalid(error: impl std::fmt::Display) -> anyhow::Error {
    Invalid(error.to_string()).into()
}
fn required(value: Option<PathBuf>, name: &str) -> Result<PathBuf> {
    runtime::absolute(&value.ok_or_else(|| invalid(format!("{name} is required")))?)
        .map_err(invalid)
}

pub async fn execute(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Validate) {
        let config = required(cli.config_dir, "--config-dir / HERDR_PLUGIN_CONFIG_DIR")?;
        let c = config::Config::load(&config).map_err(invalid)?;
        println!(
            "Valid: {} collectors, {} tokens ({})",
            c.collectors.len(),
            c.token_names().len(),
            c.hash()
        );
        return Ok(());
    }
    let socket = cli.socket.ok_or_else(|| {
        invalid("--socket / HERDR_SOCKET_PATH is required; no default session is selected")
    })?;
    let endpoint = runtime::Endpoint::new(&socket, cli.runtime_dir.as_deref())?;
    if matches!(cli.command, Command::Run | Command::Start) {
        let config = required(cli.config_dir, "--config-dir / HERDR_PLUGIN_CONFIG_DIR")?;
        let state = required(cli.state_dir, "--state-dir / HERDR_PLUGIN_STATE_DIR")?;
        let identity = endpoint.identity(config, state);
        let binary = match cli.herdr_bin {
            Some(p) => runtime::absolute(&p)?,
            None => process::resolve(
                std::ffi::OsStr::new("herdr"),
                &std::env::current_dir()?,
                &process::environment(),
            )
            .context("Herdr executable not found on PATH")?,
        };
        let herdr = herdr::Herdr {
            binary,
            socket: endpoint.socket.clone(),
        };
        if matches!(cli.command, Command::Run) {
            return runner::run(endpoint, identity, herdr, cli.detached).await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        if let Ok(response) =
            tokio::time::timeout_at(deadline, runtime::request(&endpoint, "ping", None, false))
                .await
                .context("runner readiness timed out")?
        {
            ensure!(
                response.identity == identity,
                "runner conflict: endpoint uses different config/state locations"
            );
            ensure!(response.ok && response.ready, "runner is not ready");
            println!("Already running");
            return Ok(());
        }
        // A rejected on-disk edit must not prevent acknowledging an already healthy runner.
        config::Config::load(&identity.config).map_err(invalid)?;
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args(["run", "--detached", "--config-dir"])
            .arg(&identity.config)
            .arg("--state-dir")
            .arg(&identity.state)
            .arg("--socket")
            .arg(&endpoint.socket)
            .arg("--runtime-dir")
            .arg(&endpoint.runtime)
            .arg("--herdr-bin")
            .arg(&herdr.binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and touches no allocator or shared state.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::from)
            });
        }
        let mut child = command.spawn().context("cannot start detached runner")?;
        loop {
            if let Ok(response) =
                tokio::time::timeout_at(deadline, runtime::request(&endpoint, "ping", None, false))
                    .await
                    .context("runner readiness timed out")?
            {
                ensure!(
                    response.identity == identity,
                    "runner conflict: endpoint uses different config/state locations"
                );
                if response.ok && response.ready {
                    println!("Started");
                    return Ok(());
                }
            }
            if let Some(status) = child.try_wait()? {
                ensure!(
                    status.success(),
                    "runner exited before readiness; run in foreground for diagnostics"
                );
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "runner readiness timed out; inspect endpoint logs or run in foreground"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let (command, workspace, values, json_output) = match cli.command {
        Command::Stop => ("stop", None, false, false),
        Command::Reload => ("reload", None, false, false),
        Command::Refresh { workspace } => {
            let action = std::env::var("HERDR_PLUGIN_ID").as_deref() == Ok("herdr-tokens")
                && std::env::var("HERDR_PLUGIN_ACTION_ID")
                    .is_ok_and(|v| v == "refresh" || v == "herdr-tokens.refresh");
            let workspace = workspace.or_else(|| {
                action
                    .then(|| std::env::var("HERDR_WORKSPACE_ID").ok())
                    .flatten()
            });
            ("refresh", workspace, false, false)
        }
        Command::Status {
            json,
            include_values,
        } => ("status", None, include_values, json),
        _ => unreachable!(),
    };
    let response = runtime::request(&endpoint, command, workspace, values).await?;
    if !response.ok {
        if command == "reload"
            && response.result.get("accepted") == Some(&serde_json::Value::Bool(false))
        {
            return Err(invalid(response.result));
        }
        anyhow::bail!("control failed: {}", response.result);
    }
    if json_output {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&response.result)?);
    }
    Ok(())
}
