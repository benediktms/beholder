use beholder_protocol::v1::{
    GetStatusRequest, GetStatusResponse, StopRequest, daemon_client::DaemonClient,
};
use std::path::PathBuf;

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

pub async fn stop() -> Result<bool, Box<dyn std::error::Error>> {
    let Ok(mut client) = DaemonClient::connect(ENDPOINT).await else {
        return Ok(false);
    };
    Ok(client.stop(StopRequest {}).await?.into_inner().accepted)
}
