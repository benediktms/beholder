use beholder_adapters_treesitter_rust::{
    analyze, source_entity_id, validate_immutable_rust_inputs,
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, Provenance, SemanticRelation, UnsafeTreeRecovery,
};
use beholder_indexing::{
    AnalysisCompleteness, AnalyzerContribution, AnalyzerMetadata, CacheStatistics,
    RepositoryContribution, WorkspaceSnapshot,
};
use beholder_protocol::{
    WorkspaceSnapshotBuilder, analyze_events,
    worker_v1::{
        AnalysisPhase, AnalysisProgress, AnalyzeEvent, AnalyzeRequest, analyze_event,
        analyzer_worker_server::{AnalyzerWorker, AnalyzerWorkerServer},
    },
};
use ra_ap_ide::{
    Analysis, AnalysisHost, FileId, FilePosition, GotoDefinitionConfig, RaFixtureConfig, TextSize,
};
use ra_ap_ide_db::ChangeWithProcMacros;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, CargoFeatures};
use ra_ap_vfs::VfsPath;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::{Request, Response, Status, Streaming};
use tracing::Instrument;

const ANALYZER_VERSION: &str = "7:7:rust.tonic:1:rust-analyzer-0.0.348:worker-10";
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const RESOLUTION_CACHE_VERSION: u32 = 2;
static MATERIALIZATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static COMPILER_LOADS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static COMPILER_EXTRACTIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static COMPILER_RESOLUTIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct RustWorker {
    analysis_pool: Arc<rayon::ThreadPool>,
    compiler: Arc<Mutex<Option<CompilerWorkspace>>>,
    cache_dir: Arc<PathBuf>,
    cache_writer: CacheWriter,
}

#[derive(Clone)]
struct CacheWriter {
    pending: Arc<(Mutex<Option<PersistRequest>>, Condvar)>,
}

struct PersistRequest {
    path: PathBuf,
    cache: PersistedWorkspaceCache,
}

impl CacheWriter {
    fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let pending = Arc::new((Mutex::new(None::<PersistRequest>), Condvar::new()));
        let worker = Arc::clone(&pending);
        thread::Builder::new()
            .name("beholder-rust-cache".into())
            .spawn(move || {
                loop {
                    let request = {
                        let (lock, ready) = &*worker;
                        let mut request = lock.lock().expect("Rust cache writer lock poisoned");
                        while request.is_none() {
                            request = ready
                                .wait(request)
                                .expect("Rust cache writer lock poisoned");
                        }
                        request.take().expect("Rust cache request exists")
                    };
                    let started = Instant::now();
                    match persist_resolution_cache(&request.path, &request.cache) {
                        Ok(bytes) => tracing::info!(
                            path = %request.path.display(),
                            bytes,
                            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "Rust compiler resolution cache persisted"
                        ),
                        Err(error) => tracing::warn!(
                            path = %request.path.display(),
                            error = %error,
                            "Rust compiler resolution cache persistence failed"
                        ),
                    }
                }
            })?;
        Ok(Self { pending })
    }

    fn schedule(&self, request: PersistRequest) {
        let (pending, ready) = &*self.pending;
        *pending.lock().expect("Rust cache writer lock poisoned") = Some(request);
        ready.notify_one();
    }
}

fn load_resolution_cache(path: &Path) -> PersistedWorkspaceCache {
    let started = Instant::now();
    let result = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedWorkspaceCache>(&bytes).ok())
        .filter(|cache| cache.version == RESOLUTION_CACHE_VERSION);
    tracing::info!(
        path = %path.display(),
        hit = result.is_some(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "Rust compiler resolution cache loaded"
    );
    result.unwrap_or_default()
}

fn persist_resolution_cache(
    path: &Path,
    cache: &PersistedWorkspaceCache,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let bytes = serde_json::to_vec(cache)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, &bytes)?;
    fs::rename(temporary, path)?;
    Ok(bytes.len())
}

#[tonic::async_trait]
impl AnalyzerWorker for RustWorker {
    type AnalyzeStream = ReceiverStream<Result<AnalyzeEvent, Status>>;

