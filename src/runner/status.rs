//! Serializable local observations; these do not assert current Herdr visibility.
use super::*;
use serde::Serialize;

#[derive(Serialize)]
struct WorkspaceStatus<'a> {
    directory: &'a Directory,
    generation: u64,
    pending_clears: Vec<String>,
    publication_error: Option<&'a (String, u64)>,
}
#[derive(Serialize)]
struct CollectorStatus<'a> {
    workspace: Option<&'a str>,
    global: bool,
    collector: &'a str,
    tokens: Vec<&'a String>,
    running: bool,
    queued: bool,
    refresh_pending: bool,
    publication_pending: bool,
    next_due_in_ms: u64,
    diagnostics: crate::diagnostics::CollectorObservations<'a>,
}
impl<'a> CollectorStatus<'a> {
    fn new(
        workspace: Option<&'a str>,
        task: &'a CollectorTask,
        pending: bool,
        values: bool,
    ) -> Self {
        let now = Instant::now();
        Self {
            workspace,
            global: workspace.is_none(),
            collector: &task.collector.name,
            tokens: task.collector.tokens.keys().collect(),
            running: task.task.is_some(),
            queued: task.due <= now,
            refresh_pending: task.refresh,
            publication_pending: pending,
            next_due_in_ms: task.due.saturating_duration_since(now).as_millis() as u64,
            diagnostics: task.status.observations(values, task.collector.ttl_ms),
        }
    }
}
#[derive(Serialize)]
struct Status<'a> {
    version: &'static str,
    endpoint: &'a str,
    source: &'static str,
    uptime_ms: u64,
    config_hash: String,
    config_files_hash: &'a str,
    config_generation: u64,
    rejected_candidates: u64,
    last_rejected_error: &'a Option<String>,
    connection: ConnectionState,
    connection_generation: u64,
    discovery_error: &'a Option<String>,
    workspaces: BTreeMap<&'a String, WorkspaceStatus<'a>>,
    jobs: Vec<CollectorStatus<'a>>,
    background_jobs: Vec<super::background::BackgroundObservations<'a>>,
    knowledge: &'static str,
}
impl Coordinator<'_> {
    pub(super) fn status(&self, values: bool) -> Value {
        let workspaces = self
            .workspaces
            .iter()
            .map(|(id, w)| {
                (
                    id,
                    WorkspaceStatus {
                        directory: &w.directory,
                        generation: w.generation,
                        pending_clears: self.publisher.pending_clears(id),
                        publication_error: self.report_errors.get(id),
                    },
                )
            })
            .collect();
        let jobs = self
            .jobs
            .iter()
            .map(|(key, task)| {
                CollectorStatus::new(
                    Some(&key.workspace),
                    task,
                    self.publisher.has_pending(key),
                    values,
                )
            })
            .chain(self.global_jobs.iter().map(|(name, task)| {
                let pending = self.workspaces.keys().any(|id| {
                    self.publisher
                        .has_pending(&Key::new(id.clone(), name.clone()))
                });
                CollectorStatus::new(None, task, pending, values)
            }))
            .collect();
        serde_json::to_value(Status {
            version: env!("CARGO_PKG_VERSION"), endpoint: &self.identity.endpoint, source: "herdr-tokens",
            uptime_ms: self.started.elapsed().as_millis() as u64, config_hash: self.config.hash(),
            config_files_hash: &self.candidate.observed, config_generation: self.config_generation,
            rejected_candidates: self.rejected_generation, last_rejected_error: &self.rejected,
            connection: self.connection, connection_generation: self.connection_generation,
            discovery_error: &self.discovery_error, workspaces, jobs,
            background_jobs: self.background.values().map(BackgroundJob::observations).collect(),
            knowledge: "Local emitter observations only; acknowledgements do not prove current Herdr ownership or visibility.",
        }).expect("status serialization")
    }
}
