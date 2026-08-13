use crate::workspace_registry::WorkspaceRegistry;
use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{observations, resolve_repository_calls, source_files};
use beholder_domain::{RepositoryState, Workspace, WorkspaceView};
use notify::{Event, EventKind};
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

pub struct IndexScheduler {
    generations: Mutex<BTreeMap<String, u64>>,
    changed: Notify,
    indexing: Mutex<()>,
}

impl IndexScheduler {
    pub fn new() -> Self {
        Self {
            generations: Mutex::new(BTreeMap::new()),
            changed: Notify::new(),
            indexing: Mutex::new(()),
        }
    }

    pub fn mark(&self, workspace: String) {
        if let Ok(mut generations) = self.generations.lock() {
            *generations.entry(workspace).or_default() += 1;
            self.changed.notify_one();
        }
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
        index_rust_workspace(store, workspace)
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
            match index_rust_workspace(store, &workspace) {
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
        for (path, source) in sources {
            all_observations.extend(observations(&state.repository.identity, &source, &path)?);
        }
    }
    resolve_repository_calls(&mut all_observations);
    store.publish(&view, &all_observations)?;
    Ok((all_observations.len(), true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

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
        let scheduler = Arc::new(IndexScheduler::new());
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
