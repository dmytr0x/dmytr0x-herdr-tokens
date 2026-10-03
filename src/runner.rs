//! Single coordinator: tasks do IO, only this module commits state transitions.
mod background;
mod job;
use crate::{
    config::{Config, Snapshot},
    diagnostics::JobStatus,
    herdr::{self, Directory, Discovery, Herdr},
    providers,
    publisher::{Generation, Key, Pending, Publication, Publisher},
    runtime::{self, Control, Endpoint, Identity, Response, Sequences},
};
use anyhow::Result;
use background::BackgroundJob;
use job::{Job, jitter};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

struct Workspace {
    directory: Directory,
    generation: u64,
}
struct Report {
    send: Publication,
    seq: u64,
    task: JoinHandle<Result<(), herdr::Error>>,
}
struct Coordinator {
    identity: Identity,
    herdr: Herdr,
    config: Config,
    config_generation: u64,
    connection_generation: u64,
    connection: &'static str,
    discovery_error: Option<String>,
    rejected: Option<String>,
    rejected_generation: u64,
    workspaces: BTreeMap<String, Workspace>,
    jobs: BTreeMap<Key, Job>,
    global_jobs: BTreeMap<String, Job>,
    background: BTreeMap<String, BackgroundJob>,
    publisher: Publisher,
    report_errors: BTreeMap<String, (String, u64)>,
    sequences: Sequences,
    report: Option<Report>,
    discovery: Option<JoinHandle<Result<Discovery, herdr::Error>>>,
    discover_due: Instant,
    backoff: u64,
    started: Instant,
    order: u64,
    scan_due: Instant,
    candidate: Option<(String, Instant)>,
    observed_hash: String,
}
impl Coordinator {
    fn generation(&self, key: &Key) -> Generation {
        Generation {
            config: self.config_generation,
            directory: self.workspaces[&key.0].generation,
            connection: self.connection_generation,
        }
    }
    fn global_generation(&self) -> Generation {
        Generation {
            config: self.config_generation,
            directory: 0,
            connection: self.connection_generation,
        }
    }
    fn current(&self, key: &Key, generation: Generation) -> bool {
        generation.config == self.config_generation
            && generation.connection == self.connection_generation
            && self
                .workspaces
                .get(&key.0)
                .is_some_and(|w| w.generation == generation.directory)
            && (self.jobs.contains_key(key) || self.global_jobs.contains_key(&key.1))
    }
    fn refresh(&mut self, workspace: Option<&str>) {
        for ((w, _), job) in &mut self.jobs {
            if workspace.is_none_or(|wanted| wanted == w) {
                job.refresh();
            }
        }
        for job in self.global_jobs.values_mut() {
            job.refresh();
        }
        if self.connection != "Connected" {
            self.discover_due = Instant::now();
        }
    }
    async fn cancel_workspace_jobs(&mut self) {
        for job in self.jobs.values() {
            if let Some(cancel) = &job.cancel {
                cancel.cancel();
            }
        }
        for job in self.jobs.values_mut() {
            if let Some(task) = job.task.take() {
                let _ = task.await;
            }
            job.cancel = None;
        }
    }
    async fn cancel_jobs(&mut self) {
        for job in self.jobs.values().chain(self.global_jobs.values()) {
            if let Some(c) = &job.cancel {
                c.cancel();
            }
        }
        // All cancellations were issued together; cleanup grace is concurrent.
        for job in self.jobs.values_mut().chain(self.global_jobs.values_mut()) {
            if let Some(task) = job.task.take() {
                let _ = task.await;
                job.due = Instant::now();
            }
            job.cancel = None;
        }
    }
    async fn disconnect(&mut self) {
        self.connection_generation += 1;
        self.connection = "Disconnected";
        self.publisher.discard_values();
        self.cancel_jobs().await;
        let delay = self.backoff.min(30);
        self.backoff = (delay * 2).min(30);
        self.discover_due = Instant::now()
            + Duration::from_millis(
                delay * 1000
                    + jitter(
                        &(self.identity.endpoint.clone(), "reconnect".into()),
                        self.connection_generation,
                        delay * 100,
                    ),
            );
    }
    async fn finish_report(&mut self) -> Result<()> {
        let Some(report) = self.report.take() else {
            return Ok(());
        };
        let result = report.task.await.unwrap_or(Err(herdr::Error::Connection));
        if result.is_ok() {
            self.publisher.acknowledged(&report.send);
            self.report_errors.remove(&report.send.workspace);
        }
        if let Some((key, generation)) = &report.send.job {
            let current = self.current(key, *generation);
            let status = if current {
                self.jobs
                    .get_mut(key)
                    .map(|job| &mut job.status)
                    .or_else(|| self.global_jobs.get_mut(&key.1).map(|job| &mut job.status))
            } else {
                None
            };
            if let Some(status) = status {
                match &result {
                    Ok(()) => {
                        status.acknowledged = Some(Instant::now());
                        status.acknowledged_completion = Some(report.send.completed);
                        status.last_acknowledged = Some(report.send.patch.clone());
                        status.publication_error = None;
                    }
                    Err(e) => {
                        status.publication_error = Some(e.to_string());
                    }
                }
            }
        }
        match result {
            Ok(()) => {
                tracing::debug!(endpoint = %self.identity.endpoint, workspace = %report.send.workspace, sequence = report.seq, "report acknowledged")
            }
            Err(e) => {
                let error = self
                    .report_errors
                    .entry(report.send.workspace.clone())
                    .or_insert_with(|| (e.to_string(), 0));
                if error.0 != e.to_string() {
                    *error = (e.to_string(), 0);
                }
                error.1 += 1;
                if error.1.is_power_of_two() {
                    tracing::warn!(endpoint = %self.identity.endpoint, workspace = %report.send.workspace, sequence = report.seq, category = %e, "report failed");
                }
                if matches!(e, herdr::Error::WorkspaceNotFound) {
                    self.remove_workspace(&report.send.workspace).await;
                    self.discover_due = Instant::now();
                } else if e.disconnected() {
                    self.disconnect().await;
                }
            }
        }
        Ok(())
    }
    async fn remove_workspace(&mut self, id: &str) {
        for ((w, _), job) in &mut self.jobs {
            if w == id
                && let Some(c) = &job.cancel
            {
                c.cancel();
            }
        }
        let keys: Vec<_> = self.jobs.keys().filter(|(w, _)| w == id).cloned().collect();
        for key in keys {
            if let Some(mut job) = self.jobs.remove(&key)
                && let Some(t) = job.task.take()
            {
                let _ = t.await;
            }
        }
        self.workspaces.remove(id);
        self.publisher.remove_workspace(id);
        self.report_errors.remove(id);
    }
    fn rebuild_jobs(&mut self) {
        let mut old = std::mem::take(&mut self.jobs);
        for (id, w) in &self.workspaces {
            if w.directory.canonical.is_none() {
                continue;
            }
            for c in self.config.collectors.iter().filter(|c| !c.global) {
                let key = (id.clone(), c.name.clone());
                self.order += 1;
                let status = old
                    .remove(&key)
                    .filter(|j| j.collector == *c)
                    .map(|j| j.status)
                    .unwrap_or_default();
                let due = Instant::now()
                    + Duration::from_millis(jitter(
                        &key,
                        self.config_generation + w.generation,
                        200.min(c.interval_ms / 10),
                    ));
                self.jobs
                    .insert(key, Job::new(c.clone(), due, self.order, status));
            }
        }
        let mut old_global = std::mem::take(&mut self.global_jobs);
        for c in self.config.collectors.iter().filter(|c| c.global) {
            if let Some(job) = old_global.remove(&c.name).filter(|job| job.collector == *c) {
                self.global_jobs.insert(c.name.clone(), job);
                continue;
            }
            self.order += 1;
            let key = ("global".into(), c.name.clone());
            let due = Instant::now()
                + Duration::from_millis(jitter(
                    &key,
                    self.config_generation,
                    200.min(c.interval_ms / 10),
                ));
            self.global_jobs.insert(
                c.name.clone(),
                Job::new(c.clone(), due, self.order, JobStatus::default()),
            );
        }
    }
    /// Keeps unchanged background jobs running; cancels changed and removed ones.
    async fn rebuild_background(&mut self) {
        let mut old = std::mem::take(&mut self.background);
        let mut stale = Vec::new();
        for job in &self.config.jobs {
            match old.remove(&job.name) {
                Some(kept) if kept.job == *job => {
                    self.background.insert(job.name.clone(), kept);
                }
                replaced => {
                    stale.extend(replaced);
                    self.order += 1;
                    self.background.insert(
                        job.name.clone(),
                        BackgroundJob::new(job.clone(), self.order),
                    );
                }
            }
        }
        stale.extend(old.into_values());
        for job in &stale {
            job.signal_cancel();
        }
        for mut job in stale {
            job.cancel().await;
        }
    }
    async fn cancel_background(&mut self) {
        for job in self.background.values() {
            job.signal_cancel();
        }
        for job in self.background.values_mut() {
            job.cancel().await;
        }
    }
    fn publish_global_cache(&mut self, workspace: &str) {
        let cached: Vec<_> = self
            .global_jobs
            .iter()
            .filter_map(|(name, job)| {
                job.cached.as_ref().map(|cached| {
                    (
                        name.clone(),
                        cached.clone(),
                        job.collector.interval_ms,
                        job.collector.ttl_ms,
                    )
                })
            })
            .collect();
        for (name, cached, interval_ms, ttl_ms) in cached {
            let key = (workspace.to_owned(), name);
            self.publisher.put(Pending {
                generation: self.generation(&key),
                key,
                patch: cached.patch,
                completed: cached.completed,
                interval_ms,
                ttl_ms,
            });
        }
    }
    async fn reconcile(&mut self, snapshot: Discovery) -> Result<()> {
        let changed = snapshot.len() != self.workspaces.len()
            || snapshot.iter().any(|(id, d)| {
                self.workspaces
                    .get(id)
                    .is_none_or(|w| w.directory.canonical != d.canonical)
            });
        let reconnect = self.connection != "Connected";
        if changed {
            // Finish any metadata mutation before constructing clear barriers.
            self.cancel_workspace_jobs().await;
            self.finish_report().await?;
            let mut added = Vec::new();
            let absent: Vec<_> = self
                .workspaces
                .keys()
                .filter(|w| !snapshot.contains_key(*w))
                .cloned()
                .collect();
            for id in absent {
                self.remove_workspace(&id).await;
            }
            for (id, directory) in snapshot {
                if let Some(old) = self.workspaces.get_mut(&id) {
                    if old.directory.canonical != directory.canonical {
                        self.publisher
                            .clear(&id, self.config.workspace_token_names());
                        old.generation += 1;
                        for ((w, _), j) in &mut self.jobs {
                            if *w == id {
                                j.status = JobStatus::default();
                            }
                        }
                    }
                    old.directory = directory;
                } else {
                    added.push(id.clone());
                    self.workspaces.insert(
                        id,
                        Workspace {
                            directory,
                            generation: 1,
                        },
                    );
                }
            }
            self.rebuild_jobs();
            for id in added {
                self.publish_global_cache(&id);
            }
        } else {
            for (id, directory) in snapshot {
                self.workspaces
                    .get_mut(&id)
                    .expect("known workspace")
                    .directory = directory;
            }
        }
        self.connection = "Connected";
        self.backoff = 1;
        self.discovery_error = None;
        if reconnect {
            self.refresh(None);
        }
        Ok(())
    }
    async fn reload(&mut self) -> Result<bool> {
        let snapshot = Snapshot::read(&self.identity.config)?;
        let config = snapshot.parse()?;
        providers::preflight(&config).await?;
        anyhow::ensure!(
            Snapshot::read(&self.identity.config)? == snapshot,
            "configuration changed during validation; retry"
        );
        self.observed_hash = snapshot.hash();
        self.candidate = None;
        if config == self.config {
            self.rejected = None;
            return Ok(false);
        }
        if config.without_jobs() == self.config.without_jobs() {
            // Collectors, generations and publications are untouched.
            self.config = config;
            self.rejected = None;
            self.rebuild_background().await;
            tracing::info!(
                endpoint = %self.identity.endpoint,
                generation = self.config_generation,
                "background job configuration committed"
            );
            return Ok(true);
        }
        self.cancel_jobs().await;
        self.finish_report().await?;
        // Discovery results computed using old overrides must never be committed.
        if let Some(discovery) = self.discovery.take() {
            let _ = discovery.await;
        }
        self.config_generation += 1;
        self.publisher.discard_values();
        let mut clear = BTreeSet::new();
        for old in &self.config.collectors {
            if !config.collectors.contains(old) {
                clear.extend(old.tokens.keys().cloned());
            }
        }
        for (id, w) in &mut self.workspaces {
            let override_changed =
                self.config.workspace_dirs.get(id) != config.workspace_dirs.get(id);
            let mut keys = clear.clone();
            if override_changed {
                keys.extend(config.workspace_token_names());
                w.generation += 1;
                w.directory.canonical = None;
                w.directory.reason = "awaiting discovery after override change".into();
            }
            self.publisher.clear(id, keys);
        }
        self.config = config;
        self.rejected = None;
        self.rebuild_jobs();
        self.rebuild_background().await;
        self.discover_due = Instant::now();
        tracing::info!(
            endpoint = %self.identity.endpoint,
            generation = self.config_generation,
            "configuration committed"
        );
        Ok(true)
    }
    fn reject(&mut self, error: &anyhow::Error) {
        self.rejected_generation += 1;
        self.rejected = Some(error.to_string());
        tracing::warn!(endpoint = %self.identity.endpoint, generation = self.config_generation, category = %error, "candidate configuration rejected");
    }
    async fn poll_config(&mut self) {
        if Instant::now() < self.scan_due {
            return;
        }
        self.scan_due = Instant::now() + Duration::from_secs(1);
        let snapshot = Snapshot::read(&self.identity.config);
        let fingerprint = match &snapshot {
            Ok(s) => s.hash(),
            Err(e) => format!("unreadable:{e}"),
        };
        if fingerprint == self.observed_hash {
            self.candidate = None;
            return;
        }
        match &self.candidate {
            Some((old, since))
                if old == &fingerprint && since.elapsed() >= Duration::from_millis(200) =>
            {
                self.observed_hash = fingerprint;
                self.candidate = None;
                if let Err(e) = self.reload().await {
                    self.reject(&e);
                }
            }
            _ => {
                self.candidate = Some((fingerprint, Instant::now()));
                self.scan_due = Instant::now() + Duration::from_millis(200);
            }
        }
    }
    async fn tick(&mut self) -> Result<()> {
        if self.report.as_ref().is_some_and(|r| r.task.is_finished()) {
            self.finish_report().await?;
        }
        if self.discovery.as_ref().is_some_and(JoinHandle::is_finished) {
            let discovery = self.discovery.take().expect("discovery");
            match discovery.await.unwrap_or(Err(herdr::Error::Connection)) {
                Ok(snapshot) => {
                    self.reconcile(snapshot).await?;
                    self.discover_due = Instant::now()
                        + Duration::from_millis(self.config.runtime.discovery_interval_ms);
                }
                Err(e) => {
                    let message = e.to_string();
                    if self.discovery_error.as_ref() != Some(&message) {
                        tracing::warn!(endpoint = %self.identity.endpoint, category = %e, "discovery failed; retaining snapshot");
                    }
                    self.discovery_error = Some(message);
                    if e.disconnected() {
                        self.disconnect().await;
                    } else {
                        self.discover_due = Instant::now()
                            + Duration::from_millis(self.config.runtime.discovery_interval_ms);
                    }
                }
            }
        }
        self.poll_config().await;
        if self.discovery.is_none() && Instant::now() >= self.discover_due {
            let herdr = self.herdr.clone();
            let config = self.config.clone();
            self.discovery = Some(tokio::spawn(async move { herdr.discover(&config).await }));
        }
        let finished: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.task.as_ref().is_some_and(|t| t.is_finished()))
            .map(|(k, _)| k.clone())
            .collect();
        for key in finished {
            let generation = self.generation(&key);
            let job = self.jobs.get_mut(&key).expect("job");
            if let Some(collected) = job
                .finish(
                    generation,
                    self.connection == "Connected",
                    &self.identity.endpoint,
                    Some(&key.0),
                )
                .await
            {
                self.publisher.put(Pending {
                    key,
                    generation,
                    patch: collected.patch,
                    completed: collected.completed,
                    interval_ms: job.collector.interval_ms,
                    ttl_ms: job.collector.ttl_ms,
                });
            }
        }
        let finished: Vec<_> = self
            .global_jobs
            .iter()
            .filter(|(_, job)| job.task.as_ref().is_some_and(|task| task.is_finished()))
            .map(|(name, _)| name.clone())
            .collect();
        for name in finished {
            let generation = self.global_generation();
            let job = self.global_jobs.get_mut(&name).expect("global job");
            let publish = job
                .finish(
                    generation,
                    self.connection == "Connected",
                    &self.identity.endpoint,
                    None,
                )
                .await
                .map(|cached| {
                    job.cached = Some(cached.clone());
                    (cached, job.collector.interval_ms, job.collector.ttl_ms)
                });
            if let Some((cached, interval_ms, ttl_ms)) = publish {
                let workspaces: Vec<_> = self.workspaces.keys().cloned().collect();
                for workspace in workspaces {
                    let key = (workspace, name.clone());
                    self.publisher.put(Pending {
                        generation: self.generation(&key),
                        key,
                        patch: cached.patch.clone(),
                        completed: cached.completed,
                        interval_ms,
                        ttl_ms,
                    });
                }
            }
        }
        let connected = self.connection == "Connected";
        for job in self.background.values_mut() {
            job.tick(connected, &self.workspaces, &self.identity.endpoint)
                .await;
        }
        if !connected {
            return Ok(());
        }
        let running = self
            .jobs
            .values()
            .chain(self.global_jobs.values())
            .filter(|job| job.task.is_some())
            .count();
        enum DueJob {
            Workspace(Key),
            Global(String),
        }
        let mut due: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.task.is_none() && job.due <= Instant::now())
            .map(|(key, job)| (job.due, job.order, DueJob::Workspace(key.clone())))
            .chain(
                self.global_jobs
                    .iter()
                    .filter(|(_, job)| job.task.is_none() && job.due <= Instant::now())
                    .map(|(name, job)| (job.due, job.order, DueJob::Global(name.clone()))),
            )
            .collect();
        due.sort_by_key(|(due, order, _)| (*due, *order));
        for (_, _, due_job) in due
            .into_iter()
            .take(self.config.runtime.max_concurrency.saturating_sub(running))
        {
            match due_job {
                DueJob::Workspace(key) => {
                    let generation = self.generation(&key);
                    let cwd = self.workspaces[&key.0]
                        .directory
                        .canonical
                        .clone()
                        .expect("eligible job");
                    let job = self.jobs.get_mut(&key).expect("job");
                    self.order += 1;
                    job.start(
                        Some(key.0),
                        cwd,
                        generation,
                        self.order,
                        &self.identity.endpoint,
                    );
                }
                DueJob::Global(name) => {
                    let generation = self.global_generation();
                    let cwd = self.identity.config.clone();
                    let job = self.global_jobs.get_mut(&name).expect("global job");
                    self.order += 1;
                    job.start(None, cwd, generation, self.order, &self.identity.endpoint);
                }
            }
        }
        self.dispatch_report()?;
        Ok(())
    }
    fn dispatch_report(&mut self) -> Result<()> {
        if self.report.is_some() || self.connection != "Connected" {
            return Ok(());
        }
        let config_generation = self.config_generation;
        let connection_generation = self.connection_generation;
        let (send, stale) = self.publisher.next(|key, generation| {
            (self.jobs.contains_key(key) || self.global_jobs.contains_key(&key.1))
                && generation.config == config_generation
                && generation.connection == connection_generation
                && self
                    .workspaces
                    .get(&key.0)
                    .is_some_and(|workspace| workspace.generation == generation.directory)
        });
        let mut stale_global = BTreeSet::new();
        for key in stale {
            if let Some(job) = self.jobs.get_mut(&key) {
                job.status.missed_deadlines += 1;
                if job.task.is_none() {
                    job.due = Instant::now();
                }
            } else {
                stale_global.insert(key.1);
            }
        }
        for name in stale_global {
            if let Some(job) = self.global_jobs.get_mut(&name) {
                job.status.missed_deadlines += 1;
                if job.task.is_none() {
                    job.due = Instant::now();
                }
            }
        }
        if let Some(send) = send {
            let seq = self.sequences.allocate()?;
            self.publisher.sending(&send);
            let herdr = self.herdr.clone();
            let workspace = send.workspace.clone();
            let patch = send.patch.clone();
            let ttl = send.ttl_ms;
            self.report = Some(Report {
                send,
                seq,
                task: tokio::spawn(async move { herdr.report(&workspace, &patch, seq, ttl).await }),
            });
        }
        Ok(())
    }
    fn status(&self, values: bool) -> Value {
        let workspaces: BTreeMap<_,_> = self.workspaces.iter().map(|(id,w)| (id, json!({"directory": w.directory, "generation": w.generation, "pending_clears": self.publisher.pending_clears(id), "publication_error": self.report_errors.get(id)}))).collect();
        let mut jobs: Vec<_> = self.jobs.iter().map(|(key,j)| json!({
            "workspace": key.0, "global": false, "collector": key.1, "tokens": j.collector.tokens.keys().collect::<Vec<_>>(),
            "running": j.task.is_some(), "queued": j.due <= Instant::now(), "refresh_pending": j.refresh,
            "publication_pending": self.publisher.has_pending(key),
            "next_due_in_ms": j.due.saturating_duration_since(Instant::now()).as_millis() as u64,
            "diagnostics": j.status.json(values, j.collector.ttl_ms),
        })).collect();
        jobs.extend(self.global_jobs.iter().map(|(name, job)| {
            let publication_pending = self.workspaces.keys().any(|workspace| {
                self.publisher
                    .has_pending(&(workspace.clone(), name.clone()))
            });
            json!({
                "workspace": Value::Null, "global": true, "collector": name,
                "tokens": job.collector.tokens.keys().collect::<Vec<_>>(),
                "running": job.task.is_some(), "queued": job.due <= Instant::now(), "refresh_pending": job.refresh,
                "publication_pending": publication_pending,
                "next_due_in_ms": job.due.saturating_duration_since(Instant::now()).as_millis() as u64,
                "diagnostics": job.status.json(values, job.collector.ttl_ms),
            })
        }));
        json!({"version": env!("CARGO_PKG_VERSION"), "endpoint": self.identity.endpoint, "source": "herdr-tokens", "uptime_ms": self.started.elapsed().as_millis() as u64,
            "config_hash": self.config.hash(), "config_files_hash": self.observed_hash, "config_generation": self.config_generation,
            "rejected_candidates": self.rejected_generation, "last_rejected_error": self.rejected, "connection": self.connection,
            "connection_generation": self.connection_generation, "discovery_error": self.discovery_error, "workspaces": workspaces, "jobs": jobs,
            "background_jobs": self.background.values().map(BackgroundJob::json).collect::<Vec<_>>(),
            "knowledge": "Local emitter observations only; acknowledgements do not prove current Herdr ownership or visibility."})
    }
    async fn control(&mut self, control: Control) -> (bool, Response) {
        let mut ok = true;
        let mut stop = false;
        let result = match control.request.command.as_str() {
            "ping" => json!({"version": env!("CARGO_PKG_VERSION")}),
            "status" => self.status(control.request.include_values),
            "refresh" => {
                self.refresh(control.request.workspace.as_deref());
                json!({"scheduled":true})
            }
            "reload" => match self.reload().await {
                Ok(changed) => json!({"accepted":true, "changed":changed}),
                Err(e) => {
                    self.reject(&e);
                    ok = false;
                    json!({"accepted":false,"error":e.to_string()})
                }
            },
            "run-job" => match control.request.job.as_deref() {
                None => {
                    for job in self.background.values_mut() {
                        job.trigger();
                    }
                    json!({"scheduled":true})
                }
                Some(name) => match self.background.get_mut(name) {
                    Some(job) => {
                        job.trigger();
                        json!({"scheduled":true})
                    }
                    None => {
                        ok = false;
                        json!({"error":"unknown job"})
                    }
                },
            },
            "stop" => {
                stop = true;
                json!({"stopping":true})
            }
            _ => {
                ok = false;
                json!({"error":"unknown command"})
            }
        };
        let response = Response {
            version: 1,
            ok,
            ready: true,
            identity: self.identity.clone(),
            result,
        };
        let _ = control.reply.send(response.clone());
        (stop, response)
    }
    async fn shutdown(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        self.config_generation += 1;
        for job in self.background.values() {
            job.signal_cancel();
        }
        self.cancel_jobs().await;
        self.cancel_background().await;
        let _ = self.finish_report().await;
        if let Some(discovery) = self.discovery.take() {
            let _ = discovery.await;
        }
        self.publisher.discard_values();
        for id in self.workspaces.keys() {
            self.publisher.clear(id, self.config.token_names());
        }
        while self.connection == "Connected"
            && self.publisher.has_clears()
            && Instant::now() + Duration::from_millis(1200) < deadline
        {
            if self.dispatch_report().is_err() {
                break;
            }
            let _ = self.finish_report().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tracing::info!(endpoint = %self.identity.endpoint, "runner stopped; remaining values expire by TTL");
    }
}
pub async fn run(
    endpoint: Endpoint,
    identity: Identity,
    herdr: Herdr,
    detached: bool,
) -> Result<()> {
    let lock = match endpoint.acquire()? {
        Some(lock) => lock,
        None => {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match tokio::time::timeout_at(deadline, runtime::matching(&endpoint, &identity))
                    .await
                    .map_err(|_| anyhow::anyhow!("runner readiness timed out"))?
                {
                    Ok(()) => return Ok(()),
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
    };
    let state = runtime::state_dir(&identity)?;
    crate::diagnostics::init(detached.then(|| state.join("logs")))?;
    let snapshot = Snapshot::read(&identity.config).map_err(crate::invalid)?;
    let config = snapshot.parse().map_err(crate::invalid)?;
    providers::preflight(&config).await?;
    anyhow::ensure!(
        Snapshot::read(&identity.config)? == snapshot,
        "configuration changed during startup; retry"
    );
    let sequences = Sequences::open(state, &lock)?;
    let listener = endpoint.bind(&lock)?;
    let (tx, mut rx) = mpsc::channel(32);
    let control_cancel = CancellationToken::new();
    let control_task = tokio::spawn(runtime::serve(
        listener,
        tx,
        identity.clone(),
        control_cancel.clone(),
    ));
    let now = Instant::now();
    let mut c = Coordinator {
        identity,
        herdr,
        config,
        config_generation: 1,
        connection_generation: 1,
        connection: "Connecting",
        discovery_error: None,
        rejected: None,
        rejected_generation: 0,
        workspaces: BTreeMap::new(),
        jobs: BTreeMap::new(),
        global_jobs: BTreeMap::new(),
        background: BTreeMap::new(),
        publisher: Publisher::default(),
        report_errors: BTreeMap::new(),
        sequences,
        report: None,
        discovery: None,
        discover_due: now,
        backoff: 1,
        started: now,
        order: 0,
        scan_due: now + Duration::from_secs(1),
        candidate: None,
        observed_hash: snapshot.hash(),
    };
    c.rebuild_background().await;
    tracing::info!(endpoint = %endpoint.hash, generation = 1, "runner ready");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = term.recv() => break Ok(()),
            _ = interrupt.recv() => break Ok(()),
            Some(control) = rx.recv() => {
                let mut previous_reload = control.request.command == "reload";
                let (mut stop, mut response) = c.control(control).await;
                // Requests accumulated during the same reload share its result. Preserve
                // ordering across other commands and cap each admission batch.
                for _ in 0..32 {
                    if stop { break; }
                    let Ok(control) = rx.try_recv() else { break; };
                    let reload = control.request.command == "reload";
                    if reload && previous_reload { let _ = control.reply.send(response.clone()); }
                    else { (stop, response) = c.control(control).await; }
                    previous_reload = reload;
                }
                if stop { break Ok(()); }
            },
            _ = tick.tick() => { if let Err(e) = c.tick().await { break Err(e); } },
        }
    };
    c.shutdown().await;
    control_cancel.cancel();
    let _ = control_task.await;
    drop(lock);
    result
}
