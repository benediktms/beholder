use beholder_domain::Workspace;
use beholder_dto::QueryResult;
use beholder_protocol::v1::{
    ClearCacheRequest, EntityRequest, GetStatusRequest, GetStatusResponse,
    IndexRustWorkspaceRequest, ListWorkspacesRequest, PathRequest, RegisterWorkspaceRequest,
    StopRequest, daemon_client::DaemonClient,
};
use std::path::{Path, PathBuf};

const DEFAULT_ADDRESS: &str = "127.0.0.1:50051";

pub fn address() -> String {
    std::env::var("BEHOLDER_ADDRESS").unwrap_or_else(|_| DEFAULT_ADDRESS.into())
}

fn endpoint() -> String {
    format!("http://{}", address())
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
    Ok(DaemonClient::connect(endpoint())
        .await?
        .get_status(GetStatusRequest {})
        .await?
        .into_inner())
}

pub async fn clear_cache() -> Result<(), Box<dyn std::error::Error>> {
    DaemonClient::connect(endpoint())
        .await?
        .clear_cache(ClearCacheRequest {})
        .await?;
    Ok(())
}

pub async fn context(
    workspace: String,
    entity: String,
) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint())
        .await?
        .context(EntityRequest { workspace, entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn dependencies(
    workspace: String,
    entity: String,
) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint())
        .await?
        .dependencies(EntityRequest { workspace, entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn impact(
    workspace: String,
    entity: String,
) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint())
        .await?
        .impact(EntityRequest { workspace, entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn trace(
    workspace: String,
    from: String,
    to: String,
) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint())
        .await?
        .trace(PathRequest {
            workspace,
            from,
            to,
        })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn why(
    workspace: String,
    from: String,
    to: String,
) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(endpoint())
        .await?
        .why(PathRequest {
            workspace,
            from,
            to,
        })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn index_rust_workspace(
    workspace: String,
) -> Result<(usize, bool), Box<dyn std::error::Error>> {
    let response = DaemonClient::connect(endpoint())
        .await?
        .index_rust_workspace(IndexRustWorkspaceRequest { workspace })
        .await?
        .into_inner();
    Ok((response.observation_count.try_into()?, response.published))
}

pub async fn register_workspace(
    name: String,
    repositories: &[PathBuf],
) -> Result<Workspace, Box<dyn std::error::Error>> {
    let repositories = repositories
        .iter()
        .map(|path| path_string(path))
        .collect::<Result<_, _>>()?;
    let workspace = DaemonClient::connect(endpoint())
        .await?
        .register_workspace(RegisterWorkspaceRequest { name, repositories })
        .await?
        .into_inner()
        .workspace
        .ok_or("daemon returned no workspace")?;
    Ok(workspace.try_into()?)
}

pub async fn list_workspaces() -> Result<Vec<Workspace>, Box<dyn std::error::Error>> {
    DaemonClient::connect(endpoint())
        .await?
        .list_workspaces(ListWorkspacesRequest {})
        .await?
        .into_inner()
        .workspaces
        .into_iter()
        .map(|workspace| workspace.try_into().map_err(Into::into))
        .collect()
}

fn path_string(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("repository path is not UTF-8: {}", path.display()).into())
}

pub async fn stop() -> Result<bool, Box<dyn std::error::Error>> {
    let Ok(mut client) = DaemonClient::connect(endpoint()).await else {
        return Ok(false);
    };
    Ok(client.stop(StopRequest {}).await?.into_inner().accepted)
}
