use crate::daemon::{BeholderDaemon, collect_garbage, start_claimed_garbage_collector};
use crate::jobs::{
    self, IndexJob, IndexTarget, StoredJob, StoredJobKind, StoredJobStatus, StoredJobTarget,
};
use crate::repository_registry::RegisteredRepository;
use beholder_domain::{BeholderError, BeholderErrorCode, BeholderErrorKind};
use beholder_dto::{
    DEFAULT_MAX_HOPS, GarbageCollectionPhase,
    GarbageCollectionProgress as DtoGarbageCollectProgress,
    RepositoryStatus as DtoRepositoryStatus, Revisioned, WhyResult,
};
use beholder_protocol::ERROR_CODE_METADATA_KEY;
use beholder_protocol::v1::{
    ClearCacheRequest, ClearCacheResponse, ContextResponse, DeleteRepositoryRequest,
    DeleteRepositoryResponse, DependenciesResponse, EntityRequest, GarbageCollectEvent,
    GarbageCollectPhase, GarbageCollectProgress, GarbageCollectRequest, GarbageCollectResponse,
    GetGarbageCollectionStatusRequest, GetGarbageCollectionStatusResponse, GetJobRequest,
    GetJobResponse, GetRepositoryRequest, GetStatusRequest, GetStatusResponse, ImpactResponse,
    IndexJobOutcome, Job, JobStatus, JobSummary, JobTarget, JobTrigger, JobType, JobWaitReason,
    ListJobsRequest, ListJobsResponse, ListWorkspacesRequest, ListWorkspacesResponse, PathRequest,
    RegisterRepositoryRequest, RegisterWorkspaceRequest, RegisterWorkspaceResponse,
    RepositoryResponse, SetWorkspacePluginRequest, SetWorkspacePluginResponse, StopRequest,
    StopResponse, SubmitIndexRequest, SubmitIndexResponse, TraceResponse, TraversalEntityRequest,
    WhyResponse, daemon_server::Daemon, garbage_collect_event, index_destination, job_target,
    submit_index_request,
};
use std::{error::Error, path::PathBuf, sync::atomic::Ordering};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::{Code, Request, Response, Status, metadata::MetadataValue};

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

    type GarbageCollectStream = UnboundedReceiverStream<Result<GarbageCollectEvent, Status>>;

    #[tracing::instrument(name = "rpc.garbage_collect", skip_all, err)]
    async fn garbage_collect(
        &self,
        _request: Request<GarbageCollectRequest>,
    ) -> Result<Response<Self::GarbageCollectStream>, Status> {
        let store = self.store.clone();
        let scheduler = self.scheduler.clone();
        let garbage_collector_running = self.garbage_collector_running.clone();
        let garbage_collection_progress = self.garbage_collection_progress.clone();
        let request_span = tracing::Span::current();
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            if garbage_collector_running.load(Ordering::Acquire) {
                let event = store
                    .garbage_collection_queued()
                    .map(|repository_states_queued| GarbageCollectEvent {
                        event: Some(garbage_collect_event::Event::Completed(
                            GarbageCollectResponse {
                                repository_states_queued,
                            },
                        )),
                    })
                    .map_err(|source| {
                        operation_status(
                            BeholderError::new(
                                BeholderErrorKind::Internal,
                                BeholderErrorCode::GarbageCollectionFailed,
                                "garbage collection status failed",
                            )
                            .with_source(std::io::Error::other(source.to_string())),
                        )
                    });
                let _ = sender.send(event);
                return;
            }
            let _ = sender.send(Ok(progress_event(DtoGarbageCollectProgress::phase(
                GarbageCollectionPhase::ClaimingObsoleteStates,
            ))));
            let result = tokio::task::spawn_blocking(move || {
                let _entered = request_span.enter();
                collect_garbage(
                    store,
                    scheduler,
                    garbage_collector_running,
                    garbage_collection_progress,
                    "manual",
                )
                .map_err(|source| {
                    BeholderError::new(
                        BeholderErrorKind::Internal,
                        BeholderErrorCode::GarbageCollectionFailed,
                        "semantic store garbage collection claim failed",
                    )
                    .with_source(std::io::Error::other(source.to_string()))
                })
            })
            .await;
            let event = match result {
                Ok(Ok(collected)) => {
                    tracing::info!(
                        repository_states_queued = collected.repository_states_queued,
                        "semantic store garbage collection queued"
                    );
                    Ok(GarbageCollectEvent {
                        event: Some(garbage_collect_event::Event::Completed(
                            GarbageCollectResponse {
                                repository_states_queued: collected.repository_states_queued,
                            },
                        )),
                    })
                }
                Ok(Err(error)) => Err(operation_status(error)),
                Err(source) => Err(operation_status(
                    BeholderError::new(
                        BeholderErrorKind::Internal,
                        BeholderErrorCode::GarbageCollectionWorkerFailed,
                        "garbage collection worker failed",
                    )
                    .with_source(source),
                )),
            };
            let _ = sender.send(event);
        });
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }

    #[tracing::instrument(name = "rpc.get_garbage_collection_status", skip_all, err)]
    async fn get_garbage_collection_status(
        &self,
        _request: Request<GetGarbageCollectionStatusRequest>,
    ) -> Result<Response<GetGarbageCollectionStatusResponse>, Status> {
        let repository_states_queued =
            self.store.garbage_collection_queued().map_err(|source| {
                operation_status(
                    BeholderError::new(
                        BeholderErrorKind::Internal,
                        BeholderErrorCode::GarbageCollectionFailed,
                        "garbage collection status failed",
                    )
                    .with_source(std::io::Error::other(source.to_string())),
                )
            })?;
        let repository_states_collectible =
            self.store
                .garbage_collection_candidates()
                .map_err(|source| {
                    operation_status(
                        BeholderError::new(
                            BeholderErrorKind::Internal,
                            BeholderErrorCode::GarbageCollectionFailed,
                            "garbage collection status failed",
                        )
                        .with_source(std::io::Error::other(source.to_string())),
                    )
                })?;
        let reclaimable_database_pages =
            self.store.reclaimable_database_pages().map_err(|source| {
                operation_status(
                    BeholderError::new(
                        BeholderErrorKind::Internal,
                        BeholderErrorCode::GarbageCollectionFailed,
                        "garbage collection status failed",
                    )
                    .with_source(std::io::Error::other(source.to_string())),
                )
            })?;
        let progress = self
            .garbage_collection_progress
            .lock()
            .map_err(|_| {
                operation_status(
                    BeholderError::new(
                        BeholderErrorKind::Internal,
                        BeholderErrorCode::GarbageCollectionFailed,
                        "garbage collection status failed",
                    )
                    .with_source(std::io::Error::other(
                        "garbage collection progress lock poisoned",
                    )),
                )
            })?
            .clone()
            .map(protocol_progress);
        Ok(Response::new(GetGarbageCollectionStatusResponse {
            running: self.garbage_collector_running.load(Ordering::Acquire),
            repository_states_queued,
            progress,
            repository_states_collectible,
            reclaimable_database_pages,
        }))
    }

    #[tracing::instrument(
        name = "rpc.context",
        skip_all,
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
        name = "rpc.delete_repository",
        skip_all,
        err,
        fields(repository = %request.get_ref().identity)
    )]
    async fn delete_repository(
        &self,
        request: Request<DeleteRepositoryRequest>,
    ) -> Result<Response<DeleteRepositoryResponse>, Status> {
        let identity = request.into_inner().identity;
        let mut registry = self
            .workspaces
            .lock()
            .map_err(|_| repository_registry_unavailable())?;
        let repository_states_queued = self
            .scheduler
            .delete_repository(&self.store, &mut registry, &identity)
            .map_err(operation_status)?;
        drop(registry);
        if let Err(source) = start_claimed_garbage_collector(
            self.store.clone(),
            self.scheduler.clone(),
            self.garbage_collector_running.clone(),
            self.garbage_collection_progress.clone(),
            "repository_deletion",
            repository_states_queued,
        ) {
            tracing::error!(%source, "repository cleanup worker failed to start");
        }
        Ok(Response::new(DeleteRepositoryResponse {
            repository_states_queued,
        }))
    }

    #[tracing::instrument(
        name = "rpc.dependencies",
        skip_all,
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
            protocol_version: 19,
            pid: std::process::id(),
        }))
    }

    #[tracing::instrument(name = "rpc.list_jobs", skip_all, err)]
    async fn list_jobs(
        &self,
        request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let cursor = request
            .into_inner()
            .page_token
            .map(|token| decode_job_page_token(&token))
            .transpose()?;
        let (jobs, next) = self
            .jobs
            .list(cursor)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(ListJobsResponse {
            jobs: jobs.into_iter().map(job_summary).collect(),
            next_page_token: next.map(|(done_at, id)| format!("{done_at}:{id}")),
        }))
    }

    #[tracing::instrument(name = "rpc.get_job", skip_all, err)]
    async fn get_job(
        &self,
        request: Request<GetJobRequest>,
    ) -> Result<Response<GetJobResponse>, Status> {
        let id = request.into_inner().id;
        ulid::Ulid::from_string(&id)
            .map_err(|_| Status::invalid_argument("job ID must be a ULID"))?;
        let job = self
            .jobs
            .get(&id)
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .ok_or_else(|| Status::not_found("job not found"))?;
        Ok(Response::new(GetJobResponse {
            job: Some(job_detail(job)),
        }))
    }

    #[tracing::instrument(name = "rpc.submit_index", skip_all, err)]
    async fn submit_index(
        &self,
        request: Request<SubmitIndexRequest>,
    ) -> Result<Response<SubmitIndexResponse>, Status> {
        let target = match request
            .into_inner()
            .target
            .ok_or_else(|| Status::invalid_argument("index target is required"))?
        {
            submit_index_request::Target::Workspace(workspace) => {
                let registry = self
                    .workspaces
                    .lock()
                    .map_err(|_| repository_registry_unavailable())?;
                if registry.get(&workspace).is_none() {
                    return Err(operation_status(BeholderError::new(
                        BeholderErrorKind::NotFound,
                        BeholderErrorCode::WorkspaceNotRegistered,
                        format!("workspace not registered: {workspace}"),
                    )));
                }
                IndexTarget::Workspace { workspace }
            }
            submit_index_request::Target::Repository(repository) => {
                let registry = self
                    .workspaces
                    .lock()
                    .map_err(|_| repository_registry_unavailable())?;
                if registry.repository(&repository.repository).is_none() {
                    return Err(operation_status(BeholderError::new(
                        BeholderErrorKind::NotFound,
                        BeholderErrorCode::RepositoryNotRegistered,
                        format!("repository not registered: {}", repository.repository),
                    )));
                }
                if let Some(scope) = repository.workspace_scope.as_deref() {
                    let workspace = registry.get(scope).ok_or_else(|| {
                        operation_status(BeholderError::new(
                            BeholderErrorKind::NotFound,
                            BeholderErrorCode::WorkspaceNotRegistered,
                            format!("workspace not registered: {scope}"),
                        ))
                    })?;
                    if !workspace
                        .repositories
                        .iter()
                        .any(|candidate| candidate.repository.identity == repository.repository)
                    {
                        return Err(Status::invalid_argument(format!(
                            "repository {} is not in workspace {scope}",
                            repository.repository
                        )));
                    }
                }
                IndexTarget::Repository {
                    repository: repository.repository,
                    workspace_scope: repository.workspace_scope,
                }
            }
        };
        let workspaces = self
            .workspaces
            .lock()
            .map_err(|_| repository_registry_unavailable())?
            .list();
        let job = IndexJob {
            target,
            trigger: jobs::JobTrigger::Manual,
            prerequisite_index_jobs: Vec::new(),
            generation: None,
            repository_intents: Vec::new(),
        };
        let (id, overlapping) = self
            .jobs
            .enqueue_manual_index(job, &workspaces)
            .await
            .map_err(|error| {
                if error.to_string() == "job admission is closed" {
                    Status::unavailable("job admission is closed")
                } else {
                    Status::internal(error.to_string())
                }
            })?;
        let submitted = self
            .jobs
            .get(&id.0)
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .ok_or_else(|| Status::internal("submitted index job disappeared"))?;
        Ok(Response::new(SubmitIndexResponse {
            job: Some(job_summary(submitted)),
            overlapping_jobs: overlapping.into_iter().map(job_summary).collect(),
        }))
    }

    #[tracing::instrument(name = "rpc.register_repository", skip_all, err)]
    async fn register_repository(
        &self,
        request: Request<RegisterRepositoryRequest>,
    ) -> Result<Response<RepositoryResponse>, Status> {
        let registered = self
            .workspaces
            .lock()
            .map_err(|_| repository_registry_unavailable())?
            .register_repository(PathBuf::from(request.into_inner().path))
            .map_err(|source| {
                operation_status(
                    BeholderError::new(
                        BeholderErrorKind::InvalidInput,
                        BeholderErrorCode::RepositoryRegistryFailed,
                        "repository registration failed",
                    )
                    .with_source(std::io::Error::other(source.to_string())),
                )
            })?;
        Ok(Response::new(RepositoryResponse {
            repository: Some(self.repository_status(&registered)?.into()),
        }))
    }

    #[tracing::instrument(
        name = "rpc.get_repository",
        skip_all,
        fields(repository = %request.get_ref().identity)
    )]
    async fn get_repository(
        &self,
        request: Request<GetRepositoryRequest>,
    ) -> Result<Response<RepositoryResponse>, Status> {
        let registered = self.registered_repository(&request.into_inner().identity)?;
        Ok(Response::new(RepositoryResponse {
            repository: Some(self.repository_status(&registered)?.into()),
        }))
    }

    #[tracing::instrument(
        name = "rpc.impact",
        skip_all,
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
                .register_with_plugins(
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
                    request.enabled_plugins,
                )
                .map_err(|error| Status::invalid_argument(error.to_string()))?;
            (previous, workspace)
        };
        let mut watcher = self
            .watcher
            .lock()
            .map_err(|_| Status::internal("filesystem watcher lock poisoned"))?;
        watcher
            .update(previous.as_ref(), &workspace)
            .map_err(|error| Status::internal(error.to_string()))?;
        self.scheduler.mark(&workspace);
        tracing::info!(workspace = %workspace.name, "workspace registered");
        Ok(Response::new(RegisterWorkspaceResponse {
            workspace: Some(workspace.into()),
        }))
    }

    #[tracing::instrument(
        name = "rpc.set_workspace_plugin",
        skip_all,
        err,
        fields(workspace = %request.get_ref().workspace, plugin = %request.get_ref().plugin)
    )]
    async fn set_workspace_plugin(
        &self,
        request: Request<SetWorkspacePluginRequest>,
    ) -> Result<Response<SetWorkspacePluginResponse>, Status> {
        let request = request.into_inner();
        let workspace = self
            .workspaces
            .lock()
            .map_err(|_| Status::internal("workspace registry lock poisoned"))?
            .set_plugin(&request.workspace, request.plugin, request.enabled)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        self.scheduler.mark(&workspace);
        Ok(Response::new(SetWorkspacePluginResponse {
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
                analysis: revisioned.analysis,
            });
        self.query_response(&request.workspace, revisioned)
    }
}

