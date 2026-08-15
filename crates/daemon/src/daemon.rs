use super::indexing::IndexScheduler;
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::Workspace;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub(super) struct BeholderDaemon {
    pub(super) store: Arc<SemanticStore>,
    pub(super) workspaces: Arc<Mutex<super::workspace_registry::WorkspaceRegistry>>,
    pub(super) scheduler: Arc<IndexScheduler>,
    pub(super) watcher: Mutex<RecommendedWatcher>,
    pub(super) shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

pub(super) type DaemonParts = (BeholderDaemon, oneshot::Receiver<()>, Arc<IndexScheduler>);

pub(super) fn build(
    store: SemanticStore,
    workspaces: super::workspace_registry::WorkspaceRegistry,
    cache_dir: PathBuf,
) -> Result<DaemonParts, Box<dyn Error>> {
    let (shutdown, stopped) = oneshot::channel();
    let workspaces = Arc::new(Mutex::new(workspaces));
    let scheduler = Arc::new(IndexScheduler::new(cache_dir));
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
    Ok((
        BeholderDaemon {
            store: Arc::new(store),
            workspaces,
            scheduler: scheduler.clone(),
            watcher: Mutex::new(watcher),
            shutdown: Mutex::new(Some(shutdown)),
        },
        stopped,
        scheduler,
    ))
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
