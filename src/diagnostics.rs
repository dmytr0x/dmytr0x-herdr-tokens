use crate::providers::Patch;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

#[derive(Default, Clone)]
pub struct JobStatus {
    pub attempted: Option<Instant>,
    pub collected: Option<Instant>,
    pub acknowledged: Option<Instant>,
    pub duration_ms: Option<u64>,
    pub failures: u64,
    pub error: Option<String>,
    pub publication_error: Option<String>,
    pub last_collected: Option<Patch>,
    pub last_acknowledged: Option<Patch>,
    pub acknowledged_completion: Option<Instant>,
    pub missed_deadlines: u64,
    pub truncated: bool,
}
fn age(time: Option<Instant>) -> Option<u64> {
    time.map(|t| Instant::now().saturating_duration_since(t).as_millis() as u64)
}
#[derive(Serialize)]
pub struct CollectorObservations<'a> {
    last_attempt_age_ms: Option<u64>,
    last_collection_age_ms: Option<u64>,
    last_acknowledgement_age_ms: Option<u64>,
    duration_ms: Option<u64>,
    consecutive_failures: u64,
    collection_error: &'a Option<String>,
    publication_error: &'a Option<String>,
    estimated_expiry_in_ms: Option<u64>,
    missed_deadlines: u64,
    output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_collected: Option<&'a Option<Patch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_acknowledged: Option<&'a Option<Patch>>,
}
impl JobStatus {
    pub fn observations(&self, include_values: bool, ttl_ms: u64) -> CollectorObservations<'_> {
        CollectorObservations {
            last_attempt_age_ms: age(self.attempted),
            last_collection_age_ms: age(self.collected),
            last_acknowledgement_age_ms: age(self.acknowledged),
            duration_ms: self.duration_ms,
            consecutive_failures: self.failures,
            collection_error: &self.error,
            publication_error: &self.publication_error,
            estimated_expiry_in_ms: age(self.acknowledged_completion)
                .map(|a| ttl_ms.saturating_sub(a)),
            missed_deadlines: self.missed_deadlines,
            output_truncated: self.truncated,
            last_collected: include_values.then_some(&self.last_collected),
            last_acknowledged: include_values.then_some(&self.last_acknowledged),
        }
    }
    pub fn json(&self, include_values: bool, ttl_ms: u64) -> Value {
        serde_json::to_value(self.observations(include_values, ttl_ms))
            .expect("diagnostics serialization")
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TargetStatus {
    pub workspaces: Vec<String>,
    pub running: bool,
    pub last_exit: Option<i32>,
    pub duration_ms: Option<u64>,
    pub consecutive_failures: u64,
    pub error: Option<String>,
}
#[derive(Serialize)]
pub struct TargetObservations<'a> {
    dir: &'a Path,
    #[serde(flatten)]
    status: &'a TargetStatus,
}
impl TargetStatus {
    pub fn json(&self, dir: &Path) -> Value {
        serde_json::to_value(TargetObservations { dir, status: self })
            .expect("target serialization")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastRun {
    pub completed: Instant,
    pub duration_ms: u64,
    pub targets: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped: u64,
}
#[derive(Clone, Debug, Default)]
pub struct BackgroundStatus {
    pub missed_deadlines: u64,
    pub last_run: Option<LastRun>,
    pub targets: BTreeMap<PathBuf, TargetStatus>,
}
#[derive(Serialize)]
pub struct RunObservations {
    age_ms: Option<u64>,
    duration_ms: u64,
    targets: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
}
#[derive(Serialize)]
pub struct BackgroundDiagnostics<'a> {
    missed_deadlines: u64,
    last_run: Option<RunObservations>,
    targets: Vec<TargetObservations<'a>>,
}
impl BackgroundStatus {
    pub fn observations(&self) -> BackgroundDiagnostics<'_> {
        BackgroundDiagnostics {
            missed_deadlines: self.missed_deadlines,
            last_run: self.last_run.as_ref().map(|r| RunObservations {
                age_ms: age(Some(r.completed)),
                duration_ms: r.duration_ms,
                targets: r.targets,
                succeeded: r.succeeded,
                failed: r.failed,
                skipped: r.skipped,
            }),
            targets: self
                .targets
                .iter()
                .map(|(dir, status)| TargetObservations { dir, status })
                .collect(),
        }
    }
    pub fn json(&self) -> Value {
        serde_json::to_value(self.observations()).expect("background serialization")
    }
}
struct Log {
    dir: PathBuf,
    day: u64,
    file: Option<File>,
    bytes: u64,
    suppressed: bool,
}
#[derive(Clone)]
struct Writer(Arc<Mutex<Log>>);
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut log = self.0.lock().map_err(|_| io::Error::other("log lock"))?;
        let day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86400;
        if log.file.is_none() || day != log.day {
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                .open(log.dir.join(format!("day-{day:08}.jsonl")))?;
            crate::runtime::managed_file(&file).map_err(io::Error::other)?;
            log.bytes = file.metadata()?.len();
            log.file = Some(file);
            log.day = day;
            log.suppressed = log.bytes >= 10 * 1_048_576;
            let mut files: Vec<_> = fs::read_dir(&log.dir)?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().is_some_and(|n| {
                        n.to_string_lossy().starts_with("day-")
                            && n.to_string_lossy().ends_with(".jsonl")
                    })
                })
                .collect();
            files.sort();
            let excess = files.len().saturating_sub(7);
            for p in files.into_iter().take(excess) {
                fs::remove_file(p)?;
            }
        }
        if log.bytes + buf.len() as u64 > 10 * 1_048_576 - 128 {
            if !log.suppressed {
                log.file.as_mut().expect("open log").write_all(
                    b"{\"event\":\"daily log cap reached; further events suppressed\"}\n",
                )?;
                log.suppressed = true;
            }
        } else if !log.suppressed {
            log.file.as_mut().expect("open log").write_all(buf)?;
            log.bytes += buf.len() as u64;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self
            .0
            .lock()
            .map_err(|_| io::Error::other("log lock"))?
            .file
        {
            f.flush()?;
        }
        Ok(())
    }
}
pub fn init(log_dir: Option<PathBuf>) -> Result<()> {
    if let Some(dir) = log_dir {
        crate::runtime::private_dir(&dir)?;
        let writer = Writer(Arc::new(Mutex::new(Log {
            dir,
            day: 0,
            file: None,
            bytes: 0,
            suppressed: false,
        })));
        tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_writer(writer)
            .try_init()
            .map_err(|_| anyhow::anyhow!("logging initialization failed"))?;
    } else {
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|_| anyhow::anyhow!("logging initialization failed"))?;
    }
    Ok(())
}
