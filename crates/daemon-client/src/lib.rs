use beholder_domain::{BeholderError, BeholderErrorCode, BeholderErrorKind, Workspace};
use beholder_dto::{
    ContextResult, DependenciesResult, GarbageCollection, GarbageCollectionEvent,
    GarbageCollectionPhase, GarbageCollectionProgress, GarbageCollectionStatus, ImpactResult,
    RepositoryStatus, TraceResult, WhyResult,
};
use beholder_protocol::{
    ERROR_CODE_METADATA_KEY,
    v1::{
        ClearCacheRequest, DeleteRepositoryRequest, EntityRequest,
        GarbageCollectEvent as ProtocolGarbageCollectEvent, GarbageCollectPhase,
        GarbageCollectProgress as ProtocolGarbageCollectProgress, GarbageCollectRequest,
        GetGarbageCollectionStatusRequest, GetJobRequest, GetJobResponse, GetRepositoryRequest,
        GetStatusRequest, GetStatusResponse, ListJobsRequest, ListJobsResponse,
        ListWorkspacesRequest, PathRequest, RegisterRepositoryRequest, RegisterWorkspaceRequest,
        RepositoryIndexTarget, SetWorkspacePluginRequest, StopRequest, SubmitIndexRequest,
        SubmitIndexResponse, TraversalEntityRequest, daemon_client::DaemonClient,
        garbage_collect_event, submit_index_request,
    },
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tonic::{Code, Request, Status, transport::Channel};

const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const SUBMIT_INDEX_TIMEOUT: Duration = Duration::from_secs(10);

pub fn socket_path() -> Result<PathBuf, String> {
    Ok(state_dir()?.join("beholder.sock"))
}

fn endpoint() -> Result<String, String> {
    socket_path()?
        .to_str()
        .map(|path| format!("unix:{path}"))
        .ok_or_else(|| "daemon socket path is not UTF-8".into())
}

async fn connect() -> Result<DaemonClient<Channel>, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint()?)
        .await?
        .max_decoding_message_size(MAX_RESPONSE_BYTES))
}

pub fn state_dir() -> Result<PathBuf, String> {
    let base = if let Some(path) = env_path("BEHOLDER_STATE_DIR") {
        path
    } else if let Some(path) = env_path("XDG_STATE_HOME") {
        path.join("beholder")
    } else if let Some(path) = env_path("HOME") {
        path.join(".local/state/beholder")
    } else {
        return Err("cannot locate daemon state: set HOME or BEHOLDER_STATE_DIR".into());
    };
    Ok(base.join("daemon"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub async fn get_status() -> Result<GetStatusResponse, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .get_status(request(GetStatusRequest {}))
        .await?
        .into_inner())
}

pub async fn list_jobs(
    page_token: Option<String>,
) -> Result<ListJobsResponse, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .list_jobs(request(ListJobsRequest { page_token }))
        .await?
        .into_inner())
}

pub async fn get_job(id: String) -> Result<GetJobResponse, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .get_job(request(GetJobRequest { id }))
        .await?
        .into_inner())
}

#[derive(Debug, Eq, PartialEq)]
pub enum IndexTarget {
    Workspace(String),
    Repository {
        repository: String,
        workspace_scope: Option<String>,
    },
}

pub async fn submit_index(
    target: IndexTarget,
) -> Result<SubmitIndexResponse, Box<dyn std::error::Error>> {
    let target = match target {
        IndexTarget::Workspace(workspace) => submit_index_request::Target::Workspace(workspace),
        IndexTarget::Repository {
            repository,
            workspace_scope,
        } => submit_index_request::Target::Repository(RepositoryIndexTarget {
            repository,
            workspace_scope,
        }),
    };
    let mut request = request(SubmitIndexRequest {
        target: Some(target),
    });
    request.set_timeout(SUBMIT_INDEX_TIMEOUT);
    Ok(operation_client()
        .await?
        .submit_index(request)
        .await
        .map_err(operation_error)?
        .into_inner())
}

