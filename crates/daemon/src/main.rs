use beholder_adapters_graphql::GraphqlAnalyzer;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_protobuf::ProtobufAnalyzer;
use beholder_adapters_treesitter_csharp::CsharpAnalyzer;
use beholder_adapters_treesitter_elixir::ElixirAnalyzer;
use beholder_adapters_treesitter_rust::RustAnalyzer;
use beholder_adapters_treesitter_typescript::TypescriptAnalyzer;
use beholder_daemon_client::{socket_path, state_dir};
#[cfg(not(test))]
use beholder_indexing::AnalysisInputKind;
use beholder_indexing::{Indexer, IndexerBuilder};
use beholder_observability::LogOutput;
use beholder_protocol::v1::daemon_server::DaemonServer;
use beholder_worker_client::{PluginRegistry, plugin_analyzer};
#[cfg(not(test))]
use beholder_worker_client::{WorkerAnalyzerBuilder, worker_environment_variable};
use std::error::Error;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

mod daemon;
mod indexing;
mod ipc;
pub mod jobs;
mod repository_registry;
mod rpc;
mod rpc_service;
mod single_instance;
mod workspace_registry;

use workspace_registry::WorkspaceRegistry;

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(not(unix))]
    return Err("beholderd local IPC is supported on Unix platforms".into());

    #[cfg(unix)]
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(run_daemon());
        runtime.shutdown_timeout(Duration::ZERO);
        result
    }
}

