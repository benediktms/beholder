use crate::workspace_registry::WorkspaceRegistry;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_protobuf::{
    FRONTEND_VERSION as PROTOBUF_FRONTEND_VERSION, facts as protobuf_facts,
};
use beholder_adapters_treesitter_elixir::{
    ElixirAnalysis, FRONTEND_VERSION as ELIXIR_FRONTEND_VERSION,
    RESOLVER_VERSION as ELIXIR_RESOLVER_VERSION, diagnostics_from_analysis as elixir_diagnostics,
    entities_from_analysis as elixir_entities, generated_entities as elixir_generated_entities,
    generated_observations as elixir_generated_observations,
    observations_from_analysis as elixir_observations,
    resolve_repository_calls as resolve_elixir_repository_calls, resolve_workspace_modules,
};
use beholder_adapters_treesitter_rust::{
    FRONTEND_VERSION, RESOLVER_VERSION, RustAnalysis,
    diagnostics_from_analysis as rust_diagnostics, entities_from_analysis as rust_entities,
    observations_from_analysis, resolve_repository_calls as resolve_rust_repository_calls,
};
use beholder_domain::{EntityFact, RepositoryFacts, RepositoryState, Workspace, WorkspaceView};
use beholder_dto::{Freshness, GarbageCollection, QueryMetadata};
use notify::{Event, EventKind};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, MissedTickBehavior},
};

#[path = "cache.rs"]
mod cache;
#[path = "elixir_analysis.rs"]
mod elixir_analysis;
#[path = "pipeline.rs"]
mod pipeline;
#[path = "rust_analysis.rs"]
mod rust_analysis;
#[path = "sources.rs"]
mod sources;
use cache::{RepositoryAnalysis, RepositoryAnalysisKey, SourceAnalysisKey};
use sources::{RepositorySources, repository_sources};

const QUIET_PERIOD: Duration = Duration::from_millis(200);
const MAX_LATENCY: Duration = Duration::from_secs(2);
const RECONCILIATION_PERIOD: Duration = Duration::from_secs(60);
const CORE_RULE_PACK_VERSION: &str = "5";

#[derive(Clone, Copy)]
struct AnalysisVersions {
    rust_frontend: &'static str,
    rust_resolver: &'static str,
    elixir_frontend: &'static str,
    elixir_resolver: &'static str,
    protobuf_frontend: &'static str,
    rule_pack: &'static str,
}

const CURRENT_ANALYSIS_VERSIONS: AnalysisVersions = AnalysisVersions {
    rust_frontend: FRONTEND_VERSION,
    rust_resolver: RESOLVER_VERSION,
    elixir_frontend: ELIXIR_FRONTEND_VERSION,
    elixir_resolver: ELIXIR_RESOLVER_VERSION,
    protobuf_frontend: PROTOBUF_FRONTEND_VERSION,
    rule_pack: CORE_RULE_PACK_VERSION,
};

impl AnalysisVersions {
    fn repository_key(
        self,
        fingerprint: String,
        has_rust: bool,
        has_elixir: bool,
        has_protobuf: bool,
    ) -> RepositoryAnalysisKey {
        RepositoryAnalysisKey {
            fingerprint,
            rust: has_rust.then_some((self.rust_frontend, self.rust_resolver)),
            elixir: has_elixir.then_some((self.elixir_frontend, self.elixir_resolver)),
            protobuf: has_protobuf.then_some(self.protobuf_frontend),
        }
    }

    fn workspace_identity(self, repositories: &[RepositorySources]) -> String {
        let key = self.repository_key(
            String::new(),
            repositories.iter().any(|sources| !sources.rust.is_empty()),
            repositories
                .iter()
                .any(|sources| !sources.elixir.is_empty()),
            repositories
                .iter()
                .any(|sources| !sources.protobuf.is_empty()),
        );
        format!("{}:core-rules:{}", key.analysis_identity(), self.rule_pack)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

type Cached<T> = Result<(Arc<T>, CacheStatus), Box<dyn Error>>;

pub struct IndexScheduler {
    generations: Mutex<BTreeMap<String, u64>>,
    dirty_repositories: Mutex<BTreeMap<String, BTreeMap<String, DirtyRepository>>>,
    active_workspace: Mutex<Option<String>>,
    idle: Condvar,
    changed: Notify,
    shutdown: Notify,
    cache_dir: PathBuf,
    // ponytail: memory and disk caches are unbounded; evict after measured daemon pressure.
    rust_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<RustAnalysis>>>,
    elixir_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<ElixirAnalysis>>>,
    repository_cache: Mutex<BTreeMap<RepositoryAnalysisKey, Arc<RepositoryAnalysis>>>,
    analysis_pool: rayon::ThreadPool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DirtyRepository {
    All,
    Sources(BTreeSet<PathBuf>),
}

struct ActiveIndex<'a> {
    active_workspace: &'a Mutex<Option<String>>,
    idle: &'a Condvar,
}

impl Drop for ActiveIndex<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_workspace.lock() {
            *active = None;
            self.idle.notify_all();
        }
    }
}