pub async fn clear_cache() -> Result<(), Box<dyn std::error::Error>> {
    connect()
        .await?
        .clear_cache(request(ClearCacheRequest {}))
        .await?;
    Ok(())
}

pub struct GarbageCollectionStream {
    inner: tonic::Streaming<ProtocolGarbageCollectEvent>,
    completed: bool,
}

impl GarbageCollectionStream {
    pub async fn message(&mut self) -> Result<Option<GarbageCollectionEvent>, BeholderError> {
        let Some(event) = self.inner.message().await.map_err(operation_error)? else {
            return if self.completed {
                Ok(None)
            } else {
                Err(invalid_garbage_collection_event())
            };
        };
        let event = match event.event {
            Some(garbage_collect_event::Event::Progress(progress)) => {
                GarbageCollectionEvent::Progress(garbage_collection_progress(progress)?)
            }
            Some(garbage_collect_event::Event::Completed(completed)) => {
                self.completed = true;
                GarbageCollectionEvent::Completed(GarbageCollection {
                    repository_states_queued: completed.repository_states_queued,
                })
            }
            None => return Err(invalid_garbage_collection_event()),
        };
        Ok(Some(event))
    }
}

pub async fn garbage_collect() -> Result<GarbageCollectionStream, BeholderError> {
    let response = operation_client()
        .await?
        .garbage_collect(request(GarbageCollectRequest {}))
        .await
        .map_err(operation_error)?
        .into_inner();
    Ok(GarbageCollectionStream {
        inner: response,
        completed: false,
    })
}

pub async fn get_garbage_collection_status() -> Result<GarbageCollectionStatus, BeholderError> {
    let status = operation_client()
        .await?
        .get_garbage_collection_status(request(GetGarbageCollectionStatusRequest {}))
        .await
        .map_err(operation_error)?
        .into_inner();
    Ok(GarbageCollectionStatus {
        running: status.running,
        repository_states_collectible: status.repository_states_collectible,
        repository_states_queued: status.repository_states_queued,
        reclaimable_database_pages: status.reclaimable_database_pages,
        progress: status
            .progress
            .map(garbage_collection_progress)
            .transpose()?,
    })
}

fn garbage_collection_progress(
    progress: ProtocolGarbageCollectProgress,
) -> Result<GarbageCollectionProgress, BeholderError> {
    let phase = match GarbageCollectPhase::try_from(progress.phase) {
        Ok(GarbageCollectPhase::ClaimingObsoleteStates) => {
            GarbageCollectionPhase::ClaimingObsoleteStates
        }
        Ok(GarbageCollectPhase::SweepingObsoleteStates) => {
            GarbageCollectionPhase::SweepingObsoleteStates
        }
        Ok(GarbageCollectPhase::CheckpointingDatabase) => {
            GarbageCollectionPhase::CheckpointingDatabase
        }
        Ok(GarbageCollectPhase::ReclaimingDatabaseSpace) => {
            GarbageCollectionPhase::ReclaimingDatabaseSpace
        }
        _ => return Err(invalid_garbage_collection_event()),
    };
    Ok(GarbageCollectionProgress {
        phase,
        step: (!progress.step.is_empty()).then_some(progress.step),
        rows: progress.rows,
        completed_rows: progress.completed_rows,
        stale_states: progress.stale_states,
        repositories: progress.repositories,
        completed_steps: progress.completed_steps,
        total_steps: progress.total_steps,
    })
}

fn invalid_garbage_collection_event() -> BeholderError {
    BeholderError::new(
        BeholderErrorKind::Internal,
        BeholderErrorCode::TransportGrpc,
        "Beholder daemon returned an invalid garbage collection event",
    )
}

