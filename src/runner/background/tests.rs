use super::*;
use crate::herdr::Directory;
use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Creates `root/main` with a commit and a linked worktree at `root/linked`.
fn repo(root: &Path) -> (PathBuf, PathBuf) {
    let main = root.join("main");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&main, &["worktree", "add", "-q", "../linked"]);
    (
        main.canonicalize().unwrap(),
        root.join("linked").canonicalize().unwrap(),
    )
}

fn job(command: &str, worktrees: Worktrees, chunk_size: usize) -> Job {
    Job {
        name: "fetch".into(),
        command: vec!["/bin/sh".into(), "-c".into(), command.into()],
        worktrees,
        interval_ms: 60_000,
        timeout_ms: 5_000,
        chunk_size,
        chunk_delay_ms: 0,
        env: BTreeMap::new(),
        env_allow: vec![],
    }
}

fn workspaces(dirs: &[(&str, &Path)]) -> BTreeMap<String, Workspace> {
    dirs.iter()
        .map(|(id, dir)| {
            (
                (*id).to_owned(),
                Workspace {
                    directory: Directory {
                        reported: Some(dir.to_path_buf()),
                        canonical: Some(dir.to_path_buf()),
                        reason: String::new(),
                    },
                    generation: 1,
                },
            )
        })
        .collect()
}

fn inputs(w: &BTreeMap<String, Workspace>) -> Vec<(String, PathBuf, u64)> {
    w.iter()
        .map(|(id, w)| {
            (
                id.clone(),
                w.directory.canonical.clone().unwrap(),
                w.generation,
            )
        })
        .collect()
}

