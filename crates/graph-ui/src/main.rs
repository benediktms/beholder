#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use beholder_domain::Workspace;
use beholder_dto::{QueryMetadata, WorkspaceTopology};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRepositorySummary {
    identity: String,
    display_name: String,
}

#[derive(Clone, Debug, Serialize)]
struct WorkspaceSummary {
    name: String,
    repositories: Vec<WorkspaceRepositorySummary>,
}

impl From<Workspace> for WorkspaceSummary {
    fn from(workspace: Workspace) -> Self {
        Self {
            name: workspace.name,
            repositories: workspace
                .repositories
                .into_iter()
                .map(|repository| WorkspaceRepositorySummary {
                    identity: repository.repository.identity,
                    display_name: repository.display_name,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GraphRequest {
    workspace: String,
}

#[derive(Debug, Serialize)]
struct GraphSnapshot {
    workspace: WorkspaceSummary,
    #[serde(flatten)]
    topology: WorkspaceTopology,
}

#[tauri::command]
async fn list_workspaces() -> Result<Vec<WorkspaceSummary>, String> {
    beholder_daemon_client::list_workspaces()
        .await
        .map(|workspaces| workspaces.into_iter().map(Into::into).collect())
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn load_graph(request: GraphRequest) -> Result<GraphSnapshot, String> {
    let workspace = beholder_daemon_client::list_workspaces()
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|workspace| workspace.name == request.workspace)
        .ok_or_else(|| format!("unknown workspace: {}", request.workspace))?;
    let topology = beholder_daemon_client::workspace_topology(request.workspace)
        .await
        .map_err(|error| error.to_string())?;
    Ok(GraphSnapshot {
        workspace: workspace.into(),
        topology,
    })
}

#[tauri::command]
async fn topology_status(request: GraphRequest) -> Result<QueryMetadata, String> {
    beholder_daemon_client::workspace_topology_status(request.workspace)
        .await
        .map_err(|error| error.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            list_workspaces,
            load_graph,
            topology_status
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Beholder graph UI");
}
