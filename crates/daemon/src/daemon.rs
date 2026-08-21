use super::indexing::IndexScheduler;
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::Workspace;
use beholder_dto::{GarbageCollectionPhase, GarbageCollectionProgress};
use beholder_indexing::Indexer;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
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
    pub(super) watcher: Mutex<RecommendedWatcher>,
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
    let mut watcher = notify::recommended_watcher(move |event| {
        callback_scheduler.add_event(event, &callback_workspaces);
    })?;
    let registered = workspaces
        .lock()
        .map_err(|_| "workspace registry lock poisoned")?
        .list();
    for workspace in &registered {
        watch_workspace(&mut watcher, workspace)?;
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

fn watch_workspace(watcher: &mut RecommendedWatcher, workspace: &Workspace) -> notify::Result<()> {
    for repository in &workspace.repositories {
        watcher.watch(&repository.base, RecursiveMode::Recursive)?;
    }
    Ok(())
}

pub(super) fn update_workspace_watch(
    watcher: &mut RecommendedWatcher,
    previous: Option<&Workspace>,
    workspace: &Workspace,
) -> notify::Result<()> {
    let previous = previous
        .into_iter()
        .flat_map(|workspace| &workspace.repositories)
        .map(|repository| &repository.base)
        .collect::<BTreeSet<_>>();
    let current = workspace
        .repositories
        .iter()
        .map(|repository| &repository.base)
        .collect::<BTreeSet<_>>();
    for repository in previous.difference(&current) {
        watcher.unwatch(repository)?;
    }
    for repository in current.difference(&previous) {
        watcher.watch(repository, RecursiveMode::Recursive)?;
    }
    Ok(())
}
