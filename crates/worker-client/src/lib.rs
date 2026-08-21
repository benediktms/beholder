//! Builder facade for native analyzer workers.

use beholder_indexing::{
    AnalyzerError, AnalyzerMetadata, EnrichmentFuture, EnrichmentSnapshot, WorkspaceEnricher,
};
use beholder_protocol::{
    analyze_requests, contribution_from_events,
    worker_v1::{AnalysisPhase, analyze_event, analyzer_worker_client::AnalyzerWorkerClient},
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
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
    extensions: BTreeSet<OsString>,
    file_names: BTreeSet<OsString>,
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
            extensions: BTreeSet::new(),
            file_names: BTreeSet::new(),
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
        self.extensions.insert(extension.into());
        self
    }

    pub fn accept_file_name(mut self, file_name: impl Into<OsString>) -> Self {
        self.file_names.insert(file_name.into());
        self
    }

    pub fn build(self) -> Result<WorkerAnalyzer, AnalyzerError> {
        if self.metadata.id.is_empty() {
            return Err("worker analyzer identity must not be empty".into());
        }
        if self.metadata.version.is_empty() {
            return Err("worker analyzer version must not be empty".into());
        }
        if self.extensions.is_empty() && self.file_names.is_empty() {
            return Err("worker analyzer must accept at least one input".into());
        }
        Ok(WorkerAnalyzer {
            metadata: self.metadata,
            executable: self.executable,
            socket_dir: self.socket_dir,
            extensions: self.extensions,
            file_names: self.file_names,
        })
    }
}

pub struct WorkerAnalyzer {
    metadata: AnalyzerMetadata,
    executable: PathBuf,
    socket_dir: PathBuf,
    extensions: BTreeSet<OsString>,
    file_names: BTreeSet<OsString>,
}

impl WorkspaceEnricher for WorkerAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        self.metadata.clone()
    }

    fn accepts(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| self.extensions.contains(extension))
            || path
                .file_name()
                .is_some_and(|file_name| self.file_names.contains(file_name))
    }

    fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
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