#[cfg(unix)]
async fn run_daemon() -> Result<(), Box<dyn Error>> {
    let state_dir = state_dir()?;
    std::fs::create_dir_all(&state_dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    let _lock = single_instance::acquire(&state_dir)?;
    let _observability_guard = beholder_observability::init(
        "beholderd",
        LogOutput::Rolling {
            directory: state_dir.clone(),
            prefix: "beholderd".into(),
        },
    );
    let jobs = jobs::JobQueue::open(&state_dir.join("queue.sqlite")).await?;
    jobs.recover().await?;
    let socket_path = socket_path()?;
    let (listener, _socket_file) = ipc::bind_socket(&socket_path)?;
    tracing::info!(pid = std::process::id(), socket = %socket_path.display(), "daemon started");
    let cache_dir = state_dir.join("frontend-cache");
    let (service, stopped, index_scheduler) = daemon::build(
        SemanticStore::persistent(&state_dir.join("beholder.db"), true)?,
        WorkspaceRegistry::open(workspace_registry::registry_path(&state_dir))?,
        built_in_indexer(cache_dir)?,
        jobs.clone(),
    )?;
    let mut index_worker = jobs::start_index_worker(
        jobs.clone(),
        index_scheduler.clone(),
        service.store.clone(),
        service.workspaces.clone(),
    );
    while !index_worker.context.is_ready() {
        if index_worker.task.is_finished() {
            return Err(format!(
                "index worker exited during startup: {}",
                join_result(index_worker.task.await)
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut watcher_task = tokio::spawn(index_scheduler.clone().run(
        service.store.clone(),
        service.workspaces.clone(),
        jobs.clone(),
    ));
    let mut garbage_collection_task = tokio::spawn(daemon::run_garbage_collection_monitor(
        service.store.clone(),
        service.scheduler.clone(),
        service.garbage_collector_running.clone(),
        service.garbage_collection_progress.clone(),
    ));
    let server_shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stopping = server_shutdown.clone();
    let mut server_task = tokio::spawn(
        Server::builder()
            .trace_fn(rpc_span)
            .add_service(DaemonServer::new(service))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async move {
                server_stopping.notified().await;
            }),
    );
    let mut shutdown_signal = Box::pin(ipc::shutdown_signal(stopped));
    let fatal = tokio::select! {
        () = &mut shutdown_signal => None,
        result = &mut index_worker.task => Some(format!("index worker exited unexpectedly: {}", join_result(result))),
        result = &mut watcher_task => Some(format!("automatic index producer exited unexpectedly: {}", join_result(result))),
        result = &mut garbage_collection_task => Some(format!("garbage collection monitor exited unexpectedly: {}", join_result(result.map(|()| Ok::<(), String>(()))))),
        result = &mut server_task => Some(format!("gRPC server exited unexpectedly: {}", join_result(result.map(|result| result.map_err(|error| error.to_string()))))),
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    jobs.close_admission().await;
    index_scheduler.stop();
    let _ = index_worker.context.stop();
    server_shutdown.notify_waiters();
    let drain = async {
        if !server_task.is_finished() {
            let _ = (&mut server_task).await;
        }
        if !watcher_task.is_finished() {
            let _ = (&mut watcher_task).await;
        }
        if !index_worker.task.is_finished() {
            let _ = (&mut index_worker.task).await;
        }
        index_scheduler.wait_for_checkpoint().await;
        if !garbage_collection_task.is_finished() {
            let _ = (&mut garbage_collection_task).await;
        }
    };
    if tokio::time::timeout_at(deadline, drain).await.is_err() {
        tracing::error!("daemon shutdown deadline expired; terminating remaining work");
    }
    tracing::info!("daemon stopped");
    fatal.map_or(Ok(()), |error| Err(error.into()))
}

#[cfg(unix)]
fn join_result<T: std::fmt::Debug>(result: Result<T, tokio::task::JoinError>) -> String {
    match result {
        Ok(result) => format!("{result:?}"),
        Err(error) => error.to_string(),
    }
}

fn rpc_span(request: &tonic::codegen::http::Request<()>) -> tracing::Span {
    let span = tracing::info_span!(
        "rpc.server",
        rpc.system = "grpc",
        rpc.route = %request.uri().path()
    );
    beholder_observability::set_parent_from_headers(&span, request.headers());
    span
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
    let builder = builder.add_enricher({
        let mut worker = WorkerAnalyzerBuilder::new(
            rust_worker_executable()?,
            cache_dir
                .parent()
                .unwrap_or(cache_dir.as_path())
                .join("workers"),
        )
        .identity("rust", "7:7:rust.tonic:1:rust-analyzer-0.0.348:worker-7")
        .accept_extension("rs")
        .accept_file_name_as("Cargo.toml", AnalysisInputKind::Dependency)
        .accept_file_name_as("Cargo.lock", AnalysisInputKind::Dependency)
        .accept_file_name_as("rust-toolchain", AnalysisInputKind::Toolchain)
        .accept_file_name_as("rust-toolchain.toml", AnalysisInputKind::Toolchain)
        .accept_path_suffix_as(".cargo/config", AnalysisInputKind::Configuration)
        .accept_path_suffix_as(".cargo/config.toml", AnalysisInputKind::Configuration)
        .identity_input(
            "$toolchain/rustc",
            command_identity("rustc", &["--version", "--verbose"]),
            AnalysisInputKind::Toolchain,
        )
        .identity_input(
            "$toolchain/cargo",
            command_identity("cargo", &["--version", "--verbose"]),
            AnalysisInputKind::Toolchain,
        );
        for variable in [
            "CARGO_BUILD_TARGET",
            "CARGO_ENCODED_RUSTFLAGS",
            "BEHOLDER_RUST_ALL_FEATURES",
            "BEHOLDER_RUST_FEATURES",
            "BEHOLDER_RUST_NO_DEFAULT_FEATURES",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            worker = worker.identity_input(
                format!("$environment/{variable}"),
                std::env::var_os(variable)
                    .map(|value| value.as_encoded_bytes().to_vec())
                    .unwrap_or_default(),
                AnalysisInputKind::Environment,
            );
        }
        worker.build().map_err(|error| error.to_string())?
    });
    #[cfg(not(test))]
    let builder = if let Some(executable) = elixir_worker_executable()? {
        let mix_env = std::env::var("BEHOLDER_ELIXIR_MIX_ENV")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "dev".into());
        let mix_program = std::env::var("BEHOLDER_ELIXIR_MIX_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "mix".into());
        let mut worker = WorkerAnalyzerBuilder::new(
            executable,
            cache_dir
                .parent()
                .unwrap_or(cache_dir.as_path())
                .join("workers"),
        )
        .identity("elixir", "18:10:elixir-compiler:12")
        .timeout(std::time::Duration::from_secs(20 * 60))
        .accept_extension("ex")
        .accept_extension("exs")
        .accept_file_name_as("mix.exs", AnalysisInputKind::Dependency)
        .accept_file_name_as("mix.lock", AnalysisInputKind::Dependency)
        .accept_parent_suffix_as("config", AnalysisInputKind::Configuration)
        .exclude_path_suffix("config/runtime.exs")
        .identity_input(
            "$toolchain/elixir",
            command_identity("elixir", &["--version"]),
            AnalysisInputKind::Toolchain,
        )
        .identity_input(
            "$toolchain/mix",
            command_identity(&mix_program, &["--version"]),
            AnalysisInputKind::Toolchain,
        )
        .identity_input(
            "$environment/BEHOLDER_ELIXIR_MIX_ENV",
            mix_env.as_bytes().to_vec(),
            AnalysisInputKind::Environment,
        );
        for environment in ["dev", "test", "prod"] {
            if environment != mix_env {
                worker = worker.exclude_path_suffix(format!("config/{environment}.exs"));
            }
        }
        for variable in [
            "BEHOLDER_ELIXIR_MIX_PATH",
            "ELIXIR_ERL_OPTIONS",
            "ERL_AFLAGS",
            "ERL_COMPILER_OPTIONS",
        ] {
            worker = worker.identity_input(
                format!("$environment/{variable}"),
                std::env::var_os(variable)
                    .map(|value| value.as_encoded_bytes().to_vec())
                    .unwrap_or_default(),
                AnalysisInputKind::Environment,
            );
        }
        builder.add_enricher(worker.build().map_err(|error| error.to_string())?)
    } else {
        tracing::info!("Elixir analyzer worker not found; compiler enrichment disabled");
        builder
    };
    let mut builder = builder
        .add_analyzer(ElixirAnalyzer::new(cache_dir.clone()))
        .add_analyzer(CsharpAnalyzer::new(cache_dir.clone()))
        .add_analyzer(TypescriptAnalyzer::new(cache_dir.clone()))
        .add_analyzer(GraphqlAnalyzer)
        .add_analyzer(ProtobufAnalyzer::new(cache_dir.clone()));
    let registry = PluginRegistry::open(cache_dir.parent().unwrap_or(cache_dir.as_path()))?;
    for plugin in registry.plugins() {
        let executable = registry.executable(plugin);
        if executable.is_file() {
            builder = builder.add_enricher(
                plugin_analyzer(
                    executable,
                    cache_dir
                        .parent()
                        .unwrap_or(cache_dir.as_path())
                        .join("plugin-workers"),
                    plugin.digest.clone(),
                    plugin.descriptor.clone(),
                )
                .map_err(|error| error.to_string())?,
            );
        } else {
            tracing::warn!(
                plugin = %plugin.descriptor.id,
                path = %executable.display(),
                "installed plugin executable is missing"
            );
        }
    }
    builder.build().map_err(|error| error.to_string().into())
}

#[cfg(not(test))]
fn command_identity(program: &str, arguments: &[&str]) -> Vec<u8> {
    std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_else(|| b"unavailable".to_vec())
}

#[cfg(not(test))]
fn rust_worker_executable() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let executable = std::env::var_os(worker_environment_variable("rust", "PATH"))
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("beholder-worker-rust"));
    if !executable.is_file() {
        return Err(format!("Rust analyzer worker not found at {}", executable.display()).into());
    }
    Ok(executable)
}

#[cfg(not(test))]
fn elixir_worker_executable() -> Result<Option<std::path::PathBuf>, Box<dyn Error>> {
    let configured = std::env::var_os(worker_environment_variable("elixir", "PATH"));
    let executable = configured
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("beholder-worker-elixir"));
    if executable.is_file() {
        Ok(Some(executable))
    } else if configured.is_some() {
        Err(format!(
            "configured Elixir analyzer worker not found at {}",
            executable.display()
        )
        .into())
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::BeholderErrorCode;
    use beholder_protocol::{
        ERROR_CODE_METADATA_KEY,
        v1::{
            ClearCacheRequest, DeleteRepositoryRequest, EntityKind, EntityRequest, EvidenceKind,
            GarbageCollectPhase, GarbageCollectRequest, GetGarbageCollectionStatusRequest,
            GetJobRequest, GetRepositoryRequest, GetStatusRequest, JobStatus, JobTrigger, JobType,
            ListJobsRequest, ListWorkspacesRequest, PathRequest, RegisterRepositoryRequest,
            RegisterWorkspaceRequest, RelationKind, RepositoryIndexTarget, StopRequest,
            SubmitIndexRequest, TraversalEntityRequest, daemon_client::DaemonClient,
            garbage_collect_event, submit_index_request,
        },
    };
    use std::{env, fs, path::Path, time::Duration};

    async fn wait_for_job(
        client: &mut DaemonClient<tonic::transport::Channel>,
        id: String,
    ) -> beholder_protocol::v1::Job {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let job = client
                    .get_job(GetJobRequest { id: id.clone() })
                    .await
                    .unwrap()
                    .into_inner()
                    .job
                    .unwrap();
                if job
                    .summary
                    .as_ref()
                    .is_some_and(|summary| summary.status == JobStatus::Completed as i32)
                {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("index job did not complete")
    }

    #[tokio::test]
    async fn daemon_smoke() {
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
        let jobs = jobs::JobQueue::open(&state.join("queue.sqlite"))
            .await
            .unwrap();
        let (service, stopped, index_scheduler) = daemon::build(
            SemanticStore::persistent(&database, true).unwrap(),
            WorkspaceRegistry::open(registry_path.clone()).unwrap(),
            built_in_indexer(state.join("frontend-cache")).unwrap(),
            jobs.clone(),
        )
        .unwrap();
        let test_workspaces = service.workspaces.clone();
        let index_worker = jobs::start_index_worker(
            jobs.clone(),
            index_scheduler.clone(),
            service.store.clone(),
            service.workspaces.clone(),
        );
        let mut watcher_task = tokio::spawn(index_scheduler.clone().run(
            service.store.clone(),
            service.workspaces.clone(),
            jobs.clone(),
        ));
        let shutdown_scheduler = index_scheduler.clone();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(DaemonServer::new(service))
                .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
                    let _ = stopped.await;
                    shutdown_scheduler.stop();
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
        assert_eq!(status.protocol_version, 19);
        assert_eq!(status.pid, std::process::id());

        let standalone = state.join("standalone");
        fs::create_dir_all(&standalone).unwrap();
        fs::write(
            standalone.join("schema.graphql"),
            "type Query { package: Package! } type Package { id: ID! }",
        )
        .unwrap();
        let registered_repository = client
            .register_repository(RegisterRepositoryRequest {
                path: standalone.to_string_lossy().into_owned(),
            })
            .await
            .unwrap()
            .into_inner()
            .repository
            .unwrap();
        let identity = registered_repository.repository.unwrap().identity;
        assert!(registered_repository.revision.is_none());
        let submitted = client
            .submit_index(SubmitIndexRequest {
                target: Some(submit_index_request::Target::Repository(
                    RepositoryIndexTarget {
                        repository: identity.clone(),
                        workspace_scope: None,
                    },
                )),
            })
            .await
            .unwrap()
            .into_inner();
        assert!(submitted.overlapping_jobs.is_empty());
        let first_job = submitted.job.unwrap();
        let first_id = first_job.id.clone();
        let indexed_repository = wait_for_job(&mut client, first_job.id).await;
        let destination = &indexed_repository
            .index_result
            .as_ref()
            .unwrap()
            .destinations[0];
        assert!(destination.published);
        assert!(destination.observation_count > 0);
        assert!(
            client
                .get_repository(GetRepositoryRequest {
                    identity: identity.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .repository
                .unwrap()
                .revision
                .is_some()
        );
        let submitted_again = client
            .submit_index(SubmitIndexRequest {
                target: Some(submit_index_request::Target::Repository(
                    RepositoryIndexTarget {
                        repository: identity.clone(),
                        workspace_scope: None,
                    },
                )),
            })
            .await
            .unwrap()
            .into_inner()
            .job
            .unwrap();
        assert_ne!(submitted_again.id, first_id);
        assert!(
            !wait_for_job(&mut client, submitted_again.id)
                .await
                .index_result
                .unwrap()
                .destinations[0]
                .published
        );
        assert_eq!(
            client
                .delete_repository(DeleteRepositoryRequest {
                    identity: identity.clone(),
                })
                .await
                .unwrap()
                .into_inner()
                .repository_states_queued,
            1
        );
        let deleted = client
            .get_repository(GetRepositoryRequest { identity })
            .await
            .unwrap_err();
        assert_eq!(deleted.code(), tonic::Code::NotFound);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = client
                    .get_garbage_collection_status(GetGarbageCollectionStatusRequest {})
                    .await
                    .unwrap()
                    .into_inner();
                if !status.running && status.repository_states_queued == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("deleted repository state was not cleaned up");

        let unavailable = client
            .context(EntityRequest {
                workspace: "main".into(),
                entity: "repo/source".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(unavailable.code(), tonic::Code::Unavailable);
        assert_eq!(
            unavailable
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .unwrap()
                .to_str()
                .unwrap(),
            BeholderErrorCode::WorkspaceRevisionUnavailable.as_str()
        );

        let missing = client
            .submit_index(SubmitIndexRequest {
                target: Some(submit_index_request::Target::Workspace("missing".into())),
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
                enabled_plugins: Vec::new(),
            })
            .await
            .unwrap()
            .into_inner()
            .workspace
            .unwrap();
        assert_eq!(registered.name, "main");
        let referenced = client
            .delete_repository(DeleteRepositoryRequest {
                identity: first_identity.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(referenced.code(), tonic::Code::FailedPrecondition);
        assert_eq!(
            referenced
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .unwrap()
                .to_str()
                .unwrap(),
            BeholderErrorCode::RepositoryDeleteFailed.as_str()
        );
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
                let context = match client
                    .context(EntityRequest {
                        workspace: "main".into(),
                        entity: caller.clone(),
                    })
                    .await
                {
                    Ok(response) => response.into_inner(),
                    Err(error) if error.code() == tonic::Code::Unavailable => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    }
                    Err(error) => panic!("initial workspace query failed: {error}"),
                };
                if format!("{context:?}").contains(&helper) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("registered workspace was not indexed");
        let indexed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let indexed = client
                    .list_jobs(ListJobsRequest { page_token: None })
                    .await
                    .unwrap()
                    .into_inner()
                    .jobs
                    .into_iter()
                    .find(|job| {
                        job.target
                            .as_ref()
                            .and_then(|target| target.target.as_ref())
                            .is_some_and(|target| {
                                matches!(target, beholder_protocol::v1::job_target::Target::Workspace(name) if name == "main")
                            })
                    });
                if let Some(indexed) = indexed
                    && indexed.status == JobStatus::Completed as i32
                {
                    break indexed;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("automatic index job did not complete durably");
        assert_eq!(indexed.r#type, JobType::Index as i32);
        assert_eq!(indexed.trigger, JobTrigger::Automatic as i32);
        assert!(indexed.submitted_at_ms > 0);
        let detail = client
            .get_job(GetJobRequest { id: indexed.id })
            .await
            .unwrap()
            .into_inner()
            .job
            .unwrap();
        assert_eq!(detail.max_attempts, jobs::MAX_ATTEMPTS);
        assert!(detail.run_at_ms.is_some());
        assert!(detail.started_at_ms.is_some());
        assert!(detail.completed_at_ms.is_some());
        assert!(
            detail
                .index_result
                .unwrap()
                .destinations
                .iter()
                .any(|destination| destination.published)
        );
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
            .submit_index(SubmitIndexRequest {
                target: Some(submit_index_request::Target::Workspace("main".into())),
            })
            .await
            .unwrap()
            .into_inner()
            .job
            .unwrap();
        let unchanged = wait_for_job(&mut client, unchanged.id).await;
        assert!(!unchanged.index_result.unwrap().destinations[0].published);
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
        let metadata = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
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
                if !metadata.freshness.as_ref().unwrap().indexing {
                    break metadata;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("automatic index job did not become ready after durable acknowledgement");
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
                enabled_plugins: Vec::new(),
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
                match client
                    .context(EntityRequest {
                        workspace: "secondary".into(),
                        entity: isolated.clone(),
                    })
                    .await
                {
                    Ok(response) if !response.get_ref().edges.is_empty() => break,
                    Ok(_) => {}
                    Err(error) if error.code() == tonic::Code::Unavailable => {}
                    Err(error) => panic!("secondary workspace query failed: {error}"),
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
        let replacement = format!("repo://{second_identity}/rust/lib/replacement");
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
                if format!("{context:?}").contains(&replacement) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("filesystem change was not indexed");

        let completed = client
            .context(EntityRequest {
                workspace: "main".into(),
                entity: caller.clone(),
            })
            .await
            .unwrap()
            .into_inner();
        let completed_revision = completed.metadata.unwrap().revision;
        let pending = format!("repo://{first_identity}/rust/lib/after_wait");
        let blocker = index_scheduler.block_indexing("main");
        fs::write(
            first.join("src/lib.rs"),
            "fn caller() { after_wait(); } fn after_wait() {}",
        )
        .unwrap();
        let workspace = test_workspaces.lock().unwrap().get("main").unwrap().clone();
        index_scheduler.mark(&workspace);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let during_index = tokio::time::timeout(
            Duration::from_millis(500),
            client.context(EntityRequest {
                workspace: "main".into(),
                entity: caller.clone(),
            }),
        )
        .await
        .expect("semantic query blocked behind indexing")
        .unwrap()
        .into_inner();
        let metadata = during_index.metadata.as_ref().unwrap();
        assert_eq!(metadata.revision, completed_revision);
        let freshness = metadata.freshness.as_ref().unwrap();
        assert!(freshness.stale);
        assert!(freshness.indexing);
        assert!(format!("{during_index:?}").contains(&replacement));
        assert!(!format!("{during_index:?}").contains(&pending));

        let other_workspace = tokio::time::timeout(
            Duration::from_millis(500),
            client.context(EntityRequest {
                workspace: "secondary".into(),
                entity: isolated,
            }),
        )
        .await
        .expect("indexing one workspace delayed a query against another")
        .unwrap()
        .into_inner();
        let freshness = other_workspace.metadata.unwrap().freshness.unwrap();
        assert!(!freshness.stale);
        assert!(!freshness.indexing);

        assert!(
            client
                .stop(StopRequest {})
                .await
                .unwrap()
                .into_inner()
                .accepted
        );
        server.await.unwrap();
        let _ = index_worker.context.stop();
        tokio::time::timeout(Duration::from_millis(500), &mut watcher_task)
            .await
            .expect("daemon did not cancel queued indexing")
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_millis(500), index_worker.task)
            .await
            .expect("index worker did not stop")
            .unwrap()
            .unwrap();
        assert!(socket_path.exists());
        assert!(single_instance::acquire(&state).is_err());
        drop(blocker);
        drop(socket_file);
        assert!(!socket_path.exists());
        let reloaded = WorkspaceRegistry::open(registry_path).unwrap();
        assert_eq!(reloaded.get("main").unwrap().protobuf_descriptors.len(), 1);
        assert!(reloaded.get("secondary").is_some());
        let indexed = SemanticStore::persistent(&database, false).unwrap();
        assert!(indexed.inspect_revisions().unwrap().rows.iter().any(|row| {
            row[0].as_str() == Some("main")
                && row[1]
                    .as_i64()
                    .is_some_and(|revision| revision == completed_revision as i64)
        }));
        assert!(!format!("{:?}", indexed.context("main", &caller).unwrap()).contains(&pending));
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