impl BeholderDaemon {
    fn registered_repository(&self, identity: &str) -> Result<RegisteredRepository, Status> {
        self.workspaces
            .lock()
            .map_err(|_| repository_registry_unavailable())?
            .repository(identity)
            .cloned()
            .ok_or_else(|| {
                operation_status(BeholderError::new(
                    BeholderErrorKind::NotFound,
                    BeholderErrorCode::RepositoryNotRegistered,
                    format!("repository not registered: {identity}"),
                ))
            })
    }

    fn repository_status(
        &self,
        registered: &RegisteredRepository,
    ) -> Result<DtoRepositoryStatus, Status> {
        let selection = &registered.selection;
        let identity = &selection.repository.identity;
        let revision = self.store.repository_revision(identity).map_err(|source| {
            operation_status(
                BeholderError::new(
                    BeholderErrorKind::Internal,
                    BeholderErrorCode::RepositoryIndexFailed,
                    "repository revision lookup failed",
                )
                .with_source(std::io::Error::other(source.to_string())),
            )
        })?;
        Ok(DtoRepositoryStatus {
            identity: identity.clone(),
            display_name: selection.display_name.clone(),
            base: selection.base.clone(),
            alternatives: selection.alternatives.clone(),
            revision,
            indexing: self.scheduler.repository_indexing(identity),
        })
    }
}

