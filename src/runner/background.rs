//! Periodic per-worktree jobs. Spawned tasks do IO; the coordinator owns all state.
#[cfg(test)]
mod tests;

use super::{Workspace, job::jitter};
use crate::task::OwnedTask;
use crate::{
    config::{Job, Worktrees},
    diagnostics::{BackgroundStatus, LastRun, TargetStatus},
    providers::{self, JobOutcome},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub dir: PathBuf,
    /// Workspace id to its directory generation at resolution time.
    pub workspaces: BTreeMap<String, u64>,
    /// Resolution failure; such a target is reported as failed and never run.
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Resolution {
    /// Sorted by `dir`.
    pub targets: Vec<Target>,
    /// Workspaces outside any Git work tree.
    pub skipped: u64,
}

/// Maps `(id, canonical dir, generation)` workspaces onto job targets.
pub(super) async fn resolve(
    workspaces: Vec<(String, PathBuf, u64)>,
    mode: Worktrees,
    cancel: CancellationToken,
) -> Resolution {
    let mut by_dir: BTreeMap<PathBuf, BTreeMap<String, u64>> = BTreeMap::new();
    for (id, dir, generation) in workspaces {
        by_dir.entry(dir).or_default().insert(id, generation);
    }
    // Keyed by common dir in `main` mode and by toplevel in `all` mode.
    let mut groups: BTreeMap<PathBuf, BTreeMap<String, u64>> = BTreeMap::new();
    let mut skipped = 0;
    let mut targets = Vec::new();
    for (dir, ids) in by_dir {
        match providers::resolve_worktree(&dir, cancel.clone()).await {
            Ok(providers::WorktreeResolution::Repository { toplevel, common }) => {
                let key = match mode {
                    Worktrees::Main => common,
                    Worktrees::All => toplevel,
                };
                groups.entry(key).or_default().extend(ids);
            }
            Ok(providers::WorktreeResolution::NotRepository) => skipped += ids.len() as u64,
            Err(error) => targets.push(Target {
                dir,
                workspaces: ids,
                error: Some(error.to_string()),
            }),
        }
    }
    for (key, workspaces) in groups {
        let (dir, error) = match mode {
            Worktrees::All => (key, None),
            Worktrees::Main => match providers::main_worktree(&key, cancel.clone()).await {
                Ok(dir) => (dir, None),
                Err(error) => (key, Some(error.to_string())),
            },
        };
        targets.push(Target {
            dir,
            workspaces,
            error,
        });
    }
    targets.sort_by(|a, b| a.dir.cmp(&b.dir));
    Resolution { targets, skipped }
}

struct Run {
    started: Instant,
    cancel: CancellationToken,
    resolve: Option<OwnedTask<Resolution>>,
    chunks: VecDeque<Vec<Target>>,
    /// 1-based index of the latest started chunk, and the chunk count.
    chunk: (usize, usize),
    current: Vec<(PathBuf, OwnedTask<JobOutcome>)>,
    next_chunk_at: Instant,
    targets: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
}

pub(super) struct BackgroundJob {
    pub job: Job,
    pub due: Instant,
    pub run_pending: bool,
    pub status: BackgroundStatus,
    run: Option<Run>,
}

/// Commits one target result; returns whether it succeeded.
fn record(
    status: &mut TargetStatus,
    code: Option<i32>,
    duration_ms: Option<u64>,
    error: Option<String>,
    job: &str,
    endpoint: &str,
) -> bool {
    status.running = false;
    status.last_exit = code;
    status.duration_ms = duration_ms;
    match error {
        None => {
            status.consecutive_failures = 0;
            status.error = None;
            true
        }
        Some(error) => {
            status.consecutive_failures += 1;
            if status.consecutive_failures.is_power_of_two() {
                tracing::warn!(endpoint, job, failures = status.consecutive_failures, category = %error, "background job failed");
            }
            status.error = Some(error);
            false
        }
    }
}

impl BackgroundJob {
    pub fn new(job: Job, order: u64) -> Self {
        let ceiling = (job.interval_ms / 10).min(30_000);
        let due = Instant::now()
            + Duration::from_millis(jitter(
                &crate::publisher::Key::new("background", job.name.clone()),
                order,
                ceiling,
            ));
        Self {
            job,
            due,
            run_pending: false,
            status: BackgroundStatus::default(),
            run: None,
        }
    }

    /// Requests a run as soon as the job is idle and connected; triggers coalesce.
    pub fn trigger(&mut self) {
        self.run_pending = true;
    }

    pub fn signal_cancel(&self) {
        if let Some(run) = &self.run {
            run.cancel.cancel();
        }
    }

    /// Terminates the active run and waits for its tasks.
    pub async fn cancel(&mut self) {
        self.signal_cancel();
        if let Some(run) = self.run.take() {
            if let Some(task) = run.resolve {
                let _ = task.join().await;
            }
            for (_, task) in run.current {
                let _ = task.join().await;
            }
        }
        for target in self.status.targets.values_mut() {
            target.running = false;
        }
    }

    /// Advances the job; never blocks on unfinished tasks. Disconnected, it only
    /// collects finished work: no run or chunk starts.
    pub async fn tick(
        &mut self,
        connected: bool,
        workspaces: &BTreeMap<String, Workspace>,
        endpoint: &str,
    ) {
        let Some(run) = &self.run else {
            if connected && (self.run_pending || Instant::now() >= self.due) {
                self.start(workspaces);
            }
            return;
        };
        if run.resolve.as_ref().is_some_and(OwnedTask::is_finished) {
            self.resolved(endpoint).await;
        }
        self.collect(endpoint).await;
        let run = self.run.as_ref().expect("active run");
        if run.resolve.is_some() || !run.current.is_empty() {
            return;
        }
        if run.chunks.is_empty() {
            self.finish();
        } else if connected && Instant::now() >= run.next_chunk_at {
            self.start_chunk(workspaces);
        }
    }

    fn start(&mut self, workspaces: &BTreeMap<String, Workspace>) {
        self.run_pending = false;
        let inputs = workspaces
            .iter()
            .filter_map(|(id, w)| {
                w.directory
                    .canonical
                    .clone()
                    .map(|dir| (id.clone(), dir, w.generation))
            })
            .collect();
        let cancel = CancellationToken::new();
        let now = Instant::now();
        let mode = self.job.worktrees;
        self.run = Some(Run {
            started: now,
            resolve: Some(OwnedTask::spawn(cancel.child_token(), move |cancel| {
                resolve(inputs, mode, cancel)
            })),
            cancel,
            chunks: VecDeque::new(),
            chunk: (0, 0),
            current: Vec::new(),
            next_chunk_at: now,
            targets: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        });
    }

    async fn resolved(&mut self, endpoint: &str) {
        let Self {
            job, status, run, ..
        } = self;
        let run = run.as_mut().expect("active run");
        let resolution = match run.resolve.take().expect("resolution").join().await {
            Ok(resolution) => resolution,
            Err(_) => {
                // Preserve the previous target diagnostics: discovery did not succeed.
                run.failed += 1;
                for target in status.targets.values_mut() {
                    record(
                        target,
                        None,
                        None,
                        Some("resolver task failed".into()),
                        &job.name,
                        endpoint,
                    );
                }
                tracing::warn!(endpoint, job = %job.name, "resolver task failed");
                return;
            }
        };
        run.targets = resolution.targets.len() as u64;
        run.skipped += resolution.skipped;
        let mut old = std::mem::take(&mut status.targets);
        let mut runnable = Vec::new();
        for target in resolution.targets {
            let mut target_status = old.remove(&target.dir).unwrap_or_default();
            target_status.workspaces = target.workspaces.keys().cloned().collect();
            target_status.running = false;
            if let Some(error) = &target.error {
                record(
                    &mut target_status,
                    None,
                    None,
                    Some(error.clone()),
                    &job.name,
                    endpoint,
                );
                run.failed += 1;
            } else {
                runnable.push(target.clone());
            }
            status.targets.insert(target.dir, target_status);
        }
        run.chunks = runnable
            .chunks(job.chunk_size)
            .map(<[Target]>::to_vec)
            .collect();
        run.chunk = (0, run.chunks.len());
        run.next_chunk_at = Instant::now();
    }

    async fn collect(&mut self, endpoint: &str) {
        let Self {
            job, status, run, ..
        } = self;
        let run = run.as_mut().expect("active run");
        let busy = !run.current.is_empty();
        let mut i = 0;
        while i < run.current.len() {
            if !run.current[i].1.is_finished() {
                i += 1;
                continue;
            }
            let (dir, task) = run.current.swap_remove(i);
            let outcome = task.join().await.unwrap_or_else(|_| JobOutcome {
                error: Some("job task panicked".into()),
                ..JobOutcome::default()
            });
            let target = status.targets.entry(dir).or_default();
            if record(
                target,
                outcome.code,
                Some(outcome.duration_ms),
                outcome.error,
                &job.name,
                endpoint,
            ) {
                run.succeeded += 1;
            } else {
                run.failed += 1;
            }
        }
        if busy && run.current.is_empty() {
            run.next_chunk_at = Instant::now() + Duration::from_millis(job.chunk_delay_ms);
        }
    }

    /// Starts the next chunk, dropping workspaces that vanished or changed
    /// directory since resolution; a target left without workspaces is skipped.
    fn start_chunk(&mut self, workspaces: &BTreeMap<String, Workspace>) {
        let Self {
            job, status, run, ..
        } = self;
        let run = run.as_mut().expect("active run");
        let chunk = run.chunks.pop_front().expect("pending chunk");
        run.chunk.0 += 1;
        for mut target in chunk {
            target.workspaces.retain(|id, generation| {
                workspaces
                    .get(id)
                    .is_some_and(|w| w.generation == *generation && w.directory.canonical.is_some())
            });
            if target.workspaces.is_empty() {
                run.skipped += 1;
                status.targets.remove(&target.dir);
                continue;
            }
            let ids: Vec<String> = target.workspaces.into_keys().collect();
            let target_status = status.targets.entry(target.dir.clone()).or_default();
            target_status.running = true;
            target_status.workspaces = ids.clone();
            let job = job.clone();
            let dir = target.dir.clone();
            let cancel = run.cancel.clone();
            run.current.push((
                target.dir,
                OwnedTask::spawn(cancel.child_token(), move |cancel| async move {
                    providers::run_job(&job, &dir, &ids, cancel).await
                }),
            ));
        }
    }

    fn finish(&mut self) {
        let run = self.run.take().expect("active run");
        let now = Instant::now();
        let next = run.started + Duration::from_millis(self.job.interval_ms);
        if now >= next {
            self.status.missed_deadlines += 1;
            self.due = now;
        } else {
            self.due = next;
        }
        if self.run_pending {
            self.due = now;
        }
        self.status.last_run = Some(LastRun {
            completed: now,
            duration_ms: now.saturating_duration_since(run.started).as_millis() as u64,
            targets: run.targets,
            succeeded: run.succeeded,
            failed: run.failed,
            skipped: run.skipped,
        });
    }

    pub fn observations(&self) -> BackgroundObservations<'_> {
        let (phase, chunk) = match &self.run {
            None => ("idle", None),
            Some(run) if run.resolve.is_some() => ("resolving", None),
            Some(run) => ("chunk", Some([run.chunk.0, run.chunk.1])),
        };
        BackgroundObservations {
            diagnostics: self.status.observations(),
            name: &self.job.name,
            worktrees: self.job.worktrees,
            phase,
            chunk,
            run_pending: self.run_pending,
            next_due_in_ms: self
                .due
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
        }
    }
}
#[derive(Serialize)]
pub(super) struct BackgroundObservations<'a> {
    #[serde(flatten)]
    diagnostics: crate::diagnostics::BackgroundDiagnostics<'a>,
    name: &'a str,
    worktrees: Worktrees,
    phase: &'static str,
    chunk: Option<[usize; 2]>,
    run_pending: bool,
    next_due_in_ms: u64,
}
