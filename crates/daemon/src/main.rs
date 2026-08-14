use beholder_adapters_mnestic::SemanticStore;
use beholder_daemon_client::{socket_path, state_dir};
use beholder_dto::{Revisioned, SemanticQueryResult, WhyResult};
use beholder_protocol::v1::{
    ClearCacheRequest, ClearCacheResponse, ContextResponse, DependenciesResponse, EntityRequest,
    GetStatusRequest, GetStatusResponse, ImpactResponse, ListWorkspacesRequest,
    ListWorkspacesResponse, PathRequest, RegisterWorkspaceRequest, RegisterWorkspaceResponse,
    ReindexWorkspaceRequest, ReindexWorkspaceResponse, StopRequest, StopResponse, TraceResponse,
    WhyResponse,
    daemon_server::{Daemon, DaemonServer},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::oneshot;
#[cfg(unix)]
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status, transport::Server};

mod indexing;
mod logging;
mod single_instance;
mod workspace_registry;

use indexing::IndexScheduler;
use workspace_registry::WorkspaceRegistry;

#[cfg(unix)]
struct SocketFile(PathBuf);

#[cfg(unix)]
impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
fn bind_socket(path: &Path) -> Result<(UnixListener, SocketFile), Box<dyn Error>> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    let socket = SocketFile(path.to_path_buf());
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, socket))
}