fn decode_job_page_token(token: &str) -> Result<(i64, String), Status> {
    let (done_at, id) = token
        .split_once(':')
        .ok_or_else(|| Status::invalid_argument("malformed job page token"))?;
    let done_at = done_at
        .parse()
        .map_err(|_| Status::invalid_argument("malformed job page token"))?;
    ulid::Ulid::from_string(id)
        .map_err(|_| Status::invalid_argument("malformed job page token"))?;
    Ok((done_at, id.into()))
}

fn job_summary(job: StoredJob) -> JobSummary {
    JobSummary {
        id: job.id,
        status: match job.status {
            StoredJobStatus::Queued => JobStatus::Queued,
            StoredJobStatus::Waiting => JobStatus::Waiting,
            StoredJobStatus::Running => JobStatus::Running,
            StoredJobStatus::Completed => JobStatus::Completed,
            StoredJobStatus::Failed => JobStatus::Failed,
        }
        .into(),
        r#type: match job.kind {
            StoredJobKind::Index => JobType::Index,
            StoredJobKind::Enrichment => JobType::Enrichment,
        }
        .into(),
        target: Some(JobTarget {
            target: Some(match &job.target {
                StoredJobTarget::Workspace(workspace) => {
                    job_target::Target::Workspace(workspace.clone())
                }
                StoredJobTarget::Repository { repository, .. } => {
                    job_target::Target::Repository(repository.clone())
                }
            }),
            workspace_scope: match &job.target {
                StoredJobTarget::Repository {
                    workspace_scope, ..
                } => workspace_scope.clone(),
                StoredJobTarget::Workspace(_) => None,
            },
            worker_id: match &job.target {
                StoredJobTarget::Repository { worker_id, .. } => worker_id.clone(),
                StoredJobTarget::Workspace(_) => None,
            },
        }),
        trigger: match job.trigger {
            jobs::JobTrigger::Automatic => JobTrigger::Automatic,
            jobs::JobTrigger::Manual => JobTrigger::Manual,
        }
        .into(),
        submitted_at_ms: job.submitted_at_ms.max(0) as u64,
    }
}