async fn run_to_idle(job: &mut BackgroundJob, w: &BTreeMap<String, Workspace>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    job.trigger();
    job.tick(true, w, "test").await;
    while job.run.is_some() {
        assert!(Instant::now() < deadline, "run did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.tick(true, w, "test").await;
    }
}

#[tokio::test]
async fn main_mode_groups_worktrees_and_skips_non_repositories() {
    let t = tempfile::tempdir().unwrap();
    let (main, linked) = repo(t.path());
    let sub = main.join("sub");
    fs::create_dir(&sub).unwrap();
    let plain = t.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let plain = plain.canonicalize().unwrap();
    let w = workspaces(&[("a", &sub), ("b", &linked), ("c", &plain)]);
    let r = resolve(inputs(&w), Worktrees::Main, CancellationToken::new()).await;
    assert_eq!(r.skipped, 1);
    assert_eq!(
        r.targets,
        vec![Target {
            dir: main,
            workspaces: BTreeMap::from([("a".into(), 1), ("b".into(), 1)]),
            error: None,
        }]
    );
}

#[tokio::test]
async fn all_mode_targets_each_worktree_toplevel_sorted() {
    let t = tempfile::tempdir().unwrap();
    let (main, linked) = repo(t.path());
    let w = workspaces(&[("a", &linked), ("b", &main), ("c", &main)]);
    let r = resolve(inputs(&w), Worktrees::All, CancellationToken::new()).await;
    assert_eq!(r.skipped, 0);
    let dirs: Vec<_> = r.targets.iter().map(|t| t.dir.clone()).collect();
    assert_eq!(dirs, vec![linked, main.clone()]);
    assert_eq!(r.targets[1].workspaces.len(), 2);
}

#[tokio::test]
async fn unresolvable_targets_fail_without_running() {
    let t = tempfile::tempdir().unwrap();
    let marker = t.path().join("ran");
    let dir = t.path().canonicalize().unwrap();
    let w = workspaces(&[("a", &dir)]);
    let script = format!("touch {}", marker.display());
    let mut job = BackgroundJob::new(job(&script, Worktrees::Main, 4), 1);
    job.trigger();
    job.tick(true, &w, "test").await;
    let failed = Target {
        dir: dir.clone(),
        workspaces: BTreeMap::from([("a".into(), 1)]),
        error: Some("main worktree unavailable".into()),
    };
    let run = job.run.as_mut().unwrap();
    run.resolve.take().unwrap().abort();
    run.resolve = Some(tokio::spawn(async move {
        Resolution {
            targets: vec![failed],
            skipped: 0,
        }
    }));
    run_to_idle(&mut job, &w).await;
    let last = job.status.last_run.clone().unwrap();
    assert_eq!((last.targets, last.succeeded, last.failed), (1, 0, 1));
    assert_eq!(
        job.status.targets[&dir].error.as_deref(),
        Some("main worktree unavailable")
    );
    assert!(!marker.exists());
}

#[tokio::test]
async fn chunks_run_sequentially_with_context_and_count_failures() {
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("log");
    let mut dirs = Vec::new();
    for name in ["r1", "r2", "r3"] {
        let dir = t.path().join(name);
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        dirs.push(dir.canonicalize().unwrap());
    }
    let w = workspaces(&[("a", &dirs[0]), ("b", &dirs[1]), ("c", &dirs[2])]);
    let script = format!(
        "echo \"start $HERDR_TOKENS_JOB $HERDR_TOKENS_WORKSPACE_IDS $PWD\" >> {log}; sleep 0.2; \
         echo \"end $HERDR_TOKENS_WORKSPACE_IDS\" >> {log}; echo noise; [ \"$HERDR_TOKENS_WORKSPACE_IDS\" != c ]",
        log = log.display()
    );
    let mut job = BackgroundJob::new(job(&script, Worktrees::Main, 2), 1);
    run_to_idle(&mut job, &w).await;
    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(Into::into)
        .collect();
    assert_eq!(lines.len(), 6);
    // The second chunk starts only after both first-chunk processes ended.
    assert!(lines[..4].iter().all(|l| !l.contains(" c ")));
    assert_eq!(lines[4], format!("start fetch c {}", dirs[2].display()));
    let last = job.status.last_run.clone().unwrap();
    assert_eq!(
        (last.targets, last.succeeded, last.failed, last.skipped),
        (3, 2, 1, 0)
    );
    let failed = &job.status.targets[&dirs[2]];
    assert_eq!(
        (failed.last_exit, failed.consecutive_failures),
        (Some(1), 1)
    );
    assert!(job.status.targets.values().all(|t| !t.running));
    run_to_idle(&mut job, &w).await;
    assert_eq!(job.status.targets[&dirs[2]].consecutive_failures, 2);
    assert_eq!(job.status.targets[&dirs[0]].consecutive_failures, 0);
    assert!(job.due > Instant::now());
}

#[tokio::test]
async fn stale_workspaces_are_pruned_before_each_chunk() {
    let t = tempfile::tempdir().unwrap();
    let mut dirs = Vec::new();
    for name in ["r1", "r2"] {
        let dir = t.path().join(name);
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        dirs.push(dir.canonicalize().unwrap());
    }
    let mut w = workspaces(&[("a", &dirs[0]), ("b", &dirs[1])]);
    let mut job = BackgroundJob::new(job("sleep 0.2", Worktrees::Main, 1), 1);
    job.trigger();
    job.tick(true, &w, "test").await;
    while job.run.as_ref().is_some_and(|r| r.chunk.0 == 0) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.tick(true, &w, "test").await;
    }
    w.get_mut("b").unwrap().generation += 1;
    while job.run.is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.tick(true, &w, "test").await;
    }
    let last = job.status.last_run.clone().unwrap();
    assert_eq!((last.targets, last.succeeded, last.skipped), (2, 1, 1));
    assert!(!job.status.targets.contains_key(&dirs[1]));
}

#[tokio::test]
async fn disconnected_runs_hold_and_triggers_coalesce() {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q"]);
    let dir = t.path().canonicalize().unwrap();
    let w = workspaces(&[("a", &dir)]);
    let mut job = BackgroundJob::new(job("sleep 0.2", Worktrees::Main, 4), 1);
    job.trigger();
    job.tick(false, &w, "test").await;
    assert!(job.run.is_none() && job.run_pending);
    job.tick(true, &w, "test").await;
    assert!(job.run.is_some() && !job.run_pending);
    job.trigger();
    job.trigger();
    while job.run.is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.tick(true, &w, "test").await;
    }
    assert!(job.run_pending);
    assert!(job.due <= Instant::now());
    job.tick(true, &w, "test").await;
    assert!(job.run.is_some() && !job.run_pending);
    job.cancel().await;
    assert!(job.run.is_none());
}