    async fn analyze(
        &self,
        request: Request<Streaming<AnalyzeRequest>>,
    ) -> Result<Response<Self::AnalyzeStream>, Status> {
        let span = tracing::info_span!(
            "worker.analyze",
            workspace = tracing::field::Empty,
            rpc.system = "grpc",
            rpc.service = "beholder.worker.v1.AnalyzerWorker",
            rpc.method = "Analyze"
        );
        beholder_observability::set_parent_from_metadata(&span, request.metadata());
        let mut stream = request.into_inner();
        let analysis_pool = self.analysis_pool.clone();
        let compiler = self.compiler.clone();
        let cache_dir = self.cache_dir.clone();
        let cache_writer = self.cache_writer.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        tokio::spawn(
            async move {
                if send_progress(&sender, AnalysisPhase::ReceivingSnapshot).await {
                    return;
                }
                let mut snapshot = WorkspaceSnapshotBuilder::default();
                loop {
                    match stream.message().await {
                        Ok(Some(request)) => {
                            if let Err(error) = snapshot.push(request) {
                                let _ = sender.send(Err(Status::invalid_argument(error))).await;
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    }
                }
                let snapshot = match snapshot.finish() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let _ = sender.send(Err(Status::invalid_argument(error))).await;
                        return;
                    }
                };
                tracing::Span::current().record("workspace", &snapshot.workspace.name);
                if send_progress(&sender, AnalysisPhase::Analyzing).await {
                    return;
                }
                let analysis_span = tracing::info_span!(
                    "worker.rust.semantic_analysis",
                    workspace = snapshot.workspace.name,
                    target_repository = snapshot.target_repository,
                    repositories = snapshot.workspace.repositories.len()
                );
                let result = tokio::task::spawn_blocking(
                    move || -> Result<Vec<AnalyzeEvent>, Box<dyn Error + Send + Sync>> {
                        analysis_span.in_scope(|| {
                            analysis_pool.install(|| {
                                let mut contribution = baseline_contribution(&snapshot);
                                let enrichment_started = Instant::now();
                                enrich_semantics(
                                    &snapshot.workspace,
                                    &snapshot.target_repository,
                                    &mut contribution,
                                    &compiler,
                                    &cache_dir,
                                    &cache_writer,
                                );
                                tracing::info!(
                                    elapsed_ms =
                                        enrichment_started.elapsed().as_secs_f64() * 1_000.0,
                                    "Rust compiler enrichment completed"
                                );
                                retain_semantic_enrichment(
                                    &mut contribution,
                                    &snapshot.target_repository,
                                );
                                contribution.metadata.version = ANALYZER_VERSION.into();
                                analyze_events(contribution).map_err(Into::into)
                            })
                        })
                    },
                )
                .await;
                let events = match result {
                    Ok(Ok(events)) => events,
                    Ok(Err(error)) => {
                        let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                        return;
                    }
                    Err(error) => {
                        let _ = sender
                            .send(Err(Status::internal(format!(
                                "Rust worker task failed: {error}"
                            ))))
                            .await;
                        return;
                    }
                };
                for event in events {
                    if sender.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
            .instrument(span),
        );
        Ok(Response::new(ReceiverStream::new(receiver)))
    }
}

fn baseline_contribution(snapshot: &beholder_indexing::EnrichmentSnapshot) -> AnalyzerContribution {
    AnalyzerContribution {
        metadata: AnalyzerMetadata {
            id: "rust".into(),
            version: ANALYZER_VERSION.into(),
        },
        active_repositories: vec![snapshot.target_repository.clone()],
        repositories: vec![RepositoryContribution {
            repository: snapshot.target_repository.clone(),
            completeness: AnalysisCompleteness::Complete,
            entities: snapshot.baseline.entities.clone(),
            grpc_bindings: Vec::new(),
            observations: snapshot.baseline.observations.clone(),
            diagnostics: Vec::new(),
            fact_shards: Vec::new(),
        }],
        overrides: Vec::new(),
        graphql_resolvers: Vec::new(),
        diagnostics: Vec::new(),
        cache: CacheStatistics::default(),
    }
}

async fn send_progress(
    sender: &tokio::sync::mpsc::Sender<Result<AnalyzeEvent, Status>>,
    phase: AnalysisPhase,
) -> bool {
    sender
        .send(Ok(AnalyzeEvent {
            event: Some(analyze_event::Event::Progress(AnalysisProgress {
                phase: phase as i32,
                detail: None,
            })),
        }))
        .await
        .is_err()
}

struct MaterializedWorkspace {
    root: PathBuf,
    repositories: BTreeMap<String, PathBuf>,
    contents: BTreeMap<PathBuf, Arc<[u8]>>,
}

impl MaterializedWorkspace {
    fn new(snapshot: &WorkspaceSnapshot) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let bases = snapshot
            .repositories
            .iter()
            .map(|repository| absolute_lexical(&repository.base))
            .collect::<Vec<_>>();
        let mut common = bases
            .first()
            .cloned()
            .ok_or("Rust enrichment snapshot contains no repositories")?;
        while !bases.iter().all(|base| base.starts_with(&common)) {
            if !common.pop() {
                return Err("Rust enrichment repositories have no common filesystem root".into());
            }
        }
        let root = std::env::temp_dir().join(format!(
            "beholder-rust-snapshot-{}-{}",
            std::process::id(),
            MATERIALIZATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        let result = (|| {
            let mut repositories = BTreeMap::new();
            let mut contents = BTreeMap::new();
            for (repository, base) in snapshot.repositories.iter().zip(bases) {
                let relative = base.strip_prefix(&common)?;
                let materialized_base = root.join(relative);
                if repositories
                    .values()
                    .any(|existing| existing == &materialized_base)
                {
                    return Err(
                        "Rust enrichment snapshot maps repositories to the same directory".into(),
                    );
                }
                if repositories
                    .insert(
                        repository.state.repository.identity.clone(),
                        materialized_base.clone(),
                    )
                    .is_some()
                {
                    return Err("Rust enrichment snapshot contains duplicate repositories".into());
                }
                for input in &repository.inputs {
                    if input.path.is_absolute()
                        || input
                            .path
                            .components()
                            .any(|component| matches!(component, std::path::Component::ParentDir))
                    {
                        return Err(format!(
                            "Rust enrichment input escapes its repository: {}",
                            input.path.display()
                        )
                        .into());
                    }
                    let destination = materialized_base.join(&input.path);
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(destination, input.content.as_ref())?;
                    contents.insert(
                        PathBuf::from(&repository.state.repository.identity).join(&input.path),
                        Arc::clone(&input.content),
                    );
                }
            }
            Ok::<_, Box<dyn Error + Send + Sync>>((repositories, contents))
        })();
        match result {
            Ok((repositories, contents)) => Ok(Self {
                root,
                repositories,
                contents,
            }),
            Err(error) => {
                let _ = fs::remove_dir_all(&root);
                Err(error)
            }
        }
    }

    fn repository(&self, identity: &str) -> Option<&Path> {
        self.repositories.get(identity).map(PathBuf::as_path)
    }

    fn update_sources(
        &mut self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        for repository in &snapshot.repositories {
            let base = self
                .repositories
                .get(&repository.state.repository.identity)
                .ok_or("materialized Rust repository is missing")?;
            for input in repository
                .inputs
                .iter()
                .filter(|input| is_rust_source(&input.path))
            {
                let key = PathBuf::from(&repository.state.repository.identity).join(&input.path);
                if self.contents.get(&key) == Some(&input.content) {
                    continue;
                }
                fs::write(base.join(&input.path), input.content.as_ref())?;
                self.contents.insert(key, Arc::clone(&input.content));
            }
        }
        Ok(())
    }

    fn verify(&self, snapshot: &WorkspaceSnapshot) -> Result<(), Box<dyn Error + Send + Sync>> {
        for repository in &snapshot.repositories {
            let base = self
                .repository(&repository.state.repository.identity)
                .ok_or("materialized Rust repository is missing")?;
            for input in &repository.inputs {
                if fs::read(base.join(&input.path))?.as_slice() != input.content.as_ref() {
                    return Err(format!(
                        "{} changed during immutable Rust analysis",
                        input.path.display()
                    )
                    .into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompilerWorkspaceShape {
    workspace: String,
    target_repository: String,
    repositories: Vec<CompilerRepositoryShape>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompilerRepositoryShape {
    identity: String,
    base: PathBuf,
    inputs: Vec<CompilerInputShape>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompilerInputShape {
    path: PathBuf,
    kind: beholder_indexing::InputKind,
    configuration: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
struct PersistedWorkspaceCache {
    version: u32,
    cargo: BTreeMap<String, PersistedCargoCache>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
struct PersistedCargoCache {
    files: BTreeMap<String, PersistedFileAnalysis>,
    resolutions: Vec<PersistedResolution>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct PersistedResolution {
    key: ResolutionKey,
    resolution: CompilerResolution,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct PersistedFileAnalysis {
    analyzable: bool,
    module_hash: [u8; 32],
    symbols: BTreeMap<String, SymbolHashes>,
    #[serde(default)]
    dependencies: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct SymbolHashes {
    interface: [u8; 32],
    body: [u8; 32],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
struct ResolutionKey {
    symbol: String,
    ordinal: u32,
    unresolved: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
enum CompilerResolution {
    Resolved(String),
    Unrepresented,
    Unresolved,
}

impl CompilerWorkspaceShape {
    fn new(snapshot: &WorkspaceSnapshot, target_repository: &str) -> Self {
        Self {
            workspace: snapshot.name.clone(),
            target_repository: target_repository.to_owned(),
            repositories: snapshot
                .repositories
                .iter()
                .map(|repository| CompilerRepositoryShape {
                    identity: repository.state.repository.identity.clone(),
                    base: repository.base.clone(),
                    inputs: repository
                        .inputs
                        .iter()
                        .map(|input| CompilerInputShape {
                            path: input.path.clone(),
                            kind: input.kind,
                            configuration: (!is_rust_source(&input.path))
                                .then(|| Arc::clone(&input.content)),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn cache_key(&self) -> String {
        let mut digest = Sha256::new();
        hash_part(&mut digest, self.workspace.as_bytes());
        hash_part(&mut digest, self.target_repository.as_bytes());
        for repository in &self.repositories {
            hash_part(&mut digest, repository.identity.as_bytes());
            hash_part(&mut digest, repository.base.as_os_str().as_encoded_bytes());
            for input in &repository.inputs {
                hash_part(&mut digest, input.path.as_os_str().as_encoded_bytes());
                hash_part(&mut digest, format!("{:?}", input.kind).as_bytes());
                if let Some(configuration) = &input.configuration {
                    hash_part(&mut digest, configuration);
                }
            }
        }
        format!("{:x}", digest.finalize())
    }
}

fn hash_part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn symbol_interfaces_changed(
    before: &BTreeMap<String, SymbolHashes>,
    after: &BTreeMap<String, SymbolHashes>,
) -> bool {
    before.len() != after.len()
        || before.iter().any(|(symbol, hashes)| {
            after
                .get(symbol)
                .is_none_or(|current| current.interface != hashes.interface)
        })
}

fn affected_modules(
    files: &BTreeMap<String, PersistedFileAnalysis>,
    changed: &BTreeSet<String>,
) -> BTreeSet<String> {
    let graph = files
        .iter()
        .map(|(file, analysis)| (file.clone(), analysis.dependencies.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for start in graph.keys() {
        if visited.contains(start) {
            continue;
        }
        let mut stack = vec![(start.clone(), false)];
        while let Some((node, exiting)) = stack.pop() {
            if exiting {
                order.push(node);
            } else if visited.insert(node.clone()) {
                stack.push((node.clone(), true));
                stack.extend(
                    graph
                        .get(&node)
                        .into_iter()
                        .flatten()
                        .filter(|dependency| graph.contains_key(*dependency))
                        .map(|dependency| (dependency.clone(), false)),
                );
            }
        }
    }

    let mut reverse = graph
        .keys()
        .map(|node| (node.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (from, dependencies) in &graph {
        for dependency in dependencies {
            if let Some(dependents) = reverse.get_mut(dependency) {
                dependents.insert(from.clone());
            }
        }
    }
    let mut components = BTreeMap::new();
    while let Some(start) = order.pop() {
        if components.contains_key(&start) {
            continue;
        }
        let component = components.len();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if components.contains_key(&node) {
                continue;
            }
            components.insert(node.clone(), component);
            stack.extend(
                reverse
                    .get(&node)
                    .into_iter()
                    .flatten()
                    .filter(|dependent| !components.contains_key(*dependent))
                    .cloned(),
            );
        }
    }

    let mut reverse_components = BTreeMap::<usize, BTreeSet<usize>>::new();
    for (from, dependencies) in &graph {
        for dependency in dependencies {
            let (Some(&from), Some(&dependency)) =
                (components.get(from), components.get(dependency))
            else {
                continue;
            };
            if from != dependency {
                reverse_components
                    .entry(dependency)
                    .or_default()
                    .insert(from);
            }
        }
    }
    let mut affected_components = changed
        .iter()
        .filter_map(|file| components.get(file).copied())
        .collect::<BTreeSet<_>>();
    let mut pending = affected_components.iter().copied().collect::<Vec<_>>();
    while let Some(component) = pending.pop() {
        for dependent in reverse_components.get(&component).into_iter().flatten() {
            if affected_components.insert(*dependent) {
                pending.push(*dependent);
            }
        }
    }
    components
        .into_iter()
        .filter_map(|(file, component)| affected_components.contains(&component).then_some(file))
        .collect()
}

fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
}

impl Drop for MaterializedWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

struct CompilerWorkspace {
    shape: CompilerWorkspaceShape,
    materialized: MaterializedWorkspace,
    cargo: BTreeMap<String, CompilerCargo>,
    cache_path: PathBuf,
}

impl CompilerWorkspace {
    fn new(
        snapshot: &WorkspaceSnapshot,
        target_repository: &str,
        cache_dir: &Path,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let shape = CompilerWorkspaceShape::new(snapshot, target_repository);
        let cache_path = cache_dir
            .join("rust-compiler-resolution-v1")
            .join(format!("{}.json", shape.cache_key()));
        let mut persisted = load_resolution_cache(&cache_path);
        let materialized = MaterializedWorkspace::new(snapshot)?;
        let repository = snapshot
            .repositories
            .iter()
            .find(|repository| repository.state.repository.identity == target_repository)
            .ok_or("Rust enrichment target repository is missing")?;
        let materialized_target = materialized
            .repository(target_repository)
            .ok_or("materialized Rust target repository is missing")?;
        let mut cargo = BTreeMap::new();
        for root in cargo_roots(repository, materialized_target) {
            let key = root
                .strip_prefix(materialized_target)
                .unwrap_or(&root)
                .to_string_lossy()
                .into_owned();
            cargo.insert(
                key.clone(),
                CompilerCargo::load(
                    snapshot,
                    &materialized,
                    &root,
                    persisted.cargo.remove(&key).unwrap_or_default(),
                )?,
            );
        }
        Ok(Self {
            shape,
            materialized,
            cargo,
            cache_path,
        })
    }

    fn update(&mut self, snapshot: &WorkspaceSnapshot) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.materialized.update_sources(snapshot)?;
        for cargo in self.cargo.values_mut() {
            cargo.update(snapshot, &self.materialized)?;
        }
        Ok(())
    }

    fn persist_request(&mut self) -> Option<PersistRequest> {
        if !self.cargo.values().any(|cargo| cargo.cache_dirty) {
            return None;
        }
        let cargo = self
            .cargo
            .iter_mut()
            .map(|(key, compiler)| {
                compiler.cache_dirty = false;
                (key.clone(), compiler.persisted_cache())
            })
            .collect();
        Some(PersistRequest {
            path: self.cache_path.clone(),
            cache: PersistedWorkspaceCache {
                version: RESOLUTION_CACHE_VERSION,
                cargo,
            },
        })
    }
}

struct CompilerCargo {
    host: AnalysisHost,
    vfs: ra_ap_vfs::Vfs,
    sources: BTreeMap<FileId, Arc<[u8]>>,
    dirty: BTreeSet<FileId>,
    extractions: BTreeMap<FileId, Option<CompilerFileAnalysis>>,
    persisted_files: BTreeMap<String, PersistedFileAnalysis>,
    resolutions: BTreeMap<ResolutionKey, CompilerResolution>,
    cache_dirty: bool,
}

impl CompilerCargo {
    fn load(
        snapshot: &WorkspaceSnapshot,
        materialized: &MaterializedWorkspace,
        cargo_root: &Path,
        persisted: PersistedCargoCache,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        #[cfg(test)]
        COMPILER_LOADS.fetch_add(1, Ordering::Relaxed);
        let mut cargo = CargoConfig {
            features: cargo_features(
                std::env::var("BEHOLDER_RUST_FEATURES").ok().as_deref(),
                environment_enabled("BEHOLDER_RUST_ALL_FEATURES"),
                environment_enabled("BEHOLDER_RUST_NO_DEFAULT_FEATURES"),
            ),
            target: std::env::var("CARGO_BUILD_TARGET")
                .ok()
                .filter(|target| !target.trim().is_empty()),
            set_test: true,
            no_deps: snapshot.repositories.len() == 1,
            ..CargoConfig::default()
        };
        let cargo_home = materialized.root.join(".cargo-home");
        fs::create_dir_all(&cargo_home)?;
        cargo.extra_env.insert(
            "CARGO_HOME".into(),
            Some(cargo_home.to_string_lossy().into_owned()),
        );
        let load = LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 1,
        };
        let (database, vfs, _) = load_workspace_at(cargo_root, &cargo, &load, &|_| {})?;
        let mut compiler = Self {
            host: AnalysisHost::with_database(database),
            vfs,
            sources: BTreeMap::new(),
            dirty: BTreeSet::new(),
            extractions: BTreeMap::new(),
            persisted_files: persisted.files,
            resolutions: persisted
                .resolutions
                .into_iter()
                .map(|entry| (entry.key, entry.resolution))
                .collect(),
            cache_dirty: false,
        };
        compiler.update(snapshot, materialized)?;
        Ok(compiler)
    }

    fn persisted_cache(&self) -> PersistedCargoCache {
        PersistedCargoCache {
            files: self.persisted_files.clone(),
            resolutions: self
                .resolutions
                .iter()
                .map(|(key, resolution)| PersistedResolution {
                    key: key.clone(),
                    resolution: resolution.clone(),
                })
                .collect(),
        }
    }

    fn update_file_analyses(&mut self, analyses: Vec<(String, PersistedFileAnalysis)>) {
        if analyses.is_empty() {
            return;
        }
        let previous = self.persisted_files.clone();
        let updated_files = analyses
            .iter()
            .map(|(file, _)| file.clone())
            .collect::<BTreeSet<_>>();
        for (file, analysis) in analyses {
            self.persisted_files.insert(file, analysis);
        }
        if self.persisted_files == previous {
            return;
        }
        self.cache_dirty = true;
        if previous.is_empty()
            || updated_files.iter().any(|file| {
                previous
                    .get(file)
                    .is_none_or(|analysis| !analysis.analyzable)
                    || !self.persisted_files[file].analyzable
            })
        {
            self.resolutions.clear();
            return;
        }

        let changed_modules = previous
            .iter()
            .filter_map(|(file, before)| {
                let after = self.persisted_files.get(file)?;
                (before.module_hash != after.module_hash
                    || symbol_interfaces_changed(&before.symbols, &after.symbols))
                .then_some(file.clone())
            })
            .collect::<BTreeSet<_>>();
        if previous
            .keys()
            .any(|file| !self.persisted_files.contains_key(file))
        {
            self.resolutions.clear();
            return;
        }

        let mut invalidated_symbols = previous
            .iter()
            .flat_map(|(file, before)| {
                let after = &self.persisted_files[file];
                before.symbols.iter().filter_map(|(symbol, hashes)| {
                    after
                        .symbols
                        .get(symbol)
                        .is_some_and(|current| current.body != hashes.body)
                        .then_some(symbol.clone())
                })
            })
            .collect::<BTreeSet<_>>();
        let affected = if changed_modules.is_empty() {
            BTreeSet::new()
        } else {
            affected_modules(&previous, &changed_modules)
                .into_iter()
                .chain(affected_modules(&self.persisted_files, &changed_modules))
                .collect()
        };
        invalidated_symbols.extend(affected.iter().flat_map(|file| {
            previous
                .get(file)
                .into_iter()
                .chain(self.persisted_files.get(file))
                .flat_map(|analysis| analysis.symbols.keys().cloned())
        }));
        self.resolutions
            .retain(|key, _| !invalidated_symbols.contains(&key.symbol));
    }

    fn update(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        materialized: &MaterializedWorkspace,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut change = ChangeWithProcMacros::default();
        let mut changed = false;
        for repository in &snapshot.repositories {
            let Some(base) = materialized.repository(&repository.state.repository.identity) else {
                continue;
            };
            for input in repository
                .inputs
                .iter()
                .filter(|input| is_rust_source(&input.path))
            {
                let path =
                    VfsPath::new_real_path(base.join(&input.path).to_string_lossy().into_owned());
                let Some((file_id, _)) = self.vfs.file_id(&path) else {
                    continue;
                };
                if self.sources.get(&file_id) == Some(&input.content) {
                    continue;
                }
                let source = std::str::from_utf8(&input.content)?.to_owned();
                change.change_file(file_id, Some(source));
                self.sources.insert(file_id, Arc::clone(&input.content));
                self.dirty.insert(file_id);
                changed = true;
            }
        }
        if changed {
            self.host.apply_change(change);
        }
        Ok(())
    }
}

pub async fn serve(socket: &Path, cache_dir: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket)?;
    let analysis_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .thread_name(|index| format!("beholder-rust-{index}"))
            .stack_size(16 * 1024 * 1024)
            .build()?,
    );
    let cache_writer = CacheWriter::new()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    }
    tonic::transport::Server::builder()
        .add_service(
            AnalyzerWorkerServer::new(RustWorker {
                analysis_pool,
                compiler: Arc::new(Mutex::new(None)),
                cache_dir: Arc::new(cache_dir),
                cache_writer,
            })
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES),
        )
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

fn enrich_semantics(
    snapshot: &WorkspaceSnapshot,
    target_repository: &str,
    contribution: &mut AnalyzerContribution,
    compiler: &Mutex<Option<CompilerWorkspace>>,
    cache_dir: &Path,
    cache_writer: &CacheWriter,
) {
    let Some(repository) = snapshot
        .repositories
        .iter()
        .find(|repository| repository.state.repository.identity == target_repository)
    else {
        return;
    };
    if !repository.inputs.iter().any(|input| {
        input
            .path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml")
    }) {
        return;
    }
    let mut enriched = contribution.clone();
    let result = (|| {
        validate_immutable_rust_inputs(snapshot)?;
        let shape = CompilerWorkspaceShape::new(snapshot, target_repository);
        let mut cached = compiler
            .lock()
            .map_err(|_| "Rust compiler workspace lock poisoned")?;
        let reused = cached
            .as_ref()
            .is_some_and(|workspace| workspace.shape == shape);
        if reused {
            cached
                .as_mut()
                .expect("matching compiler workspace exists")
                .update(snapshot)?;
        } else {
            *cached = Some(CompilerWorkspace::new(
                snapshot,
                target_repository,
                cache_dir,
            )?);
        }
        tracing::info!(
            reused,
            target_repository,
            "Rust compiler workspace prepared"
        );
        let workspace = cached
            .as_mut()
            .expect("Rust compiler workspace was prepared");
        let CompilerWorkspace {
            materialized,
            cargo,
            ..
        } = workspace;
        for compiler in cargo.values_mut() {
            enrich_repository(snapshot, materialized, repository, compiler, &mut enriched)?;
        }
        materialized.verify(snapshot)?;
        if let Some(request) = workspace.persist_request() {
            cache_writer.schedule(request);
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })();
    if let Err(error) = result {
        contribution.diagnostics.push((
            target_repository.into(),
            AnalysisDiagnostic {
                code: "rust.semantic_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: PathBuf::from("Cargo.toml"),
                line: None,
                detail: Some(error.to_string()),
            },
        ));
    } else {
        *contribution = enriched;
    }
}

fn cargo_roots(
    repository: &beholder_indexing::RepositorySnapshot,
    materialized_base: &Path,
) -> Vec<PathBuf> {
    let manifest_dirs = repository
        .inputs
        .iter()
        .filter(|input| {
            input
                .path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        })
        .filter_map(|input| input.path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    manifest_dirs
        .iter()
        .filter(|directory| {
            !directory
                .ancestors()
                .skip(1)
                .any(|ancestor| manifest_dirs.contains(ancestor))
        })
        .map(|directory| materialized_base.join(directory))
        .collect()
}

fn retain_semantic_enrichment(contribution: &mut AnalyzerContribution, target_repository: &str) {
    let target_prefix = format!("repo://{target_repository}/");
    contribution.overrides.retain(|override_| {
        override_.provenance == Provenance::Compiler
            && override_.from.as_str().starts_with(&target_prefix)
    });
    contribution
        .active_repositories
        .retain(|repository| repository == target_repository);
    contribution
        .repositories
        .retain(|repository| repository.repository == target_repository);
    contribution
        .diagnostics
        .retain(|(repository, _)| repository == target_repository);
    for repository in &mut contribution.repositories {
        repository.entities.clear();
        repository.grpc_bindings.clear();
        repository.observations.clear();
        repository.diagnostics.retain(|diagnostic| {
            diagnostic.code == "rust.receiver_method_resolution_unavailable"
                || diagnostic.code == "rust.compiler_target_unrepresented"
        });
    }
}

fn enrich_repository(
    snapshot: &WorkspaceSnapshot,
    materialized: &MaterializedWorkspace,
    repository: &beholder_indexing::RepositorySnapshot,
    compiler: &mut CompilerCargo,
    contribution: &mut AnalyzerContribution,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let started = Instant::now();
    let mut definitions = BTreeMap::new();
    let mut call_sites = Vec::new();
    let mut local_files = BTreeSet::new();
    let mut semantic_files = BTreeMap::new();
    let mut changed_files = Vec::new();
    for snapshot_repository in &snapshot.repositories {
        let Some(materialized_base) =
            materialized.repository(&snapshot_repository.state.repository.identity)
        else {
            continue;
        };
        for input in snapshot_repository.inputs.iter().filter(|input| {
            input
                .path
                .extension()
                .is_some_and(|extension| extension == "rs")
        }) {
            let absolute = materialized_base.join(&input.path);
            let semantic_file = format!(
                "{}/{}",
                snapshot_repository.state.repository.identity,
                input.path.display()
            );
            let path = VfsPath::new_real_path(absolute.to_string_lossy().into_owned());
            let Some((file_id, _)) = compiler.vfs.file_id(&path) else {
                continue;
            };
            semantic_files.insert(file_id, semantic_file.clone());
            if compiler.dirty.contains(&file_id) || !compiler.extractions.contains_key(&file_id) {
                #[cfg(test)]
                COMPILER_EXTRACTIONS.fetch_add(1, Ordering::Relaxed);
                let source = std::str::from_utf8(&input.content)?.to_owned();
                let extraction = match analyze(&source) {
                    Ok(syntax) => {
                        let source_id = source_entity_id(
                            &snapshot_repository.state.repository.identity,
                            &input.path,
                        );
                        let mut file = CompilerFileAnalysis::default();
                        for function in syntax.functions() {
                            let function_id = format!("{source_id}/{}", function.qualified_name());
                            file.symbols.insert(
                                function_id.clone(),
                                SymbolHashes {
                                    interface: function.interface_hash(),
                                    body: function.body_hash(),
                                },
                            );
                            file.definitions
                                .push((text_size(function.name_offset())?, function_id.clone()));
                            if snapshot_repository.state.repository == repository.state.repository {
                                for (ordinal, call) in function.calls().enumerate() {
                                    let unresolved = if call.receiver_method() {
                                        format!("rust-method://{}", call.name())
                                    } else {
                                        format!("rust-call://{}", call.name())
                                    };
                                    file.call_sites.push(CallSite {
                                        file_id,
                                        from: function_id.clone(),
                                        key: ResolutionKey {
                                            symbol: function_id.clone(),
                                            ordinal: u32::try_from(ordinal)?,
                                            unresolved: unresolved.clone(),
                                        },
                                        unresolved,
                                        evidence: format!(
                                            "{}:{}",
                                            input.path.display(),
                                            line(&source, call.offset())
                                        ),
                                        offset: call.offset(),
                                    });
                                }
                            }
                        }
                        file.module_hash = syntax.module_hash();
                        file.module_reference_offsets = syntax
                            .module_reference_offsets()
                            .map(text_size)
                            .collect::<Result<_, _>>()?;
                        Some(file)
                    }
                    Err(error) if error.downcast_ref::<UnsafeTreeRecovery>().is_some() => None,
                    Err(error) => return Err(error),
                };
                changed_files.push((
                    semantic_file,
                    file_id,
                    Sha256::digest(source.as_bytes()).into(),
                ));
                compiler.extractions.insert(file_id, extraction);
                compiler.dirty.remove(&file_id);
            }
            let Some(extraction) = compiler.extractions.get(&file_id).and_then(Option::as_ref)
            else {
                continue;
            };
            definitions.extend(
                extraction
                    .definitions
                    .iter()
                    .map(|(offset, entity)| ((file_id, *offset), entity.clone())),
            );
            if snapshot_repository.state.repository == repository.state.repository {
                local_files.insert(file_id);
                call_sites.extend(extraction.call_sites.iter().cloned());
            }
        }
    }
    let analysis = compiler.host.analysis();
    let config = GotoDefinitionConfig {
        ra_fixture: RaFixtureConfig {
            disable_ra_fixture: true,
            ..RaFixtureConfig::default()
        },
    };
    let changed_analyses = changed_files
        .into_iter()
        .map(|(semantic_file, file_id, source_hash)| {
            let Some(extraction) = compiler.extractions.get(&file_id).and_then(Option::as_ref)
            else {
                return Ok((
                    semantic_file,
                    PersistedFileAnalysis {
                        analyzable: false,
                        module_hash: source_hash,
                        symbols: BTreeMap::new(),
                        dependencies: BTreeSet::new(),
                    },
                ));
            };
            Ok((
                semantic_file,
                PersistedFileAnalysis {
                    analyzable: true,
                    module_hash: extraction.module_hash,
                    symbols: extraction.symbols.clone(),
                    dependencies: resolve_module_dependencies(
                        &analysis,
                        &config,
                        file_id,
                        &extraction.module_reference_offsets,
                        &semantic_files,
                    )?,
                },
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error + Send + Sync>>>()?;
    compiler.update_file_analyses(changed_analyses);
    let repository_contribution = contribution
        .repositories
        .iter_mut()
        .find(|facts| facts.repository == repository.state.repository.identity)
        .ok_or("Rust contribution omitted an active repository")?;
    let candidates = repository_contribution
        .observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && (observation.to.as_str().starts_with("rust-call://")
                    || observation.to.as_str().starts_with("rust-method://")
                    || observation.provenance == Provenance::UniqueNameHeuristic)
        })
        .map(|observation| {
            (
                observation.from.to_string(),
                observation.evidence.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    call_sites.retain(|call| candidates.contains(&(call.from.clone(), call.evidence.clone())));
    let mut compiler_diagnostics = Vec::new();
    let call_site_count = call_sites.len();
    let extraction_elapsed = started.elapsed();
    let mut goto_elapsed = Duration::ZERO;
    let mut resolution_hits = 0usize;
    let mut resolution_misses = 0usize;
    for call in call_sites {
        let resolution = if let Some(resolution) = compiler.resolutions.get(&call.key) {
            resolution_hits += 1;
            resolution.clone()
        } else {
            resolution_misses += 1;
            #[cfg(test)]
            COMPILER_RESOLUTIONS.fetch_add(1, Ordering::Relaxed);
            let goto_started = Instant::now();
            let resolution = resolve_call(&analysis, &config, &call, &definitions, &local_files)?;
            goto_elapsed += goto_started.elapsed();
            compiler
                .resolutions
                .insert(call.key.clone(), resolution.clone());
            compiler.cache_dirty = true;
            resolution
        };
        let target = match resolution {
            CompilerResolution::Resolved(target) => target,
            CompilerResolution::Unrepresented => {
                let (path, _) = evidence_location(&call.evidence);
                compiler_diagnostics.push(AnalysisDiagnostic {
                    code: "rust.compiler_target_unrepresented".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path,
                    line: None,
                    detail: Some(format!(
                        "compiler resolved {} to a local definition absent from syntax facts",
                        call.unresolved
                    )),
                });
                continue;
            }
            CompilerResolution::Unresolved => continue,
        };
        let Some(observation) =
            repository_contribution
                .observations
                .iter_mut()
                .find(|observation| {
                    observation.from.as_str() == call.from
                        && observation.relation
                            == SemanticRelation::Dependency(DependencyRelation::Calls)
                        && observation.evidence.as_str() == call.evidence
                        && (observation.to.as_str() == call.unresolved
                            || observation.provenance == Provenance::UniqueNameHeuristic)
                })
        else {
            continue;
        };
        if observation.to.as_str() == target {
            contribution.overrides.retain(|override_| {
                override_.from != observation.from
                    || override_.relation != DependencyRelation::Calls
                    || override_.unresolved_to.as_str() != call.unresolved
            });
            contribution.overrides.push(DependencyOverride {
                from: observation.from.clone(),
                relation: DependencyRelation::Calls,
                unresolved_to: call.unresolved.clone().into(),
                resolved_to: target.clone().into(),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Exact,
                provenance: Provenance::Compiler,
            });
            observation.confidence = Confidence::Exact;
            observation.provenance = Provenance::Compiler;
            continue;
        }
        contribution.overrides.retain(|override_| {
            override_.from != observation.from
                || override_.relation != DependencyRelation::Calls
                || override_.evidence != observation.evidence
        });
        contribution.overrides.push(DependencyOverride {
            from: observation.from.clone(),
            relation: DependencyRelation::Calls,
            unresolved_to: call.unresolved.into(),
            resolved_to: target.clone().into(),
            evidence: observation.evidence.clone(),
            confidence: Confidence::Exact,
            provenance: Provenance::Compiler,
        });
        observation.to = target.into();
        observation.confidence = Confidence::Exact;
        observation.provenance = Provenance::Compiler;
    }
    repository_contribution
        .diagnostics
        .retain(|diagnostic| diagnostic.code != "rust.receiver_method_resolution_unavailable");
    let mut unresolved_by_path = BTreeMap::<PathBuf, usize>::new();
    for observation in repository_contribution
        .observations
        .iter()
        .filter(|observation| observation.to.as_str().starts_with("rust-method://"))
    {
        let (path, _) = evidence_location(observation.evidence.as_str());
        unresolved_by_path
            .entry(path)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }
    for (path, count) in unresolved_by_path {
        repository_contribution
            .diagnostics
            .push(AnalysisDiagnostic {
                code: "rust.receiver_method_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path,
                line: None,
                detail: Some(format!(
                    "{count} receiver method calls remain unresolved after compiler analysis"
                )),
            });
    }
    repository_contribution
        .diagnostics
        .extend(compiler_diagnostics);
    tracing::info!(
        call_site_count,
        resolution_hits,
        resolution_misses,
        extraction_ms = extraction_elapsed.as_secs_f64() * 1_000.0,
        goto_definition_ms = goto_elapsed.as_secs_f64() * 1_000.0,
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "Rust compiler repository enrichment completed"
    );
    Ok(())
}

fn resolve_call(
    analysis: &Analysis,
    config: &GotoDefinitionConfig,
    call: &CallSite,
    definitions: &BTreeMap<(FileId, TextSize), String>,
    local_files: &BTreeSet<FileId>,
) -> Result<CompilerResolution, Box<dyn Error + Send + Sync>> {
    let Some(targets) = analysis.goto_definition(
        FilePosition {
            file_id: call.file_id,
            offset: text_size(call.offset)?,
        },
        config,
    )?
    else {
        return Ok(CompilerResolution::Unresolved);
    };
    let mut local = targets.info.iter().filter_map(|target| {
        definitions
            .get(&(target.file_id, target.focus_or_full_range().start()))
            .or_else(|| {
                definitions.iter().find_map(|((file_id, offset), entity)| {
                    (*file_id == target.file_id && target.full_range.contains(*offset))
                        .then_some(entity)
                })
            })
            .cloned()
    });
    let Some(target) = local.next() else {
        return Ok(
            if targets
                .info
                .iter()
                .any(|target| local_files.contains(&target.file_id))
            {
                CompilerResolution::Unrepresented
            } else {
                CompilerResolution::Unresolved
            },
        );
    };
    Ok(if local.next().is_some() {
        CompilerResolution::Unresolved
    } else {
        CompilerResolution::Resolved(target)
    })
}

fn resolve_module_dependencies(
    analysis: &Analysis,
    config: &GotoDefinitionConfig,
    file_id: FileId,
    references: &[TextSize],
    semantic_files: &BTreeMap<FileId, String>,
) -> Result<BTreeSet<String>, Box<dyn Error + Send + Sync>> {
    let mut dependencies = BTreeSet::new();
    for offset in references {
        let Some(targets) = analysis.goto_definition(
            FilePosition {
                file_id,
                offset: *offset,
            },
            config,
        )?
        else {
            continue;
        };
        dependencies.extend(
            targets
                .info
                .iter()
                .filter_map(|target| semantic_files.get(&target.file_id))
                .cloned(),
        );
    }
    if let Some(file) = semantic_files.get(&file_id) {
        dependencies.remove(file);
    }
    Ok(dependencies)
}

fn cargo_features(
    selected: Option<&str>,
    all_features: bool,
    no_default_features: bool,
) -> CargoFeatures {
    if all_features {
        return CargoFeatures::All;
    }
    CargoFeatures::Selected {
        features: selected
            .into_iter()
            .flat_map(|features| features.split(','))
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(str::to_owned)
            .collect(),
        no_default_features,
    }
}

fn environment_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

#[derive(Default)]
struct CompilerFileAnalysis {
    module_hash: [u8; 32],
    symbols: BTreeMap<String, SymbolHashes>,
    module_reference_offsets: Vec<TextSize>,
    definitions: Vec<(TextSize, String)>,
    call_sites: Vec<CallSite>,
}

#[derive(Clone)]
struct CallSite {
    file_id: FileId,
    from: String,
    key: ResolutionKey,
    unresolved: String,
    evidence: String,
    offset: usize,
}

fn line(source: &str, offset: usize) -> usize {
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn evidence_location(evidence: &str) -> (PathBuf, Option<u32>) {
    if let Some((path, line)) = evidence.rsplit_once(':')
        && let Ok(line) = line.parse()
    {
        return (PathBuf::from(path), Some(line));
    }
    (PathBuf::from(evidence), None)
}

fn text_size(offset: usize) -> Result<TextSize, Box<dyn Error + Send + Sync>> {
    Ok(TextSize::from(u32::try_from(offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_adapters_treesitter_rust::RustAnalyzer;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{
        EnrichmentSnapshot, InputKind, RepositoryInput, RepositorySnapshot, SemanticSnapshot,
        WorkspaceAnalyzer,
    };
    use beholder_protocol::{
        analyze_requests, contribution_from_events,
        worker_v1::{AnalysisPhase, analyze_event, analyzer_worker_client::AnalyzerWorkerClient},
    };
    use std::{
        fs,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[tokio::test]
    async fn streams_compiler_resolved_snapshot_calls() {
        let base = std::env::temp_dir().join(format!(
            "beholder-rust-worker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(base.join("src")).unwrap();
        fs::create_dir_all(base.join("dependency/src")).unwrap();
        fs::create_dir_all(base.join(".cargo")).unwrap();
        let manifest = "[package]\nname = \"worker-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [dependencies]\ncontext = { path = \"dependency\" }\n";
        let dependency_manifest =
            "[package]\nname = \"context\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        let lockfile = "version = 4\n\n[[package]]\nname = \"context\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"worker-test\"\nversion = \"0.1.0\"\ndependencies = [\n \"context\",\n]\n";
        let dependency_source = "pub fn external() {}\n";
        let cargo_config = "[build]\ntarget-dir = \"target\"\n";
        let source = r#"
mod broken;
mod inner;
mod unrelated;
mod api { pub use crate::inner::renamed; }
use api::renamed as call_me;
use context::external;
trait Run { fn run(&self); }
struct Thing;
impl Run for Thing { fn run(&self) {} }
impl Thing { fn inherent(&self) {} }
fn generic<T: Run>(value: &T) { value.run(); }
macro_rules! generate { () => { fn generated() {} }; }
generate!();
fn caller() {
    call_me();
    generic(&Thing);
    let thing = Thing;
    thing.inherent();
    let boxed = Box::new(Thing);
    boxed.inherent();
    generated();
    external();
}
"#;
        let inner = "pub fn renamed() {}\n";
        let unrelated = "trait Other { fn other(&self); } struct Value; impl Other for Value { fn other(&self) {} } pub fn untouched() { Value.other(); }\n";
        let broken = "fn broken( {\n";
        fs::write(base.join("Cargo.toml"), "not the snapshot manifest").unwrap();
        fs::write(base.join("Cargo.lock"), "not the snapshot lockfile").unwrap();
        fs::write(
            base.join(".cargo/config.toml"),
            "not snapshot configuration",
        )
        .unwrap();
        fs::write(
            base.join("src/lib.rs"),
            "mod inner; pub fn stale_disk_source() {}",
        )
        .unwrap();
        fs::write(base.join("src/inner.rs"), "pub fn stale_inner() {}").unwrap();
        fs::write(base.join("src/unrelated.rs"), "pub fn stale_unrelated() {}").unwrap();
        fs::write(base.join("src/broken.rs"), "pub fn stale_broken() {}").unwrap();
        fs::write(base.join("dependency/Cargo.toml"), dependency_manifest).unwrap();
        fs::write(
            base.join("dependency/src/lib.rs"),
            "pub fn stale_external() {}",
        )
        .unwrap();
        let mut snapshot = EnrichmentSnapshot {
            target_repository: "example/repo".into(),
            workspace: WorkspaceSnapshot {
                name: "test".into(),
                repositories: vec![
                    RepositorySnapshot {
                        base: base.clone(),
                        state: RepositoryState {
                            repository: LogicalRepository {
                                identity: "example/repo".into(),
                            },
                            head: None,
                            fingerprint: "state".into(),
                        },
                        inputs: vec![
                            RepositoryInput {
                                path: "Cargo.toml".into(),
                                content: Arc::from(manifest.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "Cargo.lock".into(),
                                content: Arc::from(lockfile.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: ".cargo/config.toml".into(),
                                content: Arc::from(cargo_config.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/lib.rs".into(),
                                content: Arc::from(source.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/inner.rs".into(),
                                content: Arc::from(inner.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/unrelated.rs".into(),
                                content: Arc::from(unrelated.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/broken.rs".into(),
                                content: Arc::from(broken.as_bytes()),
                                kind: InputKind::Source,
                            },
                        ],
                    },
                    RepositorySnapshot {
                        base: base.join("dependency"),
                        state: RepositoryState {
                            repository: LogicalRepository {
                                identity: "example/context".into(),
                            },
                            head: None,
                            fingerprint: "context-state".into(),
                        },
                        inputs: vec![
                            RepositoryInput {
                                path: "Cargo.toml".into(),
                                content: Arc::from(dependency_manifest.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/lib.rs".into(),
                                content: Arc::from(dependency_source.as_bytes()),
                                kind: InputKind::Source,
                            },
                        ],
                    },
                ],
            },
            baseline: Default::default(),
        };
        let baseline = RustAnalyzer::new(base.join("syntax-cache"))
            .analyze(&snapshot.workspace)
            .unwrap()
            .repositories
            .into_iter()
            .find(|repository| repository.repository == snapshot.target_repository)
            .unwrap();
        snapshot.baseline = SemanticSnapshot {
            entities: baseline.entities,
            observations: baseline.observations,
        };
        let socket = PathBuf::from(format!(
            "/tmp/beholder-worker-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = base.join("cache");
        let mut server = tokio::spawn({
            let socket = socket.clone();
            let cache = cache.clone();
            async move { serve(&socket, cache).await }
        });
        let endpoint = format!("unix:{}", socket.display());
        let mut client = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if server.is_finished() {
                    panic!(
                        "Rust worker server stopped before accepting: {:?}",
                        (&mut server).await
                    );
                }
                match AnalyzerWorkerClient::connect(endpoint.clone()).await {
                    Ok(client) => break client,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .unwrap();
        let resolutions_before_initial = COMPILER_RESOLUTIONS.load(Ordering::Relaxed);
        let mut updated_snapshot = snapshot.clone();
        let mut stream = client
            .analyze(tokio_stream::iter(analyze_requests(snapshot).unwrap()))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match &event.event {
                    Some(analyze_event::Event::Progress(progress)) => {
                        AnalysisPhase::try_from(progress.phase).ok()
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [AnalysisPhase::ReceivingSnapshot, AnalysisPhase::Analyzing]
        );
        let contribution = contribution_from_events(events).unwrap();

        let overrides = &contribution.overrides;
        assert!(overrides.iter().any(|override_| {
            override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                && override_.resolved_to.as_str() == "repo://example/repo/rust/inner/renamed"
                && override_.provenance == Provenance::Compiler
                && override_.confidence == Confidence::Exact
        }));
        assert!(overrides.iter().any(|override_| {
            override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                && override_.resolved_to.as_str() == "repo://example/context/rust/lib/external"
                && override_.provenance == Provenance::Compiler
        }));
        assert!(
            overrides.iter().any(|override_| {
                override_.from.as_str() == "repo://example/repo/rust/lib/generic"
                    && override_.resolved_to.as_str() == "repo://example/repo/rust/lib/run"
                    && override_.provenance == Provenance::Compiler
                    && override_.confidence == Confidence::Exact
            }),
            "generic overrides: {:#?}",
            overrides
                .iter()
                .filter(|override_| override_.from.as_str().ends_with("/generic"))
                .collect::<Vec<_>>()
        );
        assert!(
            overrides.iter().any(|override_| {
                override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                    && override_.resolved_to.as_str()
                        == "repo://example/repo/rust/lib/impl/Thing/inherent"
                    && override_.provenance == Provenance::Compiler
                    && override_.confidence == Confidence::Exact
            }),
            "caller overrides: {:#?}\ndiagnostics: {:#?}",
            overrides
                .iter()
                .filter(|override_| override_.from.as_str().ends_with("/caller"))
                .collect::<Vec<_>>(),
            contribution.repositories[0].diagnostics
        );
        assert!(contribution.repositories[0].observations.is_empty());
        assert!(
            contribution.diagnostics.iter().all(|(_, diagnostic)| {
                diagnostic.code != "rust.semantic_resolution_unavailable"
            })
        );
        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic.code == "rust.compiler_target_unrepresented"
                        && diagnostic.path == Path::new("src/lib.rs")
                        && diagnostic.line.is_none()
                })
        );
        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic.code == "rust.receiver_method_resolution_unavailable"
                        && diagnostic.path == Path::new("src/lib.rs")
                        && diagnostic.line.is_none()
                })
        );
        let compiler_loads = COMPILER_LOADS.load(Ordering::Relaxed);
        let compiler_extractions = COMPILER_EXTRACTIONS.load(Ordering::Relaxed);
        let compiler_resolutions = COMPILER_RESOLUTIONS.load(Ordering::Relaxed);
        let source = format!("{source}\n// comment-only change\n");
        updated_snapshot.workspace.repositories[0]
            .inputs
            .iter_mut()
            .find(|input| input.path == Path::new("src/lib.rs"))
            .unwrap()
            .content = Arc::from(source.as_bytes());
        let mut stream = client
            .analyze(tokio_stream::iter(
                analyze_requests(updated_snapshot.clone()).unwrap(),
            ))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        let updated = contribution_from_events(events).unwrap();
        assert_eq!(updated.overrides, contribution.overrides);
        assert_eq!(COMPILER_LOADS.load(Ordering::Relaxed), compiler_loads);
        assert_eq!(
            COMPILER_RESOLUTIONS.load(Ordering::Relaxed),
            compiler_resolutions
        );
        assert_eq!(
            COMPILER_EXTRACTIONS.load(Ordering::Relaxed),
            compiler_extractions + 1
        );

        let body_source =
            source.replace("    external();", "    let _marker = 1;\n    external();");
        updated_snapshot.workspace.repositories[0]
            .inputs
            .iter_mut()
            .find(|input| input.path == Path::new("src/lib.rs"))
            .unwrap()
            .content = Arc::from(body_source.as_bytes());
        let baseline = RustAnalyzer::new(base.join("body-syntax-cache"))
            .analyze(&updated_snapshot.workspace)
            .unwrap()
            .repositories
            .into_iter()
            .find(|repository| repository.repository == updated_snapshot.target_repository)
            .unwrap();
        updated_snapshot.baseline = SemanticSnapshot {
            entities: baseline.entities,
            observations: baseline.observations,
        };
        let resolutions_before_body = COMPILER_RESOLUTIONS.load(Ordering::Relaxed);
        let mut stream = client
            .analyze(tokio_stream::iter(
                analyze_requests(updated_snapshot.clone()).unwrap(),
            ))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        contribution_from_events(events).unwrap();
        let body_queries = COMPILER_RESOLUTIONS.load(Ordering::Relaxed) - resolutions_before_body;
        let initial_queries = compiler_resolutions - resolutions_before_initial;
        assert!(body_queries > 0);
        assert!(body_queries < initial_queries);

        let interface_inner = "pub fn renamed(_value: u32) {}\n";
        updated_snapshot.workspace.repositories[0]
            .inputs
            .iter_mut()
            .find(|input| input.path == Path::new("src/inner.rs"))
            .unwrap()
            .content = Arc::from(interface_inner.as_bytes());
        let baseline = RustAnalyzer::new(base.join("interface-syntax-cache"))
            .analyze(&updated_snapshot.workspace)
            .unwrap()
            .repositories
            .into_iter()
            .find(|repository| repository.repository == updated_snapshot.target_repository)
            .unwrap();
        updated_snapshot.baseline = SemanticSnapshot {
            entities: baseline.entities,
            observations: baseline.observations,
        };
        let resolutions_before_interface = COMPILER_RESOLUTIONS.load(Ordering::Relaxed);
        let mut stream = client
            .analyze(tokio_stream::iter(
                analyze_requests(updated_snapshot.clone()).unwrap(),
            ))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        let interface_updated = contribution_from_events(events).unwrap();
        let interface_queries =
            COMPILER_RESOLUTIONS.load(Ordering::Relaxed) - resolutions_before_interface;
        assert!(
            interface_queries > body_queries,
            "interface={interface_queries}, body={body_queries}, initial={initial_queries}"
        );
        assert!(
            interface_queries < initial_queries,
            "interface={interface_queries}, body={body_queries}, initial={initial_queries}"
        );

        let persisted = cache.join("rust-compiler-resolution-v1");
        let expected_body = analyze(&body_source)
            .unwrap()
            .functions()
            .find(|function| function.qualified_name() == "caller")
            .unwrap()
            .body_hash();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = fs::read_dir(&persisted).ok().and_then(|files| {
                    files.filter_map(Result::ok).find_map(|file| {
                        let cache = load_resolution_cache(&file.path());
                        cache.cargo.values().find_map(|cargo| {
                            cargo.files.values().find_map(|file| {
                                file.symbols
                                    .iter()
                                    .find(|(symbol, _)| symbol.ends_with("/caller"))
                                    .map(|(_, hashes)| hashes.body)
                            })
                        })
                    })
                });
                if current == Some(expected_body) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.abort();
        let _ = server.await;
        let _ = fs::remove_file(&socket);

        let mut restarted_server = tokio::spawn({
            let socket = socket.clone();
            let cache = cache.clone();
            async move { serve(&socket, cache).await }
        });
        let endpoint = format!("unix:{}", socket.display());
        let mut restarted = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if restarted_server.is_finished() {
                    panic!(
                        "restarted Rust worker stopped before accepting: {:?}",
                        (&mut restarted_server).await
                    );
                }
                match AnalyzerWorkerClient::connect(endpoint.clone()).await {
                    Ok(client) => break client,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .unwrap();
        let resolutions_before_restart = COMPILER_RESOLUTIONS.load(Ordering::Relaxed);
        let mut stream = restarted
            .analyze(tokio_stream::iter(
                analyze_requests(updated_snapshot).unwrap(),
            ))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        let restarted = contribution_from_events(events).unwrap();
        assert_eq!(restarted.overrides, interface_updated.overrides);
        assert_eq!(
            COMPILER_RESOLUTIONS.load(Ordering::Relaxed),
            resolutions_before_restart
        );
        assert_eq!(COMPILER_LOADS.load(Ordering::Relaxed), compiler_loads + 1);
        restarted_server.abort();
        let _ = restarted_server.await;
        let _ = fs::remove_file(socket);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn selects_topmost_cargo_roots() {
        let repository = RepositorySnapshot {
            base: PathBuf::from("repo"),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repo".into(),
                },
                head: None,
                fingerprint: "state".into(),
            },
            inputs: [
                "services/one/Cargo.toml",
                "services/one/crates/member/Cargo.toml",
                "tools/two/Cargo.toml",
            ]
            .into_iter()
            .map(|path| RepositoryInput {
                path: path.into(),
                content: Arc::from(&b""[..]),
                kind: InputKind::Source,
            })
            .collect(),
        };

        assert_eq!(
            cargo_roots(&repository, Path::new("repo")),
            [
                PathBuf::from("repo/services/one"),
                PathBuf::from("repo/tools/two")
            ]
        );
    }

    #[test]
    fn resolution_cache_round_trips_and_corruption_is_a_miss() {
        let directory = std::env::temp_dir().join(format!(
            "beholder-rust-resolution-cache-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("cache.json");
        let key = ResolutionKey {
            symbol: "repo://example/repo/rust/lib/run".into(),
            ordinal: 0,
            unresolved: "rust-call://work".into(),
        };
        let cache = PersistedWorkspaceCache {
            version: RESOLUTION_CACHE_VERSION,
            cargo: BTreeMap::from([(
                String::new(),
                PersistedCargoCache {
                    files: BTreeMap::from([(
                        "example/repo/src/lib.rs".into(),
                        PersistedFileAnalysis {
                            analyzable: true,
                            module_hash: [1; 32],
                            symbols: BTreeMap::from([(
                                key.symbol.clone(),
                                SymbolHashes {
                                    interface: [2; 32],
                                    body: [3; 32],
                                },
                            )]),
                            dependencies: BTreeSet::new(),
                        },
                    )]),
                    resolutions: vec![PersistedResolution {
                        key,
                        resolution: CompilerResolution::Resolved(
                            "repo://example/repo/rust/lib/work".into(),
                        ),
                    }],
                },
            )]),
        };
        persist_resolution_cache(&path, &cache).unwrap();
        assert_eq!(load_resolution_cache(&path), cache);

        fs::write(&path, b"not json").unwrap();
        assert_eq!(
            load_resolution_cache(&path),
            PersistedWorkspaceCache::default()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn module_invalidation_crosses_cycles_and_reverse_dependents_only() {
        let file = |dependencies: &[&str]| PersistedFileAnalysis {
            analyzable: true,
            module_hash: [0; 32],
            symbols: BTreeMap::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
        };
        let files = BTreeMap::from([
            ("a".into(), file(&["b"])),
            ("b".into(), file(&["a"])),
            ("c".into(), file(&["a"])),
            ("d".into(), file(&["c"])),
            ("unrelated".into(), file(&[])),
        ]);

        assert_eq!(
            affected_modules(&files, &BTreeSet::from(["b".into()])),
            BTreeSet::from(["a".into(), "b".into(), "c".into(), "d".into()])
        );
    }

    #[test]
    fn selects_explicit_cargo_features() {
        assert_eq!(cargo_features(None, true, false), CargoFeatures::All);
        assert_eq!(
            cargo_features(Some("api, serde ,"), false, true),
            CargoFeatures::Selected {
                features: vec!["api".into(), "serde".into()],
                no_default_features: true,
            }
        );
    }
}
