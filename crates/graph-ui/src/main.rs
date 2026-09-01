#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fixture;

use beholder_dto::{EntityRef, QueryMetadata, SemanticEdge, TraversalMetadata};
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

#[derive(Debug, Deserialize)]
struct GraphRequest {
    workspace: String,
}

#[derive(Debug, Serialize)]
struct GraphSnapshot {
    schema: &'static str,
    workspace: WorkspaceSummary,
    metadata: QueryMetadata,
    traversal: TraversalMetadata,
    nodes: Vec<EntityRef>,
    edges: Vec<SemanticEdge>,
}

#[tauri::command]
fn list_workspaces() -> Vec<WorkspaceSummary> {
    vec![fixture::workspace()]
}

#[tauri::command]
fn load_graph(request: GraphRequest) -> Result<GraphSnapshot, String> {
    if request.workspace != fixture::WORKSPACE_NAME {
        return Err(format!("unknown fixture workspace: {}", request.workspace));
    }
    Ok(fixture::graph())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![list_workspaces, load_graph])
        .run(tauri::generate_context!())
        .expect("failed to run Beholder graph UI");
}