impl IndexScheduler {
    pub fn new(cache_dir: PathBuf) -> Self {
        let workers = std::env::var("BEHOLDER_INDEX_WORKERS")
            .ok()
            .and_then(|workers| workers.parse().ok())
            .filter(|workers| *workers > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(4)
            });
        Self::with_workers(cache_dir, workers)
    }

    fn with_workers(cache_dir: PathBuf, workers: usize) -> Self {
        let analysis_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("beholder-index-{index}"))
            .build()
            .expect("bounded indexing pool should start");
        tracing::info!(workers, "index analysis pool configured");
        Self {
            generations: Mutex::new(BTreeMap::new()),
            dirty_repositories: Mutex::new(BTreeMap::new()),
            active_workspace: Mutex::new(None),
            idle: Condvar::new(),
            changed: Notify::new(),
            shutdown: Notify::new(),
            cache_dir,
            rust_cache: Mutex::new(BTreeMap::new()),
            elixir_cache: Mutex::new(BTreeMap::new()),
            repository_cache: Mutex::new(BTreeMap::new()),
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

    pub fn query_metadata(&self, workspace: &str, analysis_revision: u64) -> QueryMetadata {
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
            .active_workspace
            .lock()
            .is_ok_and(|active| active.as_deref() == Some(workspace));
        QueryMetadata {
            revision: analysis_revision,
            view: workspace.into(),
            freshness: Freshness {
                stale,
                indexing,
                dirty_repositories,
            },
        }
    }

    pub fn clear_cache(&self) -> Result<(), Box<dyn Error>> {
        let _active = self.begin("cache clear")?;
        self.rust_cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .clear();
        self.elixir_cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .clear();
        self.repository_cache
            .lock()
            .map_err(|_| "repository cache lock poisoned")?
            .clear();
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        Ok(())
    }

    pub fn garbage_collect(
        &self,
        store: &SemanticStore,
    ) -> Result<GarbageCollection, Box<dyn Error>> {
        let _active = self.begin("garbage collection")?;
        store.garbage_collect()
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

    fn begin(&self, workspace: &str) -> Result<ActiveIndex<'_>, Box<dyn Error>> {
        let mut active = self
            .active_workspace
            .lock()
            .map_err(|_| "active workspace lock poisoned")?;
        while active.is_some() {
            active = self
                .idle
                .wait(active)
                .map_err(|_| "active workspace lock poisoned")?;
        }
        *active = Some(workspace.into());
        Ok(ActiveIndex {
            active_workspace: &self.active_workspace,
            idle: &self.idle,
        })
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
                                    let source = relative.extension().is_some_and(|extension| {
                                        matches!(extension.to_str(), Some("rs" | "ex" | "exs"))
                                    }) && !relative.components().any(|component| {
                                        matches!(
                                            component.as_os_str().to_str(),
                                            Some(
                                                ".git"
                                                    | "target"
                                                    | "_build"
                                                    | "deps"
                                                    | "node_modules"
                                            )
                                        )
                                    });
                                    source
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
        self.run_with_reconciliation_period(store, workspaces, RECONCILIATION_PERIOD)
            .await;
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
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
        loop {
            let dirty = tokio::select! {
                _ = self.changed.notified() => true,
                _ = reconciliation.tick() => self.mark_registered(&workspaces),
                _ = self.shutdown.notified() => return,
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
                    _ = self.shutdown.notified() => return,
                }
            }
            let scheduler = self.clone();
            let store = store.clone();
            let workspaces = workspaces.clone();
            if let Err(error) =
                tokio::task::spawn_blocking(move || scheduler.reindex_dirty(&store, &workspaces))
                    .await
            {
                tracing::error!(%error, "index worker failed");
            }
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

    fn rust_analysis_versioned(
        &self,
        source: &str,
        frontend_version: &'static str,
    ) -> Cached<RustAnalysis> {
        rust_analysis::analysis_versioned(self, source, frontend_version)
    }

    fn elixir_analysis_versioned(
        &self,
        source: &str,
        frontend_version: &'static str,
    ) -> Cached<ElixirAnalysis> {
        elixir_analysis::analysis_versioned(self, source, frontend_version)
    }

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

    fn repository_observations_versioned(
        &self,
        state: &RepositoryState,
        rust_sources: &[(PathBuf, String)],
        elixir_sources: &[(PathBuf, String)],
        descriptors: &[(PathBuf, Vec<u8>)],
        versions: AnalysisVersions,
    ) -> Result<(Arc<RepositoryAnalysis>, CacheStatus, String), Box<dyn Error>> {
        let key = versions.repository_key(
            state.fingerprint.clone(),
            !rust_sources.is_empty(),
            !elixir_sources.is_empty(),
            !descriptors.is_empty(),
        );
        let analysis_identity = key.analysis_identity();
        if let Some(analysis) = self
            .repository_cache
            .lock()
            .map_err(|_| "repository cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            tracing::debug!(repository = %state.repository.identity, cache_status = "memory", "repository cache lookup");
            return Ok((analysis, CacheStatus::Memory, analysis_identity));
        }
        let (rust_frontend, rust_resolver) = key.rust.unwrap_or(("_", "_"));
        let (elixir_frontend, elixir_resolver) = key.elixir.unwrap_or(("_", "_"));
        let path = self
            .cache_dir
            .join("repository")
            .join("semantic")
            .join(rust_frontend)
            .join(rust_resolver)
            .join(elixir_frontend)
            .join(elixir_resolver)
            .join(key.protobuf.unwrap_or("_"))
            .join(format!("{}.json", state.fingerprint));
        if let Ok(bytes) = fs::read(&path)
            && let Ok(analysis) = serde_json::from_slice::<RepositoryAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.repository_cache
                .lock()
                .map_err(|_| "repository cache lock poisoned")?
                .insert(key, analysis.clone());
            tracing::debug!(repository = %state.repository.identity, cache_status = "disk", "repository cache lookup");
            return Ok((analysis, CacheStatus::Disk, analysis_identity));
        }
        let mut observations = Vec::new();
        let mut entities = Vec::<EntityFact>::new();
        let mut diagnostics = Vec::new();
        let rust_analyses = self.analysis_pool.install(|| {
            rust_sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, cache_status) = self
                        .rust_analysis_versioned(source, versions.rust_frontend)
                        .map_err(|error| {
                            format!("failed to analyze {}: {error}", path.display())
                        })?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    Ok::<_, String>((
                        observations_from_analysis(&state.repository.identity, &analysis, path),
                        rust_entities(&state.repository.identity, &analysis, path),
                        rust_diagnostics(&analysis, path),
                    ))
                })
                .collect::<Vec<_>>()
        });
        for analysis in rust_analyses {
            let (source_observations, source_entities, source_diagnostics) = analysis?;
            observations.extend(source_observations);
            entities.extend(source_entities);
            diagnostics.extend(source_diagnostics);
        }
        resolve_rust_repository_calls(&mut observations);
        let mut elixir_analyses = Vec::new();
        let analyzed_elixir = self.analysis_pool.install(|| {
            elixir_sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, cache_status) = self
                        .elixir_analysis_versioned(source, versions.elixir_frontend)
                        .map_err(|error| {
                            format!("failed to analyze {}: {error}", path.display())
                        })?;
                    tracing::debug!(
                        repository = %state.repository.identity,
                        path = %path.display(),
                        ?cache_status,
                        "frontend cache lookup"
                    );
                    Ok::<_, String>((
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
        let analysis = Arc::new(RepositoryAnalysis {
            entities,
            grpc_bindings: Vec::new(),
            observations,
            diagnostics,
        });
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
        {
            let _ = fs::write(path, bytes);
        }
        self.repository_cache
            .lock()
            .map_err(|_| "repository cache lock poisoned")?
            .insert(key, analysis.clone());
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
    index_workspace_versioned(
        scheduler,
        store,
        workspace,
        dirty,
        CURRENT_ANALYSIS_VERSIONS,
    )
}

fn index_workspace_versioned(
    scheduler: &IndexScheduler,
    store: &SemanticStore,
    workspace: &Workspace,
    dirty: Option<&BTreeMap<String, DirtyRepository>>,
    versions: AnalysisVersions,
) -> Result<(usize, bool), Box<dyn Error>> {
    let source_loading_started = Instant::now();
    let repositories = workspace
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

    let mut repository_facts = Vec::new();
    let mut memory_hits = 0;
    let mut disk_hits = 0;
    let mut misses = 0;
    let mut dirty_source_units = 0;
    let mut diagnostics = Vec::new();
    let repository_analysis_started = Instant::now();
    for sources in repositories {
        let RepositorySources {
            state,
            rust,
            elixir,
            protobuf,
        } = sources;
        dirty_source_units +=
            match dirty.and_then(|repositories| repositories.get(&state.repository.identity)) {
                Some(DirtyRepository::Sources(sources)) => sources.len(),
                Some(DirtyRepository::All) | None => rust.len() + elixir.len() + protobuf.len(),
            };
        let (analysis, cache_status, analysis_identity) = scheduler
            .repository_observations_versioned(&state, &rust, &elixir, &protobuf, versions)?;
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
        repository_facts.push(RepositoryFacts {
            state,
            analysis_identity,
            entities: analysis.entities.clone(),
            grpc_bindings: analysis.grpc_bindings.clone(),
            observations: analysis.observations.clone(),
        });
    }
    let repository_analysis = repository_analysis_started.elapsed();
    let workspace_resolution_started = Instant::now();
    let mut all_observations = repository_facts
        .iter()
        .flat_map(|facts| facts.observations.iter().cloned())
        .collect::<Vec<_>>();
    let mut overrides = resolve_rust_repository_calls(&mut all_observations);
    overrides.extend(resolve_workspace_modules(&all_observations));
    let workspace_resolution = workspace_resolution_started.elapsed();
    let publication_started = Instant::now();
    let changes = store.publish(&view, &repository_facts, &overrides)?;
    let publication = publication_started.elapsed();
    let checkpoint_started = Instant::now();
    if let Err(error) = store.checkpoint() {
        tracing::warn!(workspace = %workspace.name, %error, "Mnestic checkpoint failed");
    }
    let checkpoint = checkpoint_started.elapsed();
    pipeline::report_analysis_diagnostics(&workspace.name, &diagnostics);
    tracing::info!(
        workspace = %workspace.name,
        observation_count = all_observations.len(),
        facts_inserted = changes.inserted,
        facts_updated = changes.updated,
        facts_removed = changes.removed,
        facts_unchanged = changes.unchanged,
        repository_cache_memory_hits = memory_hits,
        repository_cache_disk_hits = disk_hits,
        repository_cache_misses = misses,
        dirty_source_units,
        source_loading_ms = source_loading.as_secs_f64() * 1000.0,
        repository_analysis_ms = repository_analysis.as_secs_f64() * 1000.0,
        workspace_resolution_ms = workspace_resolution.as_secs_f64() * 1000.0,
        publication_ms = publication.as_secs_f64() * 1000.0,
        checkpoint_ms = checkpoint.as_secs_f64() * 1000.0,
        "workspace indexed"
    );
    Ok((all_observations.len(), true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, WorkspaceRepository};
    use std::time::SystemTime;

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
    fn repository_cache_ignores_versions_for_absent_languages() {
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
                &sources,
                &[],
                &[],
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
            .repository_observations_versioned(&state, &sources, &[], &[], irrelevant_versions)
            .unwrap();

        assert_eq!(first_status, CacheStatus::Miss);
        assert_eq!(second_status, CacheStatus::Memory);
        assert_eq!(first_identity, second_identity);
        assert_eq!(
            first_identity,
            format!("rust:{FRONTEND_VERSION}:{RESOLVER_VERSION}")
        );
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.observations.iter().any(|observation| {
            observation.from.as_str() == "repo://repo/rust/lib/caller"
                && observation.to.as_str() == "repo://repo/rust/lib/helper"
        }));

        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .repository_observations_versioned(&state, &sources, &[], &[], irrelevant_versions,)
                .unwrap()
                .1,
            CacheStatus::Disk
        );
        assert_eq!(
            scheduler
                .repository_observations_versioned(
                    &state,
                    &sources,
                    &[],
                    &[],
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
        assert!(scheduler.repository_cache.lock().unwrap().is_empty());
        assert!(!cache.exists());
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
                    &[],
                    &sources,
                    &[],
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
            protobuf_frontend: "protobuf-1",
            rule_pack: "rules-1",
        };
        let changed = AnalysisVersions {
            rust_frontend: "rust-2",
            rust_resolver: "rust-resolver-2",
            elixir_frontend: "elixir-2",
            elixir_resolver: "elixir-resolver-2",
            protobuf_frontend: "protobuf-2",
            ..versions
        };

        for (languages, expected) in [
            ((true, false, false), "rust:rust-1:rust-resolver-1"),
            ((false, true, false), "elixir:elixir-1:elixir-resolver-1"),
            ((false, false, true), "protobuf:protobuf-1"),
            ((false, false, false), "none"),
        ] {
            let (rust, elixir, protobuf) = languages;
            let key = versions.repository_key("state".into(), rust, elixir, protobuf);
            assert_eq!(key.analysis_identity(), expected);
            if rust || elixir || protobuf {
                assert_ne!(
                    key,
                    changed.repository_key("state".into(), rust, elixir, protobuf)
                );
            }
        }

        assert_eq!(
            versions.repository_key("state".into(), true, false, false),
            AnalysisVersions {
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), true, false, false)
        );
        assert_eq!(
            versions.repository_key("state".into(), false, true, false),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                protobuf_frontend: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), false, true, false)
        );
        assert_eq!(
            versions.repository_key("state".into(), false, false, true),
            AnalysisVersions {
                rust_frontend: "irrelevant",
                rust_resolver: "irrelevant",
                elixir_frontend: "irrelevant",
                elixir_resolver: "irrelevant",
                rule_pack: "irrelevant",
                ..versions
            }
            .repository_key("state".into(), false, false, true)
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
            protobuf_frontend: "1",
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
            protobuf_frontend: "1",
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
        assert_eq!(scheduler.repository_cache.lock().unwrap().len(), 2);
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
        let pending = scheduler.query_metadata("main", 4);
        assert_eq!(pending.revision, 4);
        assert!(pending.freshness.stale);
        assert!(!pending.freshness.indexing);
        assert_eq!(pending.freshness.dirty_repositories, ["repo"]);

        *scheduler.active_workspace.lock().unwrap() = Some("main".into());
        assert!(scheduler.query_metadata("main", 4).freshness.indexing);
        assert!(!scheduler.query_metadata("other", 4).freshness.indexing);
        *scheduler.active_workspace.lock().unwrap() = None;

        let indexed_generation = scheduler.generations.lock().unwrap()["main"];
        scheduler.mark(&workspace);
        scheduler.complete_generation("main", Some(indexed_generation));
        assert!(scheduler.query_metadata("main", 4).freshness.stale);

        scheduler.index(&store, &workspace).unwrap();
        let current = scheduler.query_metadata("main", 5);
        assert!(!current.freshness.stale);
        assert!(!current.freshness.indexing);
        assert!(current.freshness.dirty_repositories.is_empty());
        assert!(scheduler.query_metadata("other", 4).freshness.stale);
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn concurrent_index_claims_wait_for_the_active_job() {
        let scheduler = Arc::new(IndexScheduler::new(PathBuf::new()));
        let active = scheduler.begin("first").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let waiting = scheduler.clone();
        let thread = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _active = waiting.begin("second").unwrap();
            claimed_tx.send(()).unwrap();
        });

        ready_rx.recv().unwrap();
        assert!(claimed_rx.recv_timeout(Duration::from_millis(25)).is_err());
        drop(active);
        claimed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
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
        assert_eq!(scheduler.rust_cache.lock().unwrap().len(), 2);

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
        assert_eq!(scheduler.rust_cache.lock().unwrap().len(), 3);
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
                .query_metadata("main", 2)
                .freshness
                .dirty_repositories
                .is_empty()
        );
        drop(store);
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
        assert_eq!(scheduler.elixir_cache.lock().unwrap().len(), 2);

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
        assert_eq!(scheduler.elixir_cache.lock().unwrap().len(), 3);
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