pub async fn context(
    workspace: String,
    entity: String,
) -> Result<ContextResult, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .context(request(EntityRequest { workspace, entity }))
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn dependencies(
    workspace: String,
    entity: String,
    max_hops: u32,
) -> Result<DependenciesResult, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .dependencies(request(TraversalEntityRequest {
            workspace,
            entity,
            max_hops: Some(max_hops),
        }))
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn impact(
    workspace: String,
    entity: String,
    max_hops: u32,
) -> Result<ImpactResult, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .impact(request(TraversalEntityRequest {
            workspace,
            entity,
            max_hops: Some(max_hops),
        }))
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn trace(
    workspace: String,
    from: String,
    to: String,
    max_hops: u32,
) -> Result<TraceResult, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .trace(request(PathRequest {
            workspace,
            from,
            to,
            max_hops: Some(max_hops),
        }))
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn why(
    workspace: String,
    from: String,
    to: String,
    max_hops: u32,
) -> Result<WhyResult, Box<dyn std::error::Error>> {
    Ok(connect()
        .await?
        .why(request(PathRequest {
            workspace,
            from,
            to,
            max_hops: Some(max_hops),
        }))
        .await?
        .into_inner()
        .try_into()?)
}

async fn operation_client() -> Result<DaemonClient<Channel>, BeholderError> {
    let endpoint = endpoint().map_err(|source| {
        BeholderError::new(
            BeholderErrorKind::Unavailable,
            BeholderErrorCode::DaemonUnavailable,
            "Beholder daemon is unavailable",
        )
        .with_source(std::io::Error::other(source))
    })?;
    DaemonClient::connect(endpoint)
        .await
        .map(|client| client.max_decoding_message_size(MAX_RESPONSE_BYTES))
        .map_err(|source| {
            BeholderError::new(
                BeholderErrorKind::Unavailable,
                BeholderErrorCode::DaemonUnavailable,
                "Beholder daemon is unavailable",
            )
            .with_source(source)
        })
}

fn operation_error(status: Status) -> BeholderError {
    let kind = match status.code() {
        Code::InvalidArgument => BeholderErrorKind::InvalidInput,
        Code::NotFound => BeholderErrorKind::NotFound,
        Code::FailedPrecondition => BeholderErrorKind::FailedPrecondition,
        Code::Unavailable => BeholderErrorKind::Unavailable,
        _ => BeholderErrorKind::Internal,
    };
    let code = status
        .metadata()
        .get(ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(BeholderErrorCode::TransportGrpc);
    let message = status.message().to_owned();
    BeholderError::new(kind, code, message).with_source(status)
}

pub async fn register_workspace(
    name: String,
    repositories: &[PathBuf],
    protobuf_descriptors: &[PathBuf],
    enabled_plugins: &[String],
) -> Result<Workspace, Box<dyn std::error::Error>> {
    let repository_paths = repositories
        .iter()
        .map(|path| path_string(path))
        .collect::<Result<_, _>>()?;
    let workspace = connect()
        .await?
        .register_workspace(request(RegisterWorkspaceRequest {
            name,
            repository_paths,
            protobuf_descriptor_paths: protobuf_descriptors
                .iter()
                .map(|path| path_string(path))
                .collect::<Result<_, _>>()?,
            enabled_plugins: enabled_plugins.to_vec(),
        }))
        .await?
        .into_inner()
        .workspace
        .ok_or("daemon returned no workspace")?;
    Ok(workspace.try_into()?)
}

pub async fn register_repository(
    path: &Path,
) -> Result<RepositoryStatus, Box<dyn std::error::Error>> {
    let response = operation_client()
        .await?
        .register_repository(request(RegisterRepositoryRequest {
            path: path_string(path)?,
        }))
        .await
        .map_err(operation_error)?
        .into_inner();
    Ok(response
        .repository
        .ok_or("daemon returned no repository")?
        .try_into()?)
}

pub async fn get_repository(
    identity: String,
) -> Result<RepositoryStatus, Box<dyn std::error::Error>> {
    let response = operation_client()
        .await?
        .get_repository(request(GetRepositoryRequest { identity }))
        .await
        .map_err(operation_error)?
        .into_inner();
    Ok(response
        .repository
        .ok_or("daemon returned no repository")?
        .try_into()?)
}

pub async fn delete_repository(identity: String) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(operation_client()
        .await?
        .delete_repository(request(DeleteRepositoryRequest { identity }))
        .await
        .map_err(operation_error)?
        .into_inner()
        .repository_states_queued)
}

