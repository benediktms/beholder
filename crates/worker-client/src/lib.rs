//! Builder facade for native analyzer workers.

use beholder_indexing::{
    AnalysisInput, AnalysisInputKind, AnalyzerError, AnalyzerMetadata, EnrichmentFuture,
    EnrichmentSnapshot, WorkspaceEnricher,
};
use beholder_protocol::{
    analyze_requests, contribution_from_events,
    worker_v1::{AnalysisPhase, analyze_event, analyzer_worker_client::AnalyzerWorkerClient},
};
use std::{
    collections::BTreeMap,
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
use tokio::process::Command;
use tonic::Request;
use tracing::Instrument;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
static WORKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        }
    }

    pub fn identity(mut self, id: impl Into<String>, version: impl Into<String>) -> Self {
        self.metadata = AnalyzerMetadata {
            id: id.into(),
            version: version.into(),
        };
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
}

impl WorkspaceEnricher for WorkerAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        self.metadata.clone()
    }

    fn accepts(&self, path: &Path) -> bool {
        self.analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
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
                let _socket_file = SocketFile(socket.clone());
                let mut child = Command::new(&self.executable)
                    .arg("--socket")
                    .arg(&socket)
                    .arg("--cache-dir")
                    .arg(self.socket_dir.join("cache"))
                    .stdin(Stdio::null())
                    .env("OTEL_SERVICE_NAME", worker_service_name(&self.metadata.id))
                    .kill_on_drop(true)
                    .spawn()?;
                let analysis_started = tokio::time::Instant::now();
                let workspace = snapshot.workspace.name.clone();
                let endpoint = format!("unix:{}", socket.display());
                let started = tokio::time::Instant::now();
                let mut client = loop {
                    match AnalyzerWorkerClient::connect(endpoint.clone()).await {
                        Ok(client) => break client,
                        Err(_) if started.elapsed() < CONNECT_TIMEOUT => {
                            if let Some(status) = child.try_wait()? {
                                return Err(
                                    format!("worker exited before readiness: {status}").into()
                                );
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                .max_encoding_message_size(MAX_MESSAGE_BYTES)
                .max_decoding_message_size(MAX_MESSAGE_BYTES);
                let mut request = Request::new(tokio_stream::iter(analyze_requests(snapshot)));
                beholder_observability::inject_current_context(request.metadata_mut());
                let mut stream = client.analyze(request).await?.into_inner();
                let mut events = Vec::new();
                while let Some(event) = stream.message().await? {
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
                if child.try_wait()?.is_none() {
                    child.kill().await?;
                }
                Ok(contribution)
            }
            .instrument(span),
        )
    }
}

impl WorkerAnalyzer {
    fn declared_snapshot(&self, mut snapshot: EnrichmentSnapshot) -> EnrichmentSnapshot {
        for repository in &mut snapshot.workspace.repositories {
            repository
                .inputs
                .retain(|input| self.analysis_input_kind(&input.path).is_some());
        }
        snapshot
    }
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
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot, WorkspaceSnapshot};
    use std::sync::Arc;

    #[test]
    fn worker_inputs_preserve_semantic_roles_and_shared_identity() {
        let worker = WorkerAnalyzerBuilder::new("worker", "sockets")
            .identity("rust", "1")
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
        assert_eq!(worker.analysis_input_kind(Path::new("config/prod.exs")), None);
        assert_eq!(worker.identity_inputs().len(), 1);
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
        };

        let declared = worker.declared_snapshot(snapshot);

        assert_eq!(declared.workspace.repositories[0].inputs.len(), 1);
        assert_eq!(
            declared.workspace.repositories[0].inputs[0].path,
            Path::new("src/lib.rs")
        );
    }
}
