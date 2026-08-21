use crate::workspace_registry::WorkspaceRegistry;
#[cfg(test)]
use beholder_adapters_graphql::GraphqlSource;
#[cfg(test)]
use beholder_adapters_graphql::{FRONTEND_VERSION as GRAPHQL_FRONTEND_VERSION, GraphqlAnalyzer};
use beholder_adapters_mnestic::SemanticStore;
#[cfg(test)]
use beholder_adapters_protobuf::{FRONTEND_VERSION as PROTOBUF_FRONTEND_VERSION, ProtobufAnalyzer};
#[cfg(test)]
use beholder_adapters_protobuf::{SourceCompiler, facts as protobuf_facts};
#[cfg(test)]
use beholder_adapters_treesitter_csharp::{
    CsharpAnalysis, CsharpProject, CsharpSource, UnityPrefab,
    diagnostics_from_analysis as csharp_diagnostics, entities_from_analysis as csharp_entities,
    observations_from_analysis as csharp_observations, parse_project as parse_csharp_project,
    parse_unity_assemblies, resolve_repository_calls as resolve_csharp_repository_calls,
    source_assemblies as csharp_source_assemblies, unity_lifecycle as csharp_unity_lifecycle,
    unity_prefab_dependencies as csharp_unity_prefab_dependencies,
};
#[cfg(test)]
use beholder_adapters_treesitter_csharp::{
    CsharpAnalyzer, FRONTEND_VERSION as CSHARP_FRONTEND_VERSION,
    RESOLVER_VERSION as CSHARP_RESOLVER_VERSION,
};
#[cfg(test)]
use beholder_adapters_treesitter_elixir::{
    ElixirAnalysis, diagnostics_from_analysis as elixir_diagnostics,
    entities_from_analysis as elixir_entities, generated_entities as elixir_generated_entities,
    generated_observations as elixir_generated_observations,
    graphql_resolver_bindings as elixir_graphql_resolver_bindings,
    grpc_bindings as elixir_grpc_bindings, observations_from_analysis as elixir_observations,
    resolve_repository_calls as resolve_elixir_repository_calls, resolve_workspace_modules,
};
#[cfg(test)]
use beholder_adapters_treesitter_elixir::{
    ElixirAnalyzer, FRONTEND_VERSION as ELIXIR_FRONTEND_VERSION,
    RESOLVER_VERSION as ELIXIR_RESOLVER_VERSION,
};
#[cfg(test)]
use beholder_adapters_treesitter_rust::{FRONTEND_VERSION, RESOLVER_VERSION, RustAnalyzer};
#[cfg(test)]
use beholder_adapters_treesitter_rust::{
    RustAnalysis, diagnostics_from_analysis as rust_diagnostics,
    entities_from_analysis as rust_entities, observations_from_analysis,
    resolve_repository_calls as resolve_rust_repository_calls, tonic_bindings,
};
#[cfg(test)]
use beholder_adapters_treesitter_typescript::{
    FRONTEND_VERSION as TYPESCRIPT_FRONTEND_VERSION,
    RESOLVER_VERSION as TYPESCRIPT_RESOLVER_VERSION, TypescriptAnalyzer,
};
#[cfg(test)]
use beholder_adapters_treesitter_typescript::{
    GraphqlFactInput, GraphqlResolverInput, GraphqlResolverSource,
    GrpcBindingInput as TypescriptGrpcBindingInput, SourceLanguage, TypescriptAnalysis,
    TypescriptRepository, collect_graphql_facts as collect_typescript_graphql_facts,
    collect_graphql_resolvers as collect_typescript_graphql_resolvers,
    diagnostics_from_analysis as typescript_diagnostics,
    entities_from_analysis as typescript_entities, grpc_bindings as typescript_grpc_bindings,
    observations_from_analysis as typescript_observations,
    resolve_repository_calls as resolve_typescript_repository_calls,
    resolve_workspace_calls as resolve_typescript_workspace_calls,
    unresolved_call_diagnostics as unresolved_typescript_call_diagnostics,
};
#[cfg(test)]
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, RepositoryFacts, RepositoryState, SourceAnalysisError,
};
use beholder_domain::{
    BeholderError, BeholderErrorCode, BeholderErrorKind, Workspace, WorkspaceView,
};
use beholder_dto::{Freshness, QueryMetadata};
#[cfg(test)]
use beholder_indexing::{
    AnalyzerMetadata, EnrichmentFuture, IndexerBuilder, WorkspaceAnalyzer, WorkspaceEnricher,
};
use beholder_indexing::{CacheStatus as IndexerCacheStatus, Indexer, WorkspaceSnapshot};
use notify::{Event, EventKind};
#[cfg(test)]
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[cfg(test)]
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
};
use tokio::{
    sync::Notify,
    time::{Instant, MissedTickBehavior},
};

#[cfg(test)]
#[path = "cache.rs"]
mod cache;
#[cfg(test)]
#[path = "csharp_analysis.rs"]
mod csharp_analysis;
#[cfg(test)]
#[path = "elixir_analysis.rs"]
mod elixir_analysis;
#[path = "enrichment.rs"]
mod enrichment;
#[path = "pipeline.rs"]
mod pipeline;
#[cfg(test)]
#[path = "rust_analysis.rs"]
mod rust_analysis;
#[path = "sources.rs"]
mod sources;
#[cfg(test)]
#[path = "typescript_analysis.rs"]
mod typescript_analysis;
#[cfg(test)]
use cache::{RepositoryAnalysis, RepositoryAnalysisKey, SourceAnalysisKey};
use enrichment::{EnrichmentJob, EnrichmentRun};
use sources::{RepositoryInventory, repository_inventory};
#[cfg(test)]
use sources::{RepositorySources, decode_csharp_source, repository_sources};

const QUIET_PERIOD: Duration = Duration::from_millis(200);
const MAX_LATENCY: Duration = Duration::from_secs(2);
const RECONCILIATION_PERIOD: Duration = Duration::from_secs(60);
#[cfg(test)]
const CORE_RULE_PACK_VERSION: &str = "5";

#[cfg(test)]
#[derive(Clone, Copy)]
struct AnalysisVersions {
    rust_frontend: &'static str,
    rust_resolver: &'static str,
    elixir_frontend: &'static str,
    elixir_resolver: &'static str,
    csharp_frontend: &'static str,
    csharp_resolver: &'static str,
    typescript_frontend: &'static str,
    typescript_resolver: &'static str,
    protobuf_frontend: &'static str,
    graphql_frontend: &'static str,
    rule_pack: &'static str,
}

#[cfg(test)]
const CURRENT_ANALYSIS_VERSIONS: AnalysisVersions = AnalysisVersions {
    rust_frontend: FRONTEND_VERSION,
    rust_resolver: RESOLVER_VERSION,
    elixir_frontend: ELIXIR_FRONTEND_VERSION,
    elixir_resolver: ELIXIR_RESOLVER_VERSION,
    csharp_frontend: CSHARP_FRONTEND_VERSION,
    csharp_resolver: CSHARP_RESOLVER_VERSION,
    typescript_frontend: TYPESCRIPT_FRONTEND_VERSION,
    typescript_resolver: TYPESCRIPT_RESOLVER_VERSION,
    protobuf_frontend: PROTOBUF_FRONTEND_VERSION,
    graphql_frontend: GRAPHQL_FRONTEND_VERSION,
    rule_pack: CORE_RULE_PACK_VERSION,
};

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct RepositoryLanguages {
    rust: bool,
    elixir: bool,
    csharp: bool,
    typescript: bool,
    protobuf: bool,
    graphql: bool,
}

#[cfg(test)]
impl AnalysisVersions {
    fn repository_key(
        self,
        fingerprint: String,
        languages: RepositoryLanguages,
    ) -> RepositoryAnalysisKey {
        RepositoryAnalysisKey {
            fingerprint,
            rust: languages
                .rust
                .then_some((self.rust_frontend, self.rust_resolver)),
            elixir: languages
                .elixir
                .then_some((self.elixir_frontend, self.elixir_resolver)),
            csharp: languages
                .csharp
                .then_some((self.csharp_frontend, self.csharp_resolver)),
            typescript: languages
                .typescript
                .then_some((self.typescript_frontend, self.typescript_resolver)),
            protobuf: languages.protobuf.then_some(self.protobuf_frontend),
            graphql: languages.graphql.then_some(self.graphql_frontend),
        }
    }

    #[cfg(test)]
    fn workspace_identity(self, repositories: &[RepositorySources]) -> String {
        let key = self.repository_key(
            String::new(),
            RepositoryLanguages {
                rust: repositories.iter().any(|sources| !sources.rust.is_empty()),
                elixir: repositories
                    .iter()
                    .any(|sources| !sources.elixir.is_empty()),
                csharp: repositories.iter().any(|sources| {
                    !sources.csharp.is_empty() || !sources.csharp_projects.is_empty()
                }),
                typescript: repositories
                    .iter()
                    .any(|sources| !sources.typescript.is_empty()),
                protobuf: repositories.iter().any(|sources| {
                    !sources.protobuf.is_empty() || !sources.protobuf_source.is_empty()
                }),
                graphql: repositories
                    .iter()
                    .any(|sources| !sources.graphql.is_empty()),
            },
        );
        format!("{}:core-rules:{}", key.analysis_identity(), self.rule_pack)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

#[cfg(test)]
type Cached<T> = Result<(Arc<T>, CacheStatus), Box<dyn Error>>;

fn erase_error(error: Box<dyn Error + Send + Sync>) -> Box<dyn Error> {
    error
}

fn scheduler_unavailable() -> BeholderError {
    BeholderError::new(
        BeholderErrorKind::Internal,
        BeholderErrorCode::SchedulerUnavailable,
        "index scheduler is unavailable",
    )
}

pub struct IndexScheduler {
    generations: Mutex<BTreeMap<String, u64>>,
    dirty_repositories: Mutex<BTreeMap<String, BTreeMap<String, DirtyRepository>>>,
    active_operation: Mutex<Option<String>>,
    enrichment_jobs: Mutex<BTreeMap<(String, String), EnrichmentJob>>,
    enriching: Mutex<BTreeMap<(String, String), EnrichmentRun>>,
    idle: Condvar,
    changed: Notify,
    enrichment_changed: Notify,
    shutdown: Notify,
    enrichment_shutdown: Notify,
    checkpointing: AtomicBool,
    indexer: Indexer,
    #[cfg(test)]
    cache_dir: PathBuf,
    #[cfg(test)]
    rust_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<RustAnalysis>>>,
    #[cfg(test)]
    elixir_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<ElixirAnalysis>>>,
    #[cfg(test)]
    csharp_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<CsharpAnalysis>>>,
    #[cfg(test)]
    typescript_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<TypescriptAnalysis>>>,
    #[cfg(test)]
    protobuf_compiler: SourceCompiler,
    #[cfg(test)]
    analysis_pool: rayon::ThreadPool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DirtyRepository {
    All,
    Sources(BTreeSet<PathBuf>),
}

struct ActiveOperation<'a> {
    operation: String,
    started: Instant,
    active_operation: &'a Mutex<Option<String>>,
    idle: &'a Condvar,
}

#[derive(Clone, Copy, Default)]
#[cfg(test)]
struct RepositoryAnalysisSources<'a> {
    rust: &'a [(PathBuf, String)],
    elixir: &'a [(PathBuf, String)],
    csharp: &'a [(PathBuf, Vec<u8>)],
    csharp_projects: &'a [(PathBuf, String)],
    unity_prefabs: &'a [UnityPrefab],
    unity_script_metas: &'a [(PathBuf, String, Vec<u8>)],
    unity_prefab_metas: &'a [(PathBuf, String, Vec<u8>)],
    typescript: &'a [(PathBuf, String, SourceLanguage)],
    typescript_manifests: &'a [(PathBuf, String)],
    typescript_configs: &'a [(PathBuf, String)],
    graphql: &'a [(PathBuf, String)],
    descriptors: &'a [(PathBuf, Vec<u8>)],
}

impl Drop for ActiveOperation<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_operation.lock() {
            *active = None;
            self.idle.notify_all();
        }
        tracing::info!(
            operation = self.operation,
            elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0,
            "scheduler operation finished"
        );
    }
}

impl IndexScheduler {
    pub fn with_indexer(indexer: Indexer) -> Self {
        Self {
            generations: Mutex::new(BTreeMap::new()),
            dirty_repositories: Mutex::new(BTreeMap::new()),
            active_operation: Mutex::new(None),
            enrichment_jobs: Mutex::new(BTreeMap::new()),
            enriching: Mutex::new(BTreeMap::new()),
            idle: Condvar::new(),
            changed: Notify::new(),
            enrichment_changed: Notify::new(),
            shutdown: Notify::new(),
            enrichment_shutdown: Notify::new(),
            checkpointing: AtomicBool::new(false),
            indexer,
            #[cfg(test)]
            cache_dir: PathBuf::new(),
            #[cfg(test)]
            rust_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            elixir_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            csharp_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            typescript_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            protobuf_compiler: SourceCompiler::new(PathBuf::new()),
            #[cfg(test)]
            analysis_pool: rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("test indexing pool should start"),
        }
    }

