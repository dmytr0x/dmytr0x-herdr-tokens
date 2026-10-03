use herdr_tokens::process::{self, Error, Request};
use std::{ffi::OsString, time::Duration};
use tokio_util::sync::CancellationToken;
fn request(script: &str) -> Request {
    Request {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: "/".into(),
        env: process::environment(),
        timeout: Duration::from_millis(150),
        stdout_limit: 1024,
        stderr_limit: 1024,
        capture: process::Capture::Bounded,
    }
}
#[tokio::test]
async fn exit_pipes_limits_timeout_and_cancellation() {
    let out = process::execute(
        request("printf yes; printf error >&2; exit 7"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(out.stdout, b"yes");
    assert_eq!(out.stderr, b"error");
    assert_eq!(out.status.code(), Some(7));
    for script in [
        "while :; do printf 1234567890; done",
        "while :; do printf 1234567890 >&2; done",
    ] {
        assert!(matches!(
            process::execute(request(script), CancellationToken::new()).await,
            Err(Error::Overflow)
        ));
    }
    for script in ["sleep 20", "sleep 20 & exit 0", "trap '' TERM; sleep 20"] {
        let start = std::time::Instant::now();
        assert!(matches!(
            process::execute(request(script), CancellationToken::new()).await,
            Err(Error::Timeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    let task = tokio::spawn(process::execute(request("sleep 20"), child));
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();
    assert!(matches!(task.await.unwrap(), Err(Error::Cancelled)));
}
#[tokio::test]
async fn process_group_descendants_do_not_survive_cleanup() {
    let t = tempfile::tempdir().unwrap();
    let marker = t.path().join("leaked");
    let script = format!("(sleep 0.6; touch '{}') & sleep 20", marker.display());
    assert!(
        process::execute(request(&script), CancellationToken::new())
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!marker.exists());
    let script = format!(
        "(sleep 0.6; touch '{}') >/dev/null 2>&1 & exit 0",
        marker.display()
    );
    process::execute(request(&script), CancellationToken::new())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!marker.exists());
}
#[tokio::test]
async fn explicit_environment_and_cwd() {
    let t = tempfile::tempdir().unwrap();
    let mut r = request("printf '%s|%s|%s' \"$PWD\" \"$ONLY\" \"${HERDR_SOCKET_PATH-unset}\"");
    r.cwd = t.path().canonicalize().unwrap();
    r.env
        .insert(OsString::from("ONLY"), OsString::from("allowed"));
    let out = process::execute(r.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{}|allowed|unset", r.cwd.display())
    );
}
