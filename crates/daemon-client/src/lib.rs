use beholder_dto::QueryResult;
use beholder_protocol::v1::{
    EntityRequest, GetStatusRequest, GetStatusResponse, IndexRustWorkspaceRequest, PathRequest,
    StopRequest, daemon_client::DaemonClient,
};
use std::path::{Path, PathBuf};

pub const ADDRESS: &str = "127.0.0.1:50051";
pub const ENDPOINT: &str = "http://127.0.0.1:50051";

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
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .get_status(GetStatusRequest {})
        .await?
        .into_inner())
}

pub async fn context(entity: String) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .context(EntityRequest { entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn dependencies(entity: String) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .dependencies(EntityRequest { entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn impact(entity: String) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .impact(EntityRequest { entity })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn trace(from: String, to: String) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .trace(PathRequest { from, to })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn why(from: String, to: String) -> Result<QueryResult, Box<dyn std::error::Error>> {
    Ok(DaemonClient::connect(ENDPOINT)
        .await?
        .why(PathRequest { from, to })
        .await?
        .into_inner()
        .try_into()?)
}

pub async fn index_rust_workspace(
    repositories: &[PathBuf],
) -> Result<(usize, bool), Box<dyn std::error::Error>> {
    let repositories = repositories
        .iter()
        .map(|path| path_string(path))
        .collect::<Result<_, _>>()?;
    let response = DaemonClient::connect(ENDPOINT)
        .await?
        .index_rust_workspace(IndexRustWorkspaceRequest { repositories })
        .await?
        .into_inner();
    Ok((response.observation_count.try_into()?, response.published))
}

fn path_string(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("repository path is not UTF-8: {}", path.display()).into())
}

pub async fn stop() -> Result<bool, Box<dyn std::error::Error>> {
    let Ok(mut client) = DaemonClient::connect(ENDPOINT).await else {
        return Ok(false);
    };
    Ok(client.stop(StopRequest {}).await?.into_inner().accepted)
}
