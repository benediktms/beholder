use super::indexing::IndexScheduler;
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::Workspace;
use beholder_dto::{GarbageCollectionPhase, GarbageCollectionProgress};
use beholder_indexing::Indexer;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::oneshot;

pub(super) struct BeholderDaemon {
    pub(super) store: Arc<SemanticStore>,
    pub(super) workspaces: Arc<Mutex<super::workspace_registry::WorkspaceRegistry>>,
    pub(super) scheduler: Arc<IndexScheduler>,
    pub(super) garbage_collector_running: Arc<AtomicBool>,
    pub(super) garbage_collection_progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
    pub(super) watcher: Mutex<WorkspaceWatcher>,
    pub(super) shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

pub(super) type DaemonParts = (BeholderDaemon, oneshot::Receiver<()>, Arc<IndexScheduler>);

pub(super) fn build(
    store: SemanticStore,
    workspaces: super::workspace_registry::WorkspaceRegistry,
    indexer: Indexer,
) -> Result<DaemonParts, Box<dyn Error>> {
    let (shutdown, stopped) = oneshot::channel();
    let workspaces = Arc::new(Mutex::new(workspaces));
    let scheduler = Arc::new(IndexScheduler::with_indexer(indexer));
    let callback_workspaces = workspaces.clone();
    let callback_scheduler = scheduler.clone();
    let watcher = notify::recommended_watcher(move |event| {
        callback_scheduler.add_event(event, &callback_workspaces);
    })?;
    let mut watcher = WorkspaceWatcher::new(watcher);
    let registered = workspaces
        .lock()
        .map_err(|_| "workspace registry lock poisoned")?
        .list();
    for workspace in &registered {
        watcher.update(None, workspace)?;
    }
    for workspace in registered {
        scheduler.mark(&workspace);
    }
    let store = Arc::new(store);
    let garbage_collector_running = Arc::new(AtomicBool::new(false));
    let garbage_collection_progress = Arc::new(Mutex::new(None));
    start_garbage_collector(
        store.clone(),
        scheduler.clone(),
        garbage_collector_running.clone(),
        garbage_collection_progress.clone(),
    )?;
    Ok((
        BeholderDaemon {
            store,
            workspaces,
            scheduler: scheduler.clone(),
            garbage_collector_running,
            garbage_collection_progress,
            watcher: Mutex::new(watcher),
            shutdown: Mutex::new(Some(shutdown)),
        },
        stopped,
        scheduler,
    ))
}

pub(super) fn start_garbage_collector(
    store: Arc<SemanticStore>,
    scheduler: Arc<IndexScheduler>,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
) -> Result<(), Box<dyn Error>> {
    if !store.garbage_collection_pending()? {
        return Ok(());
    }
    if running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    let restart_store = store.clone();
    let restart_scheduler = scheduler.clone();
    let restart_running = running.clone();
    let restart_progress = progress.clone();
    let worker_running = running.clone();
    let spawn = std::thread::Builder::new()
        .name("beholder-garbage-collector".into())
        .spawn(move || {
            if let Ok(mut current) = progress.lock() {
                *current = Some(GarbageCollectionProgress::phase(
                    GarbageCollectionPhase::SweepingObsoleteStates,
                ));
            }
            let result = scheduler.run_exclusive("garbage collection", || {
                store.sweep_garbage_collection(|update| {
                    if let Ok(mut current) = progress.lock() {
                        *current = Some(update.clone());
                    }
                    tracing::info!(
                        step = update.step,
                        completed_rows = update.completed_rows,
                        rows = update.rows,
                        "semantic store garbage collection progress"
                    );
                })
            });
            let pending = restart_store.garbage_collection_pending().unwrap_or(false);
            match result {
                Ok(states_resolved) => tracing::info!(
                    states_resolved,
                    "semantic store garbage collection sweep completed"
                ),
                Err(error) if pending => tracing::info!(
                    %error,
                    "semantic store garbage collection interrupted; retrying"
                ),
                Err(error) => tracing::error!(%error, "semantic store garbage collection failed"),
            }
            worker_running.store(false, Ordering::Release);
            if let Ok(mut current) = progress.lock() {
                *current = None;
            }
            if pending {
                let _ = start_garbage_collector(
                    restart_store,
                    restart_scheduler,
                    restart_running,
                    restart_progress,
                );
            }
        });
    if let Err(error) = spawn {
        running.store(false, Ordering::Release);
        return Err(error.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum WatchMode {
    NonRecursive,
    Recursive,
}

impl From<WatchMode> for RecursiveMode {
    fn from(mode: WatchMode) -> Self {
        match mode {
            WatchMode::NonRecursive => RecursiveMode::NonRecursive,
            WatchMode::Recursive => RecursiveMode::Recursive,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WatchOwners {
    non_recursive: usize,
    recursive: usize,
}

impl WatchOwners {
    fn mode(self) -> Option<WatchMode> {
        if self.recursive > 0 {
            Some(WatchMode::Recursive)
        } else if self.non_recursive > 0 {
            Some(WatchMode::NonRecursive)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default)]
struct WatchOwnership {
    targets: BTreeMap<std::path::PathBuf, WatchOwners>,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct WatchChanges {
    remove: Vec<std::path::PathBuf>,
    add: Vec<(std::path::PathBuf, WatchMode)>,
}

impl WatchOwnership {
    fn replace(
        &mut self,
        previous: &BTreeSet<(std::path::PathBuf, WatchMode)>,
        current: &BTreeSet<(std::path::PathBuf, WatchMode)>,
    ) -> WatchChanges {
        let before = self
            .targets
            .iter()
            .filter_map(|(path, owners)| owners.mode().map(|mode| (path.clone(), mode)))
            .collect::<BTreeMap<_, _>>();
        for (path, mode) in previous {
            let owners = self.targets.entry(path.clone()).or_default();
            match mode {
                WatchMode::NonRecursive => {
                    owners.non_recursive = owners.non_recursive.saturating_sub(1)
                }
                WatchMode::Recursive => owners.recursive = owners.recursive.saturating_sub(1),
            }
        }
        for (path, mode) in current {
            let owners = self.targets.entry(path.clone()).or_default();
            match mode {
                WatchMode::NonRecursive => owners.non_recursive += 1,
                WatchMode::Recursive => owners.recursive += 1,
            }
        }
        self.targets.retain(|_, owners| owners.mode().is_some());
        let after = self
            .targets
            .iter()
            .filter_map(|(path, owners)| owners.mode().map(|mode| (path.clone(), mode)))
            .collect::<BTreeMap<_, _>>();
        let mut changes = WatchChanges::default();
        for (path, mode) in &before {
            if after.get(path) != Some(mode) {
                changes.remove.push(path.clone());
            }
        }
        for (path, mode) in after {
            if before.get(&path) != Some(&mode) {
                changes.add.push((path, mode));
            }
        }
        changes
    }
}

pub(super) struct WorkspaceWatcher {
    watcher: RecommendedWatcher,
    ownership: WatchOwnership,
    workspaces: BTreeMap<String, BTreeSet<(std::path::PathBuf, WatchMode)>>,
}

impl WorkspaceWatcher {
    fn new(watcher: RecommendedWatcher) -> Self {
        Self {
            watcher,
            ownership: WatchOwnership::default(),
            workspaces: BTreeMap::new(),
        }
    }

    pub(super) fn update(
        &mut self,
        _previous: Option<&Workspace>,
        workspace: &Workspace,
    ) -> notify::Result<()> {
        let previous = self
            .workspaces
            .get(&workspace.name)
            .cloned()
            .unwrap_or_default();
        let current = workspace_watch_targets(workspace);
        let mut ownership = self.ownership.clone();
        let changes = ownership.replace(&previous, &current);
        for path in &changes.remove {
            self.watcher.unwatch(path)?;
        }
        for (path, mode) in &changes.add {
            self.watcher.watch(path, (*mode).into())?;
        }
        self.ownership = ownership;
        self.workspaces.insert(workspace.name.clone(), current);
        Ok(())
    }
}

fn workspace_watch_targets(workspace: &Workspace) -> BTreeSet<(std::path::PathBuf, WatchMode)> {
    let repository_roots = workspace
        .repositories
        .iter()
        .map(|repository| repository.base.clone())
        .collect::<BTreeSet<_>>();
    let mut targets = repository_roots
        .iter()
        .cloned()
        .map(|path| (path, WatchMode::Recursive))
        .collect::<BTreeSet<_>>();
    for repository in &workspace.repositories {
        let administrative =
            beholder_adapters_git::repository_watch_paths(&repository.base).unwrap_or_default();
        for target in administrative {
            if repository_roots
                .iter()
                .any(|root| target.path.starts_with(root))
            {
                continue;
            }
            targets.insert((
                target.path,
                if target.recursive {
                    WatchMode::Recursive
                } else {
                    WatchMode::NonRecursive
                },
            ));
        }
    }
    targets
}

#[cfg(test)]
mod watcher_tests {
    use super::*;

    #[test]
    fn shared_watch_targets_are_reference_counted() {
        let root = std::path::PathBuf::from("/workspace/shared");
        let targets = BTreeSet::from([(root.clone(), WatchMode::Recursive)]);
        let mut ownership = WatchOwnership::default();

        assert_eq!(
            ownership.replace(&BTreeSet::new(), &targets),
            WatchChanges {
                add: vec![(root.clone(), WatchMode::Recursive)],
                ..WatchChanges::default()
            }
        );
        assert_eq!(
            ownership.replace(&BTreeSet::new(), &targets),
            WatchChanges::default()
        );
        assert_eq!(
            ownership.replace(&targets, &BTreeSet::new()),
            WatchChanges::default()
        );
        assert_eq!(
            ownership.replace(&targets, &BTreeSet::new()),
            WatchChanges {
                remove: vec![root],
                ..WatchChanges::default()
            }
        );
    }
}
