use crate::daemon::BeholderDaemon;
use beholder_domain::{BeholderError, BeholderErrorCode, BeholderErrorKind};
use beholder_dto::{DEFAULT_MAX_HOPS, Revisioned, WhyResult};
use beholder_protocol::ERROR_CODE_METADATA_KEY;
use beholder_protocol::v1::{
    ClearCacheRequest, ClearCacheResponse, ContextResponse, DependenciesResponse, EntityRequest,
    GarbageCollectRequest, GarbageCollectResponse, GetStatusRequest, GetStatusResponse,
    ImpactResponse, ListWorkspacesRequest, ListWorkspacesResponse, PathRequest,
    RegisterWorkspaceRequest, RegisterWorkspaceResponse, ReindexWorkspaceRequest,
    ReindexWorkspaceResponse, StopRequest, StopResponse, TraceResponse, TraversalEntityRequest,
    WhyResponse, daemon_server::Daemon,
};
use std::{error::Error, path::PathBuf};
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
            protocol_version: 11,
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
            .map_err(|_| {
                operation_status(BeholderError::new(
                    BeholderErrorKind::Internal,
                    BeholderErrorCode::WorkspaceRegistryFailed,
                    "workspace registry is unavailable",
                ))
            })?
            .get(&workspace_name)
            .cloned()
            .ok_or_else(|| {
                operation_status(BeholderError::new(
                    BeholderErrorKind::NotFound,
                    BeholderErrorCode::WorkspaceNotRegistered,
                    format!("workspace not registered: {workspace_name}"),
                ))
            })?;
        let scheduler = self.scheduler.clone();
        let store = self.store.clone();
        let (observation_count, published) = tokio::task::spawn_blocking(move || {
            scheduler
                .index(&store, &workspace)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|source| {
            operation_status(
                BeholderError::new(
                    BeholderErrorKind::Internal,
                    BeholderErrorCode::WorkspaceIndexWorkerFailed,
                    "workspace index worker failed",
                )
                .with_source(source),
            )
        })?
        .map_err(|source| {
            operation_status(
                BeholderError::new(
                    BeholderErrorKind::FailedPrecondition,
                    BeholderErrorCode::WorkspaceIndexFailed,
                    "workspace indexing failed",
                )
                .with_source(std::io::Error::other(source)),
            )
        })?;
        if published {
            self.scheduler.schedule_checkpoint(self.store.clone());
        }
        Ok(Response::new(ReindexWorkspaceResponse {
            observation_count: observation_count.try_into().map_err(|_| {
                operation_status(BeholderError::new(
                    BeholderErrorKind::Internal,
                    BeholderErrorCode::WorkspaceObservationCountOverflow,
                    "workspace observation count exceeds protocol capacity",
                ))
            })?,
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
        crate::daemon::update_workspace_watch(&mut watcher, previous.as_ref(), &workspace)
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

fn operation_status(error: BeholderError) -> Status {
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
}
