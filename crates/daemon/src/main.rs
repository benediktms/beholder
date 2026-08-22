use beholder_adapters_graphql::GraphqlAnalyzer;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_protobuf::ProtobufAnalyzer;
use beholder_adapters_treesitter_csharp::CsharpAnalyzer;
use beholder_adapters_treesitter_elixir::ElixirAnalyzer;
use beholder_adapters_treesitter_rust::RustAnalyzer;
use beholder_adapters_treesitter_typescript::TypescriptAnalyzer;
use beholder_daemon_client::{socket_path, state_dir};
use beholder_indexing::{Indexer, IndexerBuilder};
use beholder_observability::{ExportMode, LogOutput};
use beholder_protocol::v1::daemon_server::DaemonServer;
#[cfg(not(test))]
use beholder_worker_client::WorkerAnalyzerBuilder;
use std::error::Error;
#[cfg(unix)]
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

mod daemon;
mod indexing;
mod ipc;
mod rpc;
mod rpc_service;
mod single_instance;
mod workspace_registry;

use workspace_registry::WorkspaceRegistry;

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
        let _observability_guard = beholder_observability::init(
            "beholderd",
            LogOutput::Rolling {
                directory: state_dir.clone(),
                prefix: "beholderd".into(),
            },
            ExportMode::Batch,
        );
        tracing::info!(pid = std::process::id(), socket = %socket_path.display(), "daemon started");
        let cache_dir = state_dir.join("frontend-cache");
        let (service, stopped, index_scheduler) = daemon::build(
            SemanticStore::persistent(&state_dir.join("beholder.db"), true)?,
            WorkspaceRegistry::open(workspace_registry::registry_path(&state_dir))?,
            built_in_indexer(cache_dir)?,
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

fn built_in_indexer(cache_dir: std::path::PathBuf) -> Result<Indexer, Box<dyn Error>> {
    let workers = std::env::var("BEHOLDER_INDEX_WORKERS")
        .ok()
        .and_then(|workers| workers.parse().ok())
        .filter(|workers| *workers > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));
    tracing::info!(workers, "index analysis pool configured");
    let builder = IndexerBuilder::new(cache_dir.clone(), workers)
        .add_analyzer(RustAnalyzer::new(cache_dir.clone()));
    #[cfg(not(test))]
    let builder = builder.add_enricher(
        WorkerAnalyzerBuilder::new(
            rust_worker_executable()?,
            cache_dir
                .parent()
                .unwrap_or(cache_dir.as_path())
                .join("workers"),
        )
        .identity("rust", "7:6:rust.tonic:1:rust-analyzer-0.0.348:worker-6")
        .accept_extension("rs")
        .accept_file_name("Cargo.toml")
        .build()
        .map_err(|error| error.to_string())?,
    );
    builder
        .add_analyzer(ElixirAnalyzer::new(cache_dir.clone()))
        .add_analyzer(CsharpAnalyzer::new(cache_dir.clone()))
        .add_analyzer(TypescriptAnalyzer::new(cache_dir.clone()))
        .add_analyzer(GraphqlAnalyzer)
        .add_analyzer(ProtobufAnalyzer::new(cache_dir))
        .build()
        .map_err(|error| error.to_string().into())
}

#[cfg(not(test))]
fn rust_worker_executable() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let executable = std::env::var_os("BEHOLDER_RUST_WORKER_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("beholder-worker-rust"));
    if !executable.is_file() {
        return Err(format!("Rust analyzer worker not found at {}", executable.display()).into());
    }
    Ok(executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::BeholderErrorCode;
    use beholder_protocol::{
        ERROR_CODE_METADATA_KEY,
        v1::{
            ClearCacheRequest, EntityKind, EntityRequest, EvidenceKind, GarbageCollectPhase,
            GarbageCollectRequest, GetGarbageCollectionStatusRequest, GetStatusRequest,
            ListWorkspacesRequest, PathRequest, RegisterWorkspaceRequest, ReindexWorkspaceRequest,
            RelationKind, StopRequest, TraversalEntityRequest, daemon_client::DaemonClient,
            garbage_collect_event,
        },
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
            built_in_indexer(state.join("frontend-cache")).unwrap(),
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
        assert_eq!(status.protocol_version, 14);
        assert_eq!(status.pid, std::process::id());

        let missing = client
            .reindex_workspace(ReindexWorkspaceRequest {
                workspace: "missing".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::NotFound);
        assert_eq!(
            missing
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .unwrap()
                .to_str()
                .unwrap(),
            BeholderErrorCode::WorkspaceNotRegistered.as_str()
        );

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
        let mut events = client
            .garbage_collect(GarbageCollectRequest {})
            .await
            .unwrap()
            .into_inner();
        let mut phases = Vec::new();
        let mut collected = None;
        while let Some(event) = events.message().await.unwrap() {
            match event.event {
                Some(garbage_collect_event::Event::Progress(progress)) => {
                    phases.push(GarbageCollectPhase::try_from(progress.phase).unwrap());
                }
                Some(garbage_collect_event::Event::Completed(result)) => {
                    collected = Some(result);
                }
                None => panic!("garbage collection event should have a value"),
            }
        }
        assert_eq!(phases, [GarbageCollectPhase::ClaimingObsoleteStates]);
        let collected = collected.unwrap();
        assert!(collected.repository_states_queued > 0);
        let garbage_collection_status = client
            .get_garbage_collection_status(GetGarbageCollectionStatusRequest {})
            .await
            .unwrap()
            .into_inner();
        assert!(
            garbage_collection_status.repository_states_queued
                <= collected.repository_states_queued
        );

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
