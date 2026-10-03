#[cfg(test)]
mod tests;

use crate::task::OwnedTask;
use crate::{
    config::{Collector, hash},
    diagnostics::JobStatus,
    providers::{self, Collected, Patch},
    publisher::{Generation, Key},
};
use std::{path::PathBuf, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(super) struct CollectorTask {
    pub collector: Collector,
    pub due: Instant,
    pub order: u64,
    pub refresh: bool,
    pub task: Option<OwnedTask<Completion>>,
    pub status: JobStatus,
    pub cached: Option<Cached>,
}

#[derive(Clone)]
pub(super) struct Cached {
    pub patch: Patch,
    pub completed: Instant,
}

pub(super) struct Completion {
    generation: Generation,
    result: Result<Collected, providers::Error>,
    completed: Instant,
    duration_ms: u64,
}

pub(super) fn jitter(key: &Key, generation: u64, ceiling: u64) -> u64 {
    if ceiling == 0 {
        return 0;
    }
    let digest = hash(format!("{}\0{}\0{generation}", key.workspace, key.collector).as_bytes());
    u64::from_str_radix(&digest[..16], 16).expect("hex hash") % (ceiling + 1)
}

impl CollectorTask {
    pub fn new(collector: Collector, due: Instant, order: u64, status: JobStatus) -> Self {
        Self {
            collector,
            due,
            order,
            status,
            refresh: false,
            task: None,
            cached: None,
        }
    }

    pub fn refresh(&mut self) {
        if self.task.is_some() {
            self.refresh = true;
        } else {
            self.due = Instant::now();
        }
    }

    pub fn start(
        &mut self,
        workspace: Option<String>,
        cwd: PathBuf,
        generation: Generation,
        order: u64,
        endpoint: &str,
    ) {
        let collector = self.collector.clone();
        let cancel = CancellationToken::new();
        let start = Instant::now();
        self.order = order;
        if start.saturating_duration_since(self.due) > Duration::from_millis(collector.interval_ms)
        {
            self.status.missed_deadlines += 1;
            if self.status.missed_deadlines.is_power_of_two() {
                tracing::warn!(endpoint, workspace = ?workspace, collector = %collector.name, "collection deadline missed; endpoint overloaded");
            }
        }
        let key = Key::new(
            workspace.clone().unwrap_or_else(|| "global".into()),
            collector.name.clone(),
        );
        self.due = start
            + Duration::from_millis(
                collector.interval_ms + jitter(&key, order, collector.interval_ms / 10),
            );
        self.status.attempted = Some(start);
        self.task = Some(OwnedTask::spawn(cancel, move |child_cancel| async move {
            let result = match workspace {
                Some(workspace) => {
                    providers::collect(&collector, &workspace, &cwd, child_cancel).await
                }
                None => providers::collect_global(&collector, &cwd, child_cancel).await,
            };
            Completion {
                generation,
                result,
                completed: Instant::now(),
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }));
    }

    pub async fn finish(
        &mut self,
        generation: Generation,
        connected: bool,
        endpoint: &str,
        workspace: Option<&str>,
    ) -> Option<Cached> {
        let completion = self.task.take().expect("finished task").join().await;
        if self.refresh {
            self.due = Instant::now();
            self.refresh = false;
        }
        let completion = match completion {
            Ok(completion) => completion,
            Err(_) => {
                self.status.error = Some(providers::Error::Panic.to_string());
                self.status.failures += 1;
                return None;
            }
        };
        if completion.generation != generation || !connected {
            return None;
        }
        self.status.duration_ms = Some(completion.duration_ms);
        match completion.result {
            Ok(collected) => {
                self.status.collected = Some(completion.completed);
                self.status.failures = 0;
                self.status.error = None;
                self.status.last_collected = Some(collected.patch.clone());
                self.status.truncated = collected.truncated;
                Some(Cached {
                    patch: collected.patch,
                    completed: completion.completed,
                })
            }
            Err(error) => {
                self.status.failures += 1;
                self.status.error = Some(error.to_string());
                if self.status.failures.is_power_of_two() {
                    tracing::warn!(endpoint, workspace, collector = %self.collector.name, generation = generation.config, duration_ms = completion.duration_ms, category = %error, "collection failed");
                }
                None
            }
        }
    }
}
