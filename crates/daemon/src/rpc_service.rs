use crate::daemon::{BeholderDaemon, collect_garbage, start_claimed_garbage_collector};
use crate::jobs::{
    self, EnrichmentJob, IndexJob, IndexJobId, IndexTarget, ManualEnrichmentSubmission, StoredJob,
    StoredJobKind, StoredJobStatus, StoredJobTarget,
};
use crate::repository_registry::RegisteredRepository;
use crate::rpc::semantic_query;
use beholder_domain::{BeholderError, BeholderErrorCode, BeholderErrorKind, Workspace};
use beholder_dto::{
    DEFAULT_MAX_HOPS, GarbageCollectionPhase,
    GarbageCollectionProgress as DtoGarbageCollectProgress,
    RepositoryStatus as DtoRepositoryStatus, Revisioned, WhyResult,
};
use beholder_protocol::ERROR_CODE_METADATA_KEY;
use beholder_protocol::v1::{
    ClearCacheRequest, ClearCacheResponse, ContextResponse, DeleteRepositoryRequest,
    DeleteRepositoryResponse, DependenciesResponse, EnrichmentJobOutcome, EnrichmentSubmission,
    EnrichmentSubmissionDisposition, EntityRequest, GarbageCollectEvent, GarbageCollectPhase,
    GarbageCollectProgress, GarbageCollectRequest, GarbageCollectResponse,
    GetGarbageCollectionStatusRequest, GetGarbageCollectionStatusResponse, GetJobRequest,
    GetJobResponse, GetRepositoryRequest, GetStatusRequest, GetStatusResponse, ImpactResponse,
    IndexJobOutcome, Job, JobStatus, JobSummary, JobTarget, JobTrigger, JobType, JobWaitReason,
    ListJobsRequest, ListJobsResponse, ListWorkspacesRequest, ListWorkspacesResponse, PathRequest,
    RegisterRepositoryRequest, RegisterWorkspaceRequest, RegisterWorkspaceResponse,
    RepositoryResponse, SetWorkspacePluginRequest, SetWorkspacePluginResponse, StopRequest,
    StopResponse, SubmitEnrichmentRequest, SubmitEnrichmentResponse, SubmitIndexRequest,
    SubmitIndexResponse, TraceResponse, TraversalEntityRequest, WhyResponse, daemon_server::Daemon,
    garbage_collect_event, index_destination, job_target, submit_index_request,
};
use std::{collections::BTreeSet, error::Error, path::PathBuf, sync::atomic::Ordering};
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
        let enriching = self
            .jobs
            .active_enrichment_repositories(&request.workspace)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let store = self.store.clone();
        let workspace = request.workspace.clone();
        let entity = request.entity.clone();
        let result = semantic_query(move || store.context_snapshot(&workspace, &entity)).await?;
        self.query_response(&request.workspace, enriching, result)
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
        let enriching = self
            .jobs
            .active_enrichment_repositories(&request.workspace)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let store = self.store.clone();
        let workspace = request.workspace.clone();
        let entity = request.entity.clone();
        let result =
            semantic_query(move || store.dependencies_snapshot(&workspace, &entity, max_hops))
                .await?;
        self.query_response(&request.workspace, enriching, result)
    }

    #[tracing::instrument(name = "rpc.get_status", skip_all, err)]
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            status: "ready".into(),
            protocol_version: 20,
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

    #[tracing::instrument(name = "rpc.submit_enrichment", skip_all, err)]
    async fn submit_enrichment(
        &self,
        request: Request<SubmitEnrichmentRequest>,
    ) -> Result<Response<SubmitEnrichmentResponse>, Status> {
        let request = request.into_inner();
        if request.repository.trim().is_empty() {
            return Err(Status::invalid_argument("repository is required"));
        }
        let requested_workers = request
            .worker_ids
            .into_iter()
            .map(|worker| {
                if worker.trim().is_empty() {
                    Err(Status::invalid_argument("worker ID must not be empty"))
                } else {
                    Ok(worker)
                }
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let workspaces = {
            let registry = self
                .workspaces
                .lock()
                .map_err(|_| repository_registry_unavailable())?;
            if registry.repository(&request.repository).is_none() {
                return Err(operation_status(BeholderError::new(
                    BeholderErrorKind::NotFound,
                    BeholderErrorCode::RepositoryNotRegistered,
                    format!("repository not registered: {}", request.repository),
                )));
            }
            if let Some(scope) = request.workspace_scope.as_deref() {
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
                    .any(|candidate| candidate.repository.identity == request.repository)
                {
                    return Err(Status::invalid_argument(format!(
                        "repository {} is not in workspace {scope}",
                        request.repository
                    )));
                }
            }
            registry.list()
        };
        let target = IndexTarget::Repository {
            repository: request.repository.clone(),
            workspace_scope: request.workspace_scope.clone(),
        };
        let mut prerequisites = self
            .jobs
            .overlapping_index_jobs(&target, &workspaces)
            .await
            .map_err(queue_status)?;
        let planned = self
            .scheduler
            .manual_enrichment_targets(
                &self.store,
                &self.workspaces,
                &request.repository,
                request.workspace_scope.as_deref(),
                &requested_workers,
            )
            .map_err(|error| {
                if error.starts_with("unknown enrichment worker ") {
                    Status::invalid_argument(error)
                } else {
                    Status::internal(error)
                }
            })?;
        prerequisites.retain(|job| {
            planned.iter().any(|candidate| {
                index_job_covers_enrichment_target(job, &candidate.target, &workspaces)
            })
        });
        if planned.iter().any(|candidate| {
            candidate.input_fingerprint.is_none()
                && !prerequisites.iter().any(|job| {
                    index_job_covers_enrichment_target(job, &candidate.target, &workspaces)
                })
        }) {
            let (id, _) = self
                .jobs
                .enqueue_manual_index(
                    IndexJob {
                        target,
                        trigger: jobs::JobTrigger::Manual,
                        prerequisite_index_jobs: Vec::new(),
                        generation: None,
                        repository_intents: Vec::new(),
                    },
                    &workspaces,
                )
                .await
                .map_err(queue_status)?;
            prerequisites.push(
                self.jobs
                    .get(&id.0)
                    .await
                    .map_err(|error| Status::internal(error.to_string()))?
                    .ok_or_else(|| Status::internal("submitted index prerequisite disappeared"))?,
            );
        }
        prerequisites.sort_by(|left, right| left.id.cmp(&right.id));
        prerequisites.dedup_by(|left, right| left.id == right.id);
        let mut results = Vec::new();
        for candidate in planned {
            let prerequisite_ids = prerequisites
                .iter()
                .filter(|job| {
                    index_job_covers_enrichment_target(job, &candidate.target, &workspaces)
                })
                .map(|job| IndexJobId(job.id.clone()))
                .collect::<Vec<_>>();
            let has_prerequisites = !prerequisite_ids.is_empty();
            if !has_prerequisites && candidate.current {
                results.push(EnrichmentSubmission {
                    target: Some(enrichment_job_target(
                        &candidate.target,
                        &candidate.worker_id,
                    )),
                    disposition: EnrichmentSubmissionDisposition::AlreadyCurrent.into(),
                    job: None,
                });
                continue;
            }
            let job = EnrichmentJob {
                target: candidate.target,
                worker_id: candidate.worker_id,
                expected_worker_version: candidate.worker_version,
                trigger: jobs::JobTrigger::Manual,
                prerequisite_index_jobs: prerequisite_ids,
                input_fingerprint: (!has_prerequisites)
                    .then_some(candidate.input_fingerprint)
                    .flatten(),
            };
            let (disposition, submitted) = match self
                .jobs
                .enqueue_manual_enrichment(job)
                .await
                .map_err(queue_status)?
            {
                ManualEnrichmentSubmission::Enqueued(id) => {
                    let submitted = self
                        .jobs
                        .get(&id)
                        .await
                        .map_err(|error| Status::internal(error.to_string()))?
                        .ok_or_else(|| Status::internal("submitted enrichment job disappeared"))?;
                    (EnrichmentSubmissionDisposition::Enqueued, submitted)
                }
                ManualEnrichmentSubmission::InProgress(job) => {
                    (EnrichmentSubmissionDisposition::InProgress, *job)
                }
            };
            let summary = job_summary(submitted);
            results.push(EnrichmentSubmission {
                target: summary.target.clone(),
                disposition: disposition.into(),
                job: Some(summary),
            });
        }
        Ok(Response::new(SubmitEnrichmentResponse {
            results,
            prerequisite_jobs: prerequisites.into_iter().map(job_summary).collect(),
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
        let enriching = self
            .jobs
            .active_enrichment_repositories(&request.workspace)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let store = self.store.clone();
        let workspace = request.workspace.clone();
        let entity = request.entity.clone();
        let result =
            semantic_query(move || store.impact_snapshot(&workspace, &entity, max_hops)).await?;
        self.query_response(&request.workspace, enriching, result)
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
        let enriching = self
            .jobs
            .active_enrichment_repositories(&request.workspace)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let store = self.store.clone();
        let workspace = request.workspace.clone();
        let from = request.from.clone();
        let to = request.to.clone();
        let result =
            semantic_query(move || store.trace_snapshot(&workspace, &from, &to, max_hops)).await?;
        self.query_response(&request.workspace, enriching, result)
    }

    #[tracing::instrument(
        name = "rpc.why",
        skip_all,
        fields(workspace = %request.get_ref().workspace, from = %request.get_ref().from, to = %request.get_ref().to)
    )]
    async fn why(&self, request: Request<PathRequest>) -> Result<Response<WhyResponse>, Status> {
        let request = request.into_inner();
        let max_hops = max_hops(request.max_hops)?;
        let enriching = self
            .jobs
            .active_enrichment_repositories(&request.workspace)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let store = self.store.clone();
        let workspace = request.workspace.clone();
        let from = request.from.clone();
        let to = request.to.clone();
        let revisioned = semantic_query(move || {
            store
                .trace_snapshot(&workspace, &from, &to, max_hops)
                .map(|revisioned| Revisioned {
                    result: WhyResult::from(revisioned.result),
                    analysis_revision: revisioned.analysis_revision,
                    analysis: revisioned.analysis,
                })
        })
        .await?;
        self.query_response(&request.workspace, enriching, revisioned)
    }
}

fn index_job_covers_enrichment_target(
    job: &StoredJob,
    target: &jobs::EnrichmentTarget,
    workspaces: &[Workspace],
) -> bool {
    let (workspace, repository) = match target {
        jobs::EnrichmentTarget::WorkspaceRepository {
            workspace,
            repository,
        } => (Some(workspace.as_str()), repository),
        jobs::EnrichmentTarget::StandaloneRepository { repository } => (None, repository),
    };
    match &job.target {
        StoredJobTarget::Workspace(index_workspace) => {
            workspace == Some(index_workspace.as_str())
                && workspaces
                    .iter()
                    .find(|candidate| candidate.name == *index_workspace)
                    .is_some_and(|candidate| {
                        candidate
                            .repositories
                            .iter()
                            .any(|candidate| candidate.repository.identity == *repository)
                    })
        }
        StoredJobTarget::Repository {
            repository: index_repository,
            workspace_scope,
            ..
        } => {
            index_repository == repository
                && workspace_scope
                    .as_deref()
                    .is_none_or(|scope| workspace == Some(scope))
        }
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

fn enrichment_job_target(target: &jobs::EnrichmentTarget, worker_id: &str) -> JobTarget {
    let (repository, workspace_scope) = match target {
        jobs::EnrichmentTarget::WorkspaceRepository {
            workspace,
            repository,
        } => (repository.clone(), Some(workspace.clone())),
        jobs::EnrichmentTarget::StandaloneRepository { repository } => (repository.clone(), None),
    };
    JobTarget {
        target: Some(job_target::Target::Repository(repository)),
        workspace_scope,
        worker_id: Some(worker_id.into()),
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
        warnings: job.warnings,
        index_result: job
            .index_result
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
        enrichment_result: job.enrichment_result.map(|result| {
            beholder_protocol::v1::EnrichmentJobResult {
                target: Some(enrichment_job_target(&result.target, &result.worker_id)),
                expected_worker_version: result.expected_worker_version,
                outcome: match result.outcome {
                    jobs::EnrichmentOutcome::Published => EnrichmentJobOutcome::Published,
                    jobs::EnrichmentOutcome::Unchanged => EnrichmentJobOutcome::Unchanged,
                    jobs::EnrichmentOutcome::AlreadyCurrent => EnrichmentJobOutcome::AlreadyCurrent,
                    jobs::EnrichmentOutcome::Superseded => EnrichmentJobOutcome::Superseded,
                }
                .into(),
                failed_prerequisite_job_ids: result
                    .failed_prerequisite_index_jobs
                    .into_iter()
                    .map(|id| id.0)
                    .collect(),
            }
        }),
    }
}

fn queue_status(error: Box<dyn Error + Send + Sync>) -> Status {
    if error.to_string() == "job admission is closed" {
        Status::unavailable("job admission is closed")
    } else {
        Status::internal(error.to_string())
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

#[cfg(test)]
mod manual_enrichment_tests {
    use super::*;
    use crate::{daemon, jobs, workspace_registry, workspace_registry::WorkspaceRegistry};
    use beholder_adapters_mnestic::SemanticStore;
    use beholder_indexing::{
        AnalyzerContribution, AnalyzerMetadata, EnrichmentFuture, EnrichmentSnapshot,
        IndexerBuilder, WorkspaceEnricher,
    };
    use std::{fs, path::Path, time::Duration};

    struct EmptyEnricher {
        id: &'static str,
        requires_enablement: bool,
    }

    impl WorkspaceEnricher for EmptyEnricher {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: self.id.into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            path.extension().is_some_and(|extension| extension == "rs")
        }

        fn requires_workspace_enablement(&self) -> bool {
            self.requires_enablement
        }

        fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
            Box::pin(async move {
                Ok(AnalyzerContribution {
                    metadata: self.metadata(),
                    active_repositories: vec![snapshot.target_repository],
                    repositories: Vec::new(),
                    overrides: Vec::new(),
                    candidate_overrides: Vec::new(),
                    graphql_resolvers: Vec::new(),
                    diagnostics: Vec::new(),
                    cache: Default::default(),
                })
            })
        }
    }

    async fn wait_for_terminal(queue: &jobs::JobQueue, ids: &[String]) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut terminal = true;
                for id in ids {
                    terminal &= queue.get(id).await.unwrap().is_some_and(|job| {
                        matches!(
                            job.status,
                            StoredJobStatus::Completed | StoredJobStatus::Failed
                        )
                    });
                }
                if terminal {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("manual enrichment jobs did not finish");
    }

    #[tokio::test]
    async fn submit_enrichment_fans_out_reuses_and_becomes_current() {
        let state =
            std::env::temp_dir().join(format!("beholder-submit-enrichment-{}", ulid::Ulid::new()));
        let repository = state.join("repository");
        let standalone = state.join("standalone");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::create_dir_all(standalone.join("src")).unwrap();
        fs::write(repository.join("src/lib.rs"), "fn workspace() {}").unwrap();
        fs::write(standalone.join("src/lib.rs"), "fn standalone() {}").unwrap();
        let mut registry =
            WorkspaceRegistry::open(workspace_registry::registry_path(&state)).unwrap();
        let workspace = registry
            .register_with_plugins(
                "first".into(),
                vec![repository.clone()],
                Vec::new(),
                vec!["plugin".into()],
            )
            .unwrap();
        registry
            .register("second".into(), vec![repository], Vec::new())
            .unwrap();
        let repository = workspace.repositories[0].repository.identity.clone();
        let standalone = registry
            .register_repository(standalone)
            .unwrap()
            .selection
            .repository
            .identity;
        let registered_workspaces = registry.list();
        let queue = jobs::JobQueue::open(&state.join("queue.sqlite"))
            .await
            .unwrap();
        let (partial_prerequisite, _) = queue
            .enqueue_manual_index(
                jobs::IndexJob {
                    target: jobs::IndexTarget::Repository {
                        repository: repository.clone(),
                        workspace_scope: Some("first".into()),
                    },
                    trigger: jobs::JobTrigger::Manual,
                    prerequisite_index_jobs: Vec::new(),
                    generation: None,
                    repository_intents: Vec::new(),
                },
                &registered_workspaces,
            )
            .await
            .unwrap();
        let (service, _, scheduler) = daemon::build(
            SemanticStore::memory().unwrap(),
            registry,
            IndexerBuilder::new(state.join("cache"), 1)
                .add_enricher(EmptyEnricher {
                    id: "semantic",
                    requires_enablement: false,
                })
                .add_enricher(EmptyEnricher {
                    id: "plugin",
                    requires_enablement: true,
                })
                .build()
                .unwrap(),
            queue.clone(),
        )
        .unwrap();

        let submit = |repository: String| SubmitEnrichmentRequest {
            repository,
            workspace_scope: None,
            worker_ids: vec!["semantic".into()],
        };
        let first = Daemon::submit_enrichment(&service, Request::new(submit(repository.clone())))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(first.results.len(), 2);
        assert_eq!(first.prerequisite_jobs.len(), 2);
        assert!(
            first
                .prerequisite_jobs
                .iter()
                .any(|job| job.id == partial_prerequisite.0)
        );
        assert!(first.results.iter().all(|result| {
            result.disposition == EnrichmentSubmissionDisposition::Enqueued as i32
                && result.job.is_some()
        }));
        for result in &first.results {
            let job = queue
                .get(&result.job.as_ref().unwrap().id)
                .await
                .unwrap()
                .unwrap();
            let expected = usize::from(
                result.target.as_ref().unwrap().workspace_scope.as_deref() == Some("first"),
            ) + 1;
            assert_eq!(job.prerequisites.len(), expected);
        }
        let repeated =
            Daemon::submit_enrichment(&service, Request::new(submit(repository.clone())))
                .await
                .unwrap()
                .into_inner();
        assert_eq!(
            repeated
                .prerequisite_jobs
                .iter()
                .map(|job| &job.id)
                .collect::<Vec<_>>(),
            first
                .prerequisite_jobs
                .iter()
                .map(|job| &job.id)
                .collect::<Vec<_>>()
        );
        assert!(repeated.results.iter().all(|result| {
            result.disposition == EnrichmentSubmissionDisposition::InProgress as i32
        }));
        let scoped = Daemon::submit_enrichment(
            &service,
            Request::new(SubmitEnrichmentRequest {
                repository: repository.clone(),
                workspace_scope: Some("first".into()),
                worker_ids: vec!["semantic".into()],
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(scoped.results.len(), 1);
        assert!(
            scoped.results[0]
                .target
                .as_ref()
                .is_some_and(|target| target.workspace_scope.as_deref() == Some("first"))
        );
        let plugin = Daemon::submit_enrichment(
            &service,
            Request::new(SubmitEnrichmentRequest {
                repository: repository.clone(),
                workspace_scope: None,
                worker_ids: vec!["plugin".into()],
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(plugin.results.len(), 1);
        assert!(
            plugin.results[0]
                .target
                .as_ref()
                .is_some_and(|target| target.worker_id.as_deref() == Some("plugin")
                    && target.workspace_scope.as_deref() == Some("first"))
        );
        let all = Daemon::submit_enrichment(
            &service,
            Request::new(SubmitEnrichmentRequest {
                repository: repository.clone(),
                workspace_scope: None,
                worker_ids: Vec::new(),
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(all.results.len(), 3);
        let standalone_response =
            Daemon::submit_enrichment(&service, Request::new(submit(standalone)))
                .await
                .unwrap()
                .into_inner();
        assert_eq!(standalone_response.results.len(), 1);
        assert!(
            standalone_response.results[0]
                .target
                .as_ref()
                .is_some_and(|target| target.workspace_scope.is_none())
        );
        let unknown = Daemon::submit_enrichment(
            &service,
            Request::new(SubmitEnrichmentRequest {
                repository: repository.clone(),
                workspace_scope: None,
                worker_ids: vec!["missing".into()],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.code(), Code::InvalidArgument);

        let index_worker = jobs::start_index_worker(
            queue.clone(),
            scheduler.clone(),
            service.store.clone(),
            service.workspaces.clone(),
        );
        let enrichment_worker = jobs::start_enrichment_worker(
            queue.clone(),
            scheduler.clone(),
            service.store.clone(),
            service.workspaces.clone(),
        );
        let ids = first
            .prerequisite_jobs
            .iter()
            .chain(&standalone_response.prerequisite_jobs)
            .map(|job| job.id.clone())
            .chain(
                first
                    .results
                    .iter()
                    .chain(&plugin.results)
                    .chain(&standalone_response.results)
                    .filter_map(|result| result.job.as_ref().map(|job| job.id.clone())),
            )
            .collect::<Vec<_>>();
        wait_for_terminal(&queue, &ids).await;
        let fingerprints = ["first", "second"]
            .into_iter()
            .map(|workspace| {
                (
                    workspace,
                    service
                        .store
                        .revision_enrichment_input_fingerprint(workspace, &repository, "semantic")
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        for id in &ids {
            let job = queue.get(id).await.unwrap().unwrap();
            assert_eq!(
                job.status,
                StoredJobStatus::Completed,
                "job {id} failed: {:?}; fingerprints={fingerprints:?}",
                job.last_error,
            );
        }

        let current = Daemon::submit_enrichment(&service, Request::new(submit(repository.clone())))
            .await
            .unwrap()
            .into_inner();
        assert!(
            current.prerequisite_jobs.is_empty(),
            "unexpected prerequisites: {current:?}"
        );
        assert!(current.results.iter().all(|result| {
            result.disposition == EnrichmentSubmissionDisposition::AlreadyCurrent as i32
                && result.job.is_none()
        }));

        let failed_prerequisite = queue
            .enqueue_automatic_index(jobs::IndexJob {
                target: jobs::IndexTarget::Workspace {
                    workspace: "missing".into(),
                },
                trigger: jobs::JobTrigger::Automatic,
                prerequisite_index_jobs: Vec::new(),
                generation: None,
                repository_intents: Vec::new(),
            })
            .await
            .unwrap()
            .unwrap();
        wait_for_terminal(&queue, std::slice::from_ref(&failed_prerequisite.0)).await;
        let input_fingerprint = service
            .store
            .revision_enrichment_input_fingerprint("first", &repository, "semantic")
            .unwrap();
        let enrichment = match queue
            .enqueue_manual_enrichment(jobs::EnrichmentJob {
                target: jobs::EnrichmentTarget::WorkspaceRepository {
                    workspace: "first".into(),
                    repository,
                },
                worker_id: "semantic".into(),
                expected_worker_version: "1".into(),
                trigger: jobs::JobTrigger::Manual,
                prerequisite_index_jobs: vec![failed_prerequisite.clone()],
                input_fingerprint,
            })
            .await
            .unwrap()
        {
            ManualEnrichmentSubmission::Enqueued(id) => id,
            ManualEnrichmentSubmission::InProgress(_) => {
                panic!("failed prerequisite submission reused unrelated work")
            }
        };
        wait_for_terminal(&queue, std::slice::from_ref(&enrichment)).await;
        let enrichment = queue.get(&enrichment).await.unwrap().unwrap();
        assert_eq!(enrichment.status, StoredJobStatus::Completed);
        assert_eq!(enrichment.warnings.len(), 1);
        assert!(
            enrichment.enrichment_result.is_some_and(|result| result
                .failed_prerequisite_index_jobs
                == [failed_prerequisite])
        );
        assert_eq!(
            Daemon::get_job(&service, Request::new(GetJobRequest { id: "bad".into() }))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        assert_eq!(
            Daemon::get_job(
                &service,
                Request::new(GetJobRequest {
                    id: ulid::Ulid::new().to_string(),
                })
            )
            .await
            .unwrap_err()
            .code(),
            Code::NotFound
        );

        scheduler.stop();
        let _ = index_worker.context.stop();
        let _ = enrichment_worker.context.stop();
        let _ = index_worker.task.await;
        let _ = enrichment_worker.task.await;
        let _ = fs::remove_dir_all(state);
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
