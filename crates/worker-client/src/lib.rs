//! Builder facade for native analyzer workers.

mod plugin_registry;
#[cfg(unix)]
mod process_memory;

pub use plugin_registry::{InstalledPlugin, PluginRegistry, describe_plugin};

use beholder_domain::{EntityKind, SemanticRelation};
use beholder_indexing::{
    AnalysisInput, AnalysisInputKind, AnalyzerError, AnalyzerMetadata, EnrichmentFuture,
    EnrichmentSnapshot, EnrichmentSourceCurrentness, PluginDescriptor, PluginInputScope,
    WorkspaceEnricher,
};
use beholder_protocol::{
    ContributionAccumulator, analyze_requests,
    worker_v1::{AnalysisPhase, analyze_event, analyzer_worker_client::AnalyzerWorkerClient},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
};
use tonic::{Request, transport::Channel};
use tracing::Instrument;

#[cfg(unix)]
use process_memory::{
    MemoryGuardEvent, ProcessMemoryGuard, isolate_process_group, terminate_process_group,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ANALYSIS_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const WORKER_SETTINGS: [&str; 3] = ["MAX_OUTPUT_BYTES", "MEMORY_LIMIT_BYTES", "TIMEOUT_MS"];
static WORKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const RUST_WORKER_ID: &str = "rust";
pub const ELIXIR_WORKER_ID: &str = "elixir";
pub const TYPESCRIPT_WORKER_ID: &str = "typescript";

pub fn worker_environment_variable(worker: &str, setting: &str) -> String {
    let normalize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!(
        "BEHOLDER_{}_WORKER_{}",
        normalize(worker),
        normalize(setting)
    )
}

fn configured_worker_environment(worker: &str) -> BTreeMap<String, OsString> {
    WORKER_SETTINGS
        .into_iter()
        .filter_map(|setting| {
            std::env::var_os(worker_environment_variable(worker, setting))
                .map(|value| (format!("BEHOLDER_WORKER_{setting}"), value))
        })
        .collect()
}

pub struct WorkerAnalyzerBuilder {
    metadata: AnalyzerMetadata,
    executable: PathBuf,
    socket_dir: PathBuf,
    extensions: BTreeMap<OsString, AnalysisInputKind>,
    file_names: BTreeMap<OsString, AnalysisInputKind>,
    path_suffixes: BTreeMap<PathBuf, AnalysisInputKind>,
    parent_suffixes: BTreeMap<PathBuf, AnalysisInputKind>,
    excluded_path_suffixes: Vec<PathBuf>,
    activation_paths: Vec<PathBuf>,
    identity_inputs: Vec<AnalysisInput>,
    repository_identity_files: Vec<RepositoryIdentityFile>,
    semantic_entities: BTreeSet<EntityKind>,
    semantic_relations: BTreeSet<SemanticRelation>,
    semantic_shard_producers: BTreeSet<String>,
    plugin: Option<PluginDescriptor>,
    persistent: bool,
    memory_limit_bytes: Option<u64>,
    timeout: Duration,
}

struct RepositoryIdentityFile {
    path: PathBuf,
    file: PathBuf,
    kind: AnalysisInputKind,
}

impl WorkerAnalyzerBuilder {
    pub fn new(executable: impl Into<PathBuf>, socket_dir: impl Into<PathBuf>) -> Self {
        Self {
            metadata: AnalyzerMetadata {
                id: String::new(),
                version: String::new(),
            },
            executable: executable.into(),
            socket_dir: socket_dir.into(),
            extensions: BTreeMap::new(),
            file_names: BTreeMap::new(),
            path_suffixes: BTreeMap::new(),
            parent_suffixes: BTreeMap::new(),
            excluded_path_suffixes: Vec::new(),
            activation_paths: Vec::new(),
            identity_inputs: Vec::new(),
            repository_identity_files: Vec::new(),
            semantic_entities: BTreeSet::new(),
            semantic_relations: BTreeSet::new(),
            semantic_shard_producers: BTreeSet::new(),
            plugin: None,
            persistent: false,
            memory_limit_bytes: None,
            timeout: ANALYSIS_INACTIVITY_TIMEOUT,
        }
    }

    pub fn identity(mut self, id: impl Into<String>, version: impl Into<String>) -> Self {
        self.metadata = AnalyzerMetadata {
            id: id.into(),
            version: version.into(),
        };
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn memory_limit(mut self, bytes: u64) -> Self {
        self.memory_limit_bytes = Some(bytes);
        self
    }

    pub fn persistent(mut self) -> Self {
        self.persistent = true;
        self
    }

    pub fn accept_extension(mut self, extension: impl Into<OsString>) -> Self {
        self.extensions
            .insert(extension.into(), AnalysisInputKind::Source);
        self
    }

    pub fn accept_extension_as(
        mut self,
        extension: impl Into<OsString>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.extensions.insert(extension.into(), kind);
        self
    }

    pub fn accept_file_name(mut self, file_name: impl Into<OsString>) -> Self {
        self.file_names
            .insert(file_name.into(), AnalysisInputKind::Source);
        self
    }

    pub fn accept_file_name_as(
        mut self,
        file_name: impl Into<OsString>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.file_names.insert(file_name.into(), kind);
        self
    }

    pub fn accept_path_suffix_as(
        mut self,
        suffix: impl Into<PathBuf>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.path_suffixes.insert(suffix.into(), kind);
        self
    }

    pub fn exclude_path_suffix(mut self, suffix: impl Into<PathBuf>) -> Self {
        self.excluded_path_suffixes.push(suffix.into());
        self
    }

    pub fn activate_when_path_exists(mut self, path: impl Into<PathBuf>) -> Self {
        self.activation_paths.push(path.into());
        self
    }

    pub fn accept_parent_suffix_as(
        mut self,
        suffix: impl Into<PathBuf>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.parent_suffixes.insert(suffix.into(), kind);
        self
    }

    pub fn identity_input(
        mut self,
        path: impl Into<PathBuf>,
        content: impl Into<Vec<u8>>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.identity_inputs.push(AnalysisInput {
            path: path.into(),
            content: Arc::from(content.into()),
            kind,
        });
        self
    }

    pub fn repository_file_identity(
        mut self,
        path: impl Into<PathBuf>,
        file: impl Into<PathBuf>,
        kind: AnalysisInputKind,
    ) -> Self {
        self.repository_identity_files.push(RepositoryIdentityFile {
            path: path.into(),
            file: file.into(),
            kind,
        });
        self
    }

    pub fn semantic_relation(mut self, relation: SemanticRelation) -> Self {
        self.semantic_relations.insert(relation);
        self
    }

    pub fn semantic_entity(mut self, kind: EntityKind) -> Self {
        self.semantic_entities.insert(kind);
        self
    }

    pub fn semantic_shard_producer(mut self, producer: impl Into<String>) -> Self {
        self.semantic_shard_producers.insert(producer.into());
        self
    }

    pub fn build(self) -> Result<WorkerAnalyzer, AnalyzerError> {
        if self.metadata.id.is_empty() {
            return Err("worker analyzer identity must not be empty".into());
        }
        if self.metadata.version.is_empty() {
            return Err("worker analyzer version must not be empty".into());
        }
        if self.extensions.is_empty() && self.file_names.is_empty() && self.path_suffixes.is_empty()
        {
            return Err("worker analyzer must accept at least one input".into());
        }
        Ok(WorkerAnalyzer {
            metadata: self.metadata,
            executable: self.executable,
            socket_dir: self.socket_dir,
            extensions: self.extensions,
            file_names: self.file_names,
            path_suffixes: self.path_suffixes,
            parent_suffixes: self.parent_suffixes,
            excluded_path_suffixes: self.excluded_path_suffixes,
            activation_paths: self.activation_paths,
            identity_inputs: self.identity_inputs,
            repository_identity_files: self.repository_identity_files,
            semantic_entities: self.semantic_entities,
            semantic_relations: self.semantic_relations,
            semantic_shard_producers: self.semantic_shard_producers,
            plugin: self.plugin,
            persistent: self.persistent,
            memory_limit_bytes: self.memory_limit_bytes,
            session: Mutex::new(None),
            timeout: self.timeout,
        })
    }
}

pub struct WorkerAnalyzer {
    metadata: AnalyzerMetadata,
    executable: PathBuf,
    socket_dir: PathBuf,
    extensions: BTreeMap<OsString, AnalysisInputKind>,
    file_names: BTreeMap<OsString, AnalysisInputKind>,
    path_suffixes: BTreeMap<PathBuf, AnalysisInputKind>,
    parent_suffixes: BTreeMap<PathBuf, AnalysisInputKind>,
    excluded_path_suffixes: Vec<PathBuf>,
    activation_paths: Vec<PathBuf>,
    identity_inputs: Vec<AnalysisInput>,
    repository_identity_files: Vec<RepositoryIdentityFile>,
    semantic_entities: BTreeSet<EntityKind>,
    semantic_relations: BTreeSet<SemanticRelation>,
    semantic_shard_producers: BTreeSet<String>,
    plugin: Option<PluginDescriptor>,
    persistent: bool,
    memory_limit_bytes: Option<u64>,
    session: Mutex<Option<WorkerSession>>,
    timeout: Duration,
}

struct WorkerSession {
    child: Child,
    client: AnalyzerWorkerClient<Channel>,
    #[cfg(unix)]
    memory_guard: Option<ProcessMemoryGuard>,
    _socket: SocketFile,
}

pub fn plugin_analyzer(
    executable: impl Into<PathBuf>,
    socket_dir: impl Into<PathBuf>,
    digest: impl Into<String>,
    descriptor: PluginDescriptor,
) -> Result<WorkerAnalyzer, AnalyzerError> {
    descriptor.validate()?;
    Ok(WorkerAnalyzer {
        metadata: AnalyzerMetadata {
            id: descriptor.id.clone(),
            version: digest.into(),
        },
        executable: executable.into(),
        socket_dir: socket_dir.into(),
        extensions: BTreeMap::new(),
        file_names: BTreeMap::new(),
        path_suffixes: BTreeMap::new(),
        parent_suffixes: BTreeMap::new(),
        excluded_path_suffixes: Vec::new(),
        activation_paths: Vec::new(),
        identity_inputs: Vec::new(),
        repository_identity_files: Vec::new(),
        semantic_entities: BTreeSet::new(),
        semantic_relations: BTreeSet::new(),
        semantic_shard_producers: BTreeSet::new(),
        plugin: Some(descriptor),
        persistent: false,
        memory_limit_bytes: None,
        session: Mutex::new(None),
        timeout: ANALYSIS_INACTIVITY_TIMEOUT,
    })
}

impl WorkspaceEnricher for WorkerAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        self.metadata.clone()
    }

    fn accepts(&self, path: &Path) -> bool {
        self.analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        if let Some(plugin) = &self.plugin {
            return plugin
                .input_kind(PluginInputScope::Target, path)
                .or_else(|| plugin.input_kind(PluginInputScope::Context, path));
        }
        if self
            .excluded_path_suffixes
            .iter()
            .any(|suffix| path.ends_with(suffix))
        {
            return None;
        }
        self.path_suffixes
            .iter()
            .find_map(|(suffix, kind)| path.ends_with(suffix).then_some(*kind))
            .or_else(|| {
                self.parent_suffixes.iter().find_map(|(suffix, kind)| {
                    path.ancestors()
                        .skip(1)
                        .any(|parent| parent.ends_with(suffix))
                        .then_some(*kind)
                })
            })
            .or_else(|| {
                path.file_name()
                    .and_then(|file_name| self.file_names.get(file_name).copied())
            })
            .or_else(|| {
                path.extension()
                    .and_then(|extension| self.extensions.get(extension).copied())
            })
    }

    fn identity_inputs(&self) -> Vec<AnalysisInput> {
        self.identity_inputs.clone()
    }

    fn repository_identity_inputs(
        &self,
        repository: &beholder_indexing::RepositorySnapshot,
    ) -> Vec<AnalysisInput> {
        self.repository_identity_files
            .iter()
            .map(|identity| {
                let content = fs::read(repository.base.join(&identity.file))
                    .ok()
                    .unwrap_or_else(|| b"unavailable".to_vec());
                AnalysisInput {
                    path: identity.path.clone(),
                    content: Arc::from(content),
                    kind: identity.kind,
                }
            })
            .collect()
    }

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        if let Some(plugin) = &self.plugin {
            return repository.inputs.iter().any(|input| {
                plugin
                    .input_kind(PluginInputScope::Target, &input.path)
                    .is_some()
            });
        }
        if !self.activation_paths.is_empty()
            && !self
                .activation_paths
                .iter()
                .any(|path| repository.base.join(path).is_file())
        {
            return false;
        }
        repository
            .inputs
            .iter()
            .any(|input| self.analysis_input_kind(&input.path) == Some(AnalysisInputKind::Source))
    }

    fn requires_workspace_enablement(&self) -> bool {
        self.plugin.is_some()
    }

    fn context_repositories(
        &self,
        snapshot: &beholder_indexing::WorkspaceSnapshot,
        target: &str,
    ) -> Vec<String> {
        let Some(plugin) = &self.plugin else {
            return Vec::new();
        };
        snapshot
            .repositories
            .iter()
            .filter(|repository| repository.state.repository.identity != target)
            .filter(|repository| {
                repository.inputs.iter().any(|input| {
                    plugin
                        .input_kind(PluginInputScope::Context, &input.path)
                        .is_some()
                })
            })
            .map(|repository| repository.state.repository.identity.clone())
            .collect()
    }

    fn semantic_inputs(
        &self,
    ) -> (
        BTreeSet<beholder_domain::EntityKind>,
        BTreeSet<beholder_domain::SemanticRelation>,
    ) {
        self.plugin.as_ref().map_or_else(
            || {
                (
                    self.semantic_entities.clone(),
                    self.semantic_relations.clone(),
                )
            },
            |plugin| {
                (
                    plugin.semantic_entities.clone(),
                    plugin.semantic_relations.clone(),
                )
            },
        )
    }

    fn source_currentness(&self) -> EnrichmentSourceCurrentness {
        if self.semantic_shard_producers.is_empty() {
            EnrichmentSourceCurrentness::RawInputs
        } else {
            EnrichmentSourceCurrentness::SemanticShards {
                producers: self.semantic_shard_producers.clone(),
            }
        }
    }

    fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
        let snapshot = self.declared_snapshot(snapshot);
        let span = tracing::info_span!(
            "worker.analyze",
            worker = self.metadata.id,
            workspace = snapshot.workspace.name,
            target_repository = snapshot.target_repository,
            memory_limit_bytes = self.memory_limit_bytes().unwrap_or_default(),
            peak_process_tree_rss_bytes = tracing::field::Empty,
            rpc.system = "grpc",
            rpc.service = "beholder.worker.v1.AnalyzerWorker",
            rpc.method = "Analyze"
        );
        Box::pin(
            async move {
                let analysis_inactivity_timeout = self.analysis_inactivity_timeout();
                let memory_limit_bytes = self.memory_limit_bytes();
                let mut session = self.session.lock().await;
                if let Some(worker) = session.as_mut()
                    && worker.child.try_wait()?.is_some()
                {
                    session.take();
                }
                if session.is_none() {
                    *session = Some(
                        self.start_session(analysis_inactivity_timeout, memory_limit_bytes)
                            .await?,
                    );
                }
                let analysis_started = tokio::time::Instant::now();
                let workspace = snapshot.workspace.name.clone();
                let baseline = snapshot.baseline.clone();
                let mut request = Request::new(tokio_stream::iter(analyze_requests(snapshot)?));
                beholder_observability::inject_current_context(request.metadata_mut());
                let worker = session.as_mut().expect("worker session was started");
                #[cfg(unix)]
                let response = {
                    let process_group = worker
                        .memory_guard
                        .as_ref()
                        .map(ProcessMemoryGuard::process_group);
                    tokio::select! {
                        biased;
                        event = memory_guard_event(worker.memory_guard.as_mut()) => {
                            return Err(terminate_for_memory_event(&mut worker.child, process_group, event).await);
                        }
                        response = tokio::time::timeout(
                            analysis_inactivity_timeout,
                            worker.client.analyze(request),
                        ) => response
                            .map_err(|_| {
                                format!(
                                    "worker analysis timed out after {}ms without progress",
                                    analysis_inactivity_timeout.as_millis()
                                )
                            })??,
                    }
                };
                #[cfg(not(unix))]
                let response = tokio::time::timeout(
                    analysis_inactivity_timeout,
                    worker.client.analyze(request),
                )
                .await
                .map_err(|_| {
                    format!(
                        "worker analysis timed out after {}ms without progress",
                        analysis_inactivity_timeout.as_millis()
                    )
                })??;
                let mut stream = response.into_inner();
                let mut contribution = ContributionAccumulator::default();
                loop {
                    #[cfg(unix)]
                    let event = {
                        let process_group = worker
                            .memory_guard
                            .as_ref()
                            .map(ProcessMemoryGuard::process_group);
                        tokio::select! {
                            biased;
                            event = memory_guard_event(worker.memory_guard.as_mut()) => {
                                return Err(terminate_for_memory_event(&mut worker.child, process_group, event).await);
                            }
                            event = tokio::time::timeout(analysis_inactivity_timeout, stream.message()) => event
                                .map_err(|_| {
                                    format!(
                                        "worker analysis timed out after {}ms without progress",
                                        analysis_inactivity_timeout.as_millis()
                                    )
                                })??,
                        }
                    };
                    #[cfg(not(unix))]
                    let event = tokio::time::timeout(analysis_inactivity_timeout, stream.message())
                        .await
                        .map_err(|_| {
                            format!(
                                "worker analysis timed out after {}ms without progress",
                                analysis_inactivity_timeout.as_millis()
                            )
                        })??;
                    let Some(event) = event else {
                        #[cfg(unix)]
                        if let Some(memory_event) = worker
                            .memory_guard
                            .as_ref()
                            .and_then(ProcessMemoryGuard::event_if_ready)
                        {
                            let process_group = worker
                                .memory_guard
                                .as_ref()
                                .map(ProcessMemoryGuard::process_group);
                            return Err(terminate_for_memory_event(
                                &mut worker.child,
                                process_group,
                                memory_event,
                            )
                            .await);
                        }
                        break;
                    };
                    if let Some(analyze_event::Event::Progress(progress)) = &event.event {
                        let phase = AnalysisPhase::try_from(progress.phase)
                            .map_err(|_| "worker progress phase is unknown")?;
                        if phase == AnalysisPhase::Unspecified {
                            return Err("worker progress phase is missing".into());
                        }
                        tracing::info!(
                            worker = self.metadata.id,
                            workspace,
                            phase = ?phase,
                            detail = progress.detail.as_deref().unwrap_or_default(),
                            elapsed_ms = analysis_started.elapsed().as_secs_f64() * 1_000.0,
                            "worker analysis progress"
                        );
                    }
                    contribution.push(event)?;
                }
                let mut contribution = contribution.finish()?;
                if contribution.metadata != self.metadata {
                    return Err(format!(
                        "worker returned analyzer identity {}:{}; expected {}:{}",
                        contribution.metadata.id,
                        contribution.metadata.version,
                        self.metadata.id,
                        self.metadata.version
                    )
                    .into());
                }
                resolve_candidate_overrides(&baseline, &mut contribution)?;
                if let Some(descriptor) = &self.plugin {
                    validate_plugin_contribution(descriptor, &baseline, &contribution)?;
                }
                #[cfg(unix)]
                let peak_process_tree_rss_bytes = worker
                    .memory_guard
                    .as_ref()
                    .map(ProcessMemoryGuard::peak_bytes)
                    .unwrap_or_default();
                #[cfg(not(unix))]
                let peak_process_tree_rss_bytes = 0;
                tracing::Span::current().record(
                    "peak_process_tree_rss_bytes",
                    peak_process_tree_rss_bytes,
                );
                tracing::info!(
                    worker = self.metadata.id,
                    workspace,
                    elapsed_ms = analysis_started.elapsed().as_secs_f64() * 1_000.0,
                    peak_process_tree_rss_bytes,
                    override_count = contribution.overrides.len(),
                    observation_count = contribution
                        .repositories
                        .iter()
                        .map(|repository| repository.observations.len())
                        .sum::<usize>(),
                    diagnostic_count = contribution.diagnostics.len()
                        + contribution
                            .repositories
                            .iter()
                            .map(|repository| repository.diagnostics.len())
                            .sum::<usize>(),
                    "worker analysis completed"
                );
                if !self.persistent
                    && let Some(mut worker) = session.take()
                    && worker.child.try_wait()?.is_none()
                {
                    #[cfg(unix)]
                    if let Some(memory_guard) = worker.memory_guard.as_ref() {
                        terminate_process_group(
                            memory_guard.process_group(),
                            &mut worker.child,
                        )
                        .await?;
                    } else {
                        worker.child.kill().await?;
                    }
                    #[cfg(not(unix))]
                    worker.child.kill().await?;
                    tracing::debug!(
                        worker = self.metadata.id,
                        "worker process terminated after completed analysis"
                    );
                }
                Ok(contribution)
            }
            .instrument(span),
        )
    }
}

