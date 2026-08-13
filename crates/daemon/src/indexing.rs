use crate::workspace_registry::WorkspaceRegistry;
use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{
    FRONTEND_VERSION, RESOLVER_VERSION, RustAnalysis, analyze, observations_from_analysis,
    resolve_repository_calls, source_files,
};
use beholder_domain::{Observation, RepositoryState, Workspace, WorkspaceView};
use notify::{Event, EventKind};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, MissedTickBehavior},
};

const QUIET_PERIOD: Duration = Duration::from_millis(200);
const MAX_LATENCY: Duration = Duration::from_secs(2);
const RECONCILIATION_PERIOD: Duration = Duration::from_secs(60);
type RustSources = Vec<(PathBuf, String)>;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceAnalysisKey {
    content_hash: [u8; 32],
    frontend_version: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RepositoryAnalysisKey {
    fingerprint: String,
    frontend_version: &'static str,
    resolver_version: &'static str,
}

impl SourceAnalysisKey {
    fn rust(source: &str, frontend_version: &'static str) -> Self {
        Self {
            content_hash: Sha256::digest(source.as_bytes()).into(),
            frontend_version,
        }
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
    changed: Notify,
    indexing: Mutex<()>,
    cache_dir: PathBuf,
    // ponytail: memory and disk caches are unbounded; evict after measured daemon pressure.
    rust_cache: Mutex<BTreeMap<SourceAnalysisKey, Arc<RustAnalysis>>>,
    repository_cache: Mutex<BTreeMap<RepositoryAnalysisKey, Arc<Vec<Observation>>>>,
}

impl IndexScheduler {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            generations: Mutex::new(BTreeMap::new()),
            changed: Notify::new(),
            indexing: Mutex::new(()),
            cache_dir,
            rust_cache: Mutex::new(BTreeMap::new()),
            repository_cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn mark(&self, workspace: String) {
        if let Ok(mut generations) = self.generations.lock() {
            *generations.entry(workspace).or_default() += 1;
            self.changed.notify_one();
        }
    }

    pub fn clear_cache(&self) -> Result<(), Box<dyn Error>> {
        let _indexing = self
            .indexing
            .lock()
            .map_err(|_| "index coordinator lock poisoned")?;
        self.rust_cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
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

    pub fn index(
        &self,
        store: &SemanticStore,
        workspace: &Workspace,
    ) -> Result<(usize, bool), Box<dyn Error>> {
        let _indexing = self
            .indexing
            .lock()
            .map_err(|_| "index coordinator lock poisoned")?;
        index_rust_workspace(self, store, workspace)
    }

    pub fn add_event(&self, event: notify::Result<Event>, workspaces: &Mutex<WorkspaceRegistry>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                eprintln!("filesystem watcher error: {error}");
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
        let mut changed = false;
        // ponytail: linear root matching and full-workspace reindex; add per-source invalidation when dogfood shows it matters.
        for workspace in registry.list() {
            if event.paths.iter().any(|path| {
                workspace.repositories.iter().any(|root| {
                    path.strip_prefix(root).is_ok_and(|relative| {
                        relative
                            .extension()
                            .is_some_and(|extension| extension == "rs")
                            && !relative.components().any(|component| {
                                matches!(component.as_os_str().to_str(), Some(".git" | "target"))
                            })
                    })
                })
            }) {
                *generations.entry(workspace.name).or_default() += 1;
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
                }
            }
            self.reindex_dirty(&store, &workspaces);
        }
    }

    fn mark_registered(&self, workspaces: &Mutex<WorkspaceRegistry>) -> bool {
        let Ok(workspaces) = workspaces.lock() else {
            return false;
        };
        let Ok(mut generations) = self.generations.lock() else {
            return false;
        };
        let mut dirty = false;
        for workspace in workspaces.list() {
            *generations.entry(workspace.name).or_default() += 1;
            dirty = true;
        }
        dirty
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
        let Ok(_indexing) = self.indexing.lock() else {
            return;
        };
        let mut completed = BTreeSet::new();
        for workspace in registered {
            match index_rust_workspace(self, store, &workspace) {
                Ok(_) => {
                    completed.insert(workspace.name);
                }
                Err(error) => eprintln!("failed to reindex workspace {}: {error}", workspace.name),
            }
        }
        if let Ok(mut generations) = self.generations.lock() {
            generations.retain(|name, generation| {
                !completed.contains(name)
                    || snapshot
                        .get(name)
                        .is_none_or(|indexed| *generation > *indexed)
            });
            if generations.iter().any(|(name, generation)| {
                snapshot
                    .get(name)
                    .is_none_or(|indexed| generation > indexed)
            }) {
                self.changed.notify_one();
            }
        }
    }

    fn rust_analysis_versioned(
        &self,
        source: &str,
        frontend_version: &'static str,
    ) -> Cached<RustAnalysis> {
        let key = SourceAnalysisKey::rust(source, frontend_version);
        if let Some(analysis) = self
            .rust_cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let path = self.cache_path(&key);
        if let Ok(bytes) = fs::read(&path)
            && let Ok(analysis) = serde_json::from_slice::<RustAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.rust_cache
                .lock()
                .map_err(|_| "Rust frontend cache lock poisoned")?
                .insert(key, analysis.clone());
            return Ok((analysis, CacheStatus::Disk));
        }
        let analysis = Arc::new(analyze(source)?);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
        {
            let _ = fs::write(path, bytes);
        }
        let analysis = self
            .rust_cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .entry(key)
            .or_insert_with(|| analysis.clone())
            .clone();
        Ok((analysis, CacheStatus::Miss))
    }

    fn cache_path(&self, key: &SourceAnalysisKey) -> PathBuf {
        let hash = key
            .content_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.cache_dir
            .join("rust")
            .join(key.frontend_version)
            .join(format!("{hash}.json"))
    }

    fn repository_observations(
        &self,
        state: &RepositoryState,
        sources: &RustSources,
    ) -> Cached<Vec<Observation>> {
        self.repository_observations_versioned(state, sources, FRONTEND_VERSION, RESOLVER_VERSION)
    }

    fn repository_observations_versioned(
        &self,
        state: &RepositoryState,
        sources: &RustSources,
        frontend_version: &'static str,
        resolver_version: &'static str,
    ) -> Cached<Vec<Observation>> {
        let key = RepositoryAnalysisKey {
            fingerprint: state.fingerprint.clone(),
            frontend_version,
            resolver_version,
        };
        if let Some(observations) = self
            .repository_cache
            .lock()
            .map_err(|_| "repository cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((observations, CacheStatus::Memory));
        }
        let path = self
            .cache_dir
            .join("repository")
            .join("rust")
            .join(frontend_version)
            .join(resolver_version)
            .join(format!("{}.json", state.fingerprint));
        if let Ok(bytes) = fs::read(&path)
            && let Ok(observations) = serde_json::from_slice::<Vec<Observation>>(&bytes)
        {
            let observations = Arc::new(observations);
            self.repository_cache
                .lock()
                .map_err(|_| "repository cache lock poisoned")?
                .insert(key, observations.clone());
            return Ok((observations, CacheStatus::Disk));
        }
        let mut observations = Vec::new();
        for (path, source) in sources {
            let (analysis, _) = self
                .rust_analysis_versioned(source, frontend_version)
                .map_err(|error| format!("failed to analyze {}: {error}", path.display()))?;
            observations.extend(observations_from_analysis(
                &state.repository.identity,
                &analysis,
                path,
            ));
        }
        resolve_repository_calls(&mut observations);
        let observations = Arc::new(observations);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(observations.as_ref())
        {
            let _ = fs::write(path, bytes);
        }
        self.repository_cache
            .lock()
            .map_err(|_| "repository cache lock poisoned")?
            .insert(key, observations.clone());
        Ok((observations, CacheStatus::Miss))
    }
}

fn rust_repository_sources(root: &Path) -> Result<(RepositoryState, RustSources), Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("repository does not exist: {}", root.display()).into());
    }
    let mut files = Vec::new();
    source_files(root, &mut files)?;
    files.sort();
    let sources = files
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            Ok((relative_path, fs::read_to_string(path)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok((repository_state(root, &sources)?, sources))
}

fn index_rust_workspace(
    scheduler: &IndexScheduler,
    store: &SemanticStore,
    workspace: &Workspace,
) -> Result<(usize, bool), Box<dyn Error>> {
    let repositories = workspace
        .repositories
        .iter()
        .map(|root| rust_repository_sources(root))
        .collect::<Result<Vec<_>, _>>()?;
    let view = WorkspaceView::new(
        &workspace.name,
        repositories
            .iter()
            .map(|(state, _)| state.clone())
            .collect(),
    )?;
    if store.view_matches(&view)? {
        return Ok((0, false));
    }

    let mut all_observations = Vec::new();
    for (state, sources) in repositories {
        let (observations, _) = scheduler.repository_observations(&state, &sources)?;
        all_observations.extend(observations.iter().cloned());
    }
    resolve_repository_calls(&mut all_observations);
    store.publish(&view, &all_observations)?;
    Ok((all_observations.len(), true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::LogicalRepository;
    use std::time::SystemTime;

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
            observations_from_analysis("one", &first, Path::new("src/one.rs"))[0].to,
            "repo://one/rust/one/shared"
        );
        assert_eq!(
            observations_from_analysis("two", &second, Path::new("src/two.rs"))[0].to,
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
    fn repository_cache_reuses_resolved_observations_and_invalidates_versions() {
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
        let (first, first_status) = scheduler.repository_observations(&state, &sources).unwrap();
        let (second, second_status) = scheduler.repository_observations(&state, &sources).unwrap();

        assert_eq!(first_status, CacheStatus::Miss);
        assert_eq!(second_status, CacheStatus::Memory);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.iter().any(|observation| {
            observation.from == "repo://repo/rust/lib/caller"
                && observation.to == "repo://repo/rust/lib/helper"
        }));

        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .repository_observations(&state, &sources)
                .unwrap()
                .1,
            CacheStatus::Disk
        );
        let versioned_state = RepositoryState {
            fingerprint: "versioned-state".into(),
            ..state
        };
        assert_eq!(
            scheduler
                .repository_observations_versioned(
                    &versioned_state,
                    &sources,
                    FRONTEND_VERSION,
                    "old",
                )
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        drop(scheduler);
        let scheduler = IndexScheduler::new(cache.clone());
        assert_eq!(
            scheduler
                .repository_observations(&versioned_state, &sources)
                .unwrap()
                .1,
            CacheStatus::Miss
        );
        scheduler.clear_cache().unwrap();
        assert!(scheduler.rust_cache.lock().unwrap().is_empty());
        assert!(scheduler.repository_cache.lock().unwrap().is_empty());
        assert!(!cache.exists());
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
        let workspace = registry.register("main".into(), vec![repository]).unwrap();
        let registry = Arc::new(Mutex::new(registry));
        let scheduler = Arc::new(IndexScheduler::new(state.join("frontend-cache")));
        scheduler.index(&store, &workspace).unwrap();
        fs::write(&source, "fn caller() { after(); } fn after() {}").unwrap();

        let task = tokio::spawn(scheduler.run_with_reconciliation_period(
            store.clone(),
            registry,
            Duration::from_millis(10),
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if format!(
                    "{:?}",
                    store
                        .context("main", "repo://repo/rust/lib/caller")
                        .unwrap()
                )
                .contains("repo://repo/rust/lib/after")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("periodic reconciliation did not recover the missed event");
        task.abort();
        let _ = task.await;
        drop(store);
        fs::remove_dir_all(state).unwrap();
    }
}