fn job_detail(job: StoredJob) -> Job {
    let summary = job_summary(job.clone());
    let wait_reason = match job.status {
        StoredJobStatus::Waiting if !job.prerequisites.is_empty() => {
            Some(JobWaitReason::Prerequisites.into())
        }
        StoredJobStatus::Waiting => Some(JobWaitReason::Retry.into()),
        _ => None,
    };
    Job {
        summary: Some(summary),
        attempts: job.attempts,
        max_attempts: job.max_attempts,
        run_at_ms: job.eligible_at_ms.map(|millis| millis.max(0) as u64),
        started_at_ms: seconds_to_millis(job.lock_at),
        completed_at_ms: seconds_to_millis(job.done_at),
        prerequisite_job_ids: job.prerequisites,
        wait_reason,
        last_error: job.last_error,
        warnings: Vec::new(),
        index_result: job
            .result
            .map(|result| beholder_protocol::v1::IndexJobResult {
                destinations: result
                    .destinations
                    .into_iter()
                    .map(|result| beholder_protocol::v1::IndexDestinationResult {
                        destination: Some(beholder_protocol::v1::IndexDestination {
                            destination: Some(match result.destination {
                                jobs::IndexDestination::Workspace { workspace } => {
                                    index_destination::Destination::Workspace(workspace)
                                }
                                jobs::IndexDestination::StandaloneRepository { repository } => {
                                    index_destination::Destination::StandaloneRepository(repository)
                                }
                            }),
                        }),
                        observation_count: result.observation_count as u64,
                        published: result.published,
                        outcome: match result.outcome {
                            jobs::IndexOutcome::Published => IndexJobOutcome::Published,
                            jobs::IndexOutcome::Unchanged => IndexJobOutcome::Unchanged,
                            jobs::IndexOutcome::Superseded => IndexJobOutcome::Superseded,
                        }
                        .into(),
                    })
                    .collect(),
            }),
    }
}

