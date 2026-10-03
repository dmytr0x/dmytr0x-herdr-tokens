use crate::{config, herdr, process, runner, runtime};
mod startup;
const PLUGIN_ID: &str = "dmytr0x-herdr-tokens";

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
    /// Trigger background jobs now (all, or one by name).
    RunJob {
        #[arg(long)]
        job: Option<String>,
    },
}
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);
pub(crate) fn invalid(error: impl std::fmt::Display) -> anyhow::Error {
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
            "Valid configuration (syntax/schema only; runtime preflight not run): {} collectors, {} tokens ({})",
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
        return startup::start(endpoint, identity, herdr).await;
    }
    let (command, json_output) = resolve_control(cli.command);
    let response = runtime::request(&endpoint, command.clone()).await?;
    print_response(response, command, json_output)
}

fn print_response(
    response: runtime::Response,
    command: runtime::Command,
    json_output: bool,
) -> Result<()> {
    if !response.ok {
        if command == runtime::Command::Reload
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

fn resolve_control(command: Command) -> (runtime::Command, bool) {
    match command {
        Command::Stop => (runtime::Command::Stop, false),
        Command::Reload => (runtime::Command::Reload, false),
        Command::RunJob { job: name } => (runtime::Command::RunJob { name }, false),
        Command::Refresh { workspace } => {
            let action = std::env::var("HERDR_PLUGIN_ID").as_deref() == Ok(PLUGIN_ID)
                && std::env::var("HERDR_PLUGIN_ACTION_ID")
                    .is_ok_and(|v| v == "refresh" || v == format!("{PLUGIN_ID}.refresh"));
            let workspace = workspace.or_else(|| {
                action
                    .then(|| std::env::var("HERDR_WORKSPACE_ID").ok())
                    .flatten()
            });
            (runtime::Command::Refresh { workspace }, false)
        }
        Command::Status {
            json,
            include_values,
        } => (runtime::Command::Status { include_values }, json),
        _ => unreachable!(),
    }
}
