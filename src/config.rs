//! Bounded configuration snapshots and strict, normalized validation.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_discovery_interval_ms")]
    pub discovery_interval_ms: u64,
}
fn default_max_concurrency() -> usize {
    4
}
fn default_discovery_interval_ms() -> u64 {
    5000
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            max_concurrency: default_max_concurrency(),
            discovery_interval_ms: default_discovery_interval_ms(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Command,
    Git,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandOutput {
    #[default]
    Json,
    Text,
}
/// A literal field selector with optional display-only text decoration.
/// Both TOML forms normalize identically, so spelling changes do not restart jobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawMapping")]
pub struct TokenMapping {
    pub field: String,
    pub prefix: String,
    pub suffix: String,
    pub show_zero: bool,
}
fn show_zero() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum RawMapping {
    Field(String),
    Decorated {
        field: String,
        #[serde(default)]
        prefix: String,
        #[serde(default)]
        suffix: String,
        #[serde(default = "show_zero", alias = "show_always")]
        show_zero: bool,
    },
}
impl From<RawMapping> for TokenMapping {
    fn from(raw: RawMapping) -> Self {
        match raw {
            RawMapping::Field(field) => field.into(),
            RawMapping::Decorated {
                field,
                prefix,
                suffix,
                show_zero,
            } => Self {
                field,
                prefix,
                suffix,
                show_zero,
            },
        }
    }
}
impl From<String> for TokenMapping {
    fn from(field: String) -> Self {
        Self {
            field,
            prefix: String::new(),
            suffix: String::new(),
            show_zero: true,
        }
    }
}
impl From<&str> for TokenMapping {
    fn from(field: &str) -> Self {
        field.to_owned().into()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCollector {
    name: String,
    provider: Provider,
    command: Option<Vec<String>>,
    #[serde(default)]
    global: bool,
    output: Option<CommandOutput>,
    interval_ms: Option<u64>,
    timeout_ms: Option<u64>,
    ttl_ms: Option<u64>,
    env: Option<BTreeMap<String, String>>,
    env_allow: Option<Vec<String>>,
    tokens: BTreeMap<String, TokenMapping>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CommandSpec {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub env_allow: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum CommandScope {
    Workspace,
    Global,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum CollectorKind {
    Git,
    Command {
        spec: CommandSpec,
        scope: CommandScope,
        output: CommandOutput,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Collector {
    pub(crate) name: String,
    pub(crate) kind: CollectorKind,
    pub(crate) interval_ms: u64,
    pub(crate) timeout_ms: u64,
    pub(crate) ttl_ms: u64,
    pub(crate) tokens: BTreeMap<String, TokenMapping>,
}
impl Collector {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }
    pub fn tokens(&self) -> &BTreeMap<String, TokenMapping> {
        &self.tokens
    }
    pub fn global(&self) -> bool {
        matches!(
            self.kind,
            CollectorKind::Command {
                scope: CommandScope::Global,
                ..
            }
        )
    }
    pub fn output(&self) -> Option<&CommandOutput> {
        match &self.kind {
            CollectorKind::Git => None,
            CollectorKind::Command { output, .. } => Some(output),
        }
    }
}
/// Which worktrees of a repository a background job runs in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Worktrees {
    /// Once per repository, in its main worktree (or bare repository directory).
    #[default]
    Main,
    /// Once per distinct worktree open in a Herdr workspace.
    All,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJob {
    name: String,
    command: Vec<String>,
    #[serde(default)]
    worktrees: Worktrees,
    interval_ms: Option<u64>,
    timeout_ms: Option<u64>,
    chunk_size: Option<usize>,
    chunk_delay_ms: Option<u64>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_allow: Vec<String>,
}
/// A periodic command run in the repositories of discovered workspaces. Publishes no tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Job {
    pub(crate) name: String,
    pub(crate) spec: CommandSpec,
    pub(crate) worktrees: Worktrees,
    pub(crate) interval_ms: u64,
    pub(crate) timeout_ms: u64,
    pub(crate) chunk_size: usize,
    pub(crate) chunk_delay_ms: u64,
}
impl Job {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn worktrees(&self) -> Worktrees {
        self.worktrees
    }
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }
    pub fn chunk_delay_ms(&self) -> u64 {
        self.chunk_delay_ms
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Main {
    schema_version: u32,
    #[serde(default)]
    runtime: Runtime,
    #[serde(default)]
    workspace_dirs: BTreeMap<String, PathBuf>,
    #[serde(default)]
    collectors: Vec<toml::Spanned<RawCollector>>,
    #[serde(default)]
    jobs: Vec<toml::Spanned<RawJob>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fragment {
    #[serde(default)]
    collectors: Vec<toml::Spanned<RawCollector>>,
    #[serde(default)]
    jobs: Vec<toml::Spanned<RawJob>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub(crate) runtime: Runtime,
    pub(crate) workspace_dirs: BTreeMap<String, PathBuf>,
    pub(crate) collectors: Vec<Collector>,
    pub(crate) jobs: Vec<Job>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    files: Vec<(PathBuf, Vec<u8>)>,
}
fn no_symlink(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path).context("configuration entry unavailable")?;
    ensure!(!m.file_type().is_symlink(), "symlinked configuration entry");
    ensure!(
        if directory { m.is_dir() } else { m.is_file() },
        "wrong configuration entry type"
    );
    Ok(())
}
// The main config may be managed by a dotfiles tool as a symlink. Resolve it
// before opening so the regular-file and O_NOFOLLOW checks still apply to the
// actual target. Fragments remain regular files so a multi-file configuration
// cannot silently pull in arbitrary external files.
fn config_file_path(path: &Path, allow_symlink: bool) -> Result<PathBuf> {
    if allow_symlink {
        let resolved = fs::canonicalize(path).context("configuration entry unavailable")?;
        no_symlink(&resolved, false)?;
        Ok(resolved)
    } else {
        no_symlink(path, false)?;
        Ok(path.to_owned())
    }
}
impl Snapshot {
    pub fn read(dir: &Path) -> Result<Self> {
        for p in dir.ancestors() {
            no_symlink(p, true)?;
        }
        let mut names = vec![dir.join("tokens.toml")];
        let fragments = dir.join("tokens.d");
        match fs::symlink_metadata(&fragments) {
            Ok(_) => {
                no_symlink(&fragments, true)?;
                let mut children = Vec::new();
                for entry in fs::read_dir(fragments)? {
                    let entry = entry?;
                    let p = entry.path();
                    if p.extension().is_some_and(|x| x == "toml") {
                        ensure!(!entry.file_type()?.is_symlink(), "symlinked fragment");
                        if entry.file_type()?.is_file() {
                            children.push(p);
                        }
                    }
                }
                children.sort();
                names.extend(children);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("cannot read fragment directory"),
        }
        ensure!(names.len() <= 64, "configuration exceeds 64 files");
        let mut total = 0;
        let mut files = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            let read_path = config_file_path(&name, index == 0)?;
            let mut bytes = Vec::new();
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(read_path)?
                .take((1_048_577 - total) as u64)
                .read_to_end(&mut bytes)?;
            total += bytes.len();
            ensure!(total <= 1_048_576, "configuration exceeds 1 MiB");
            files.push((name, bytes));
        }
        Ok(Self { files })
    }
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        for (p, b) in &self.files {
            h.update(p.as_os_str().as_encoded_bytes());
            h.update([0]);
            h.update((b.len() as u64).to_le_bytes());
            h.update(b);
        }
        format!("{:x}", h.finalize())
    }
    pub fn parse(&self) -> Result<Config> {
        // Parser errors can contain configured secrets. Expose only category and location.
        fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
            let s = std::str::from_utf8(bytes).context("configuration is not UTF-8")?;
            toml::from_str(s).map_err(|e: toml::de::Error| {
                anyhow::anyhow!(
                    "invalid TOML/schema at byte {}",
                    e.span().map_or(0, |s| s.start)
                )
            })
        }
        let mut main: Main =
            parse(&self.files[0].1).map_err(|e| anyhow::anyhow!("main file: {e}"))?;
        ensure!(main.schema_version == 1, "unsupported schema_version");
        ensure!(
            (1..=64).contains(&main.runtime.max_concurrency),
            "max_concurrency outside 1..64"
        );
        ensure!(
            (1000..=60000).contains(&main.runtime.discovery_interval_ms),
            "discovery interval outside 1000..60000"
        );
        let mut raw_collectors: Vec<_> = std::mem::take(&mut main.collectors)
            .into_iter()
            .enumerate()
            .map(|(i, raw)| ("main file".to_owned(), i, raw))
            .collect();
        let mut raw_jobs: Vec<_> = std::mem::take(&mut main.jobs)
            .into_iter()
            .enumerate()
            .map(|(i, raw)| ("main file".to_owned(), i, raw))
            .collect();
        for (file_index, (_, bytes)) in self.files.iter().enumerate().skip(1) {
            let source = format!("fragment {file_index}");
            let fragment =
                parse::<Fragment>(bytes).map_err(|e| anyhow::anyhow!("{source}: {e}"))?;
            raw_collectors.extend(
                fragment
                    .collectors
                    .into_iter()
                    .enumerate()
                    .map(|(i, raw)| (source.clone(), i, raw)),
            );
            raw_jobs.extend(
                fragment
                    .jobs
                    .into_iter()
                    .enumerate()
                    .map(|(i, raw)| (source.clone(), i, raw)),
            );
        }
        ensure!(
            main.workspace_dirs.values().all(|p| p.is_absolute()),
            "workspace overrides must be absolute"
        );
        let mut names = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        let mut collectors = Vec::new();
        for (source, index, raw) in raw_collectors {
            let span = raw.span();
            collectors.push(
                validate_collector(raw.into_inner(), &mut names, &mut tokens).map_err(|e| {
                    anyhow::anyhow!(
                        "{source} collectors[{index}] bytes {}..{}: {e}",
                        span.start,
                        span.end
                    )
                })?,
            );
        }
        ensure!(tokens.len() <= 32, "configuration exceeds 32 tokens");
        collectors.sort_by(|a, b| a.name.cmp(&b.name));
        ensure!(raw_jobs.len() <= 16, "configuration exceeds 16 jobs");
        let mut jobs = Vec::new();
        for (source, index, raw) in raw_jobs {
            let span = raw.span();
            jobs.push(validate_job(raw.into_inner(), &mut names).map_err(|e| {
                anyhow::anyhow!(
                    "{source} jobs[{index}] bytes {}..{}: {e}",
                    span.start,
                    span.end
                )
            })?);
        }
        jobs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Config {
            runtime: main.runtime,
            workspace_dirs: main.workspace_dirs,
            collectors,
            jobs,
        })
    }
}
fn validate_collector(
    raw: RawCollector,
    names: &mut BTreeSet<String>,
    tokens: &mut BTreeSet<String>,
) -> Result<Collector> {
    ensure!(
        identifier(&raw.name, 64) && names.insert(raw.name.clone()),
        "invalid or duplicate collector name"
    );
    ensure!(
        !raw.tokens.is_empty() && raw.tokens.len() <= 16,
        "collector requires 1..16 mappings"
    );
    validate_mappings(&raw, tokens)?;
    let (interval, timeout, ttl) = collector_timing(raw.interval_ms, raw.timeout_ms, raw.ttl_ms)?;
    match raw.provider {
        Provider::Git => ensure!(
            raw.command.is_none()
                && !raw.global
                && raw.output.is_none()
                && raw.env.is_none()
                && raw.env_allow.is_none(),
            "Git does not accept command/global/output/env/env_allow"
        ),
        Provider::Command => ensure!(
            raw.command.as_deref().is_some_and(valid_argv),
            "command requires valid argv"
        ),
    }
    let env = raw.env.unwrap_or_default();
    let allow = environment(&env, raw.env_allow.unwrap_or_default())?;
    Ok(Collector {
        name: raw.name,
        kind: match raw.provider {
            Provider::Git => CollectorKind::Git,
            Provider::Command => CollectorKind::Command {
                spec: CommandSpec {
                    argv: raw.command.unwrap_or_default(),
                    env,
                    env_allow: allow,
                },
                scope: if raw.global {
                    CommandScope::Global
                } else {
                    CommandScope::Workspace
                },
                output: raw.output.unwrap_or_default(),
            },
        },
        interval_ms: interval,
        timeout_ms: timeout,
        ttl_ms: ttl,
        tokens: raw.tokens,
    })
}
fn validate_mappings(raw: &RawCollector, tokens: &mut BTreeSet<String>) -> Result<()> {
    for (token, mapping) in &raw.tokens {
        let field = &mapping.field;
        for affix in [&mapping.prefix, &mapping.suffix] {
            ensure!(
                affix.chars().count() <= 80 && !affix.chars().any(char::is_control),
                "token prefix/suffix must contain at most 80 Unicode characters and no control characters"
            );
        }
        ensure!(
            identifier(token, 32) && tokens.insert(token.clone()),
            "invalid or duplicate token name"
        );
        ensure!(
            !field.is_empty() && field.len() <= 128,
            "invalid field selector"
        );
        if raw.provider == Provider::Git {
            ensure!(
                [
                    "modified_files",
                    "staged_files",
                    "untracked_files",
                    "conflict_files"
                ]
                .contains(&field.as_str()),
                "unsupported Git field"
            );
        }
        if raw.provider == Provider::Command && raw.output == Some(CommandOutput::Text) {
            ensure!(field == "stdout", "text output only supports stdout");
        }
    }
    Ok(())
}
fn collector_timing(
    interval_ms: Option<u64>,
    timeout_ms: Option<u64>,
    ttl_ms: Option<u64>,
) -> Result<(u64, u64, u64)> {
    let interval = interval_ms.unwrap_or(10000);
    ensure!(
        (250..=28_800_000).contains(&interval),
        "invalid interval_ms"
    );
    let timeout = timeout_ms.unwrap_or(interval.min(1000));
    ensure!(
        (1..=interval.min(300000)).contains(&timeout),
        "invalid timeout_ms"
    );
    let ttl = ttl_ms.unwrap_or(3 * interval);
    ensure!((3 * interval..=86_400_000).contains(&ttl), "invalid ttl_ms");
    Ok((interval, timeout, ttl))
}
fn validate_job(raw: RawJob, names: &mut BTreeSet<String>) -> Result<Job> {
    ensure!(
        identifier(&raw.name, 64) && names.insert(raw.name.clone()),
        "invalid or duplicate job name"
    );
    ensure!(valid_argv(&raw.command), "job command requires valid argv");
    let interval = raw.interval_ms.unwrap_or(300_000);
    ensure!(
        (10_000..=86_400_000).contains(&interval),
        "invalid job interval_ms"
    );
    let timeout = raw.timeout_ms.unwrap_or(interval.min(60_000));
    ensure!(
        (1..=interval.min(600_000)).contains(&timeout),
        "invalid job timeout_ms"
    );
    let chunk_size = raw.chunk_size.unwrap_or(4);
    ensure!((1..=16).contains(&chunk_size), "invalid job chunk_size");
    let chunk_delay = raw.chunk_delay_ms.unwrap_or(0);
    ensure!(chunk_delay <= interval, "invalid job chunk_delay_ms");
    let allow = environment(&raw.env, raw.env_allow)?;
    Ok(Job {
        name: raw.name,
        spec: CommandSpec {
            argv: raw.command,
            env: raw.env,
            env_allow: allow,
        },
        worktrees: raw.worktrees,
        interval_ms: interval,
        timeout_ms: timeout,
        chunk_size,
        chunk_delay_ms: chunk_delay,
    })
}
fn valid_argv(argv: &[String]) -> bool {
    !argv.is_empty() && !argv[0].is_empty() && argv.iter().all(|s| !s.contains('\0'))
}
/// Validates explicit and allowed environment names; returns the sorted allow-list.
fn environment(env: &BTreeMap<String, String>, mut allow: Vec<String>) -> Result<Vec<String>> {
    for key in env.keys().chain(allow.iter()) {
        ensure!(
            env_name(key) && !CONTEXT.contains(&key.as_str()),
            "invalid or reserved environment name"
        );
    }
    ensure!(
        env.values().all(|v| !v.contains('\0')),
        "NUL in environment"
    );
    let mut seen = BTreeSet::new();
    for key in &allow {
        ensure!(seen.insert(key), "duplicate env_allow entry");
    }
    allow.sort();
    Ok(allow)
}
pub const CONTEXT: [&str; 6] = [
    "HERDR_TOKENS_WORKSPACE_ID",
    "HERDR_TOKENS_WORKSPACE_DIR",
    "HERDR_TOKENS_COLLECTOR",
    "HERDR_TOKENS_JOB",
    "HERDR_TOKENS_WORKTREE_DIR",
    "HERDR_TOKENS_WORKSPACE_IDS",
];
fn identifier(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn env_name(s: &str) -> bool {
    s.bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
impl Config {
    pub fn collectors(&self) -> &[Collector] {
        &self.collectors
    }
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Snapshot::read(dir)?.parse()
    }
    pub fn hash(&self) -> String {
        hash(&serde_json::to_vec(self).expect("config serialization"))
    }
    /// Compare the collector/runtime boundary without cloning background jobs.
    pub fn same_collection_config(&self, other: &Self) -> bool {
        self.runtime == other.runtime
            && self.workspace_dirs == other.workspace_dirs
            && self.collectors == other.collectors
    }
    pub fn from_toml(text: &str) -> Result<Self> {
        ensure!(text.len() <= 1_048_576, "configuration exceeds 1 MiB");
        Snapshot {
            files: vec![(PathBuf::from("tokens.toml"), text.as_bytes().to_vec())],
        }
        .parse()
    }
    pub fn token_names(&self) -> BTreeSet<String> {
        self.collectors
            .iter()
            .flat_map(|c| c.tokens.keys().cloned())
            .collect()
    }
    pub fn workspace_token_names(&self) -> BTreeSet<String> {
        self.collectors
            .iter()
            .filter(|c| !c.global())
            .flat_map(|c| c.tokens.keys().cloned())
            .collect()
    }
}