    #[cfg(test)]
    pub fn new(cache_dir: PathBuf) -> Self {
        let workers = std::env::var("BEHOLDER_INDEX_WORKERS")
            .ok()
            .and_then(|workers| workers.parse().ok())
            .filter(|workers| *workers > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));
        Self::with_workers(cache_dir, workers)
    }

    #[cfg(test)]
    fn with_workers(cache_dir: PathBuf, workers: usize) -> Self {
        let analysis_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("beholder-index-{index}"))
            .build()
            .expect("bounded indexing pool should start");
        tracing::info!(workers, "index analysis pool configured");
        #[cfg(test)]
        let protobuf_compiler = SourceCompiler::new(cache_dir.clone());
        let indexer = IndexerBuilder::new(cache_dir.clone(), workers)
            .add_analyzer(RustAnalyzer::new(cache_dir.clone()))
            .add_analyzer(ElixirAnalyzer::new(cache_dir.clone()))
            .add_analyzer(CsharpAnalyzer::new(cache_dir.clone()))
            .add_analyzer(TypescriptAnalyzer::new(cache_dir.clone()))
            .add_analyzer(GraphqlAnalyzer)
            .add_analyzer(ProtobufAnalyzer::new(cache_dir.clone()))
            .build()
            .expect("built-in analyzers should compose");
        Self {
            generations: Mutex::new(BTreeMap::new()),
            dirty_repositories: Mutex::new(BTreeMap::new()),
            active_operation: Mutex::new(None),
            enrichment_jobs: Mutex::new(BTreeMap::new()),
            enriching: Mutex::new(BTreeMap::new()),
            idle: Condvar::new(),
            changed: Notify::new(),
            enrichment_changed: Notify::new(),
            shutdown: Notify::new(),
            enrichment_shutdown: Notify::new(),
            checkpointing: AtomicBool::new(false),
            indexer,
            cache_dir,
            #[cfg(test)]
            rust_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            elixir_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            csharp_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            typescript_cache: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            protobuf_compiler,
            analysis_pool,
        }
    }

    pub fn mark(&self, workspace: &Workspace) {
        if let Ok(mut generations) = self.generations.lock() {
            *generations.entry(workspace.name.clone()).or_default() += 1;
            if let Ok(mut dirty) = self.dirty_repositories.lock() {
                dirty.entry(workspace.name.clone()).or_default().extend(
                    workspace.repositories.iter().map(|repository| {
                        (repository.repository.identity.clone(), DirtyRepository::All)
                    }),
                );
            }
            self.changed.notify_one();
        }
    }

    pub fn query_metadata(
        &self,
        workspace: &str,
        analysis_revision: u64,
        analysis: beholder_dto::AnalysisMetadata,
    ) -> QueryMetadata {
        let stale = self
            .generations
            .lock()
            .is_ok_and(|generations| generations.contains_key(workspace));
        let dirty_repositories = self
            .dirty_repositories
            .lock()
            .ok()
            .and_then(|dirty| {
                dirty
                    .get(workspace)
                    .map(|repositories| repositories.keys().cloned().collect())
            })
            .unwrap_or_default();
        let indexing = self
            .active_operation
            .lock()
            .is_ok_and(|active| active.as_deref() == Some(workspace))
            || self
                .enriching
                .lock()
                .is_ok_and(|active| active.keys().any(|(active, _)| active == workspace))
            || self.enrichment_jobs.lock().is_ok_and(|jobs| {
                jobs.keys()
                    .any(|(queued_workspace, _)| queued_workspace == workspace)
            });
        QueryMetadata {
            revision: analysis_revision,
            view: workspace.into(),
            freshness: Freshness {
                stale,
                indexing,
                dirty_repositories,
            },
            analysis,
        }
    }

    pub fn clear_cache(&self) -> Result<(), Box<dyn Error>> {
        let _active = self.begin("cache clear")?;
        #[cfg(test)]
        self.rust_cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .clear();
        #[cfg(test)]
        self.elixir_cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .clear();
        #[cfg(test)]
        self.csharp_cache
            .lock()
            .map_err(|_| "C# frontend cache lock poisoned")?
            .clear();
        #[cfg(test)]
        self.typescript_cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .clear();
        #[cfg(test)]
        self.protobuf_compiler.clear_memory()?;
        self.indexer.clear_cache().map_err(erase_error)?;
        Ok(())
    }

    #[tracing::instrument(name = "index.workspace", skip(self, store), err, fields(workspace = %workspace.name))]
    pub fn index(
        &self,
        store: &SemanticStore,
        workspace: &Workspace,
    ) -> Result<(usize, bool), Box<dyn Error>> {
        let generation = self
            .generations
            .lock()
            .map_err(|_| "index generations lock poisoned")?
            .get(&workspace.name)
            .copied();
        let result = self.index_active(store, workspace);
        if result.is_ok() {
            self.complete_generation(&workspace.name, generation);
        }
        result
    }

    fn index_active(
        &self,
        store: &SemanticStore,
        workspace: &Workspace,
    ) -> Result<(usize, bool), Box<dyn Error>> {
        let _active = self.begin(&workspace.name)?;
        let dirty = self
            .dirty_repositories
            .lock()
            .map_err(|_| "dirty repository lock poisoned")?
            .get(&workspace.name)
            .cloned();
        index_workspace(self, store, workspace, dirty.as_ref())
    }

    fn begin(&self, operation: &str) -> Result<ActiveOperation<'_>, BeholderError> {
        let mut active = self
            .active_operation
            .lock()
            .map_err(|_| scheduler_unavailable())?;
        while let Some(active_operation) = active.as_deref() {
            tracing::info!(operation, active_operation, "scheduler operation waiting");
            active = self
                .idle
                .wait(active)
                .map_err(|_| scheduler_unavailable())?;
        }
        *active = Some(operation.into());
        tracing::info!(operation, "scheduler operation started");
        Ok(ActiveOperation {
            operation: operation.into(),
            started: Instant::now(),
            active_operation: &self.active_operation,
            idle: &self.idle,
        })
    }

    pub(crate) fn run_exclusive<T>(
        &self,
        operation: &str,
        task: impl FnOnce() -> Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        let _active = self.begin(operation)?;
        task()
    }

    fn complete_generation(&self, workspace: &str, indexed: Option<u64>) {
        let Ok(mut generations) = self.generations.lock() else {
            return;
        };
        if indexed.is_some() && generations.get(workspace).copied() == indexed {
            generations.remove(workspace);
            if let Ok(mut dirty) = self.dirty_repositories.lock() {
                dirty.remove(workspace);
            }
        }
        if generations.contains_key(workspace) {
            self.changed.notify_one();
        }
    }

    pub fn add_event(&self, event: notify::Result<Event>, workspaces: &Mutex<WorkspaceRegistry>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, "filesystem watcher error");
                return;
            }
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let Ok(registry) = workspaces.lock() else {
            return;
        };
        let Ok(mut generations) = self.generations.lock() else {
            return;
        };
        let Ok(mut dirty_repositories) = self.dirty_repositories.lock() else {
            return;
        };
        let mut changed = false;
        for workspace in registry.list() {
            let dirty = workspace
                .repositories
                .iter()
                .filter_map(|repository| {
                    let sources = event
                        .paths
                        .iter()
                        .filter_map(|path| {
                            path.strip_prefix(&repository.base)
                                .ok()
                                .filter(|relative| {
                                    self.indexer.accepts(relative)
                                        || workspace.protobuf_descriptors.iter().any(|descriptor| {
                                            descriptor.repository == repository.repository
                                                && descriptor.path == *path
                                        })
                                })
                                .map(Path::to_path_buf)
                        })
                        .collect::<BTreeSet<_>>();
                    (!sources.is_empty()).then(|| (repository.repository.identity.clone(), sources))
                })
                .collect::<Vec<_>>();
            if !dirty.is_empty() {
                tracing::debug!(workspace = %workspace.name, repositories = dirty.len(), source_units = dirty.iter().map(|(_, sources)| sources.len()).sum::<usize>(), "workspace marked stale");
                *generations.entry(workspace.name.clone()).or_default() += 1;
                let repositories = dirty_repositories.entry(workspace.name).or_default();
                for (repository, sources) in dirty {
                    match repositories.entry(repository) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(DirtyRepository::Sources(sources));
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            if let DirtyRepository::Sources(pending) = entry.get_mut() {
                                pending.extend(sources);
                            }
                        }
                    }
                }
                changed = true;
            }
        }
        if changed {
            self.changed.notify_one();
        }
    }

    pub async fn run(
        self: Arc<Self>,
        store: Arc<SemanticStore>,
        workspaces: Arc<Mutex<WorkspaceRegistry>>,
    ) {
        let enrichment = tokio::spawn(self.clone().run_enrichments(store.clone()));
        self.run_with_reconciliation_period(store, workspaces, RECONCILIATION_PERIOD)
            .await;
        let _ = enrichment.await;
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
        self.enrichment_shutdown.notify_one();
    }

    pub fn schedule_checkpoint(self: &Arc<Self>, store: Arc<SemanticStore>) {
        if self.checkpointing.swap(true, Ordering::AcqRel) {
            return;
        }
        let scheduler = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("beholder-checkpoint".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let result = match scheduler.begin("checkpoint") {
                    Ok(_active) => store.checkpoint(),
                    Err(error) => Err(Box::new(error) as Box<dyn Error>),
                };
                drop(store);
                scheduler.checkpointing.store(false, Ordering::Release);
                match result {
                    Ok(()) => tracing::info!(
                        checkpoint_ms = started.elapsed().as_secs_f64() * 1000.0,
                        "Mnestic checkpoint completed"
                    ),
                    Err(error) => tracing::warn!(%error, "Mnestic checkpoint failed"),
                }
            })
        {
            self.checkpointing.store(false, Ordering::Release);
            tracing::warn!(%error, "Mnestic checkpoint worker failed to start");
        }
    }

    #[cfg(test)]
    pub(crate) fn block_indexing(&self) -> impl Drop + '_ {
        self.begin("test blocker").unwrap()
    }

    async fn run_with_reconciliation_period(
        self: Arc<Self>,
        store: Arc<SemanticStore>,
        workspaces: Arc<Mutex<WorkspaceRegistry>>,
        reconciliation_period: Duration,
    ) {
        let mut reconciliation = tokio::time::interval_at(
            Instant::now() + reconciliation_period,
            reconciliation_period,
        );
        reconciliation.set_missed_tick_behavior(MissedTickBehavior::Skip);
        'run: loop {
            let dirty = tokio::select! {
                _ = self.changed.notified() => true,
                _ = reconciliation.tick() => self.mark_registered(&workspaces),
                _ = self.shutdown.notified() => break 'run,
            };
            if !dirty {
                continue;
            }
            let first_change = Instant::now();
            let mut last_change = first_change;
            loop {
                let quiet = tokio::time::sleep_until(last_change + QUIET_PERIOD);
                let maximum = tokio::time::sleep_until(first_change + MAX_LATENCY);
                tokio::pin!(quiet, maximum);
                tokio::select! {
                    _ = self.changed.notified() => last_change = Instant::now(),
                    _ = &mut quiet => break,
                    _ = &mut maximum => break,
                    _ = self.shutdown.notified() => break 'run,
                }
            }
            let scheduler = self.clone();
            let store = store.clone();
            let checkpoint_store = store.clone();
            let workspaces = workspaces.clone();
            if let Err(error) =
                tokio::task::spawn_blocking(move || scheduler.reindex_dirty(&store, &workspaces))
                    .await
            {
                tracing::error!(%error, "index worker failed");
            } else {
                self.schedule_checkpoint(checkpoint_store);
            }
        }
        while self.checkpointing.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn mark_registered(&self, workspaces: &Mutex<WorkspaceRegistry>) -> bool {
        let Ok(registry) = workspaces.lock() else {
            return false;
        };
        let workspaces = registry.list();
        drop(registry);
        for workspace in &workspaces {
            self.mark(workspace);
        }
        !workspaces.is_empty()
    }

    fn reindex_dirty(&self, store: &SemanticStore, workspaces: &Mutex<WorkspaceRegistry>) {
        let snapshot = match self.generations.lock() {
            Ok(generations) => generations.clone(),
            Err(_) => return,
        };
        let registered = match workspaces.lock() {
            Ok(registry) => snapshot
                .keys()
                .filter_map(|name| registry.get(name).cloned())
                .collect::<Vec<_>>(),
            Err(_) => return,
        };
        for workspace in registered {
            match self.index_active(store, &workspace) {
                Ok(_) => self
                    .complete_generation(&workspace.name, snapshot.get(&workspace.name).copied()),
                Err(error) => {
                    tracing::error!(workspace = %workspace.name, %error, "workspace reindex failed");
                }
            }
        }
    }

    #[cfg(test)]
    fn rust_analysis_versioned(
        &self,
        source: &str,
        frontend_version: &'static str,
    ) -> Cached<RustAnalysis> {
        rust_analysis::analysis_versioned(self, source, frontend_version)
    }

    #[cfg(test)]
    fn elixir_analysis_versioned(
        &self,
        source: &str,
        frontend_version: &'static str,
    ) -> Cached<ElixirAnalysis> {
        elixir_analysis::analysis_versioned(self, source, frontend_version)
    }

    #[cfg(test)]
    fn csharp_analysis_versioned(&self, source: &str) -> Cached<CsharpAnalysis> {
        csharp_analysis::analysis_versioned(self, source)
    }

    #[cfg(test)]
    fn typescript_analysis_versioned(
        &self,
        source: &str,
        language: SourceLanguage,
    ) -> Cached<TypescriptAnalysis> {
        typescript_analysis::analysis_versioned(self, source, language)
    }

    #[cfg(test)]
    fn cache_path(&self, language: &str, key: &SourceAnalysisKey) -> PathBuf {
        let hash = key
            .content_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.cache_dir
            .join(language)
            .join(key.frontend_version)
            .join(format!("{hash}.json"))
    }

    #[cfg(test)]
    fn repository_observations_versioned(
        &self,
        state: &RepositoryState,
        sources: RepositoryAnalysisSources<'_>,
        versions: AnalysisVersions,
    ) -> Result<(Arc<RepositoryAnalysis>, CacheStatus, String), Box<dyn Error>> {
        let RepositoryAnalysisSources {
            rust: rust_sources,
            elixir: elixir_sources,
            csharp: csharp_sources,
            csharp_projects,
            unity_prefabs,
            unity_script_metas,
            unity_prefab_metas,
            typescript: typescript_sources,
            typescript_manifests,
            typescript_configs,
            graphql,
            descriptors,
        } = sources;
        let key = versions.repository_key(
            state.fingerprint.clone(),
            RepositoryLanguages {
                rust: !rust_sources.is_empty(),
                elixir: !elixir_sources.is_empty(),
                csharp: !csharp_sources.is_empty()
                    || !csharp_projects.is_empty()
                    || !unity_prefabs.is_empty(),
                typescript: !typescript_sources.is_empty(),
                protobuf: !descriptors.is_empty(),
                graphql: !graphql.is_empty(),
            },
        );
        let analysis_identity = key.analysis_identity();
        let (rust_frontend, rust_resolver) = key.rust.unwrap_or(("_", "_"));
        let (elixir_frontend, elixir_resolver) = key.elixir.unwrap_or(("_", "_"));
        let (csharp_frontend, csharp_resolver) = key.csharp.unwrap_or(("_", "_"));
        let (typescript_frontend, typescript_resolver) = key.typescript.unwrap_or(("_", "_"));
        let path = self
            .cache_dir
            .join("repository")
            .join("semantic")
            .join(rust_frontend)
            .join(rust_resolver)
            .join(elixir_frontend)
            .join(elixir_resolver)
            .join(csharp_frontend)
            .join(csharp_resolver)
            .join(typescript_frontend)
            .join(typescript_resolver)
            .join(key.protobuf.unwrap_or("_"))
            .join(key.graphql.unwrap_or("_"))
            .join(format!("{}.json", state.fingerprint));
        if let Ok(file) = File::open(&path)
            && let Ok(analysis) =
                serde_json::from_reader::<_, RepositoryAnalysis>(BufReader::new(file))
        {
            let analysis = Arc::new(analysis);
            tracing::debug!(repository = %state.repository.identity, cache_status = "disk", "repository cache lookup");
            return Ok((analysis, CacheStatus::Disk, analysis_identity));
        }
        let mut observations = Vec::new();
        let mut entities = Vec::<EntityFact>::new();
        let mut grpc_bindings = Vec::new();
        let mut diagnostics = Vec::new();
        let unity_assemblies = csharp_projects
            .iter()
            .filter(|(path, _)| {
                path.extension()
                    .is_some_and(|extension| extension == "asmdef")
            })
            .cloned()
            .collect::<Vec<_>>();
        let is_unity = !unity_assemblies.is_empty() || !unity_prefabs.is_empty();
        let csharp_projects = if is_unity {
            parse_unity_assemblies(&unity_assemblies).map_err(erase_error)?
        } else {
            csharp_projects
                .iter()
                .map(|(path, source)| parse_csharp_project(path, source))
                .collect::<Result<Vec<CsharpProject>, _>>()
                .map_err(erase_error)?
        };
        let mut rust_analyses = Vec::new();
        let analyzed_rust = self.analysis_pool.install(|| {
            rust_sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, cache_status) = self
                        .rust_analysis_versioned(source, versions.rust_frontend)
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    Ok::<_, SourceAnalysisError>((
                        path.as_path(),
                        analysis.clone(),
                        observations_from_analysis(&state.repository.identity, &analysis, path),
                        rust_entities(&state.repository.identity, &analysis, path),
                        rust_diagnostics(&analysis, path),
                    ))
                })
                .collect::<Vec<_>>()
        });
        for analysis in analyzed_rust {
            let (path, analysis, source_observations, source_entities, source_diagnostics) =
                analysis?;
            observations.extend(source_observations);
            entities.extend(source_entities);
            diagnostics.extend(source_diagnostics);
            rust_analyses.push((path, analysis));
        }
        let rust_sources = rust_analyses
            .iter()
            .map(|(path, analysis)| (*path, analysis.as_ref()))
            .collect::<Vec<_>>();
        let (source_bindings, source_diagnostics) =
            tonic_bindings(&state.repository.identity, &rust_sources);
        grpc_bindings.extend(source_bindings);
        diagnostics.extend(source_diagnostics);
        resolve_rust_repository_calls(&mut observations);
        let mut elixir_analyses = Vec::new();
        let analyzed_elixir = self.analysis_pool.install(|| {
            elixir_sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, cache_status) = self
                        .elixir_analysis_versioned(source, versions.elixir_frontend)
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    Ok::<_, SourceAnalysisError>((
                        path.as_path(),
                        analysis.clone(),
                        elixir_observations(&state.repository.identity, &analysis, source, path),
                        elixir_entities(&state.repository.identity, &analysis, path),
                        elixir_diagnostics(&analysis, path),
                    ))
                })
                .collect::<Vec<_>>()
        });
        for analysis in analyzed_elixir {
            let (path, analysis, source_observations, source_entities, source_diagnostics) =
                analysis?;
            observations.extend(source_observations);
            entities.extend(source_entities);
            diagnostics.extend(source_diagnostics);
            elixir_analyses.push((path, analysis));
        }
        let elixir_sources = elixir_analyses
            .iter()
            .map(|(path, analysis)| (*path, analysis.as_ref()))
            .collect::<Vec<_>>();
        let generated_observations = elixir_generated_observations(
            &state.repository.identity,
            &elixir_sources,
            &observations,
        );
        entities.extend(elixir_generated_entities(&generated_observations));
        observations.extend(generated_observations);
        resolve_elixir_repository_calls(&mut observations, &elixir_sources);
        let (source_bindings, source_diagnostics) =
            elixir_grpc_bindings(&state.repository.identity, &elixir_sources);
        grpc_bindings.extend(source_bindings);
        diagnostics.extend(source_diagnostics);
        let analyzed_csharp = self.analysis_pool.install(|| {
            csharp_sources
                .par_iter()
                .map(|(path, bytes)| {
                    let (source, lossy_encoding) = decode_csharp_source(bytes);
                    let (analysis, cache_status) = self
                        .csharp_analysis_versioned(&source)
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    let mut source_diagnostics = csharp_diagnostics(&analysis, path);
                    if lossy_encoding {
                        source_diagnostics.push(AnalysisDiagnostic {
                            code: "csharp.lossy_encoding".into(),
                            severity: AnalysisDiagnosticSeverity::Warning,
                            path: path.into(),
                            line: None,
                            detail: Some(
                                "source contained an unsupported text encoding and was decoded lossily"
                                    .into(),
                            ),
                        });
                    }
                    let assemblies = csharp_source_assemblies(&csharp_projects, path);
                    let observations = assemblies
                        .iter()
                        .flat_map(|assembly| {
                            csharp_observations(
                                &state.repository.identity,
                                assembly,
                                &analysis,
                                &source,
                                path,
                            )
                        })
                        .collect::<Vec<_>>();
                    let entities = assemblies
                        .iter()
                        .flat_map(|assembly| {
                            csharp_entities(&state.repository.identity, assembly, &analysis, path)
                        })
                        .collect::<Vec<_>>();
                    Ok::<_, SourceAnalysisError>((
                        path.clone(),
                        analysis,
                        assemblies,
                        observations,
                        entities,
                        source_diagnostics,
                    ))
                })
                .collect::<Vec<_>>()
        });
        let mut csharp_repository_sources = Vec::new();
        for analysis in analyzed_csharp {
            let (
                path,
                analysis,
                assemblies,
                source_observations,
                source_entities,
                source_diagnostics,
            ) = analysis?;
            observations.extend(source_observations);
            entities.extend(source_entities);
            diagnostics.extend(source_diagnostics);
            csharp_repository_sources.extend(
                assemblies
                    .into_iter()
                    .map(|assembly| (path.clone(), assembly, analysis.clone())),
            );
        }
        let csharp_repository_source_refs = csharp_repository_sources
            .iter()
            .map(|(path, assembly, analysis)| CsharpSource {
                path,
                assembly,
                analysis,
            })
            .collect::<Vec<_>>();
        if is_unity {
            let (unity_entities, unity_observations) = csharp_unity_lifecycle(
                &state.repository.identity,
                &csharp_projects,
                &csharp_repository_source_refs,
            );
            entities.extend(unity_entities);
            observations.extend(unity_observations);
            let script_paths = unity_script_metas
                .iter()
                .map(|(path, guid, _)| (guid.clone(), path.clone()))
                .collect();
            let prefab_paths = unity_prefab_metas
                .iter()
                .map(|(path, guid, _)| (guid.clone(), path.clone()))
                .collect();
            let (prefab_entities, prefab_observations, prefab_diagnostics) =
                csharp_unity_prefab_dependencies(
                    &state.repository.identity,
                    unity_prefabs,
                    &script_paths,
                    &prefab_paths,
                    &csharp_repository_source_refs,
                );
            entities.extend(prefab_entities);
            observations.extend(prefab_observations);
            diagnostics.extend(prefab_diagnostics);
        }
        observations.extend(resolve_csharp_repository_calls(
            &state.repository.identity,
            &csharp_projects,
            &csharp_repository_source_refs,
        ));
        let graphql_resolver_bindings =
            elixir_graphql_resolver_bindings(&state.repository.identity, &elixir_sources);
        let mut typescript_analyses = Vec::new();
        let analyzed_typescript = self.analysis_pool.install(|| {
            typescript_sources
                .par_iter()
                .map(|(path, source, language)| {
                    let (analysis, cache_status) = self
                        .typescript_analysis_versioned(source, *language)
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    Ok::<_, SourceAnalysisError>((
                        path.as_path(),
                        analysis.clone(),
                        source.as_str(),
                        typescript_observations(
                            &state.repository.identity,
                            &analysis,
                            source,
                            path,
                        ),
                        typescript_entities(&state.repository.identity, &analysis, path),
                        typescript_diagnostics(&analysis, path),
                    ))
                })
                .collect::<Vec<_>>()
        });
        for analysis in analyzed_typescript {
            let (path, analysis, source, source_observations, source_entities, source_diagnostics) =
                analysis?;
            observations.extend(source_observations);
            entities.extend(source_entities);
            diagnostics.extend(source_diagnostics);
            typescript_analyses.push((path, analysis, source));
        }
        let typescript_sources = typescript_analyses
            .iter()
            .map(|(path, analysis, _)| (*path, analysis.as_ref()))
            .collect::<Vec<_>>();
        let typescript_manifest_refs = typescript_manifests
            .iter()
            .map(|(path, source)| (path.as_path(), source.as_str()))
            .collect::<Vec<_>>();
        let typescript_config_refs = typescript_configs
            .iter()
            .map(|(path, source)| (path.as_path(), source.as_str()))
            .collect::<Vec<_>>();
        let typescript_graphql_sources = typescript_analyses
            .iter()
            .map(|(path, analysis, source)| GraphqlResolverSource {
                path,
                analysis,
                source,
            })
            .collect::<Vec<_>>();
        let graphql_resolvers = collect_typescript_graphql_resolvers(GraphqlResolverInput {
            repository: &state.repository.identity,
            sources: &typescript_graphql_sources,
            manifests: &typescript_manifest_refs,
        });
        observations.extend(graphql_resolvers.observations);
        entities.extend(graphql_resolvers.entities);
        diagnostics.extend(graphql_resolvers.diagnostics);
        resolve_typescript_repository_calls(
            &state.repository.identity,
            &mut observations,
            &typescript_sources,
            &typescript_manifest_refs,
            &typescript_config_refs,
        );
        let (source_bindings, source_diagnostics) =
            typescript_grpc_bindings(TypescriptGrpcBindingInput {
                repository: &state.repository.identity,
                sources: &typescript_sources,
                observations: &observations,
            });
        grpc_bindings.extend(source_bindings);
        diagnostics.extend(source_diagnostics);
        let typescript = (!typescript_analyses.is_empty()).then(|| {
            TypescriptRepository::new(
                state.repository.identity.clone(),
                typescript_analyses
                    .iter()
                    .map(|(path, analysis, _)| ((*path).to_path_buf(), analysis.as_ref().clone()))
                    .collect(),
                typescript_manifests.to_vec(),
                typescript_configs.to_vec(),
            )
        });
        let graphql_sources = graphql
            .iter()
            .map(|(path, source)| GraphqlSource {
                path,
                source,
                owner: None,
            })
            .collect::<Vec<_>>();
        let graphql = collect_typescript_graphql_facts(GraphqlFactInput {
            repository: &state.repository.identity,
            sources: &typescript_graphql_sources,
            schemas: &graphql_sources,
        });
        entities.extend(graphql.entities);
        observations.extend(graphql.observations);
        diagnostics.extend(graphql.diagnostics);
        let graphql_fields = entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::GraphqlField)
            .filter_map(|entity| {
                let path = entity.id.as_str().strip_prefix("graphql-field://")?;
                let (parent, field) = path.split_once('/')?;
                Some(((parent, field), entity.id.as_str()))
            })
            .collect::<BTreeMap<_, _>>();
        observations.extend(graphql_resolver_bindings.into_iter().filter_map(|binding| {
            let field = binding
                .parent
                .as_deref()
                .and_then(|parent| {
                    graphql_fields
                        .get(&(parent, binding.field.as_str()))
                        .copied()
                })
                .or_else(|| {
                    let mut fields = graphql_fields
                        .iter()
                        .filter(|((_, name), _)| *name == binding.field)
                        .map(|(_, id)| *id);
                    let field = fields.next()?;
                    fields.next().is_none().then_some(field)
                })?;
            Some(Observation::dependency(
                field,
                DependencyRelation::ResolvedBy,
                binding.resolver,
                binding.evidence,
            ))
        }));
        let descriptor_facts = self.analysis_pool.install(|| {
            descriptors
                .par_iter()
                .map(|(_, descriptor)| {
                    protobuf_facts(descriptor).map_err(|error| error.to_string())
                })
                .collect::<Vec<_>>()
        });
        for descriptor in descriptor_facts {
            let descriptor = descriptor?;
            observations.extend(descriptor.observations);
            for entity in descriptor.entities {
                if !entities.contains(&entity) {
                    entities.push(entity);
                }
            }
        }
        let incomplete = diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.ends_with(".parse_recovery"));
        let analysis = Arc::new(RepositoryAnalysis {
            incomplete,
            csharp_projects,
            entities,
            grpc_bindings,
            observations,
            diagnostics,
            typescript,
        });
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(file) = File::create(path)
        {
            let mut writer = BufWriter::new(file);
            if serde_json::to_writer(&mut writer, analysis.as_ref()).is_ok() {
                let _ = writer.flush();
            }
        }
        tracing::debug!(repository = %state.repository.identity, cache_status = "miss", "repository cache lookup");
        Ok((analysis, CacheStatus::Miss, analysis_identity))
    }
}

