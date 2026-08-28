//! Builder facade for native analyzer workers.

mod plugin_registry;

pub use plugin_registry::{InstalledPlugin, PluginRegistry, describe_plugin};

use beholder_domain::SemanticRelation;
use beholder_indexing::{
    AnalysisInput, AnalysisInputKind, AnalyzerError, AnalyzerMetadata, EnrichmentFuture,
    EnrichmentSnapshot, EnrichmentSourceCurrentness, PluginDescriptor, PluginInputScope,
    WorkspaceEnricher,
};
use beholder_protocol::{
    analyze_requests, contribution_from_events,
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

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ANALYSIS_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const WORKER_SETTINGS: [&str; 2] = ["MAX_OUTPUT_BYTES", "TIMEOUT_MS"];
static WORKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const RUST_WORKER_ID: &str = "rust";
pub const ELIXIR_WORKER_ID: &str = "elixir";

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
    identity_inputs: Vec<AnalysisInput>,
    semantic_relations: BTreeSet<SemanticRelation>,
    semantic_shard_producers: BTreeSet<String>,
    plugin: Option<PluginDescriptor>,
    persistent: bool,
    timeout: Duration,
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
            identity_inputs: Vec::new(),
            semantic_relations: BTreeSet::new(),
            semantic_shard_producers: BTreeSet::new(),
            plugin: None,
            persistent: false,
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

    pub fn semantic_relation(mut self, relation: SemanticRelation) -> Self {
        self.semantic_relations.insert(relation);
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
            identity_inputs: self.identity_inputs,
            semantic_relations: self.semantic_relations,
            semantic_shard_producers: self.semantic_shard_producers,
            plugin: self.plugin,
            persistent: self.persistent,
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
    identity_inputs: Vec<AnalysisInput>,
    semantic_relations: BTreeSet<SemanticRelation>,
    semantic_shard_producers: BTreeSet<String>,
    plugin: Option<PluginDescriptor>,
    persistent: bool,
    session: Mutex<Option<WorkerSession>>,
    timeout: Duration,
}

struct WorkerSession {
    child: Child,
    client: AnalyzerWorkerClient<Channel>,
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
        identity_inputs: Vec::new(),
        semantic_relations: BTreeSet::new(),
        semantic_shard_producers: BTreeSet::new(),
        plugin: Some(descriptor),
        persistent: false,
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

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        if let Some(plugin) = &self.plugin {
            return repository.inputs.iter().any(|input| {
                plugin
                    .input_kind(PluginInputScope::Target, &input.path)
                    .is_some()
            });
        }
        repository
            .inputs
            .iter()
            .any(|input| self.analysis_input_kind(&input.path).is_some())
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
            || (BTreeSet::new(), self.semantic_relations.clone()),
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
            rpc.system = "grpc",
            rpc.service = "beholder.worker.v1.AnalyzerWorker",
            rpc.method = "Analyze"
        );
        Box::pin(
            async move {
                let analysis_inactivity_timeout = self.analysis_inactivity_timeout();
                let mut session = self.session.lock().await;
                if let Some(worker) = session.as_mut()
                    && worker.child.try_wait()?.is_some()
                {
                    session.take();
                }
                if session.is_none() {
                    *session = Some(self.start_session(analysis_inactivity_timeout).await?);
                }
                let analysis_started = tokio::time::Instant::now();
                let workspace = snapshot.workspace.name.clone();
                let baseline = snapshot.baseline.clone();
                let mut request = Request::new(tokio_stream::iter(analyze_requests(snapshot)?));
                beholder_observability::inject_current_context(request.metadata_mut());
                let worker = session.as_mut().expect("worker session was started");
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
                let mut events = Vec::new();
                loop {
                    let event = tokio::time::timeout(analysis_inactivity_timeout, stream.message())
                        .await
                        .map_err(|_| {
                            format!(
                                "worker analysis timed out after {}ms without progress",
                                analysis_inactivity_timeout.as_millis()
                            )
                        })??;
                    let Some(event) = event else {
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
                    events.push(event);
                }
                let contribution = contribution_from_events(events)?;
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
                if let Some(descriptor) = &self.plugin {
                    validate_plugin_contribution(descriptor, &baseline, &contribution)?;
                }
                tracing::info!(
                    worker = self.metadata.id,
                    workspace,
                    elapsed_ms = analysis_started.elapsed().as_secs_f64() * 1_000.0,
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

    async fn start_session(
        &self,
        analysis_inactivity_timeout: Duration,
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
        let mut child = command.spawn()?;
        let endpoint = format!("unix:{}", socket.display());
        let started = tokio::time::Instant::now();
        let client = loop {
            match AnalyzerWorkerClient::connect(endpoint.clone()).await {
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
        DependencyRelation, LogicalRepository, RepositoryState, SemanticRelation,
    };
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot, WorkspaceSnapshot};
    use std::sync::Arc;

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
}