fn seconds_to_millis(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .map(|value| value.saturating_mul(1_000))
}

fn repository_registry_unavailable() -> Status {
    operation_status(BeholderError::new(
        BeholderErrorKind::Internal,
        BeholderErrorCode::RepositoryRegistryFailed,
        "repository registry is unavailable",
    ))
}

fn progress_event(progress: DtoGarbageCollectProgress) -> GarbageCollectEvent {
    GarbageCollectEvent {
        event: Some(garbage_collect_event::Event::Progress(protocol_progress(
            progress,
        ))),
    }
}

fn protocol_progress(progress: DtoGarbageCollectProgress) -> GarbageCollectProgress {
    let phase = match progress.phase {
        GarbageCollectionPhase::ClaimingObsoleteStates => {
            GarbageCollectPhase::ClaimingObsoleteStates
        }
        GarbageCollectionPhase::SweepingObsoleteStates => {
            GarbageCollectPhase::SweepingObsoleteStates
        }
        GarbageCollectionPhase::CheckpointingDatabase => GarbageCollectPhase::CheckpointingDatabase,
        GarbageCollectionPhase::ReclaimingDatabaseSpace => {
            GarbageCollectPhase::ReclaimingDatabaseSpace
        }
    };
    GarbageCollectProgress {
        phase: phase.into(),
        step: progress.step.unwrap_or_default(),
        completed_steps: progress.completed_steps,
        total_steps: progress.total_steps,
        rows: progress.rows,
        completed_rows: progress.completed_rows,
        stale_states: progress.stale_states,
        repositories: progress.repositories,
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

fn operation_status(error: BeholderError) -> Status {
    operation_status_ref(&error)
}

pub(super) fn operation_status_ref(error: &BeholderError) -> Status {
    let code = match error.kind() {
        BeholderErrorKind::InvalidInput => Code::InvalidArgument,
        BeholderErrorKind::NotFound => Code::NotFound,
        BeholderErrorKind::FailedPrecondition => Code::FailedPrecondition,
        BeholderErrorKind::Unavailable => Code::Unavailable,
        BeholderErrorKind::Internal => Code::Internal,
    };
    if let Some(source) = error.source() {
        tracing::error!(
            error.code = error.code().as_str(),
            error.kind = ?error.kind(),
            source = %source,
            "operation failed"
        );
    }
    let mut status = Status::new(code, error.message().to_owned());
    if let Ok(value) = MetadataValue::try_from(error.code().as_str()) {
        status.metadata_mut().insert(ERROR_CODE_METADATA_KEY, value);
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_errors_map_to_grpc_codes_and_preserve_stable_codes() {
        for (kind, expected) in [
            (BeholderErrorKind::InvalidInput, Code::InvalidArgument),
            (BeholderErrorKind::NotFound, Code::NotFound),
            (
                BeholderErrorKind::FailedPrecondition,
                Code::FailedPrecondition,
            ),
            (BeholderErrorKind::Unavailable, Code::Unavailable),
            (BeholderErrorKind::Internal, Code::Internal),
        ] {
            let status = operation_status(BeholderError::new(
                kind,
                BeholderErrorCode::TransportGrpc,
                "safe public message",
            ));
            assert_eq!(status.code(), expected);
            assert_eq!(status.message(), "safe public message");
            assert_eq!(
                status
                    .metadata()
                    .get(ERROR_CODE_METADATA_KEY)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "beholder.transport.grpc"
            );
        }
    }

    #[test]
    fn job_page_tokens_are_strict_and_round_trip() {
        let id = ulid::Ulid::new().to_string();
        assert_eq!(
            decode_job_page_token(&format!("42:{id}")).unwrap(),
            (42, id)
        );
        assert_eq!(
            decode_job_page_token("not-a-token").unwrap_err().code(),
            Code::InvalidArgument
        );
        assert_eq!(
            decode_job_page_token("42:not-a-ulid").unwrap_err().code(),
            Code::InvalidArgument
        );
    }
}