fn index_workspace(
    scheduler: &IndexScheduler,
    store: &SemanticStore,
    workspace: &Workspace,
    dirty: Option<&BTreeMap<String, DirtyRepository>>,
) -> Result<(usize, bool), Box<dyn Error>> {
    index_workspace_through_port(scheduler, store, workspace, dirty)
}

fn index_workspace_through_port(
    scheduler: &IndexScheduler,
    store: &SemanticStore,
    workspace: &Workspace,
    dirty: Option<&BTreeMap<String, DirtyRepository>>,
) -> Result<(usize, bool), Box<dyn Error>> {
    let source_loading_started = Instant::now();
    let inventories = tracing::info_span!(
        "index.inventory",
        workspace = %workspace.name,
        repositories = workspace.repositories.len()
    )
    .in_scope(|| {
        workspace
            .repositories
            .iter()
            .map(|repository| {
                let descriptors = workspace
                    .protobuf_descriptors
                    .iter()
                    .filter(|descriptor| descriptor.repository == repository.repository)
                    .map(|descriptor| descriptor.path.clone())
                    .collect::<Vec<_>>();
                repository_inventory(&repository.base, &descriptors, &scheduler.indexer)
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    let verification_fingerprint =
        workspace_verification_fingerprint(&scheduler.indexer, &inventories);
    if store.verification_matches(&workspace.name, &verification_fingerprint)?
        && scheduler.enrichments_current(store, &workspace.name)?
    {
        tracing::info!(workspace = %workspace.name, "workspace inputs unchanged");
        return Ok((0, false));
    }
    let repositories = tracing::info_span!(
        "index.load_sources",
        workspace = %workspace.name,
        repositories = inventories.len()
    )
    .in_scope(|| {
        inventories
            .into_iter()
            .map(RepositoryInventory::load)
            .collect::<Result<Vec<_>, _>>()
    })?;
    let source_loading = source_loading_started.elapsed();
    let snapshot = WorkspaceSnapshot {
        name: workspace.name.clone(),
        repositories,
    };
    let view = WorkspaceView::new(
        &workspace.name,
        scheduler.indexer.analysis_identity(&snapshot),
        snapshot
            .repositories
            .iter()
            .map(|repository| repository.state.clone())
            .collect(),
    )?;
    if store.view_matches(&view)? {
        store.store_verification_fingerprint(&workspace.name, &verification_fingerprint)?;
        scheduler.queue_enrichments(store, &snapshot, &view)?;
        tracing::info!(workspace = %workspace.name, "workspace unchanged");
        return Ok((0, false));
    }

    let dirty_source_units = snapshot
        .repositories
        .iter()
        .map(|repository| {
            match dirty
                .and_then(|repositories| repositories.get(&repository.state.repository.identity))
            {
                Some(DirtyRepository::Sources(sources)) => sources.len(),
                Some(DirtyRepository::All) | None => repository
                    .inputs
                    .iter()
                    .filter(|input| input.kind == beholder_indexing::InputKind::Source)
                    .count(),
            }
        })
        .sum::<usize>();
    let repository_analysis_started = Instant::now();
    let analysis = tracing::info_span!(
        "index.analyze",
        workspace = %workspace.name,
        dirty_source_units
    )
    .in_scope(|| scheduler.indexer.analyze(&snapshot).map_err(erase_error))?;
    let repository_analysis = repository_analysis_started.elapsed();
    let mut memory_hits = 0;
    let mut disk_hits = 0;
    let mut misses = 0;
    let repository_facts = analysis
        .repositories
        .into_iter()
        .map(|repository| {
            match repository.cache {
                IndexerCacheStatus::Memory => memory_hits += 1,
                IndexerCacheStatus::Disk => disk_hits += 1,
                IndexerCacheStatus::Miss => misses += 1,
            }
            repository.facts
        })
        .collect::<Vec<_>>();
    let observation_count = repository_facts
        .iter()
        .map(|facts| facts.observations.len())
        .sum();
    let publication_started = Instant::now();
    let changes = tracing::info_span!(
        "index.publish",
        workspace = %workspace.name,
        observation_count
    )
    .in_scope(|| {
        store.publish_verified(
            &view,
            &repository_facts,
            &analysis.overrides,
            &verification_fingerprint,
        )
    })?;
    scheduler.queue_enrichments(store, &snapshot, &view)?;
    let publication = publication_started.elapsed();
    pipeline::report_analysis_diagnostics(&workspace.name, &analysis.diagnostics);
    tracing::info!(
        workspace = %workspace.name,
        observation_count,
        facts_inserted = changes.inserted,
        facts_updated = changes.updated,
        facts_removed = changes.removed,
        facts_unchanged = changes.unchanged,
        repository_cache_memory_hits = memory_hits,
        repository_cache_disk_hits = disk_hits,
        repository_cache_misses = misses,
        analyzer_cache_memory_hits = analysis.cache.memory_hits,
        analyzer_cache_disk_hits = analysis.cache.disk_hits,
        analyzer_cache_misses = analysis.cache.misses,
        dirty_source_units,
        source_loading_ms = source_loading.as_secs_f64() * 1000.0,
        protobuf_compilation_ms = 0.0,
        repository_analysis_ms = repository_analysis.as_secs_f64() * 1000.0,
        workspace_resolution_ms = 0.0,
        publication_ms = publication.as_secs_f64() * 1000.0,
        "workspace indexed"
    );
    Ok((observation_count, true))
}

fn workspace_verification_fingerprint(
    indexer: &Indexer,
    inventories: &[RepositoryInventory],
) -> String {
    let mut digest = Sha256::new();
    let identity = indexer.catalog_identity();
    digest.update((identity.len() as u64).to_le_bytes());
    digest.update(identity.as_bytes());
    for inventory in inventories {
        digest.update((inventory.fingerprint.len() as u64).to_le_bytes());
        digest.update(inventory.fingerprint.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
fn index_workspace_versioned(
    scheduler: &IndexScheduler,
    store: &SemanticStore,
    workspace: &Workspace,
    dirty: Option<&BTreeMap<String, DirtyRepository>>,
    versions: AnalysisVersions,
) -> Result<(usize, bool), Box<dyn Error>> {
    let source_loading_started = Instant::now();
    let mut repositories = workspace
        .repositories
        .iter()
        .map(|repository| {
            let descriptors = workspace
                .protobuf_descriptors
                .iter()
                .filter(|descriptor| descriptor.repository == repository.repository)
                .map(|descriptor| descriptor.path.clone())
                .collect::<Vec<_>>();
            repository_sources(&repository.base, &descriptors)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let source_loading = source_loading_started.elapsed();
    let view = WorkspaceView::new(
        &workspace.name,
        versions.workspace_identity(&repositories),
        repositories
            .iter()
            .map(|sources| sources.state.clone())
            .collect(),
    )?;
    if store.view_matches(&view)? {
        tracing::info!(workspace = %workspace.name, "workspace unchanged");
        return Ok((0, false));
    }
    let protobuf_compilation_started = Instant::now();
    let mut diagnostics = Vec::new();
    for (repository, sources) in workspace.repositories.iter().zip(&mut repositories) {
        let descriptors = scheduler.analysis_pool.install(|| {
            scheduler
                .protobuf_compiler
                .compile_repository(&repository.base, &sources.protobuf_source)
        });
        let descriptors = match descriptors {
            Ok(descriptors) => descriptors,
            Err(error)
                if !sources.rust.is_empty()
                    || !sources.elixir.is_empty()
                    || !sources.csharp.is_empty()
                    || !sources.typescript.is_empty()
                    || !sources.protobuf.is_empty() =>
            {
                diagnostics.push((
                    sources.state.repository.identity.clone(),
                    AnalysisDiagnostic {
                        code: "protobuf.compile_failed".into(),
                        severity: AnalysisDiagnosticSeverity::Warning,
                        path: PathBuf::from("."),
                        line: None,
                        detail: Some(error),
                    },
                ));
                Vec::new()
            }
            Err(error) => return Err(error.into()),
        };
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            sources.protobuf.push((
                PathBuf::from(format!("compiled-protobuf-{index}.binpb")),
                descriptor.as_ref().clone(),
            ));
        }
    }
    let protobuf_compilation = protobuf_compilation_started.elapsed();

    let mut repository_facts = Vec::new();
    let mut memory_hits = 0;
    let mut disk_hits = 0;
    let mut misses = 0;
    let mut dirty_source_units = 0;
    let mut typescript_repositories = Vec::new();
    let mut needs_workspace_resolution = false;
    let repository_analysis_started = Instant::now();
    for sources in repositories {
        let RepositorySources {
            state,
            rust,
            elixir,
            csharp,
            csharp_projects,
            unity_prefabs,
            unity_script_metas,
            unity_prefab_metas,
            typescript,
            typescript_manifests,
            typescript_configs,
            graphql,
            protobuf,
            protobuf_source,
        } = sources;
        needs_workspace_resolution |=
            !rust.is_empty() || !elixir.is_empty() || !typescript.is_empty();
        dirty_source_units +=
            match dirty.and_then(|repositories| repositories.get(&state.repository.identity)) {
                Some(DirtyRepository::Sources(sources)) => sources.len(),
                Some(DirtyRepository::All) | None => {
                    rust.len()
                        + elixir.len()
                        + csharp.len()
                        + csharp_projects.len()
                        + unity_prefabs.len()
                        + unity_script_metas.len()
                        + unity_prefab_metas.len()
                        + typescript.len()
                        + typescript_manifests.len()
                        + typescript_configs.len()
                        + graphql.len()
                        + protobuf_source.len()
                }
            };
        let (analysis, cache_status, analysis_identity) = scheduler
            .repository_observations_versioned(
                &state,
                RepositoryAnalysisSources {
                    rust: &rust,
                    elixir: &elixir,
                    csharp: &csharp,
                    csharp_projects: &csharp_projects,
                    unity_prefabs: &unity_prefabs,
                    unity_script_metas: &unity_script_metas,
                    unity_prefab_metas: &unity_prefab_metas,
                    typescript: &typescript,
                    typescript_manifests: &typescript_manifests,
                    typescript_configs: &typescript_configs,
                    graphql: &graphql,
                    descriptors: &protobuf,
                },
                versions,
            )?;
        match cache_status {
            CacheStatus::Memory => memory_hits += 1,
            CacheStatus::Disk => disk_hits += 1,
            CacheStatus::Miss => misses += 1,
        }
        diagnostics.extend(
            analysis
                .diagnostics
                .iter()
                .cloned()
                .map(|diagnostic| (state.repository.identity.clone(), diagnostic)),
        );
        if let Some(typescript) = &analysis.typescript {
            typescript_repositories.push(typescript.clone());
        }
        repository_facts.push(RepositoryFacts {
            state,
            analysis_identity,
            incomplete: analysis.incomplete,
            diagnostics: analysis.diagnostics.clone(),
            entities: analysis.entities.clone(),
            grpc_bindings: analysis.grpc_bindings.clone(),
            observations: analysis.observations.clone(),
        });
    }
    let repository_analysis = repository_analysis_started.elapsed();
    let workspace_resolution_started = Instant::now();
    let observation_count = repository_facts
        .iter()
        .map(|facts| facts.observations.len())
        .sum();
    let mut overrides = Vec::new();
    if needs_workspace_resolution {
        let mut all_observations = repository_facts
            .iter()
            .flat_map(|facts| facts.observations.iter().cloned())
            .collect::<Vec<_>>();
        overrides = resolve_rust_repository_calls(&mut all_observations);
        overrides.extend(resolve_workspace_modules(&all_observations));
        overrides.extend(resolve_typescript_workspace_calls(
            &mut all_observations,
            &typescript_repositories,
        ));
        diagnostics.extend(unresolved_typescript_call_diagnostics(&all_observations));
    }
    let workspace_resolution = workspace_resolution_started.elapsed();
    let publication_started = Instant::now();
    let changes = store.publish(&view, &repository_facts, &overrides)?;
    let publication = publication_started.elapsed();
    pipeline::report_analysis_diagnostics(&workspace.name, &diagnostics);
    tracing::info!(
        workspace = %workspace.name,
        observation_count,
        facts_inserted = changes.inserted,
        facts_updated = changes.updated,
        facts_removed = changes.removed,
        facts_unchanged = changes.unchanged,
        repository_cache_memory_hits = memory_hits,
        repository_cache_disk_hits = disk_hits,
        repository_cache_misses = misses,
        dirty_source_units,
        source_loading_ms = source_loading.as_secs_f64() * 1000.0,
        protobuf_compilation_ms = protobuf_compilation.as_secs_f64() * 1000.0,
        repository_analysis_ms = repository_analysis.as_secs_f64() * 1000.0,
        workspace_resolution_ms = workspace_resolution.as_secs_f64() * 1000.0,
        publication_ms = publication.as_secs_f64() * 1000.0,
        "workspace indexed"
    );
    Ok((observation_count, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, WorkspaceRepository};
    use beholder_dto::{AnalysisCompleteness, EvidenceKind, RelationKind};
    use std::time::SystemTime;

    struct FakeEnricher;

    impl WorkspaceEnricher for FakeEnricher {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: "semantic".into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            path.extension().is_some_and(|extension| extension == "rs")
        }

        fn enrich<'a>(&'a self, _: WorkspaceSnapshot) -> EnrichmentFuture<'a> {
            Box::pin(async { unreachable!("an identical active job must not be queued") })
        }
    }

    fn test_workspace(name: &str, base: PathBuf) -> Workspace {
        Workspace::new(
            name,
            vec![WorkspaceRepository {
                repository: LogicalRepository {
                    identity: "repo".into(),
                },
                display_name: "repo".into(),
                base,
                alternatives: Vec::new(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn built_in_analyzers_declare_every_current_input() {
        let scheduler = IndexScheduler::new(PathBuf::new());
        for path in [
            "src/lib.rs",
            "lib/app.ex",
            "test/app.exs",
            "src/App.cs",
            "src/app.js",
            "src/app.jsx",
            "src/app.ts",
            "src/app.tsx",
            "schema.graphql",
            "operation.gql",
            "contract.proto",
            "buf.yaml",
            "buf.lock",
            "package.json",
            "App.csproj",
            "App.asmdef",
            "Thing.prefab",
            "Thing.cs.meta",
            "Thing.prefab.meta",
            "tsconfig.app.json",
            "jsconfig.app.json",
        ] {
            assert!(
                scheduler.indexer.accepts(Path::new(path)),
                "no analyzer accepts {path}"
            );
        }
        let protobuf = ProtobufAnalyzer::new(PathBuf::new());
        assert!(protobuf.is_active(&beholder_indexing::RepositorySnapshot {
            base: PathBuf::new(),
            state: RepositoryState {
                repository: beholder_domain::LogicalRepository {
                    identity: "descriptor-only".into(),
                },
                head: None,
                fingerprint: "descriptor".into(),
            },
            inputs: vec![beholder_indexing::RepositoryInput {
                path: PathBuf::from("descriptor.binpb"),
                content: Arc::from(&b"descriptor"[..]),
                kind: beholder_indexing::InputKind::ProtobufDescriptor,
            }],
        }));
    }

    #[test]
    fn does_not_queue_an_identical_active_enrichment() {
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: None,
            fingerprint: "source".into(),
        };
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![beholder_indexing::RepositorySnapshot {
                base: PathBuf::from("repo"),
                state: state.clone(),
                inputs: vec![beholder_indexing::RepositoryInput {
                    path: PathBuf::from("src/lib.rs"),
                    content: Arc::from(&b"fn main() {}"[..]),
                    kind: beholder_indexing::InputKind::Source,
                }],
            }],
        };
        let view = WorkspaceView::new("main", "syntax", vec![state]).unwrap();
        let scheduler = IndexScheduler::with_indexer(
            IndexerBuilder::new(PathBuf::new(), 1)
                .add_enricher(FakeEnricher)
                .build()
                .unwrap(),
        );
        scheduler.enriching.lock().unwrap().insert(
            ("main".into(), "semantic".into()),
            EnrichmentRun {
                fingerprint: view.fingerprint(),
                version: "1".into(),
            },
        );

        scheduler
            .queue_enrichments(&SemanticStore::memory().unwrap(), &snapshot, &view)
            .unwrap();

        assert!(scheduler.enrichment_jobs.lock().unwrap().is_empty());
    }

    #[test]
    fn frontend_cache_reuses_content_and_invalidates_versions() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-cache-{unique}"));
        let scheduler = IndexScheduler::new(cache.clone());
        let (first, first_status) = scheduler
            .rust_analysis_versioned("fn shared() {}", FRONTEND_VERSION)
            .unwrap();
        let (second, second_status) = scheduler
            .rust_analysis_versioned("fn shared() {}", FRONTEND_VERSION)
            .unwrap();

        assert_eq!(first_status, CacheStatus::Miss);
        assert_eq!(second_status, CacheStatus::Memory);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(scheduler.rust_cache.lock().unwrap().len(), 1);
        assert_eq!(
            observations_from_analysis("one", &first, Path::new("src/one.rs"))[0]
                .to
                .as_str(),
            "repo://one/rust/one/shared"
        );
        assert_eq!(
            observations_from_analysis("two", &second, Path::new("src/two.rs"))[0]
                .to
                .as_str(),
            "repo://two/rust/two/shared"
        );

        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .rust_analysis_versioned("fn shared() {}", FRONTEND_VERSION)
                .unwrap()
                .1,
            CacheStatus::Disk
        );
        let versioned = "fn versioned() {}";
        assert_eq!(
            scheduler
                .rust_analysis_versioned(versioned, "old")
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .rust_analysis_versioned(versioned, FRONTEND_VERSION)
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        drop(scheduler);
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn typescript_frontend_cache_reuses_disk_and_separates_grammars() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-typescript-cache-{unique}"));
        let source = "export function shared() {}";
        let scheduler = IndexScheduler::new(cache.clone());

        assert_eq!(
            scheduler
                .typescript_analysis_versioned(source, SourceLanguage::TypeScript)
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        assert_eq!(
            scheduler
                .typescript_analysis_versioned(source, SourceLanguage::TypeScript)
                .unwrap()
                .1,
            CacheStatus::Memory
        );
        assert_eq!(
            scheduler
                .typescript_analysis_versioned(source, SourceLanguage::JavaScript)
                .unwrap()
                .1,
            CacheStatus::Miss
        );

        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        for language in [SourceLanguage::TypeScript, SourceLanguage::JavaScript] {
            assert_eq!(
                scheduler
                    .typescript_analysis_versioned(source, language)
                    .unwrap()
                    .1,
                CacheStatus::Disk
            );
        }
        drop(scheduler);
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn csharp_frontend_cache_reuses_memory_and_disk() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-csharp-cache-{unique}"));
        let source = "class Shared {}";
        let scheduler = IndexScheduler::new(cache.clone());

        assert_eq!(
            scheduler.csharp_analysis_versioned(source).unwrap().1,
            CacheStatus::Miss
        );
        assert_eq!(
            scheduler.csharp_analysis_versioned(source).unwrap().1,
            CacheStatus::Memory
        );
        drop(scheduler);

        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler.csharp_analysis_versioned(source).unwrap().1,
            CacheStatus::Disk
        );
        drop(scheduler);
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn indexes_javascript_and_typescript_through_the_workspace_pipeline() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-typescript-index-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        for directory in [
            "packages/app/src",
            "packages/shell/src",
            "packages/i18n/src",
        ] {
            fs::create_dir_all(repository.join(directory)).unwrap();
        }
        fs::write(
            repository.join("src/service.ts"),
            "export function helper() {} export class Worker { run() { helper(); this.stop(); } stop() {} }",
        )
        .unwrap();
        fs::write(
            repository.join("src/component.tsx"),
            "export const Component = (): JSX.Element => <main />",
        )
        .unwrap();
        fs::write(
            repository.join("src/legacy.js"),
            "export const legacy = () => undefined",
        )
        .unwrap();
        fs::write(
            repository.join("src/view.jsx"),
            "export const View = () => <main />",
        )
        .unwrap();
        fs::write(
            repository.join("packages/shell/package.json"),
            r#"{"name":"@example/shell"}"#,
        )
        .unwrap();
        fs::write(
            repository.join("packages/i18n/package.json"),
            r#"{"name":"@example/i18n"}"#,
        )
        .unwrap();
        fs::write(
            repository.join("packages/app/src/start.ts"),
            "import { loadLocale } from '@example/shell/src/loadLocale'; import { setLocale } from '@i18n/current'; export function start() { loadLocale(); setLocale(); }",
        )
        .unwrap();
        fs::write(
            repository.join("tsconfig.json"),
            r#"{
                // TypeScript accepts comments and trailing commas.
                "compilerOptions": {
                    "baseUrl": ".",
                    "paths": { "@i18n/*": ["packages/i18n/src/*"], },
                },
            }"#,
        )
        .unwrap();
        fs::write(
            repository.join("packages/shell/src/loadLocale.ts"),
            "import { setLocale } from '@example/i18n/src/current'; export function loadLocale() { setLocale(); }",
        )
        .unwrap();
        fs::write(
            repository.join("packages/i18n/src/current.ts"),
            "export function setLocale() {}",
        )
        .unwrap();
        let workspace = test_workspace("main", repository);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let store = SemanticStore::memory().unwrap();

        assert!(scheduler.index(&store, &workspace).unwrap().1);
        let stored = store.inspect_observations(Some("calls")).unwrap();
        let has_call = |target: &str| {
            stored.rows.iter().any(|row| {
                row[1]
                    .as_str()
                    .is_some_and(|from| from.ends_with("/typescript/src/service/Worker/run"))
                    && row[3].as_str().is_some_and(|to| to.ends_with(target))
            })
        };
        assert!(has_call("/typescript/src/service/helper"), "{stored:?}");
        assert!(
            has_call("/typescript/src/service/Worker/stop"),
            "{stored:?}"
        );
        assert!(
            stored.rows.iter().any(|row| {
                row[1]
                    .as_str()
                    .is_some_and(|from| from.ends_with("/typescript/packages/app/src/start/start"))
                    && row[3].as_str().is_some_and(|to| {
                        to.ends_with("/typescript/packages/shell/src/loadLocale/loadLocale")
                    })
            }),
            "{stored:?}"
        );
        assert!(
            stored.rows.iter().any(|row| {
                row[1]
                    .as_str()
                    .is_some_and(|from| from.ends_with("/typescript/packages/app/src/start/start"))
                    && row[3].as_str().is_some_and(|to| {
                        to.ends_with("/typescript/packages/i18n/src/current/setLocale")
                    })
            }),
            "{stored:?}"
        );
        assert!(
            stored.rows.iter().any(|row| {
                row[1].as_str().is_some_and(|from| {
                    from.ends_with("/typescript/packages/shell/src/loadLocale/loadLocale")
                }) && row[3].as_str().is_some_and(|to| {
                    to.ends_with("/typescript/packages/i18n/src/current/setLocale")
                })
            }),
            "{stored:?}"
        );
        let observations = format!("{:?}", store.inspect_observations(None).unwrap());
        assert!(observations.contains("/typescript/src/component/Component"));
        assert!(observations.contains("/javascript/src/legacy/legacy"));
        assert!(observations.contains("/javascript/src/view/View"));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn indexes_csharp_when_unrelated_protobuf_fails() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-csharp-index-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(
            repository.join("src/Program.cs"),
            "namespace Demo; class Program { void Run() { Helper(); } void Helper() {} }",
        )
        .unwrap();
        fs::write(
            repository.join("src/App.csproj"),
            "<Project><PropertyGroup><AssemblyName>Example.App</AssemblyName></PropertyGroup></Project>",
        )
        .unwrap();
        fs::write(
            repository.join("broken.proto"),
            "syntax = \"proto3\"; import \"missing.proto\"; message Broken {}",
        )
        .unwrap();
        let workspace = test_workspace("main", repository);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let store = SemanticStore::memory().unwrap();

        assert!(scheduler.index(&store, &workspace).unwrap().1);
        let stored = store.inspect_observations(Some("calls")).unwrap();
        assert!(
            stored.rows.iter().any(|row| {
                row[1].as_str().is_some_and(|from| {
                    from.ends_with("/csharp/Example.App/src/Program/Demo/Program/Run()")
                }) && row[3].as_str().is_some_and(|to| {
                    to.ends_with("/csharp/Example.App/src/Program/Demo/Program/Helper()")
                })
            }),
            "{stored:?}"
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn resolves_typescript_workspace_packages_across_repositories() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-typescript-workspace-{unique}"));
        let consumer = state.join("consumer");
        let provider = state.join("provider");
        fs::create_dir_all(consumer.join("src")).unwrap();
        fs::create_dir_all(provider.join("src")).unwrap();
        fs::write(
            consumer.join("src/start.ts"),
            "import { Service, work } from '@example/provider'; export function start(service: Service) { work(); service.execute(); }",
        )
        .unwrap();
        fs::write(
            provider.join("package.json"),
            r#"{"name":"@example/provider"}"#,
        )
        .unwrap();
        fs::write(
            provider.join("src/index.ts"),
            "export { Service, work } from './impl';",
        )
        .unwrap();
        fs::write(
            provider.join("src/impl.ts"),
            "export class Service { execute() {} } export function work() {}",
        )
        .unwrap();
        let workspace = Workspace::new(
            "main",
            [("consumer", consumer), ("provider", provider)]
                .into_iter()
                .map(|(identity, base)| WorkspaceRepository {
                    repository: LogicalRepository {
                        identity: identity.into(),
                    },
                    display_name: identity.into(),
                    base,
                    alternatives: Vec::new(),
                })
                .collect(),
        )
        .unwrap();
        let cache = state.join("cache");

        for _ in 0..2 {
            let scheduler = IndexScheduler::new(cache.clone());
            let store = SemanticStore::memory().unwrap();
            assert!(scheduler.index(&store, &workspace).unwrap().1);
            let stored = store.inspect_observations(Some("calls")).unwrap();
            let caller = stored
                .rows
                .iter()
                .filter_map(|row| row[1].as_str())
                .find(|from| from.ends_with("/typescript/src/start/start"))
                .unwrap()
                .to_owned();
            let context = store.context("main", &caller).unwrap();
            for target in [
                "/typescript/src/impl/work",
                "/typescript/src/impl/Service/execute",
            ] {
                assert!(
                    context.edges.iter().any(|edge| {
                        edge.from == caller
                            && edge.to.starts_with("repo://local://")
                            && edge.to.ends_with(target)
                    }),
                    "{context:?}"
                );
            }
        }
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn repository_disk_cache_ignores_versions_for_absent_languages() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-repository-cache-{unique}"));
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "state".into(),
        };
        let sources = vec![(
            PathBuf::from("src/lib.rs"),
            "fn caller() { helper(); } fn helper() {}".into(),
        )];
        let scheduler = IndexScheduler::new(cache.clone());
        let (first, first_status, first_identity) = scheduler
            .repository_observations_versioned(
                &state,
                RepositoryAnalysisSources {
                    rust: &sources,
                    ..Default::default()
                },
                CURRENT_ANALYSIS_VERSIONS,
            )
            .unwrap();
        let irrelevant_versions = AnalysisVersions {
            elixir_frontend: "changed",
            elixir_resolver: "changed",
            protobuf_frontend: "changed",
            ..CURRENT_ANALYSIS_VERSIONS
        };
        let (second, second_status, second_identity) = scheduler
            .repository_observations_versioned(
                &state,
                RepositoryAnalysisSources {
                    rust: &sources,
                    ..Default::default()
                },
                irrelevant_versions,
            )
            .unwrap();

        assert_eq!(first_status, CacheStatus::Miss);
        assert_eq!(second_status, CacheStatus::Disk);
        assert_eq!(first_identity, second_identity);
        assert_eq!(
            first_identity,
            format!("rust:{FRONTEND_VERSION}:{RESOLVER_VERSION}")
        );
        assert_eq!(first.observations, second.observations);
        assert!(first.observations.iter().any(|observation| {
            observation.from.as_str() == "repo://repo/rust/lib/caller"
                && observation.to.as_str() == "repo://repo/rust/lib/helper"
        }));
        let first_weak = Arc::downgrade(&first);
        drop(first);
        drop(second);
        assert!(first_weak.upgrade().is_none());

        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .repository_observations_versioned(
                    &state,
                    RepositoryAnalysisSources {
                        rust: &sources,
                        ..Default::default()
                    },
                    irrelevant_versions,
                )
                .unwrap()
                .1,
            CacheStatus::Disk
        );
        assert_eq!(
            scheduler
                .repository_observations_versioned(
                    &state,
                    RepositoryAnalysisSources {
                        rust: &sources,
                        ..Default::default()
                    },
                    AnalysisVersions {
                        rust_resolver: "old",
                        ..CURRENT_ANALYSIS_VERSIONS
                    },
                )
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        scheduler.clear_cache().unwrap();
        assert!(scheduler.rust_cache.lock().unwrap().is_empty());
        assert!(scheduler.elixir_cache.lock().unwrap().is_empty());
        assert!(scheduler.csharp_cache.lock().unwrap().is_empty());
        assert!(scheduler.typescript_cache.lock().unwrap().is_empty());
        assert!(!cache.exists());
    }

    #[test]
    fn repository_analysis_indexes_graphql_sources() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-graphql-cache-{unique}"));
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "graphql-state".into(),
        };
        let graphql = vec![(
            PathBuf::from("src/package.graphql"),
            "type Query { order: Order } type Order { status: String total: String } type Mutation { initializeOrder(input: InitializeOrderInput!, mode: OrderMode): ID } type Subscription { order: ID } input InitializeOrderInput { id: ID! } enum OrderMode { PREVIEW COMMIT } query OrderQuery { order { status total } } query Package { packageTemplatePreview { id } }"
                .into(),
        )];
        let elixir = vec![(
            PathBuf::from("lib/schema/mutations.ex"),
            r#"
            defmodule Checkout.Schema.Mutations do
              field :initialize_order, :initialize_order_payload do
                resolve(&Checkout.Resolvers.InitializeOrder.run/3)
              end
            end
            defmodule Checkout.Resolvers.InitializeOrder do
              def run(_, _, _), do: :ok
            end
            defmodule Checkout.Schema do
              object :order do
                field :status, :string do
                  resolve fn order, _ -> Checkout.Orders.status(order) end
                end
              end
              subscription do
                field :order, :id do
                  resolve fn args, _ -> Checkout.Orders.load(args) end
                end
              end
            end
            "#
            .into(),
        )];
        let typescript = vec![(
            PathBuf::from("src/order.ts"),
            r#"
            /** @gqlType Order */
            export class OrderModel {
              /** @gqlField total */
              total() { return "10.00" }
            }
            "#
            .into(),
            SourceLanguage::TypeScript,
        )];
        let manifests = vec![(
            PathBuf::from("package.json"),
            r#"{"dependencies":{"grats":"0.0.34"}}"#.into(),
        )];
        let scheduler = IndexScheduler::new(cache.clone());
        let (analysis, status, identity) = scheduler
            .repository_observations_versioned(
                &state,
                RepositoryAnalysisSources {
                    graphql: &graphql,
                    elixir: &elixir,
                    typescript: &typescript,
                    typescript_manifests: &manifests,
                    ..Default::default()
                },
                CURRENT_ANALYSIS_VERSIONS,
            )
            .unwrap();

        assert_eq!(status, CacheStatus::Miss);
        assert_eq!(
            identity,
            format!(
                "elixir:{ELIXIR_FRONTEND_VERSION}:{ELIXIR_RESOLVER_VERSION}:typescript:{TYPESCRIPT_FRONTEND_VERSION}:{TYPESCRIPT_RESOLVER_VERSION}:graphql:{GRAPHQL_FRONTEND_VERSION}"
            )
        );
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-operation://Package"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::Selects,
                    )
                && observation.to.as_str() == "graphql-field://Query/packageTemplatePreview"
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Query/order"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::Selects,
                    )
                && observation.to.as_str() == "graphql-field://Order/status"
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Order/status"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::ResolvedBy,
                    )
                && observation.to.as_str()
                    == "repo://repo/elixir/Checkout.Schema/__absinthe_order_status_resolver/2"
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Order/total"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::ResolvedBy,
                    )
                && observation.to.as_str() == "repo://repo/typescript/src/order/OrderModel/total"
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Mutation/initializeOrder"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::ResolvedBy,
                    )
                && observation.to.as_str()
                    == "repo://repo/elixir/Checkout.Resolvers.InitializeOrder/run/3"
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Subscription/order"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::ResolvedBy,
                    )
                && observation.to.as_str()
                    == "repo://repo/elixir/Checkout.Schema/__absinthe_subscription_order_resolver/2"
        }));
        assert!(analysis.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-type://InitializeOrderInput"
                && entity.kind == beholder_domain::EntityKind::GraphqlType
                && entity.metadata
                    == Some(beholder_domain::EntityMetadata::GraphqlType {
                        kind: beholder_domain::GraphqlTypeKind::Input,
                    })
        }));
        assert!(analysis.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-argument://Mutation/initializeOrder/input"
                && entity.kind == beholder_domain::EntityKind::GraphqlArgument
        }));
        assert!(analysis.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-enum-value://OrderMode/PREVIEW"
                && entity.kind == beholder_domain::EntityKind::GraphqlEnumValue
        }));
        assert!(analysis.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Mutation/initializeOrder"
                && observation.relation
                    == beholder_domain::SemanticRelation::Structural(
                        beholder_domain::StructuralRelation::RequestType,
                    )
                && observation.to.as_str() == "graphql-type://InitializeOrderInput"
        }));
        scheduler.clear_cache().unwrap();
    }

    #[test]
    fn parallel_source_analysis_is_deterministic() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!("beholder-parallel-{unique}"));
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "parallel-determinism".into(),
        };
        let sources = (0..64)
            .map(|index| {
                (
                    PathBuf::from(format!("lib/module_{index}.ex")),
                    format!(
                        "defmodule Example.Module{index} do\n  def call(value), do: value\nend"
                    ),
                )
            })
            .collect::<Vec<_>>();
        let analyze = |workers, directory: &str| {
            let scheduler = IndexScheduler::with_workers(cache.join(directory), workers);
            let (analysis, status, _) = scheduler
                .repository_observations_versioned(
                    &state,
                    RepositoryAnalysisSources {
                        elixir: &sources,
                        ..Default::default()
                    },
                    CURRENT_ANALYSIS_VERSIONS,
                )
                .unwrap();
            assert_eq!(status, CacheStatus::Miss);
            let store = SemanticStore::memory().unwrap();
            let view = WorkspaceView::new("main", "analysis", vec![state.clone()]).unwrap();
            store
                .publish(
                    &view,
                    &[RepositoryFacts {
                        state: state.clone(),
                        analysis_identity: "analysis".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: analysis.observations.clone(),
                    }],
                    &[],
                )
                .unwrap();
            let entity = analysis.observations[0].from.as_str();
            let context = store.context("main", entity).unwrap();
            serde_json::to_vec(&(analysis.as_ref(), context)).unwrap()
        };

        assert_eq!(analyze(1, "one"), analyze(8, "eight"));
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    #[ignore = "run through `just index-bench` with an explicit local corpus"]
    fn benchmark_indexing() {
        let workers = std::env::var("BEHOLDER_INDEX_WORKERS")
            .expect("BEHOLDER_INDEX_WORKERS is required")
            .parse::<usize>()
            .expect("BEHOLDER_INDEX_WORKERS must be a positive integer");
        assert!(workers > 0);
        let repositories = std::env::var_os("BEHOLDER_INDEX_BENCH_REPOSITORIES")
            .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .filter(|paths| !paths.is_empty())
            .expect("BEHOLDER_INDEX_BENCH_REPOSITORIES is required");
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-index-bench-{unique}"));
        fs::create_dir_all(&state).unwrap();
        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("benchmark".into(), repositories, Vec::new())
            .unwrap();
        let scheduler = IndexScheduler::with_workers(state.join("frontend-cache"), workers);
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .try_init();

        let cold_started = Instant::now();
        let (cold_observations, _) = index_workspace(&scheduler, &store, &workspace, None).unwrap();
        println!(
            "workers={workers} mode=cold observations={cold_observations} elapsed_ms={:.3}",
            cold_started.elapsed().as_secs_f64() * 1000.0
        );

        let warm_started = Instant::now();
        let (warm_observations, _) = index_workspace_versioned(
            &scheduler,
            &store,
            &workspace,
            None,
            AnalysisVersions {
                rust_resolver: "benchmark",
                elixir_resolver: "benchmark",
                ..CURRENT_ANALYSIS_VERSIONS
            },
        )
        .unwrap();
        println!(
            "workers={workers} mode=warm-frontend observations={warm_observations} elapsed_ms={:.3}",
            warm_started.elapsed().as_secs_f64() * 1000.0
        );
        assert_eq!(cold_observations, warm_observations);
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn repository_keys_scope_each_analyzer_to_its_language() {
        let versions = AnalysisVersions {
            rust_frontend: "rust-1",
            rust_resolver: "rust-resolver-1",
            elixir_frontend: "elixir-1",
            elixir_resolver: "elixir-resolver-1",
            csharp_frontend: "csharp-1",
            csharp_resolver: "csharp-resolver-1",
            typescript_frontend: "typescript-1",
            typescript_resolver: "typescript-resolver-1",
            protobuf_frontend: "protobuf-1",
            graphql_frontend: "graphql-1",
            rule_pack: "rules-1",
        };
        let changed = AnalysisVersions {
            rust_frontend: "rust-2",
            rust_resolver: "rust-resolver-2",
            elixir_frontend: "elixir-2",
            elixir_resolver: "elixir-resolver-2",
            csharp_frontend: "csharp-2",
            csharp_resolver: "csharp-resolver-2",
            typescript_frontend: "typescript-2",
            typescript_resolver: "typescript-resolver-2",
            protobuf_frontend: "protobuf-2",
            graphql_frontend: "graphql-2",
            ..versions
        };

        for (languages, expected) in [
            (
                (true, false, false, false, false, false),
                "rust:rust-1:rust-resolver-1",
            ),
            (
                (false, true, false, false, false, false),
                "elixir:elixir-1:elixir-resolver-1",
            ),
            (
                (false, false, true, false, false, false),
                "csharp:csharp-1:csharp-resolver-1",
            ),
            (
                (false, false, false, true, false, false),
                "typescript:typescript-1:typescript-resolver-1",
            ),
            (
                (false, false, false, false, true, false),
                "protobuf:protobuf-1",
            ),
            (
                (false, false, false, false, false, true),
                "graphql:graphql-1",
            ),
            ((false, false, false, false, false, false), "none"),
        ] {
            let (rust, elixir, csharp, typescript, protobuf, graphql) = languages;
            let languages = RepositoryLanguages {
                rust,
                elixir,
                csharp,
                typescript,
                protobuf,
                graphql,
            };
            let key = versions.repository_key("state".into(), languages);
            assert_eq!(key.analysis_identity(), expected);
            if rust || elixir || csharp || typescript || protobuf || graphql {
                assert_ne!(key, changed.repository_key("state".into(), languages));
            }
        }

        let rust = RepositoryLanguages {
            rust: true,
            ..Default::default()
        };
        let elixir = RepositoryLanguages {
            elixir: true,
            ..Default::default()
        };
        let csharp = RepositoryLanguages {
            csharp: true,
            ..Default::default()
        };
        let typescript = RepositoryLanguages {
            typescript: true,
            ..Default::default()
        };
        let protobuf = RepositoryLanguages {
            protobuf: true,
            ..Default::default()
        };
        let graphql = RepositoryLanguages {
            graphql: true,
            ..Default::default()
        };

        assert_eq!(
            versions.repository_key("state".into(), rust),
            AnalysisVersions {
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                csharp_frontend: "irrelevant",
                csharp_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                graphql_frontend: "irrelevant",
                typescript_frontend: "irrelevant",
                typescript_resolver: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), rust)
        );
        assert_eq!(
            versions.repository_key("state".into(), elixir),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                csharp_frontend: "irrelevant",
                csharp_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                graphql_frontend: "irrelevant",
                typescript_frontend: "irrelevant",
                typescript_resolver: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), elixir)
        );
        assert_eq!(
            versions.repository_key("state".into(), csharp),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                graphql_frontend: "irrelevant",
                typescript_frontend: "irrelevant",
                typescript_resolver: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), csharp)
        );
        assert_eq!(
            versions.repository_key("state".into(), typescript),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                csharp_frontend: "irrelevant",
                csharp_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                graphql_frontend: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), typescript)
        );
        assert_eq!(
            versions.repository_key("state".into(), protobuf),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                csharp_frontend: "irrelevant",
                csharp_resolver: "irrelevant",
                typescript_frontend: "irrelevant",
                typescript_resolver: "irrelevant",
                graphql_frontend: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), protobuf)
        );
        assert_eq!(
            versions.repository_key("state".into(), graphql),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                csharp_frontend: "irrelevant",
                csharp_resolver: "irrelevant",
                typescript_frontend: "irrelevant",
                typescript_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), graphql)
        );
    }

    #[test]
    fn workspace_view_invalidates_only_relevant_versions_and_rules() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-view-version-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(repository.join("src/lib.rs"), "fn indexed() {}").unwrap();
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let workspace = test_workspace("main", repository);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let versions = AnalysisVersions {
            rust_frontend: "1",
            rust_resolver: "1",
            elixir_frontend: "1",
            elixir_resolver: "1",
            csharp_frontend: "1",
            csharp_resolver: "1",
            typescript_frontend: "1",
            typescript_resolver: "1",
            protobuf_frontend: "1",
            graphql_frontend: "1",
            rule_pack: "1",
        };

        assert!(
            index_workspace_versioned(&scheduler, &store, &workspace, None, versions)
                .unwrap()
                .1
        );
        assert!(
            !index_workspace_versioned(&scheduler, &store, &workspace, None, versions)
                .unwrap()
                .1
        );
        assert!(
            !index_workspace_versioned(
                &scheduler,
                &store,
                &workspace,
                None,
                AnalysisVersions {
                    elixir_frontend: "2",
                    elixir_resolver: "2",
                    protobuf_frontend: "2",
                    ..versions
                },
            )
            .unwrap()
            .1
        );
        let rust_changed = AnalysisVersions {
            rust_frontend: "2",
            ..versions
        };
        assert!(
            index_workspace_versioned(&scheduler, &store, &workspace, None, rust_changed)
                .unwrap()
                .1
        );
        assert!(
            index_workspace_versioned(
                &scheduler,
                &store,
                &workspace,
                None,
                AnalysisVersions {
                    rule_pack: "2",
                    ..rust_changed
                },
            )
            .unwrap()
            .1
        );
        assert_eq!(
            store.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(3)
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn unchanged_inventory_skips_source_loading_after_restart() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-warm-restart-{unique}"));
        let repository = state.join("repo");
        let source = repository.join("src/lib.rs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "fn indexed() {}").unwrap();
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let workspace = test_workspace("main", repository.clone());
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));

        assert!(scheduler.index(&store, &workspace).unwrap().1);
        drop(scheduler);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let fingerprint = workspace_verification_fingerprint(
            &scheduler.indexer,
            &[repository_inventory(&repository, &[], &scheduler.indexer).unwrap()],
        );
        assert!(store.verification_matches("main", &fingerprint).unwrap());
        assert!(!scheduler.index(&store, &workspace).unwrap().1);

        fs::write(&source, "fn indexed() {}").unwrap();
        assert!(!scheduler.index(&store, &workspace).unwrap().1);
        let refreshed = workspace_verification_fingerprint(
            &scheduler.indexer,
            &[repository_inventory(&repository, &[], &scheduler.indexer).unwrap()],
        );
        assert!(store.verification_matches("main", &refreshed).unwrap());

        fs::write(&source, "fn changed() {}").unwrap();
        assert!(scheduler.index(&store, &workspace).unwrap().1);
        assert_eq!(
            store.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(2)
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn mixed_repository_reanalyzes_only_the_changed_language() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-mixed-version-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::create_dir_all(repository.join("lib")).unwrap();
        fs::write(repository.join("src/lib.rs"), "fn indexed() {}").unwrap();
        fs::write(
            repository.join("lib/sample.ex"),
            "defmodule Sample do\n  def indexed, do: :ok\nend\n",
        )
        .unwrap();
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let workspace = test_workspace("main", repository);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let versions = AnalysisVersions {
            rust_frontend: "1",
            rust_resolver: "1",
            elixir_frontend: "1",
            elixir_resolver: "1",
            csharp_frontend: "1",
            csharp_resolver: "1",
            typescript_frontend: "1",
            typescript_resolver: "1",
            protobuf_frontend: "1",
            graphql_frontend: "1",
            rule_pack: "1",
        };

        assert!(
            index_workspace_versioned(&scheduler, &store, &workspace, None, versions)
                .unwrap()
                .1
        );
        assert_eq!(scheduler.rust_cache.lock().unwrap().len(), 1);
        assert_eq!(scheduler.elixir_cache.lock().unwrap().len(), 1);

        assert!(
            index_workspace_versioned(
                &scheduler,
                &store,
                &workspace,
                None,
                AnalysisVersions {
                    elixir_frontend: "2",
                    ..versions
                },
            )
            .unwrap()
            .1
        );
        assert_eq!(scheduler.rust_cache.lock().unwrap().len(), 1);
        assert_eq!(scheduler.elixir_cache.lock().unwrap().len(), 2);
        let observations = format!("{:?}", store.inspect_observations(None).unwrap());
        assert!(observations.contains("/rust/lib/indexed"));
        assert!(observations.contains("/elixir/Sample/indexed/0"));
        assert_eq!(
            store.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(2)
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn query_metadata_reports_pending_repository_state() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-freshness-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(repository.join("src/lib.rs"), "fn indexed() {}").unwrap();
        let workspace = test_workspace("main", repository.clone());
        let other = test_workspace("other", repository);
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));

        scheduler.mark(&workspace);
        scheduler.mark(&other);
        let pending = scheduler.query_metadata("main", 4, Default::default());
        assert_eq!(pending.revision, 4);
        assert!(pending.freshness.stale);
        assert!(!pending.freshness.indexing);
        assert_eq!(pending.freshness.dirty_repositories, ["repo"]);

        *scheduler.active_operation.lock().unwrap() = Some("main".into());
        assert!(
            scheduler
                .query_metadata("main", 4, Default::default())
                .freshness
                .indexing
        );
        assert!(
            !scheduler
                .query_metadata("other", 4, Default::default())
                .freshness
                .indexing
        );
        *scheduler.active_operation.lock().unwrap() = None;

        let indexed_generation = scheduler.generations.lock().unwrap()["main"];
        scheduler.mark(&workspace);
        scheduler.complete_generation("main", Some(indexed_generation));
        assert!(
            scheduler
                .query_metadata("main", 4, Default::default())
                .freshness
                .stale
        );

        scheduler.index(&store, &workspace).unwrap();
        let current = scheduler.query_metadata("main", 5, Default::default());
        assert!(!current.freshness.stale);
        assert!(!current.freshness.indexing);
        assert!(current.freshness.dirty_repositories.is_empty());
        assert!(
            scheduler
                .query_metadata("other", 4, Default::default())
                .freshness
                .stale
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn recovery_publication_is_incomplete_and_repair_clears_it() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-recovery-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        let source = repository.join("src/lib.rs");
        fs::write(&source, "fn old() { removed(); } fn removed() {}").unwrap();
        let workspace = test_workspace("main", repository);
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();

        scheduler.index(&store, &workspace).unwrap();
        fs::write(&source, "fn broken() { @ } fn current() {}").unwrap();
        scheduler.index(&store, &workspace).unwrap();

        let recovered = store
            .context_snapshot("main", "repo://repo/rust/lib/current")
            .unwrap();
        assert_eq!(
            recovered.analysis.completeness,
            AnalysisCompleteness::Incomplete
        );
        assert_eq!(
            recovered.analysis.diagnostics[0].code,
            "rust.parse_recovery"
        );
        assert!(
            store
                .inspect_observations(Some("calls"))
                .unwrap()
                .rows
                .iter()
                .all(|row| row[1].as_str() != Some("repo://repo/rust/lib/old"))
        );
        fs::write(&source, "fn broken() { fn nested() {}").unwrap();
        scheduler.mark(&workspace);
        let (observation_count, published) = scheduler.index(&store, &workspace).unwrap();
        assert_eq!(observation_count, 0);
        assert!(published);
        let skipped = store
            .context_snapshot("main", "repo://repo/rust/lib/current")
            .unwrap();
        assert_eq!(
            skipped.analysis.completeness,
            AnalysisCompleteness::Incomplete
        );
        assert_eq!(skipped.analysis.diagnostics[0].code, "rust.parse_recovery");
        assert!(skipped.result.edges.is_empty());
        assert_eq!(
            store.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(3)
        );
        assert!(
            !scheduler
                .query_metadata("main", 3, skipped.analysis)
                .freshness
                .stale
        );

        fs::write(&source, "fn repaired() {}").unwrap();
        scheduler.index(&store, &workspace).unwrap();
        let repaired = store
            .context_snapshot("main", "repo://repo/rust/lib/repaired")
            .unwrap();
        assert_eq!(
            repaired.analysis.completeness,
            AnalysisCompleteness::Complete
        );
        assert!(repaired.analysis.diagnostics.is_empty());
        assert_eq!(repaired.analysis_revision, 4);

        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn concurrent_index_claims_wait_for_the_active_job() {
        let scheduler = Arc::new(IndexScheduler::new(PathBuf::new()));
        let active = scheduler.begin("first").unwrap();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let waiting = scheduler.clone();
        let thread = std::thread::spawn(move || {
            let _active = waiting.begin("second").unwrap();
            claimed_tx.send(()).unwrap();
        });

        assert!(claimed_rx.recv_timeout(Duration::from_millis(25)).is_err());
        drop(active);
        claimed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn scheduler_lock_failure_returns_a_typed_error() {
        let scheduler = Arc::new(IndexScheduler::new(PathBuf::new()));
        let poisoned = scheduler.clone();
        assert!(
            std::thread::spawn(move || {
                let _active = poisoned.active_operation.lock().unwrap();
                panic!("poison scheduler lock");
            })
            .join()
            .is_err()
        );

        let error = match scheduler.begin("garbage collection") {
            Ok(_) => panic!("poisoned scheduler lock should fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), BeholderErrorKind::Internal);
        assert_eq!(error.code(), BeholderErrorCode::SchedulerUnavailable);
    }

    #[test]
    fn filesystem_events_reanalyze_only_changed_source_units() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-source-units-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        let changed = repository.join("src/changed.rs");
        fs::write(&changed, "fn before() {}").unwrap();
        fs::write(repository.join("src/stable.rs"), "fn stable() {}").unwrap();

        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let identity = workspace.repositories[0].repository.identity.clone();
        let registry = Mutex::new(registry);
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        scheduler.mark(&workspace);
        scheduler.index(&store, &workspace).unwrap();

        fs::write(&changed, "fn after() {}").unwrap();
        scheduler.add_event(
            Ok(Event {
                kind: EventKind::Modify(notify::event::ModifyKind::Any),
                paths: vec![changed.canonicalize().unwrap()],
                attrs: Default::default(),
            }),
            &registry,
        );
        assert_eq!(
            scheduler.dirty_repositories.lock().unwrap()["main"][&identity],
            DirtyRepository::Sources(BTreeSet::from([PathBuf::from("src/changed.rs")]))
        );

        scheduler.index(&store, &workspace).unwrap();
        let context = format!(
            "{:?}",
            store
                .context("main", &format!("repo://{identity}/rust/changed"))
                .unwrap()
        );
        assert!(context.contains("after"));
        assert!(!context.contains("before"));
        assert_eq!(
            store.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(2)
        );
        assert!(
            scheduler
                .query_metadata("main", 2, Default::default())
                .freshness
                .dirty_repositories
                .is_empty()
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn protobuf_filesystem_events_mark_repository_dirty() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-protobuf-event-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(&repository).unwrap();
        let changed = repository.join("contract.proto");
        fs::write(&changed, "syntax = \"proto3\"; message Contract {}").unwrap();
        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let identity = workspace.repositories[0].repository.identity.clone();
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));

        scheduler.add_event(
            Ok(Event {
                kind: EventKind::Modify(notify::event::ModifyKind::Any),
                paths: vec![changed.canonicalize().unwrap()],
                attrs: Default::default(),
            }),
            &Mutex::new(registry),
        );

        assert_eq!(
            scheduler.dirty_repositories.lock().unwrap()["main"][&identity],
            DirtyRepository::Sources(BTreeSet::from([PathBuf::from("contract.proto")]))
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn elixir_source_units_are_indexed_incrementally() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-elixir-source-units-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("lib")).unwrap();
        fs::write(
            repository.join("lib/macro.ex"),
            "defmodule MyApp.Macro do\n  defmacro __using__(_) do\n    quote do\n      def generated(value), do: MyApp.Helper.work(value)\n    end\n  end\nend\ndefmodule MyApp.Helper do\n  def work(value), do: value\nend",
        )
        .unwrap();
        let changed = repository.join("lib/sample.ex");
        fs::write(
            &changed,
            "defmodule MyApp do\n  defmodule Sample do\n    use MyApp.Macro, mode: :strict\n    import External.Helpers, only: [help: 1]\n    require External.Macros, as: Macros\n    def before, do: helper()\n    defp helper, do: :ok\n  end\nend",
        )
        .unwrap();

        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let identity = workspace.repositories[0].repository.identity.clone();
        let registry = Mutex::new(registry);
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        scheduler.mark(&workspace);
        scheduler.index(&store, &workspace).unwrap();

        let module = format!("repo://{identity}/elixir/MyApp.Sample");
        let context = store.context("main", &module).unwrap();
        assert_eq!(context.root.kind, beholder_dto::EntityKind::Namespace);
        assert!(context.nodes.iter().any(|node| {
            node.id == format!("{module}/before/0")
                && node.kind == beholder_dto::EntityKind::Callable
                && node.name == "before/0"
        }));
        assert!(context.nodes.iter().any(|node| {
            node.id == format!("{module}/generated/1")
                && node.kind == beholder_dto::EntityKind::Callable
                && node.origin == beholder_dto::EntityOrigin::Generated
        }));
        assert!(context.edges.iter().any(|edge| {
            edge.from == module
                && edge.to == format!("{module}/generated/1")
                && edge.kind == beholder_dto::RelationKind::Defines
                && edge.evidence.iter().any(|evidence| {
                    evidence.source_kind == beholder_dto::EvidenceKind::Generated
                        && evidence.path.as_deref() == Some("lib/macro.ex")
                        && evidence.line == Some(4)
                })
        }));
        assert!(context.edges.iter().any(|edge| {
            edge.from == module
                && edge.to == format!("repo://{identity}/elixir/MyApp.Macro")
                && edge.kind == beholder_dto::RelationKind::Uses
        }));
        assert!(context.edges.iter().any(|edge| {
            edge.from == module
                && edge.to == "elixir-module://External.Helpers"
                && edge.kind == beholder_dto::RelationKind::Imports
        }));
        assert!(context.edges.iter().any(|edge| {
            edge.from == module
                && edge.to == "elixir-module://External.Macros"
                && edge.kind == beholder_dto::RelationKind::Requires
        }));
        assert!(context.nodes.iter().any(|node| {
            node.id == "elixir-module://External.Helpers"
                && node.kind == beholder_dto::EntityKind::Namespace
                && node.origin == beholder_dto::EntityOrigin::ExternalDependency
                && node.repository.is_none()
        }));
        let local_call = store
            .context("main", &format!("{module}/before/0"))
            .unwrap();
        assert!(local_call.edges.iter().any(|edge| {
            edge.from == format!("{module}/before/0")
                && edge.to == format!("{module}/helper/0")
                && edge.kind == beholder_dto::RelationKind::Calls
        }));
        let generated_call = store
            .context("main", &format!("{module}/generated/1"))
            .unwrap();
        assert_eq!(
            generated_call.root.origin,
            beholder_dto::EntityOrigin::Generated
        );
        assert!(generated_call.edges.iter().any(|edge| {
            edge.from == format!("{module}/generated/1")
                && edge.to == format!("repo://{identity}/elixir/MyApp.Helper/work/1")
                && edge.kind == beholder_dto::RelationKind::Calls
                && edge
                    .evidence
                    .iter()
                    .any(|evidence| evidence.source_kind == beholder_dto::EvidenceKind::Generated)
        }));
        assert!(
            !store
                .context("main", &format!("repo://{identity}/elixir/MyApp.Macro"))
                .unwrap()
                .nodes
                .iter()
                .any(|node| node.id.ends_with("/generated/1"))
        );

        fs::write(
            &changed,
            "defmodule MyApp do\n  defmodule Sample do\n    use MyApp.Macro, mode: :strict\n    import External.Helpers, only: [help: 1]\n    require External.Macros, as: Macros\n    def updated(value), do: value\n  end\nend",
        )
        .unwrap();
        scheduler.add_event(
            Ok(Event {
                kind: EventKind::Modify(notify::event::ModifyKind::Any),
                paths: vec![changed.canonicalize().unwrap()],
                attrs: Default::default(),
            }),
            &registry,
        );
        assert_eq!(
            scheduler.dirty_repositories.lock().unwrap()["main"][&identity],
            DirtyRepository::Sources(BTreeSet::from([PathBuf::from("lib/sample.ex")]))
        );

        scheduler.index(&store, &workspace).unwrap();
        let context = store.context("main", &module).unwrap();
        assert!(
            context
                .nodes
                .iter()
                .any(|node| node.id == format!("{module}/updated/1"))
        );
        assert!(
            !context
                .nodes
                .iter()
                .any(|node| node.id == format!("{module}/before/0"))
        );
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn cross_language_grpc_workspace_resolves_both_directions() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-grpc-matrix-{unique}"));
        let contracts = state.join("contracts");
        let rust = state.join("rust-app");
        let elixir = state.join("elixir-app");
        for directory in [
            contracts.as_path(),
            rust.join("src").as_path(),
            elixir.join("lib").as_path(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let contract_path = contracts.join("matrix.proto");
        let contract_source = r#"
            syntax = "proto3";
            package phase5.v1;
            message Request {}
            message Response {}
            service Bridge {
              rpc RustToElixir(Request) returns (Response);
              rpc ElixirToRust(Request) returns (Response);
            }
            "#;
        fs::write(&contract_path, contract_source).unwrap();
        fs::write(
            rust.join("Cargo.toml"),
            "[dependencies]\ntonic = \"0.14\"\n",
        )
        .unwrap();
        fs::write(
            rust.join("src/protocol.rs"),
            "tonic::include_proto!(\"phase5.v1\");",
        )
        .unwrap();
        fs::write(
            rust.join("src/generated.rs"),
            "mod bridge_client { \
                 pub struct BridgeClient<T>(T); \
                 impl<T> BridgeClient<T> { \
                     pub async fn rust_to_elixir(&mut self) {} \
                     pub async fn elixir_to_rust(&mut self) {} \
                 } \
             }",
        )
        .unwrap();
        fs::write(
            rust.join("src/client.rs"),
            "use contract::bridge_client::BridgeClient; \
             async fn rust_to_elixir() { \
                 let mut client = BridgeClient::new(); \
                 client.rust_to_elixir().await; \
             }",
        )
        .unwrap();
        fs::write(
            rust.join("src/server.rs"),
            "use contract::bridge_server::{Bridge, BridgeServer}; \
             struct RustHandler; \
             impl Bridge for RustHandler { async fn elixir_to_rust(&self) {} }",
        )
        .unwrap();
        fs::write(
            elixir.join("lib/matrix.pb.ex"),
            r#"
            defmodule Phase5.V1.Bridge.Service do
              use GRPC.Service, name: "phase5.v1.Bridge"
              rpc :RustToElixir, Phase5.V1.Request, Phase5.V1.Response
              rpc :ElixirToRust, Phase5.V1.Request, Phase5.V1.Response
            end

            defmodule Phase5.V1.Bridge.Stub do
              use GRPC.Stub, service: Phase5.V1.Bridge.Service
            end
            "#,
        )
        .unwrap();
        fs::write(
            elixir.join("lib/client.ex"),
            r#"
            defmodule Phase5.Client do
              alias Phase5.V1.Bridge.Stub
              def elixir_to_rust(channel, request), do: Stub.elixir_to_rust(channel, request)
            end
            "#,
        )
        .unwrap();
        fs::write(
            elixir.join("lib/server.ex"),
            r#"
            defmodule Phase5.Server do
              alias Phase5.V1.Bridge.Service
              use GRPC.Server, service: Service
              def rust_to_elixir(request, stream), do: {request, stream}
            end
            "#,
        )
        .unwrap();

        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let repositories = vec![contracts.clone(), rust.clone(), elixir.clone()];
        let workspace = registry
            .register("grpc-matrix".into(), repositories.clone(), Vec::new())
            .unwrap();
        let rust_identity = beholder_adapters_git::repository_identity(&rust).unwrap();
        let elixir_identity = beholder_adapters_git::repository_identity(&elixir).unwrap();
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();
        scheduler.index(&store, &workspace).unwrap();
        let metadata = scheduler.query_metadata("grpc-matrix", 1, Default::default());
        assert_eq!(metadata.revision, 1);
        assert_eq!(metadata.view, "grpc-matrix");
        assert!(!metadata.freshness.stale);
        assert!(metadata.freshness.dirty_repositories.is_empty());

        let bindings = [
            (
                "RustToElixir",
                format!("repo://{rust_identity}/rust/client/rust_to_elixir"),
                format!("repo://{elixir_identity}/elixir/Phase5.Server/rust_to_elixir/2"),
            ),
            (
                "ElixirToRust",
                format!("repo://{elixir_identity}/elixir/Phase5.Client/elixir_to_rust/2"),
                format!(
                    "repo://{rust_identity}/rust/server/impl/Bridge-for-RustHandler/elixir_to_rust"
                ),
            ),
        ];
        for (method, client, server) in &bindings {
            let operation = format!("grpc://phase5.v1.Bridge/{method}");
            let contract = format!("proto-method://phase5.v1.Bridge/{method}");
            let context = store.context("grpc-matrix", &operation).unwrap();
            assert_eq!(context.metadata.view, "grpc-matrix");
            for (kind, from, to) in [
                (RelationKind::CallsRpc, client.as_str(), operation.as_str()),
                (
                    RelationKind::BindsContract,
                    operation.as_str(),
                    contract.as_str(),
                ),
                (
                    RelationKind::ImplementedBy,
                    operation.as_str(),
                    server.as_str(),
                ),
            ] {
                let edge = context
                    .edges
                    .iter()
                    .find(|edge| edge.kind == kind && edge.from == from && edge.to == to)
                    .unwrap_or_else(|| panic!("missing {kind:?} edge from {from} to {to}"));
                assert_eq!(edge.confidence, 1.0);
                if kind != RelationKind::BindsContract {
                    assert!(
                        edge.evidence
                            .iter()
                            .all(|evidence| evidence.repository.is_some()),
                        "{edge:#?}"
                    );
                }
                assert!(edge.evidence.iter().any(|evidence| {
                    matches!(
                        evidence.source_kind,
                        EvidenceKind::Descriptor | EvidenceKind::Generated
                    )
                }));
            }
            for entity in [client, server] {
                assert!(
                    context
                        .nodes
                        .iter()
                        .any(|node| { node.id == *entity && node.repository.as_deref().is_some() }),
                    "missing repository attribution for {entity}"
                );
            }
            let trace = store.trace("grpc-matrix", client, server, 32).unwrap();
            assert!(trace.paths.iter().any(|path| {
                path.nodes == [client.as_str(), operation.as_str(), server.as_str()]
            }));
            let impact = store.impact("grpc-matrix", &contract, 32).unwrap();
            for entity in [client, server] {
                assert!(
                    impact
                        .affected
                        .iter()
                        .any(|affected| affected.entity == *entity),
                    "contract impact did not reach {entity}"
                );
            }
        }

        let rust_cache_entries = scheduler.rust_cache.lock().unwrap().len();
        let elixir_cache_entries = scheduler.elixir_cache.lock().unwrap().len();
        fs::remove_file(&contract_path).unwrap();
        let without_contract = registry
            .register("grpc-matrix".into(), repositories.clone(), Vec::new())
            .unwrap();
        scheduler.index(&store, &without_contract).unwrap();
        assert_eq!(
            scheduler.rust_cache.lock().unwrap().len(),
            rust_cache_entries
        );
        assert_eq!(
            scheduler.elixir_cache.lock().unwrap().len(),
            elixir_cache_entries
        );
        let inspection = format!("{:?}", store.inspect_grpc_bindings().unwrap());
        assert!(inspection.contains("grpc.contract_unmatched"));
        assert!(!format!("{:?}", store.inspect_relations().unwrap()).contains("calls_rpc"));

        fs::write(&contract_path, contract_source).unwrap();
        let restored = registry
            .register("grpc-matrix".into(), repositories, Vec::new())
            .unwrap();
        scheduler.index(&store, &restored).unwrap();
        assert_eq!(
            scheduler.rust_cache.lock().unwrap().len(),
            rust_cache_entries
        );
        assert_eq!(
            scheduler.elixir_cache.lock().unwrap().len(),
            elixir_cache_entries
        );
        for (method, _, _) in &bindings {
            let operation = format!("grpc://phase5.v1.Bridge/{method}");
            let context = store.context("grpc-matrix", &operation).unwrap();
            assert!(
                context
                    .edges
                    .iter()
                    .any(|edge| edge.kind == RelationKind::BindsContract)
            );
        }

        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn graphql_workspace_resolves_operation_through_grpc_to_server() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-typescript-grpc-{unique}"));
        let contracts = state.join("contracts");
        let spa = state.join("spa");
        let client = state.join("client");
        let server = state.join("server");
        for directory in [
            contracts.as_path(),
            spa.join("src").as_path(),
            client.join("src").as_path(),
            server.join("src").as_path(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(
            client.join("package.json"),
            r#"{"dependencies":{"grats":"1.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            server.join("package.json"),
            r#"{"dependencies":{"@nestjs/common":"11.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            contracts.join("checkout.proto"),
            r#"
            syntax = "proto3";
            package phase7.checkout.v1;
            message InitializeOrderRequest {}
            message InitializeOrderResponse {}
            service RPCService {
              rpc InitializeOrder(InitializeOrderRequest) returns (InitializeOrderResponse);
            }
            "#,
        )
        .unwrap();
        fs::write(
            spa.join("src/Checkout.gql.tsx"),
            r#"
            export const InitializeOrder_Query = gql(`
              query InitializeOrder_Query { initializeOrder { id } }
            `);
            "#,
        )
        .unwrap();
        fs::write(
            spa.join("src/Checkout.tsx"),
            r#"
            import { InitializeOrder_Query } from "./Checkout.gql";
            export function Checkout() {
              return useQuery(InitializeOrder_Query);
            }
            "#,
        )
        .unwrap();
        let generated = r#"
            export const RPCServiceServiceName = "phase7.checkout.v1.RPCService";
            export class RPCServiceClientImpl {
              constructor(private readonly rpc: Rpc) {}
              initializeOrder(request: InitializeOrderRequest): Promise<InitializeOrderResponse> {
                return this.rpc.request(this.service, "InitializeOrder", request);
              }
            }
        "#;
        fs::write(client.join("src/generated.ts"), generated).unwrap();
        fs::write(
            client.join("src/messages.generated.ts"),
            r#"
            export const protobufPackage = "phase7.checkout.v1";
            export interface InitializeOrderRequest {}
            export const InitializeOrderRequest = {
              encode(message: InitializeOrderRequest) { return message; },
              decode(input: Uint8Array) { return input; },
            };
            "#,
        )
        .unwrap();
        fs::write(
            client.join("src/caller.ts"),
            r#"
            import { RPCServiceClientImpl } from "./generated";
            export class InitializeOrderResolver {
              /** @gqlQueryField initializeOrder */
              static query() {
                const client = new RPCServiceClientImpl({} as Rpc);
                return client.initializeOrder({});
              }
            }
            "#,
        )
        .unwrap();
        fs::write(server.join("src/generated.ts"), generated).unwrap();
        fs::write(
            server.join("src/controller.ts"),
            r#"
            import { GrpcMethod } from "@nestjs/microservices";
            export class CheckoutController {
              @GrpcMethod("RPCService", "InitializeOrder")
              initializeOrder(request: InitializeOrderRequest): InitializeOrderResponse {
                return request;
              }
            }
            "#,
        )
        .unwrap();

        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register(
                "typescript-grpc".into(),
                vec![
                    contracts.clone(),
                    spa.clone(),
                    client.clone(),
                    server.clone(),
                ],
                Vec::new(),
            )
            .unwrap();
        let client_identity = beholder_adapters_git::repository_identity(&client).unwrap();
        let spa_identity = beholder_adapters_git::repository_identity(&spa).unwrap();
        let server_identity = beholder_adapters_git::repository_identity(&server).unwrap();
        let caller = format!("repo://{spa_identity}/typescript/src/Checkout/Checkout");
        let graphql_operation = "graphql-operation://InitializeOrder_Query";
        let graphql_field = "graphql-field://Query/initializeOrder";
        let resolver =
            format!("repo://{client_identity}/typescript/src/caller/InitializeOrderResolver/query");
        let generated_client = format!(
            "repo://{client_identity}/typescript/src/generated/RPCServiceClientImpl/initializeOrder"
        );
        let implementation = format!(
            "repo://{server_identity}/typescript/src/controller/CheckoutController/initializeOrder"
        );
        let operation = "grpc://phase7.checkout.v1.RPCService/InitializeOrder";
        let contract = "proto-method://phase7.checkout.v1.RPCService/InitializeOrder";
        let scheduler = IndexScheduler::new(state.join("frontend-cache"));
        let store = SemanticStore::persistent(&state.join("beholder.db"), true).unwrap();

        scheduler.index(&store, &workspace).unwrap();

        let trace = store
            .trace("typescript-grpc", &caller, &implementation, 32)
            .unwrap();
        assert!(trace.paths.iter().any(|path| {
            path.nodes
                == [
                    caller.as_str(),
                    graphql_operation,
                    graphql_field,
                    resolver.as_str(),
                    generated_client.as_str(),
                    operation,
                    implementation.as_str(),
                ]
        }));
        let context = store.context("typescript-grpc", contract).unwrap();
        for (kind, message) in [
            (
                RelationKind::RequestType,
                "proto-type://phase7.checkout.v1.InitializeOrderRequest",
            ),
            (
                RelationKind::ResponseType,
                "proto-type://phase7.checkout.v1.InitializeOrderResponse",
            ),
        ] {
            assert!(
                context.edges.iter().any(|edge| {
                    edge.kind == kind && edge.from == contract && edge.to == message
                })
            );
        }
        let request = store
            .context(
                "typescript-grpc",
                "proto-type://phase7.checkout.v1.InitializeOrderRequest",
            )
            .unwrap();
        assert!(request.edges.iter().any(|edge| {
            edge.kind == RelationKind::BindsContract
                && edge.from
                    == format!(
                        "repo://{client_identity}/typescript/src/messages.generated/InitializeOrderRequest"
                    )
                && edge.to == "proto-type://phase7.checkout.v1.InitializeOrderRequest"
        }));

        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[tokio::test]
    async fn reconciliation_recovers_a_missed_event() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-reconcile-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        let source = repository.join("src/lib.rs");
        fs::write(&source, "fn caller() { before(); } fn before() {}").unwrap();

        let store = Arc::new(SemanticStore::persistent(&state.join("beholder.db"), true).unwrap());
        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let identity = &workspace.repositories[0].repository.identity;
        let caller = format!("repo://{identity}/rust/lib/caller");
        let after = format!("repo://{identity}/rust/lib/after");
        let registry = Arc::new(Mutex::new(registry));
        let scheduler = Arc::new(IndexScheduler::new(state.join("frontend-cache")));
        scheduler.index(&store, &workspace).unwrap();
        fs::write(&source, "fn caller() { after(); } fn after() {}").unwrap();

        let task = tokio::spawn(scheduler.clone().run_with_reconciliation_period(
            store.clone(),
            registry,
            Duration::from_millis(10),
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if format!("{:?}", store.context("main", &caller).unwrap()).contains(&after) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("periodic reconciliation did not recover the missed event");
        scheduler.stop();
        task.await.unwrap();
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }
}
