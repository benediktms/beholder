use beholder_adapters_mnestic::SemanticStore;
use beholder_daemon_client::{socket_path, state_dir};
use beholder_dto::{DEFAULT_MAX_HOPS, Revisioned, WhyResult};
use beholder_protocol::v1::{
    ClearCacheRequest, ClearCacheResponse, ContextResponse, DependenciesResponse, EntityRequest,
    GarbageCollectRequest, GarbageCollectResponse, GetStatusRequest, GetStatusResponse,
    ImpactResponse, ListWorkspacesRequest, ListWorkspacesResponse, PathRequest,
    RegisterWorkspaceRequest, RegisterWorkspaceResponse, ReindexWorkspaceRequest,
    ReindexWorkspaceResponse, StopRequest, StopResponse, TraceResponse, TraversalEntityRequest,
    WhyResponse,
    daemon_server::{Daemon, DaemonServer},
};
use std::{error::Error, path::PathBuf};
#[cfg(unix)]
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status, transport::Server};

mod daemon;
mod indexing;
mod ipc;
mod logging;
mod rpc;
mod single_instance;
mod workspace_registry;

use daemon::BeholderDaemon;
use workspace_registry::WorkspaceRegistry;

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

    #[tracing::instrument(name = "rpc.garbage_collect", skip_all, err)]
    async fn garbage_collect(
        &self,
        _request: Request<GarbageCollectRequest>,
    ) -> Result<Response<GarbageCollectResponse>, Status> {
        let scheduler = self.scheduler.clone();
        let store = self.store.clone();
        let collected = tokio::task::spawn_blocking(move || {
            scheduler
                .garbage_collect(&store)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| Status::internal(format!("garbage collection worker failed: {error}")))?
        .map_err(Status::internal)?;
        tracing::info!(
            repository_states_removed = collected.repository_states_removed,
            bytes_before = collected.bytes_before,
            bytes_after = collected.bytes_after,
            "semantic store garbage collected"
        );
        Ok(Response::new(GarbageCollectResponse {
            repository_states_removed: collected.repository_states_removed,
            bytes_before: collected.bytes_before,
            bytes_after: collected.bytes_after,
        }))
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
        request: Request<TraversalEntityRequest>,
    ) -> Result<Response<DependenciesResponse>, Status> {
        let request = request.into_inner();
        let max_hops = max_hops(request.max_hops)?;
        self.query_response(
            &request.workspace,
            self.store
                .dependencies_snapshot(&request.workspace, &request.entity, max_hops),
        )
    }

    #[tracing::instrument(name = "rpc.get_status", skip_all, err)]
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            status: "ready".into(),
            protocol_version: 10,
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
        request: Request<TraversalEntityRequest>,
    ) -> Result<Response<ImpactResponse>, Status> {
        let request = request.into_inner();
        let max_hops = max_hops(request.max_hops)?;
        self.query_response(
            &request.workspace,
            self.store
                .impact_snapshot(&request.workspace, &request.entity, max_hops),
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
                    request
                        .protobuf_descriptor_paths
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
        daemon::update_workspace_watch(&mut watcher, previous.as_ref(), &workspace)
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
        let max_hops = max_hops(request.max_hops)?;
        self.query_response(
            &request.workspace,
            self.store
                .trace_snapshot(&request.workspace, &request.from, &request.to, max_hops),
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
        let max_hops = max_hops(request.max_hops)?;
        let revisioned = self
            .store
            .trace_snapshot(&request.workspace, &request.from, &request.to, max_hops)
            .map(|revisioned| Revisioned {
                result: WhyResult::from(revisioned.result),
                analysis_revision: revisioned.analysis_revision,
            });
        self.query_response(&request.workspace, revisioned)
    }
}

fn max_hops(value: Option<u32>) -> Result<u32, Status> {
    match value.unwrap_or(DEFAULT_MAX_HOPS) {
        0 => Err(Status::invalid_argument(
            "max_hops must be greater than zero",
        )),
        value => Ok(value),
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
        let (listener, _socket_file) = ipc::bind_socket(&socket_path)?;
        let _log_guard = logging::init(&state_dir);
        tracing::info!(pid = std::process::id(), socket = %socket_path.display(), "daemon started");
        let (service, stopped, index_scheduler) = daemon::build(
            SemanticStore::persistent(&state_dir.join("beholder.db"), true)?,
            WorkspaceRegistry::open(workspace_registry::registry_path(&state_dir))?,
            state_dir.join("frontend-cache"),
        )?;
        let watcher_task = tokio::spawn(
            index_scheduler
                .clone()
                .run(service.store.clone(), service.workspaces.clone()),
        );
        Server::builder()
            .add_service(DaemonServer::new(service))
            .serve_with_incoming_shutdown(
                UnixListenerStream::new(listener),
                ipc::shutdown_signal(stopped),
            )
            .await?;
        index_scheduler.stop();
        watcher_task.await?;
        tracing::info!("daemon stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_protocol::v1::{
        ClearCacheRequest, EntityKind, EntityRequest, EvidenceKind, GarbageCollectRequest,
        GetStatusRequest, ListWorkspacesRequest, PathRequest, RegisterWorkspaceRequest,
        ReindexWorkspaceRequest, RelationKind, StopRequest, TraversalEntityRequest,
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
        let (listener, socket_file) = ipc::bind_socket(&socket_path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_file(&database);
        let registry_path = workspace_registry::registry_path(&state);
        let (service, stopped, index_scheduler) = daemon::build(
            SemanticStore::persistent(&database, true).unwrap(),
            WorkspaceRegistry::open(registry_path.clone()).unwrap(),
            state.join("frontend-cache"),
        )
        .unwrap();
        let test_workspaces = service.workspaces.clone();
        let mut watcher_task = tokio::spawn(
            index_scheduler
                .clone()
                .run(service.store.clone(), service.workspaces.clone()),
        );
        let server = tokio::spawn(async move {
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
        assert_eq!(status.protocol_version, 10);
        assert_eq!(status.pid, std::process::id());

        let first = state.join("repo-a");
        let second = state.join("repo-b");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::create_dir_all(second.join("src")).unwrap();
        fs::write(first.join("src/lib.rs"), "fn caller() { helper(); }").unwrap();
        fs::write(second.join("src/lib.rs"), "fn helper() {}").unwrap();
        let descriptor = first.join("pricing.descriptor.bin");
        let descriptor_bytes = include_str!("../../../scripts/fixtures/pricing.descriptor.hex")
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        fs::write(&descriptor, &descriptor_bytes).unwrap();
        let first_identity = beholder_adapters_git::repository_identity(&first).unwrap();
        let second_identity = beholder_adapters_git::repository_identity(&second).unwrap();
        let caller = format!("repo://{first_identity}/rust/lib/caller");
        let helper = format!("repo://{second_identity}/rust/lib/helper");
        let repository = |path: &Path| path.to_str().unwrap().to_owned();
        let registered = client
            .register_workspace(RegisterWorkspaceRequest {
                name: "main".into(),
                repository_paths: vec![repository(&first), repository(&second)],
                protobuf_descriptor_paths: vec![repository(&descriptor)],
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
        let protobuf = client
            .context(EntityRequest {
                workspace: "main".into(),
                entity: "proto-method://pricing.v1.Pricing/GetQuote".into(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(protobuf.root.unwrap().kind, EntityKind::Rpc as i32);
        assert!(protobuf.nodes.iter().any(|node| {
            node.id == "proto-type://pricing.v1.Request"
                && node.kind == EntityKind::ProtoMessage as i32
        }));
        assert!(protobuf.edges.iter().any(|edge| {
            edge.kind == RelationKind::RequestType as i32
                && edge
                    .evidence
                    .iter()
                    .all(|evidence| evidence.source == EvidenceKind::Descriptor as i32)
        }));
        let unchanged = client
            .reindex_workspace(ReindexWorkspaceRequest {
                workspace: "main".into(),
            })
            .await
            .unwrap()
            .into_inner();
        assert!(!unchanged.published);
        assert_eq!(unchanged.observation_count, 0);
        let mut changed_descriptor = descriptor_bytes;
        let method = changed_descriptor
            .windows(b"GetQuote".len())
            .position(|window| window == b"GetQuote")
            .unwrap();
        changed_descriptor[method..method + b"GetPrice".len()].copy_from_slice(b"GetPrice");
        fs::write(&descriptor, changed_descriptor).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let context = client
                    .context(EntityRequest {
                        workspace: "main".into(),
                        entity: "proto-method://pricing.v1.Pricing/GetPrice".into(),
                    })
                    .await
                    .unwrap()
                    .into_inner();
                if context
                    .edges
                    .iter()
                    .any(|edge| edge.kind == RelationKind::RequestType as i32)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("protobuf descriptor change was not indexed");
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
        assert_eq!(metadata.revision, 2);
        assert_eq!(metadata.view, "main");
        let freshness = metadata.freshness.unwrap();
        assert!(!freshness.stale);
        assert!(!freshness.indexing);
        assert!(freshness.dirty_repositories.is_empty());
        client.clear_cache(ClearCacheRequest {}).await.unwrap();
        assert!(!state.join("frontend-cache").exists());
        let collected = client
            .garbage_collect(GarbageCollectRequest {})
            .await
            .unwrap()
            .into_inner();
        assert!(collected.bytes_after <= collected.bytes_before);

        let third = state.join("repo-c");
        fs::create_dir_all(third.join("src")).unwrap();
        fs::write(third.join("src/lib.rs"), "fn isolated() {}").unwrap();
        client
            .register_workspace(RegisterWorkspaceRequest {
                name: "secondary".into(),
                repository_paths: vec![repository(&third)],
                protobuf_descriptor_paths: Vec::new(),
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
                .dependencies(TraversalEntityRequest {
                    workspace: "main".into(),
                    entity: caller.clone(),
                    max_hops: None,
                })
                .await
                .unwrap()
                .into_inner()
                .dependencies
                .is_empty()
        );
        assert_eq!(
            client
                .dependencies(TraversalEntityRequest {
                    workspace: "main".into(),
                    entity: caller.clone(),
                    max_hops: Some(0),
                })
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(
            format!(
                "{:?}",
                client
                    .impact(TraversalEntityRequest {
                        workspace: "main".into(),
                        entity: helper.clone(),
                        max_hops: None,
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
            max_hops: None,
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

        let blocker = index_scheduler.block_indexing();
        let workspace = test_workspaces.lock().unwrap().get("main").unwrap().clone();
        index_scheduler.mark(&workspace);
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            client
                .stop(StopRequest {})
                .await
                .unwrap()
                .into_inner()
                .accepted
        );
        server.await.unwrap();
        index_scheduler.stop();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut watcher_task)
                .await
                .is_err(),
            "daemon detached the blocking index worker"
        );
        assert!(socket_path.exists());
        assert!(single_instance::acquire(&state).is_err());
        drop(blocker);
        watcher_task.await.unwrap();
        drop(socket_file);
        assert!(!socket_path.exists());
        let reloaded = WorkspaceRegistry::open(registry_path).unwrap();
        assert_eq!(reloaded.get("main").unwrap().protobuf_descriptors.len(), 1);
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