fn resolve_candidate_overrides(
    baseline: &beholder_indexing::SemanticSnapshot,
    contribution: &mut beholder_indexing::AnalyzerContribution,
) -> Result<(), AnalyzerError> {
    let candidates = baseline
        .candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    if candidates.len() != baseline.candidates.len() {
        return Err("baseline semantic candidate IDs are not unique".into());
    }
    let known = baseline
        .entities
        .iter()
        .map(|entity| entity.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for override_ in contribution.candidate_overrides.drain(..) {
        if !seen.insert(override_.candidate_id.clone()) {
            return Err(format!(
                "worker returned semantic candidate {} more than once",
                override_.candidate_id
            )
            .into());
        }
        let candidate = candidates
            .get(override_.candidate_id.as_str())
            .ok_or_else(|| {
                format!(
                    "worker returned unknown semantic candidate {}",
                    override_.candidate_id
                )
            })?;
        if !known.contains(override_.resolved_to.as_str()) {
            return Err(format!(
                "worker resolved semantic candidate {} to unknown entity {}",
                override_.candidate_id, override_.resolved_to
            )
            .into());
        }
        contribution
            .overrides
            .push(beholder_domain::DependencyOverride {
                from: candidate.from.clone(),
                relation: candidate.relation,
                unresolved_to: candidate.unresolved_to.clone(),
                resolved_to: override_.resolved_to,
                evidence: override_.evidence,
                confidence: beholder_domain::Confidence::Exact,
                provenance: beholder_domain::Provenance::Compiler,
            });
    }
    Ok(())
}

impl WorkerAnalyzer {
    fn analysis_inactivity_timeout(&self) -> Duration {
        configured_worker_environment(&self.metadata.id)
            .get("BEHOLDER_WORKER_TIMEOUT_MS")
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_millis)
            .unwrap_or(self.timeout)
    }

    fn memory_limit_bytes(&self) -> Option<u64> {
        std::env::var_os(worker_environment_variable(
            &self.metadata.id,
            "MEMORY_LIMIT_BYTES",
        ))
        .and_then(|value| value.to_str().and_then(|value| value.parse::<u64>().ok()))
        .filter(|value| *value > 0)
        .or(self.memory_limit_bytes)
    }

    async fn start_session(
        &self,
        analysis_inactivity_timeout: Duration,
        memory_limit_bytes: Option<u64>,
    ) -> Result<WorkerSession, AnalyzerError> {
        fs::create_dir_all(&self.socket_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.socket_dir, fs::Permissions::from_mode(0o700))?;
        }
        let socket = self.socket_dir.join(format!(
            "w-{}-{}.sock",
            std::process::id(),
            WORKER_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let socket_file = SocketFile(socket.clone());
        let mut worker_environment = configured_worker_environment(&self.metadata.id);
        worker_environment.insert(
            "BEHOLDER_WORKER_TIMEOUT_MS".into(),
            analysis_inactivity_timeout.as_millis().to_string().into(),
        );
        if let Some(limit) = memory_limit_bytes {
            worker_environment.insert(
                "BEHOLDER_WORKER_MEMORY_LIMIT_BYTES".into(),
                limit.to_string().into(),
            );
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("--socket")
            .arg(&socket)
            .arg("--cache-dir")
            .arg(self.socket_dir.join("cache"))
            .stdin(Stdio::null())
            .env("OTEL_SERVICE_NAME", worker_service_name(&self.metadata.id))
            .env("BEHOLDER_PLUGIN_DIGEST", &self.metadata.version)
            .kill_on_drop(true)
            .envs(worker_environment);
        if let Some(limit) = memory_limit_bytes
            && std::env::var_os("GOMEMLIMIT").is_none()
        {
            command.env("GOMEMLIMIT", format!("{}B", soft_memory_limit_bytes(limit)));
        }
        #[cfg(unix)]
        if memory_limit_bytes.is_some() {
            isolate_process_group(&mut command);
        }
        let mut child = command.spawn()?;
        #[cfg(unix)]
        let mut memory_guard = if let Some(limit) = memory_limit_bytes {
            let process_group = child.id().ok_or("worker process ID is unavailable")? as i32;
            match ProcessMemoryGuard::start(process_group, limit).await {
                Ok(guard) => Some(guard),
                Err(error) => {
                    let _ = terminate_process_group(process_group, &mut child).await;
                    return Err(format!("start worker memory guard: {error}").into());
                }
            }
        } else {
            None
        };
        let endpoint = format!("unix:{}", socket.display());
        let started = tokio::time::Instant::now();
        let client = loop {
            #[cfg(unix)]
            let connection = tokio::select! {
                biased;
                event = memory_guard_event(memory_guard.as_mut()) => {
                    let process_group = memory_guard
                        .as_ref()
                        .map(ProcessMemoryGuard::process_group);
                    return Err(terminate_for_memory_event(&mut child, process_group, event).await);
                }
                connection = AnalyzerWorkerClient::connect(endpoint.clone()) => connection,
            };
            #[cfg(not(unix))]
            let connection = AnalyzerWorkerClient::connect(endpoint.clone()).await;
            match connection {
                Ok(client) => break client,
                Err(_) if started.elapsed() < CONNECT_TIMEOUT => {
                    if let Some(status) = child.try_wait()? {
                        return Err(format!("worker exited before readiness: {status}").into());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        .max_encoding_message_size(MAX_MESSAGE_BYTES)
        .max_decoding_message_size(MAX_MESSAGE_BYTES);
        Ok(WorkerSession {
            child,
            client,
            #[cfg(unix)]
            memory_guard,
            _socket: socket_file,
        })
    }

    fn declared_snapshot(&self, mut snapshot: EnrichmentSnapshot) -> EnrichmentSnapshot {
        if let Some(plugin) = &self.plugin {
            for repository in &mut snapshot.workspace.repositories {
                let scope = if repository.state.repository.identity == snapshot.target_repository {
                    PluginInputScope::Target
                } else {
                    PluginInputScope::Context
                };
                repository
                    .inputs
                    .retain(|input| plugin.input_kind(scope, &input.path).is_some());
            }
            return snapshot;
        }
        for repository in &mut snapshot.workspace.repositories {
            repository
                .inputs
                .retain(|input| self.analysis_input_kind(&input.path).is_some());
        }
        snapshot
    }
}

fn validate_plugin_contribution(
    descriptor: &PluginDescriptor,
    baseline: &beholder_indexing::SemanticSnapshot,
    contribution: &beholder_indexing::AnalyzerContribution,
) -> Result<(), AnalyzerError> {
    let mut known = baseline
        .entities
        .iter()
        .map(|entity| entity.id.as_str())
        .collect::<BTreeSet<_>>();
    for repository in &contribution.repositories {
        for entity in &repository.entities {
            if !descriptor.produces_entities.contains(&entity.kind) {
                return Err(format!(
                    "plugin {} produced undeclared entity kind {:?}",
                    descriptor.id, entity.kind
                )
                .into());
            }
            known.insert(entity.id.as_str());
        }
    }
    for repository in &contribution.repositories {
        for observation in &repository.observations {
            if !descriptor
                .produces_relations
                .contains(&observation.relation)
            {
                return Err(format!(
                    "plugin {} produced undeclared relation {:?}",
                    descriptor.id, observation.relation
                )
                .into());
            }
            if !known.contains(observation.from.as_str())
                || !known.contains(observation.to.as_str())
            {
                return Err(format!(
                    "plugin {} produced a relation with an unknown endpoint",
                    descriptor.id
                )
                .into());
            }
        }
    }
    Ok(())
}

fn worker_service_name(worker: &str) -> String {
    match std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .as_deref()
    {
        None | Some("beholderd") => format!("beholder-worker-{worker}"),
        Some(parent) => format!("{parent}-worker-{worker}"),
    }
}

fn soft_memory_limit_bytes(hard_limit_bytes: u64) -> u64 {
    hard_limit_bytes - hard_limit_bytes / 8
}

#[cfg(unix)]
async fn memory_guard_event(guard: Option<&mut ProcessMemoryGuard>) -> MemoryGuardEvent {
    match guard {
        Some(guard) => guard.event().await,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
async fn terminate_for_memory_event(
    child: &mut Child,
    process_group: Option<i32>,
    event: MemoryGuardEvent,
) -> AnalyzerError {
    let message = event.to_string();
    tracing::error!(
        error = message,
        "worker memory guard terminated process tree"
    );
    if let Some(process_group) = process_group
        && let Err(error) = terminate_process_group(process_group, child).await
    {
        return format!("{message}; terminate worker process tree: {error}").into();
    }
    message.into()
}

struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        CandidateOverride, DependencyRelation, EntityFact, EntityKind, LogicalRepository,
        RepositoryState, SemanticCandidate, SemanticRelation, SourcePosition, SourceSpan,
    };
    use beholder_indexing::{
        AnalyzerContribution, AnalyzerMetadata, CacheStatistics, InputKind, RepositoryInput,
        RepositorySnapshot, SemanticSnapshot, WorkspaceSnapshot,
    };
    use std::{env, fs, sync::Arc, time::SystemTime};

    #[test]
    fn worker_environment_variables_have_one_consistent_shape() {
        assert_eq!(
            worker_environment_variable("elixir", "timeout_ms"),
            "BEHOLDER_ELIXIR_WORKER_TIMEOUT_MS"
        );
        assert_eq!(
            worker_environment_variable("fresha-beholder", "timeout_ms"),
            "BEHOLDER_FRESHA_BEHOLDER_WORKER_TIMEOUT_MS"
        );
        assert_eq!(
            worker_environment_variable("rust", "max_output_bytes"),
            "BEHOLDER_RUST_WORKER_MAX_OUTPUT_BYTES"
        );
        assert_eq!(
            worker_environment_variable("typescript", "memory_limit_bytes"),
            "BEHOLDER_TYPESCRIPT_WORKER_MEMORY_LIMIT_BYTES"
        );
    }

    #[test]
    fn worker_timeout_can_override_the_shared_default() {
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("elixir", "1")
            .timeout(Duration::from_secs(1_200))
            .accept_extension("ex")
            .build()
            .unwrap();

        assert_eq!(worker.timeout, Duration::from_secs(1_200));
    }

    #[test]
    fn worker_memory_limit_keeps_gc_headroom() {
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("typescript", "1")
            .memory_limit(4 * 1024 * 1024 * 1024)
            .accept_extension("ts")
            .build()
            .unwrap();

        assert_eq!(worker.memory_limit_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(
            soft_memory_limit_bytes(worker.memory_limit_bytes.unwrap()),
            3_758_096_384
        );
    }

    #[test]
    fn worker_requires_a_configured_activation_path() {
        let root = env::temp_dir().join(format!(
            "beholder-worker-activation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("typescript", "1")
            .accept_extension("js")
            .activate_when_path_exists("node_modules/.bin/tsgo")
            .activate_when_path_exists("node_modules/.bin/tsc")
            .build()
            .unwrap();
        let repository = RepositorySnapshot {
            base: root.clone(),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: "example".into(),
                },
                head: None,
                fingerprint: "fingerprint".into(),
            },
            inputs: vec![RepositoryInput {
                path: "script.js".into(),
                content: Arc::from(Vec::<u8>::new()),
                kind: InputKind::Source,
            }],
        };

        assert!(!worker.is_active(&repository));
        let compiler = root.join("node_modules/.bin/tsgo");
        fs::create_dir_all(compiler.parent().unwrap()).unwrap();
        fs::write(compiler, []).unwrap();
        assert!(worker.is_active(&repository));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn over_limit_worker_cannot_start_an_analysis_session() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!(
            "beholder-over-limit-worker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("worker");
        fs::write(&executable, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let worker = WorkerAnalyzerBuilder::new(&executable, root.join("sockets"))
            .identity("typescript", "1")
            .memory_limit(1)
            .accept_extension("ts")
            .build()
            .unwrap();

        let error = worker
            .start_session(Duration::from_secs(1), worker.memory_limit_bytes())
            .await
            .err()
            .expect("over-limit worker unexpectedly started");

        assert!(
            error
                .to_string()
                .contains("exceeded its 1-byte memory limit")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_inputs_preserve_semantic_roles_and_shared_identity() {
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("rust", "1")
            .semantic_relation(SemanticRelation::Dependency(DependencyRelation::Calls))
            .accept_extension("rs")
            .accept_file_name_as("Cargo.lock", AnalysisInputKind::Dependency)
            .accept_path_suffix_as(".cargo/config.toml", AnalysisInputKind::Configuration)
            .accept_parent_suffix_as("config", AnalysisInputKind::Configuration)
            .exclude_path_suffix("target/generated.rs")
            .exclude_path_suffix("config/prod.exs")
            .exclude_path_suffix("config/runtime.exs")
            .identity_input(
                "$toolchain/rustc",
                b"rustc 1".to_vec(),
                AnalysisInputKind::Toolchain,
            )
            .build()
            .unwrap();

        assert_eq!(
            worker.analysis_input_kind(Path::new("src/lib.rs")),
            Some(AnalysisInputKind::Source)
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("Cargo.lock")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("nested/.cargo/config.toml")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("config/dev.exs")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("apps/api/config/nested/dev.exs")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("config/runtime.exs")),
            None
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("config/prod.exs")),
            None
        );
        assert_eq!(worker.identity_inputs().len(), 1);
        assert_eq!(
            worker.semantic_inputs().1,
            BTreeSet::from([SemanticRelation::Dependency(DependencyRelation::Calls)])
        );
        assert_eq!(
            worker.analysis_input_kind(Path::new("target/generated.rs")),
            None
        );
        assert_eq!(
            worker.identity_inputs()[0].kind,
            AnalysisInputKind::Toolchain
        );
    }

    #[test]
    fn worker_snapshot_contains_only_declared_inputs() {
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("rust", "1")
            .accept_extension("rs")
            .build()
            .unwrap();
        let input = |path: &str| RepositoryInput {
            path: path.into(),
            content: Arc::from(&b"contents"[..]),
            kind: InputKind::Source,
        };
        let snapshot = EnrichmentSnapshot {
            workspace: WorkspaceSnapshot {
                name: "main".into(),
                repositories: vec![RepositorySnapshot {
                    base: "/workspace/example".into(),
                    state: RepositoryState {
                        repository: LogicalRepository {
                            identity: "example".into(),
                        },
                        head: None,
                        fingerprint: "fingerprint".into(),
                    },
                    inputs: vec![input("src/lib.rs"), input("README.md")],
                }],
            },
            target_repository: "example".into(),
            baseline: Default::default(),
        };

        let declared = worker.declared_snapshot(snapshot);

        assert_eq!(declared.workspace.repositories[0].inputs.len(), 1);
        assert_eq!(
            declared.workspace.repositories[0].inputs[0].path,
            Path::new("src/lib.rs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_compiler_package_participates_in_currentness() {
        let root = std::env::temp_dir().join(format!(
            "beholder-worker-identity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let compiler = root.join("node_modules/typescript/package.json");
        fs::create_dir_all(compiler.parent().unwrap()).unwrap();
        fs::write(&compiler, r#"{"version":"7.0.2"}"#).unwrap();
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("typescript", "1")
            .accept_extension("ts")
            .repository_file_identity(
                "$toolchain/typescript",
                "node_modules/typescript/package.json",
                AnalysisInputKind::Toolchain,
            )
            .build()
            .unwrap();
        let repository = RepositorySnapshot {
            base: root.clone(),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: "example".into(),
                },
                head: None,
                fingerprint: "fingerprint".into(),
            },
            inputs: Vec::new(),
        };

        let identity = worker.repository_identity_inputs(&repository);

        assert_eq!(identity[0].content.as_ref(), br#"{"version":"7.0.2"}"#);
        fs::write(&compiler, r#"{"version":"7.0.3"}"#).unwrap();
        assert_ne!(identity, worker.repository_identity_inputs(&repository));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_override_uses_the_immutable_baseline_edge() {
        let target = EntityFact::new(
            "repo://example/typescript/target/run",
            EntityKind::Callable,
            None,
        )
        .unwrap();
        let baseline = SemanticSnapshot {
            entities: vec![target.clone()],
            observations: Vec::new(),
            candidates: vec![SemanticCandidate {
                id: "candidate".into(),
                repository: "example".into(),
                from: "repo://example/typescript/source/main".into(),
                relation: DependencyRelation::Calls,
                unresolved_to: "typescript-call://run".into(),
                span: SourceSpan {
                    path: "src/main.ts".into(),
                    start: SourcePosition {
                        line: 0,
                        character: 0,
                    },
                    end: SourcePosition {
                        line: 0,
                        character: 3,
                    },
                },
                evidence: "src/main.ts:1".into(),
            }],
        };
        let mut contribution = AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: "typescript".into(),
                version: "1".into(),
            },
            active_repositories: vec!["example".into()],
            repositories: Vec::new(),
            overrides: Vec::new(),
            candidate_overrides: vec![CandidateOverride {
                candidate_id: "candidate".into(),
                resolved_to: target.id,
                evidence: "tsc definition".into(),
            }],
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        };

        resolve_candidate_overrides(&baseline, &mut contribution).unwrap();

        assert!(contribution.candidate_overrides.is_empty());
        assert_eq!(contribution.overrides.len(), 1);
        let override_ = &contribution.overrides[0];
        assert_eq!(
            override_.from.as_str(),
            "repo://example/typescript/source/main"
        );
        assert_eq!(override_.unresolved_to.as_str(), "typescript-call://run");
        assert_eq!(override_.confidence, beholder_domain::Confidence::Exact);
        assert_eq!(override_.provenance, beholder_domain::Provenance::Compiler);
    }
}
