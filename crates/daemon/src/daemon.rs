use super::indexing::IndexScheduler;
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::Workspace;
use beholder_dto::{GarbageCollection, GarbageCollectionPhase, GarbageCollectionProgress};
use beholder_indexing::Indexer;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

const GARBAGE_COLLECTION_INTERVAL: Duration = Duration::from_secs(60);
const GARBAGE_COLLECTION_VACUUM_PAGES: u32 = 1_024;
const GARBAGE_COLLECTION_YIELD: Duration = Duration::from_millis(10);

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

pub(super) async fn run_garbage_collection_monitor(
    store: Arc<SemanticStore>,
    scheduler: Arc<IndexScheduler>,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
) {
    let mut interval = tokio::time::interval(GARBAGE_COLLECTION_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let trigger = tokio::select! {
            _ = interval.tick() => "automatic_recovery",
            () = scheduler.wait_for_garbage_collection() => "publication",
            () = scheduler.wait_for_stop() => return,
        };
        if scheduler.is_stopping() {
            return;
        }
        if running.load(Ordering::Acquire) {
            continue;
        }
        let store = store.clone();
        let scheduler = scheduler.clone();
        let running = running.clone();
        let progress = progress.clone();
        match tokio::task::spawn_blocking(move || {
            collect_garbage(store, scheduler, running, progress, trigger)
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(collected)) if collected.repository_states_queued > 0 => tracing::info!(
                repository_states_queued = collected.repository_states_queued,
                "automatic semantic store garbage collection queued"
            ),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "automatic semantic store garbage collection failed")
            }
            Err(error) => tracing::error!(%error, "garbage collection monitor worker failed"),
        }
    }
}

pub(super) fn collect_garbage(
    store: Arc<SemanticStore>,
    scheduler: Arc<IndexScheduler>,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
    trigger: &'static str,
) -> Result<GarbageCollection, Box<dyn Error>> {
    if running.load(Ordering::Acquire) {
        return Ok(GarbageCollection {
            repository_states_queued: store.garbage_collection_queued()?,
        });
    }
    let collectible = store.garbage_collection_candidates()?;
    let queued = store.garbage_collection_queued()?;
    let reclaimable = store.reclaimable_database_pages()?;
    if trigger == "automatic_recovery" && collectible == 0 && queued == 0 && reclaimable == 0 {
        return Ok(GarbageCollection {
            repository_states_queued: 0,
        });
    }
    let span = garbage_collection_span(trigger);
    span.record("gc.collectible_states", collectible);
    span.record("gc.queued_states", queued);
    span.record("gc.reclaimable_pages", reclaimable);
    let collected = {
        let _entered = span.enter();
        let collected = match store.garbage_collect() {
            Ok(collected) => collected,
            Err(error) => {
                record_garbage_collection_error(&span, error.as_ref());
                tracing::error!(%error, "obsolete repository state claim failed");
                return Err(error);
            }
        };
        tracing::Span::current().record("gc.claimed_states", collected.repository_states_queued);
        tracing::info!(
            repository_states_queued = collected.repository_states_queued,
            "obsolete repository states claimed"
        );
        collected
    };
    let worker_span = span.clone();
    if let Err(error) = start_garbage_collector(store, scheduler, running, progress, span) {
        record_garbage_collection_error(&worker_span, error.as_ref());
        return Err(error);
    }
    Ok(collected)
}

pub(super) fn start_claimed_garbage_collector(
    store: Arc<SemanticStore>,
    scheduler: Arc<IndexScheduler>,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
    trigger: &'static str,
    claimed_states: u64,
) -> Result<(), Box<dyn Error>> {
    let span = garbage_collection_span(trigger);
    span.record("gc.claimed_states", claimed_states);
    let worker_span = span.clone();
    start_garbage_collector(store, scheduler, running, progress, span).inspect_err(|error| {
        record_garbage_collection_error(&worker_span, error.as_ref());
    })
}

fn garbage_collection_span(trigger: &'static str) -> tracing::Span {
    tracing::info_span!(
        "garbage_collection.run",
        gc.trigger = trigger,
        gc.collectible_states = tracing::field::Empty,
        gc.claimed_states = tracing::field::Empty,
        gc.queued_states = tracing::field::Empty,
        gc.reclaimable_pages = tracing::field::Empty,
        gc.states_resolved = tracing::field::Empty,
        gc.pages_reclaimed = tracing::field::Empty,
        gc.outcome = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
        otel.status_message = tracing::field::Empty,
    )
}

fn record_garbage_collection_error(span: &tracing::Span, error: &dyn Error) {
    span.record("gc.outcome", "failed");
    span.record("otel.status_code", "ERROR");
    span.record("otel.status_message", tracing::field::display(error));
}