pub async fn list_workspaces() -> Result<Vec<Workspace>, Box<dyn std::error::Error>> {
    operation_client()
        .await?
        .list_workspaces(request(ListWorkspacesRequest {}))
        .await
        .map_err(operation_error)?
        .into_inner()
        .workspaces
        .into_iter()
        .map(|workspace| workspace.try_into().map_err(Into::into))
        .collect()
}

pub async fn set_workspace_plugin(
    workspace: String,
    plugin: String,
    enabled: bool,
) -> Result<Workspace, Box<dyn std::error::Error>> {
    let workspace = connect()
        .await?
        .set_workspace_plugin(request(SetWorkspacePluginRequest {
            workspace,
            plugin,
            enabled,
        }))
        .await?
        .into_inner()
        .workspace
        .ok_or("daemon returned no workspace")?;
    Ok(workspace.try_into()?)
}

fn path_string(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("repository path is not UTF-8: {}", path.display()).into())
}

pub async fn stop() -> Result<bool, Box<dyn std::error::Error>> {
    let Ok(mut client) = connect().await else {
        return Ok(false);
    };
    Ok(client
        .stop(request(StopRequest {}))
        .await?
        .into_inner()
        .accepted)
}

fn request<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    beholder_observability::inject_current_context(request.metadata_mut());
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_dto::{AnalysisCompleteness, AnalysisDiagnosticSeverity, QueryMetadata};
    use beholder_protocol::v1;
    use tonic::metadata::MetadataValue;

    #[test]
    fn operation_errors_preserve_codes_independently_from_messages() {
        for (grpc, expected) in [
            (Code::InvalidArgument, BeholderErrorKind::InvalidInput),
            (Code::NotFound, BeholderErrorKind::NotFound),
            (
                Code::FailedPrecondition,
                BeholderErrorKind::FailedPrecondition,
            ),
            (Code::Unavailable, BeholderErrorKind::Unavailable),
            (Code::Internal, BeholderErrorKind::Internal),
        ] {
            let mut status = Status::new(grpc, "wording can change");
            status.metadata_mut().insert(
                ERROR_CODE_METADATA_KEY,
                MetadataValue::from_static("beholder.workspace.index_failed"),
            );
            let error = operation_error(status);
            assert_eq!(error.kind(), expected);
            assert_eq!(error.code(), BeholderErrorCode::WorkspaceIndexFailed);
            assert_eq!(error.message(), "wording can change");
            assert!(std::error::Error::source(&error).is_some());
        }

        assert_eq!(
            operation_error(Status::permission_denied("denied")).code(),
            BeholderErrorCode::TransportGrpc
        );
    }

    #[test]
    fn query_metadata_preserves_incomplete_diagnostics() {
        let metadata = v1::QueryMetadata {
            revision: 3,
            view: "main".into(),
            freshness: Some(v1::Freshness::default()),
            completeness: v1::AnalysisCompleteness::Incomplete as i32,
            diagnostics: vec![v1::AnalysisDiagnostic {
                code: "typescript.syntax_recovered".into(),
                severity: v1::AnalysisDiagnosticSeverity::Warning as i32,
                repository: "repo".into(),
                path: "src/broken.ts".into(),
                line: Some(7),
                detail: None,
            }],
        };

        let metadata = QueryMetadata::try_from(metadata).unwrap();

        assert_eq!(
            metadata.analysis.completeness,
            AnalysisCompleteness::Incomplete
        );
        assert_eq!(
            metadata.analysis.diagnostics[0].severity,
            AnalysisDiagnosticSeverity::Warning
        );
    }
}