struct BeholderDaemon {
    store: Arc<SemanticStore>,
    workspaces: Arc<Mutex<WorkspaceRegistry>>,
    scheduler: Arc<IndexScheduler>,
    watcher: Mutex<RecommendedWatcher>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

type DaemonParts = (BeholderDaemon, oneshot::Receiver<()>, Arc<IndexScheduler>);

fn daemon(
    store: SemanticStore,
    workspaces: WorkspaceRegistry,
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

fn watch_workspace(
    watcher: &mut RecommendedWatcher,
    workspace: &beholder_domain::Workspace,
) -> notify::Result<()> {
    for repository in &workspace.repositories {
        watcher.watch(&repository.base, RecursiveMode::Recursive)?;
    }
    Ok(())
}

fn update_workspace_watch(
    watcher: &mut RecommendedWatcher,
    previous: Option<&beholder_domain::Workspace>,
    workspace: &beholder_domain::Workspace,
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

#[tonic::async_trait]
impl Daemon for BeholderDaemon {
    #[tracing::instrument(name = "rpc.clear_cache", skip_all, err)]
    async fn clear_cache(
        &self,
        _request: Request<ClearCacheRequest>,
    ) -> Result<Response<ClearCacheResponse>, Status> {
        self.scheduler
            .clear_cache()
            .map_err(|error| Status::internal(error.to_string()))?;
        tracing::info!("analysis cache cleared");
        Ok(Response::new(ClearCacheResponse {}))
    }

    #[tracing::instrument(
        name = "rpc.context",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, entity = %request.get_ref().entity)
    )]
    async fn context(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<ContextResponse>, Status> {
        let request = request.into_inner();
        self.query_response(
            &request.workspace,
            self.store
                .context_snapshot(&request.workspace, &request.entity),
        )
    }

    #[tracing::instrument(
        name = "rpc.dependencies",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, entity = %request.get_ref().entity)
    )]
    async fn dependencies(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<DependenciesResponse>, Status> {
        let request = request.into_inner();
        self.query_response(
            &request.workspace,
            self.store
                .dependencies_snapshot(&request.workspace, &request.entity),
        )
    }

    #[tracing::instrument(name = "rpc.get_status", skip_all, err)]
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            status: "ready".into(),
            protocol_version: 5,
            pid: std::process::id(),
        }))
    }

    #[tracing::instrument(
        name = "rpc.impact",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, entity = %request.get_ref().entity)
    )]
    async fn impact(
        &self,
        request: Request<EntityRequest>,
    ) -> Result<Response<ImpactResponse>, Status> {
        let request = request.into_inner();
        self.query_response(
            &request.workspace,
            self.store
                .impact_snapshot(&request.workspace, &request.entity),
        )
    }

    #[tracing::instrument(
        name = "rpc.reindex_workspace",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace)
    )]
    async fn reindex_workspace(
        &self,
        request: Request<ReindexWorkspaceRequest>,
    ) -> Result<Response<ReindexWorkspaceResponse>, Status> {
        let workspace_name = request.into_inner().workspace;
        let workspace = self
            .workspaces
            .lock()
            .map_err(|_| Status::internal("workspace registry lock poisoned"))?
            .get(&workspace_name)
            .cloned()
            .ok_or_else(|| {
                Status::not_found(format!("workspace not registered: {workspace_name}"))
            })?;
        let scheduler = self.scheduler.clone();
        let store = self.store.clone();
        let (observation_count, published) = tokio::task::spawn_blocking(move || {
            scheduler
                .index(&store, &workspace)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| Status::internal(format!("index worker failed: {error}")))?
        .map_err(Status::failed_precondition)?;
        Ok(Response::new(ReindexWorkspaceResponse {
            observation_count: observation_count
                .try_into()
                .map_err(|_| Status::internal("observation count exceeds protocol capacity"))?,
            published,
        }))
    }

    #[tracing::instrument(name = "rpc.list_workspaces", skip_all, err)]
    async fn list_workspaces(
        &self,
        _request: Request<ListWorkspacesRequest>,
    ) -> Result<Response<ListWorkspacesResponse>, Status> {
        let workspaces = self
            .workspaces
            .lock()
            .map_err(|_| Status::internal("workspace registry lock poisoned"))?
            .list()
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(Response::new(ListWorkspacesResponse { workspaces }))
    }

    #[tracing::instrument(
        name = "rpc.register_workspace",
        skip_all,
        err,
        fields(workspace = %request.get_ref().name, repositories = request.get_ref().repository_paths.len())
    )]
    async fn register_workspace(
        &self,
        request: Request<RegisterWorkspaceRequest>,
    ) -> Result<Response<RegisterWorkspaceResponse>, Status> {
        let request = request.into_inner();
        let (previous, workspace) = {
            let mut workspaces = self
                .workspaces
                .lock()
                .map_err(|_| Status::internal("workspace registry lock poisoned"))?;
            let previous = workspaces.get(&request.name).cloned();
            let workspace = workspaces
                .register(
                    request.name,
                    request
                        .repository_paths
                        .into_iter()
                        .map(PathBuf::from)
                        .collect(),
                )
                .map_err(|error| Status::invalid_argument(error.to_string()))?;
            (previous, workspace)
        };
        let mut watcher = self
            .watcher
            .lock()
            .map_err(|_| Status::internal("filesystem watcher lock poisoned"))?;
        update_workspace_watch(&mut watcher, previous.as_ref(), &workspace)
            .map_err(|error| Status::internal(error.to_string()))?;
        self.scheduler.mark(&workspace);
        tracing::info!(workspace = %workspace.name, "workspace registered");
        Ok(Response::new(RegisterWorkspaceResponse {
            workspace: Some(workspace.into()),
        }))
    }

    #[tracing::instrument(name = "rpc.stop", skip_all, err)]
    async fn stop(&self, _request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let accepted = self
            .shutdown
            .lock()
            .map_err(|_| Status::internal("shutdown lock poisoned"))?
            .take()
            .is_some_and(|shutdown| shutdown.send(()).is_ok());
        Ok(Response::new(StopResponse { accepted }))
    }

    #[tracing::instrument(
        name = "rpc.trace",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, from = %request.get_ref().from, to = %request.get_ref().to)
    )]
    async fn trace(
        &self,
        request: Request<PathRequest>,
    ) -> Result<Response<TraceResponse>, Status> {
        let request = request.into_inner();
        self.query_response(
            &request.workspace,
            self.store
                .trace_snapshot(&request.workspace, &request.from, &request.to),
        )
    }

    #[tracing::instrument(
        name = "rpc.why",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, from = %request.get_ref().from, to = %request.get_ref().to)
    )]
    async fn why(&self, request: Request<PathRequest>) -> Result<Response<WhyResponse>, Status> {
        let request = request.into_inner();
        let revisioned = self
            .store
            .trace_snapshot(&request.workspace, &request.from, &request.to)
            .map(|revisioned| Revisioned {
                result: WhyResult::from(revisioned.result),
                analysis_revision: revisioned.analysis_revision,
            });
        self.query_response(&request.workspace, revisioned)
    }
}

impl BeholderDaemon {
    fn query_response<T, P>(
        &self,
        workspace: &str,
        result: Result<Revisioned<T>, Box<dyn Error>>,
    ) -> Result<Response<P>, Status>
    where
        T: SemanticQueryResult,
        P: From<T>,
    {
        let revisioned = result.map_err(|error| Status::internal(error.to_string()))?;
        let mut result = revisioned.result;
        *result.metadata_mut() = self
            .scheduler
            .query_metadata(workspace, revisioned.analysis_revision);
        Ok(Response::new(result.into()))
    }
}