fn start_garbage_collector(
    store: Arc<SemanticStore>,
    scheduler: Arc<IndexScheduler>,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<GarbageCollectionProgress>>>,
    span: tracing::Span,
) -> Result<(), Box<dyn Error>> {
    let queued = store.garbage_collection_queued()?;
    let reclaimable = store.reclaimable_database_pages()?;
    span.record("gc.queued_states", queued);
    span.record("gc.reclaimable_pages", reclaimable);
    if running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        span.record("gc.outcome", "already_running");
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
            span.in_scope(|| {
                let result: Result<u64, Box<dyn Error>> = (|| {
                    let mut states_resolved = 0;
                    if store.garbage_collection_pending()? {
                        if let Ok(mut current) = progress.lock() {
                            *current = Some(GarbageCollectionProgress::phase(
                                GarbageCollectionPhase::SweepingObsoleteStates,
                            ));
                        }
                        states_resolved = store.sweep_garbage_collection(|update| {
                            if let Ok(mut current) = progress.lock() {
                                *current = Some(update.clone());
                            }
                            tracing::info!(
                                phase = ?update.phase,
                                step = update.step,
                                completed_rows = update.completed_rows,
                                rows = update.rows,
                                stale_states = update.stale_states,
                                repositories = update.repositories,
                                completed_steps = update.completed_steps,
                                total_steps = update.total_steps,
                                "semantic store garbage collection progress"
                            );
                            !scheduler.is_stopping()
                        })?;
                    }
                    if scheduler.is_stopping() {
                        return Ok(states_resolved);
                    }
                    if let Ok(mut current) = progress.lock() {
                        *current = Some(GarbageCollectionProgress::phase(
                            GarbageCollectionPhase::CheckpointingDatabase,
                        ));
                    }
                    store.checkpoint()?;
                    let pages = store.reclaimable_database_pages()?;
                    let mut reclaimed = 0;
                    while reclaimed < pages && !scheduler.is_stopping() {
                        if let Ok(mut current) = progress.lock() {
                            *current = Some(GarbageCollectionProgress {
                                phase: GarbageCollectionPhase::ReclaimingDatabaseSpace,
                                step: None,
                                rows: Some(pages),
                                completed_rows: Some(reclaimed),
                                stale_states: None,
                                repositories: None,
                                completed_steps: 0,
                                total_steps: 1,
                            });
                        }
                        let reclaimed_batch = scheduler
                            .run_exclusive("garbage collection compaction", || {
                                store.reclaim_database_pages(GARBAGE_COLLECTION_VACUUM_PAGES)
                            })?;
                        if reclaimed_batch == 0 {
                            break;
                        }
                        reclaimed += reclaimed_batch;
                        tracing::info!(
                            reclaimed_pages = reclaimed,
                            reclaimable_pages = pages,
                            "semantic store database space reclaimed"
                        );
                        std::thread::sleep(GARBAGE_COLLECTION_YIELD);
                    }
                    tracing::Span::current().record("gc.pages_reclaimed", reclaimed);
                    Ok(states_resolved)
                })();
                let pending = restart_store.garbage_collection_pending().unwrap_or(false);
                match result {
                    Ok(states_resolved) => {
                        tracing::Span::current().record("gc.states_resolved", states_resolved);
                        let outcome = if scheduler.is_stopping() {
                            "stopped"
                        } else {
                            "completed"
                        };
                        tracing::Span::current().record("gc.outcome", outcome);
                        tracing::info!(
                            states_resolved,
                            outcome,
                            "semantic store garbage collection sweep completed"
                        );
                    }
                    Err(error) if pending => {
                        tracing::Span::current().record("gc.outcome", "retrying");
                        tracing::info!(
                            %error,
                            "semantic store garbage collection interrupted; retrying"
                        );
                    }
                    Err(error) => {
                        record_garbage_collection_error(&tracing::Span::current(), error.as_ref());
                        tracing::error!(%error, "semantic store garbage collection failed");
                    }
                }
                worker_running.store(false, Ordering::Release);
                if let Ok(mut current) = progress.lock() {
                    *current = None;
                }
                if pending && !scheduler.is_stopping() {
                    let retry_span = tracing::Span::current();
                    let _ = start_garbage_collector(
                        restart_store,
                        restart_scheduler,
                        restart_running,
                        restart_progress,
                        retry_span,
                    );
                }
            })
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
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryFacts, RepositoryState, WorkspaceView};
    use std::{fs, time::SystemTime};

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

    #[test]
    fn automatic_collection_claims_and_sweeps_obsolete_states() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-auto-gc-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store =
            Arc::new(SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap());
        for fingerprint in ["old", "current"] {
            let state = RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repository".into(),
                },
                head: Some(fingerprint.into()),
                fingerprint: fingerprint.into(),
            };
            let view = WorkspaceView::new("main", "analysis", vec![state.clone()]).unwrap();
            store
                .publish(
                    &view,
                    &[RepositoryFacts {
                        state,
                        analysis_identity: "analysis".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: Vec::new(),
                    }],
                    &[],
                )
                .unwrap();
        }
        assert_eq!(store.garbage_collection_candidates().unwrap(), 1);

        let scheduler = Arc::new(IndexScheduler::new(state_dir.join("cache")));
        let running = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(None));
        assert_eq!(
            collect_garbage(
                store.clone(),
                scheduler.clone(),
                running.clone(),
                progress,
                "test",
            )
            .unwrap()
            .repository_states_queued,
            1
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while running.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(!running.load(Ordering::Acquire));
        assert_eq!(store.garbage_collection_candidates().unwrap(), 0);
        assert_eq!(store.garbage_collection_queued().unwrap(), 0);
        scheduler.stop();
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
