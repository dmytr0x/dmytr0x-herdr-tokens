use super::*;
use crate::{
    config::{CollectorKind, CommandOutput, CommandScope, CommandSpec},
    providers::Token,
};
use std::collections::BTreeMap;

fn job() -> CollectorTask {
    CollectorTask::new(
        Collector {
            name: "status".into(),
            kind: CollectorKind::Command {
                spec: CommandSpec {
                    argv: vec!["printf".into(), "ready".into()],
                    env: BTreeMap::new(),
                    env_allow: vec![],
                },
                scope: CommandScope::Workspace,
                output: CommandOutput::Text,
            },
            interval_ms: 1000,
            timeout_ms: 500,
            ttl_ms: 3000,
            tokens: BTreeMap::from([("status".into(), "stdout".into())]),
        },
        Instant::now() + Duration::from_secs(1),
        1,
        JobStatus::default(),
    )
}

fn generation() -> Generation {
    Generation {
        config: 1,
        directory: 2,
        connection: 3,
    }
}

fn complete(job: &mut CollectorTask, result: Result<Collected, providers::Error>) {
    job.task = Some(OwnedTask::spawn(CancellationToken::new(), |_| async move {
        Completion {
            generation: generation(),
            result,
            completed: Instant::now(),
            duration_ms: 12,
        }
    }));
}

fn collected() -> Result<Collected, providers::Error> {
    Ok(Collected {
        patch: BTreeMap::from([("status".into(), Token::Set("ready".into()))]),
        truncated: true,
    })
}

#[tokio::test(start_paused = true)]
async fn idle_refresh_is_immediate_and_running_refreshes_coalesce() {
    let mut job = job();
    job.refresh();
    assert_eq!(job.due, Instant::now());
    complete(&mut job, collected());
    job.refresh();
    job.refresh();
    assert!(job.refresh);
    tokio::time::advance(Duration::from_millis(10)).await;
    assert!(
        job.finish(generation(), true, "endpoint", None)
            .await
            .is_some()
    );
    assert_eq!(job.due, Instant::now());
    assert!(!job.refresh);
    assert!(job.task.is_none());
}

#[tokio::test(start_paused = true)]
async fn failures_preserve_last_success_and_success_resets_failures() {
    let mut job = job();
    complete(&mut job, collected());
    let cached = job
        .finish(generation(), true, "endpoint", None)
        .await
        .unwrap();
    assert_eq!(job.status.last_collected.as_ref(), Some(&cached.patch));
    assert!(job.status.truncated);
    assert_eq!(job.status.duration_ms, Some(12));
    for failures in 1..=2 {
        complete(&mut job, Err(providers::Error::InvalidOutput));
        assert!(
            job.finish(generation(), true, "endpoint", None)
                .await
                .is_none()
        );
        assert_eq!(job.status.failures, failures);
        assert_eq!(job.status.last_collected.as_ref(), Some(&cached.patch));
        assert_eq!(job.status.collected, Some(cached.completed));
    }
    complete(&mut job, collected());
    assert!(
        job.finish(generation(), true, "endpoint", None)
            .await
            .is_some()
    );
    assert_eq!(job.status.failures, 0);
    assert!(job.status.error.is_none());
}

#[tokio::test(start_paused = true)]
async fn every_generation_and_disconnection_fences_completions() {
    let original = generation();
    for (current, connected) in [
        (
            Generation {
                config: 2,
                ..original
            },
            true,
        ),
        (
            Generation {
                directory: 3,
                ..original
            },
            true,
        ),
        (
            Generation {
                connection: 4,
                ..original
            },
            true,
        ),
        (original, false),
    ] {
        let mut job = job();
        complete(&mut job, collected());
        assert!(
            job.finish(current, connected, "endpoint", None)
                .await
                .is_none()
        );
        assert!(job.status.collected.is_none());
        assert!(job.status.last_collected.is_none());
        assert!(job.status.duration_ms.is_none());
        assert!(job.task.is_none());
    }
}

#[tokio::test]
async fn aborted_task_is_a_failure_not_a_publication() {
    let mut job = job();
    job.task = Some(OwnedTask::spawn(CancellationToken::new(), |_| async {
        panic!("injected failure")
    }));
    assert!(
        job.finish(generation(), true, "endpoint", None)
            .await
            .is_none()
    );
    assert_eq!(job.status.failures, 1);
    assert_eq!(job.status.error.as_deref(), Some("collector task panicked"));
}

#[tokio::test]
async fn starting_records_attempt_deadline_and_order_for_both_scopes() {
    for workspace in [None, Some("w1".to_owned())] {
        let mut job = job();
        job.due = Instant::now() - Duration::from_secs(2);
        job.start(
            workspace.clone(),
            std::env::current_dir().unwrap(),
            generation(),
            7,
            "endpoint",
        );
        let attempted = job.status.attempted.unwrap();
        assert_eq!(job.order, 7);
        assert_eq!(job.status.missed_deadlines, 1);
        assert!(
            (Duration::from_millis(1000)..=Duration::from_millis(1100))
                .contains(&(job.due - attempted))
        );
        let cached = job
            .finish(generation(), true, "endpoint", workspace.as_deref())
            .await
            .unwrap();
        assert_eq!(cached.patch["status"], Token::Set("ready".into()));
    }
}

#[test]
fn jitter_is_deterministic_and_bounded() {
    let key = Key::new("w1", "status");
    assert_eq!(jitter(&key, 1, 0), 0);
    for generation in 0..100 {
        let delay = jitter(&key, generation, 100);
        assert!(delay <= 100);
        assert_eq!(delay, jitter(&key, generation, 100));
    }
}
