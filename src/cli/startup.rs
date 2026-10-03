use super::*;
pub(super) async fn start(
    endpoint: runtime::Endpoint,
    identity: runtime::Identity,
    herdr: herdr::Herdr,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    if let Ok(response) = tokio::time::timeout_at(
        deadline,
        runtime::request(&endpoint, runtime::Command::Ping),
    )
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
        if let Ok(response) = tokio::time::timeout_at(
            deadline,
            runtime::request(&endpoint, runtime::Command::Ping),
        )
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