#[tokio::test]
async fn cancel_terminates_running_processes() {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q"]);
    let dir = t.path().canonicalize().unwrap();
    let w = workspaces(&[("a", &dir)]);
    let mut job = BackgroundJob::new(job("sleep 30", Worktrees::Main, 4), 1);
    job.trigger();
    job.tick(true, &w, "test").await;
    while job.run.as_ref().is_none_or(|r| r.current.is_empty()) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.tick(true, &w, "test").await;
    }
    let started = Instant::now();
    job.cancel().await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(job.run.is_none());
    assert!(!job.status.targets[&dir].running);
}

#[tokio::test]
async fn nine_targets_split_into_chunks_of_four_four_one() {
    let w = BTreeMap::new();
    let mut job = BackgroundJob::new(job("exit 0", Worktrees::Main, 4), 1);
    job.trigger();
    job.tick(true, &w, "test").await;
    let run = job.run.as_mut().unwrap();
    run.resolve.take().unwrap().abort();
    run.resolve = Some(tokio::spawn(async {
        Resolution {
            targets: (0..9)
                .map(|i| Target {
                    dir: PathBuf::from(format!("/t{i}")),
                    workspaces: BTreeMap::from([(format!("w{i}"), 1)]),
                    error: None,
                })
                .collect(),
            skipped: 0,
        }
    }));
    while job.run.as_ref().unwrap().resolve.is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job.resolved("test").await;
    }
    let run = job.run.as_ref().unwrap();
    let sizes: Vec<_> = run.chunks.iter().map(Vec::len).collect();
    assert_eq!(sizes, [4, 4, 1]);
    assert_eq!(run.chunk, (0, 3));
}

#[tokio::test]
async fn late_runs_count_missed_deadlines_and_reschedule_now() {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q"]);
    let dir = t.path().canonicalize().unwrap();
    let w = workspaces(&[("a", &dir)]);
    let mut late = job("sleep 0.1", Worktrees::Main, 4);
    late.interval_ms = 50;
    let mut job_late = BackgroundJob::new(late, 1);
    job_late.trigger();
    job_late.tick(true, &w, "test").await;
    while job_late.run.is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        job_late.tick(true, &w, "test").await;
    }
    assert_eq!(job_late.status.missed_deadlines, 1);
    assert!(job_late.due <= Instant::now());
    let mut on_time = BackgroundJob::new(job("exit 0", Worktrees::Main, 4), 1);
    on_time.trigger();
    let started = Instant::now();
    on_time.tick(true, &w, "test").await;
    while on_time.run.is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        on_time.tick(true, &w, "test").await;
    }
    assert_eq!(on_time.status.missed_deadlines, 0);
    assert!(on_time.due >= started + Duration::from_millis(60_000));
}

#[tokio::test]
async fn resolver_panic_retains_diagnostics_and_counts_failure() {
    let mut job = BackgroundJob::new(job("exit 0", Worktrees::Main, 4), 1);
    job.status
        .targets
        .insert(PathBuf::from("/previous"), TargetStatus::default());
    job.start(&BTreeMap::new());
    let run = job.run.as_mut().unwrap();
    run.resolve.take().unwrap().await.unwrap();
    run.resolve = Some(tokio::spawn(async { panic!("injected resolver failure") }));
    job.resolved("test").await;
    job.finish();
    assert_eq!(job.status.last_run.as_ref().unwrap().failed, 1);
    assert_eq!(
        job.status.targets[&PathBuf::from("/previous")]
            .error
            .as_deref(),
        Some("resolver task failed")
    );
}

#[tokio::test]
async fn cancelled_resolution_is_failed_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = resolve(
        vec![("a".into(), dir.path().into(), 1)],
        Worktrees::All,
        cancel,
    )
    .await;
    assert_eq!(result.skipped, 0);
    assert_eq!(
        result.targets[0].error.as_deref(),
        Some("process cancelled")
    );
}