#[cfg(unix)]
async fn shutdown_signal(stopped: oneshot::Receiver<()>) {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = stopped => {}
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            tokio::select! {
                _ = stopped => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal(stopped: oneshot::Receiver<()>) {
    tokio::select! {
        _ = stopped => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(not(unix))]
    return Err("beholderd local IPC is supported on Unix platforms".into());

    #[cfg(unix)]
    {
        let state_dir = state_dir()?;
        std::fs::create_dir_all(&state_dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
        let _lock = single_instance::acquire(&state_dir)?;
        let socket_path = socket_path()?;
        let (listener, _socket_file) = bind_socket(&socket_path)?;
        let _log_guard = logging::init(&state_dir);
        tracing::info!(pid = std::process::id(), socket = %socket_path.display(), "daemon started");
        let (service, stopped, index_scheduler) = daemon(
            SemanticStore::persistent(&state_dir.join("beholder.db"), true)?,
            WorkspaceRegistry::open(workspace_registry::registry_path(&state_dir))?,
            state_dir.join("frontend-cache"),
        )?;
        let watcher_task =
            tokio::spawn(index_scheduler.run(service.store.clone(), service.workspaces.clone()));
        Server::builder()
            .add_service(DaemonServer::new(service))
            .serve_with_incoming_shutdown(
                UnixListenerStream::new(listener),
                shutdown_signal(stopped),
            )
            .await?;
        watcher_task.abort();
        tracing::info!("daemon stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_protocol::v1::{
        ClearCacheRequest, EntityRequest, GetStatusRequest, ListWorkspacesRequest, PathRequest,
        RegisterWorkspaceRequest, ReindexWorkspaceRequest, StopRequest,
        daemon_client::DaemonClient,
    };
    use std::{env, fs, path::Path, time::Duration};

    #[tokio::test]
    async fn workspace_smoke() {
        let database = env::temp_dir().join(format!("beholderd-{}.db", std::process::id()));
        let state = env::temp_dir().join(format!("beholderd-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        fs::create_dir_all(&state).unwrap();
        let lock = single_instance::acquire(&state).unwrap();
        assert_eq!(
            fs::read_to_string(state.join("beholderd.pid"))
                .unwrap()
                .trim(),
            std::process::id().to_string()
        );
        assert!(
            single_instance::acquire(&state)
                .unwrap_err()
                .to_string()
                .contains(&std::process::id().to_string())
        );
        let socket_path = state.join("beholder.sock");
        fs::write(&socket_path, "stale").unwrap();
        let (listener, socket_file) = bind_socket(&socket_path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_file(&database);
        let registry_path = workspace_registry::registry_path(&state);
        let (service, stopped, index_scheduler) = daemon(
            SemanticStore::persistent(&database, true).unwrap(),
            WorkspaceRegistry::open(registry_path.clone()).unwrap(),
            state.join("frontend-cache"),
        )
        .unwrap();
        let watcher_task =
            tokio::spawn(index_scheduler.run(service.store.clone(), service.workspaces.clone()));
        let server = tokio::spawn(async move {
            let _socket_file = socket_file;
            Server::builder()
                .add_service(DaemonServer::new(service))
                .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });

        let endpoint = format!("unix:{}", socket_path.display());
        let mut client = loop {
            match DaemonClient::connect(endpoint.clone()).await {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        let status = client
            .get_status(GetStatusRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.status, "ready");
        assert_eq!(status.protocol_version, 5);
        assert_eq!(status.pid, std::process::id());

        let first = state.join("repo-a");
        let second = state.join("repo-b");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::create_dir_all(second.join("src")).unwrap();
        fs::write(first.join("src/lib.rs"), "fn caller() { helper(); }").unwrap();
        fs::write(second.join("src/lib.rs"), "fn helper() {}").unwrap();
        let first_identity = beholder_adapters_git::repository_identity(&first).unwrap();
        let second_identity = beholder_adapters_git::repository_identity(&second).unwrap();
        let caller = format!("repo://{first_identity}/rust/lib/caller");
        let helper = format!("repo://{second_identity}/rust/lib/helper");
        let repository = |path: &Path| path.to_str().unwrap().to_owned();
        let registered = client
            .register_workspace(RegisterWorkspaceRequest {
                name: "main".into(),
                repository_paths: vec![repository(&first), repository(&second)],
            })
            .await
            .unwrap()
            .into_inner()
            .workspace
            .unwrap();
        assert_eq!(registered.name, "main");
        assert_eq!(
            client
                .list_workspaces(ListWorkspacesRequest {})
                .await
                .unwrap()
                .into_inner()
                .workspaces
                .len(),
            1
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let context = client
                    .context(EntityRequest {
                        workspace: "main".into(),
                        entity: caller.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner();
                if format!("{context:?}").contains(&helper) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("registered workspace was not indexed");
        let unchanged = client
            .reindex_workspace(ReindexWorkspaceRequest {
                workspace: "main".into(),
            })
            .await
            .unwrap()
            .into_inner();
        assert!(!unchanged.published);
        assert_eq!(unchanged.observation_count, 0);
        let metadata = client
            .context(EntityRequest {
                workspace: "main".into(),
                entity: caller.clone(),
            })
            .await
            .unwrap()
            .into_inner()
            .metadata
            .unwrap();
        assert_eq!(metadata.revision, 1);
        assert_eq!(metadata.view, "main");
        let freshness = metadata.freshness.unwrap();
        assert!(!freshness.stale);
        assert!(!freshness.indexing);
        assert!(freshness.dirty_repositories.is_empty());
        client.clear_cache(ClearCacheRequest {}).await.unwrap();
        assert!(!state.join("frontend-cache").exists());

        let third = state.join("repo-c");
        fs::create_dir_all(third.join("src")).unwrap();
        fs::write(third.join("src/lib.rs"), "fn isolated() {}").unwrap();
        client
            .register_workspace(RegisterWorkspaceRequest {
                name: "secondary".into(),
                repository_paths: vec![repository(&third)],
            })
            .await
            .unwrap();
        let isolated = format!(
            "repo://{}/rust/lib/isolated",
            beholder_adapters_git::repository_identity(&third).unwrap()
        );
        assert!(
            client
                .context(EntityRequest {
                    workspace: "main".into(),
                    entity: isolated.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .edges
                .is_empty()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !client
                    .context(EntityRequest {
                        workspace: "secondary".into(),
                        entity: isolated.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner()
                    .edges
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("secondary workspace was not indexed");
        assert!(
            format!(
                "{:?}",
                client
                    .context(EntityRequest {
                        workspace: "main".into(),
                        entity: caller.clone()
                    })
                    .await
                    .unwrap()
                    .into_inner()
            )
            .contains(&helper)
        );
        assert!(
            !client
                .dependencies(EntityRequest {
                    workspace: "main".into(),
                    entity: caller.clone()
                })
                .await
                .unwrap()
                .into_inner()
                .dependencies
                .is_empty()
        );
        assert!(
            format!(
                "{:?}",
                client
                    .impact(EntityRequest {
                        workspace: "main".into(),
                        entity: helper.clone()
                    })
                    .await
                    .unwrap()
                    .into_inner()
            )
            .contains(&caller)
        );
        let path = || PathRequest {
            workspace: "main".into(),
            from: caller.clone(),
            to: helper.clone(),
        };
        assert!(
            !client
                .trace(path())
                .await
                .unwrap()
                .into_inner()
                .paths
                .is_empty()
        );
        assert!(
            !client
                .why(path())
                .await
                .unwrap()
                .into_inner()
                .paths
                .is_empty()
        );

        fs::write(first.join("src/lib.rs"), "fn caller() { replacement(); }").unwrap();
        fs::write(second.join("src/lib.rs"), "fn replacement() {}").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let context = client
                    .context(EntityRequest {
                        workspace: "main".into(),
                        entity: caller.clone(),
                    })
                    .await
                    .unwrap()
                    .into_inner();
                if format!("{context:?}")
                    .contains(&format!("repo://{}/rust/lib/replacement", second_identity))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("filesystem change was not indexed");

        assert!(
            client
                .stop(StopRequest {})
                .await
                .unwrap()
                .into_inner()
                .accepted
        );
        server.await.unwrap();
        assert!(!socket_path.exists());
        watcher_task.abort();
        let reloaded = WorkspaceRegistry::open(registry_path).unwrap();
        assert!(reloaded.get("main").is_some());
        assert!(reloaded.get("secondary").is_some());
        let indexed = SemanticStore::persistent(&database, false).unwrap();
        assert!(indexed.inspect_revisions().unwrap().rows.iter().any(|row| {
            row[0].as_str() == Some("main") && row[1].as_i64().is_some_and(|revision| revision >= 2)
        }));
        assert!(
            format!("{:?}", indexed.context("main", &caller).unwrap())
                .contains(&format!("repo://{}/rust/lib/replacement", second_identity))
        );
        drop(indexed);
        drop(lock);
        assert!(
            fs::read_to_string(state.join("beholderd.pid"))
                .unwrap()
                .is_empty()
        );
        fs::remove_file(database).unwrap();
        fs::remove_dir_all(state).unwrap();
    }
}
