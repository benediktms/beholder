use beholder_domain::{BeholderError, BeholderErrorKind};
use beholder_protocol::v1::{
    GraphDirection as ProtocolDirection, SearchEntitiesRequest, TraverseGraphRequest,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchEntitiesInput {
    #[schemars(description = "Registered workspace name")]
    workspace: String,
    #[schemars(description = "Entity name or canonical entity ID to search for")]
    query: String,
    #[schemars(
        range(min = 1, max = 100),
        description = "Maximum matches. Defaults to 20; hard limit 100."
    )]
    limit: Option<u32>,
    #[schemars(description = "Include full diagnostic records. Defaults to false.")]
    include_diagnostics: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Direction {
    #[schemars(description = "Outgoing entities used by start")]
    Dependencies,
    #[schemars(description = "Incoming callers or users affected by start")]
    Dependents,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TraverseGraphInput {
    #[schemars(description = "Registered workspace name")]
    workspace: String,
    #[schemars(description = "Canonical entity ID, usually selected from search_entities")]
    start: String,
    #[schemars(
        description = "dependencies follows outgoing entities used by start; dependents follows incoming callers or users affected by start"
    )]
    direction: Direction,
    #[schemars(description = "Optional canonical entity ID at which matching paths terminate")]
    destination: Option<String>,
    #[serde(default)]
    #[schemars(description = "Unordered repository identities every returned path must visit")]
    target_repositories: Vec<String>,
    #[schemars(
        range(min = 0, max = 32),
        description = "Maximum edge depth. Defaults to 8; hard limit 32."
    )]
    max_hops: Option<u32>,
    #[schemars(
        range(min = 1, max = 200),
        description = "Maximum returned paths. Defaults to 50; hard limit 200."
    )]
    max_paths: Option<u32>,
    #[schemars(description = "Include full diagnostic records. Defaults to false.")]
    include_diagnostics: Option<bool>,
}

#[derive(Serialize)]
struct WorkspaceList {
    workspaces: Vec<WorkspaceSummary>,
}

#[derive(Serialize)]
struct WorkspaceSummary {
    name: String,
    repositories: Vec<WorkspaceRepositorySummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRepositorySummary {
    identity: String,
    display_name: String,
}

impl From<beholder_domain::Workspace> for WorkspaceSummary {
    fn from(workspace: beholder_domain::Workspace) -> Self {
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

#[derive(Clone, Debug)]
struct BeholderMcp {
    tool_router: ToolRouter<Self>,
}

impl BeholderMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl BeholderMcp {
    #[tool(description = "List registered Beholder workspaces and their repository summaries.")]
    async fn list_workspaces(&self) -> CallToolResult {
        match beholder_daemon_client::list_workspaces().await {
            Ok(workspaces) => structured(WorkspaceList {
                workspaces: workspaces.into_iter().map(WorkspaceSummary::from).collect(),
            }),
            Err(error) => structured_error(error.as_ref()),
        }
    }

    #[tool(
        description = "Search one immutable Beholder workspace revision for deterministic entity matches. Returns canonical entity IDs plus revision, freshness, completeness, and diagnostics metadata."
    )]
    async fn search_entities(
        &self,
        Parameters(input): Parameters<SearchEntitiesInput>,
    ) -> CallToolResult {
        match beholder_daemon_client::search_entities(SearchEntitiesRequest {
            workspace: input.workspace,
            query: input.query,
            limit: input.limit,
            include_diagnostics: Some(input.include_diagnostics.unwrap_or(false)),
        })
        .await
        {
            Ok(result) => structured(result),
            Err(error) => structured_error(error.as_ref()),
        }
    }

    #[tool(
        description = "Traverse bounded deterministic paths through one immutable Beholder workspace revision. dependencies follows outgoing entities used by start; dependents follows incoming callers or users affected by start. Results may be partial when a bound is reached; traversal.truncated and traversal.truncation_reasons explain why. Acquisition timeout before enumeration returns a structured error."
    )]
    async fn traverse_graph(
        &self,
        Parameters(input): Parameters<TraverseGraphInput>,
    ) -> CallToolResult {
        let direction = match input.direction {
            Direction::Dependencies => ProtocolDirection::Dependencies,
            Direction::Dependents => ProtocolDirection::Dependents,
        };
        match beholder_daemon_client::traverse_graph(TraverseGraphRequest {
            workspace: input.workspace,
            start: input.start,
            direction: direction as i32,
            destination: input.destination,
            max_hops: input.max_hops,
            max_paths: input.max_paths,
            target_repositories: input.target_repositories,
            include_diagnostics: Some(input.include_diagnostics.unwrap_or(false)),
        })
        .await
        {
            Ok(result) => structured(result),
            Err(error) => structured_error(error.as_ref()),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BeholderMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Use search_entities to obtain canonical IDs before traverse_graph.")
    }
}

fn structured(value: impl Serialize) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => CallToolResult::structured(value),
        Err(_) => CallToolResult::structured_error(json!({
            "error": {
                "kind": "internal",
                "code": "beholder.mcp.serialization_failed",
                "message": "Beholder MCP could not serialize the daemon response"
            }
        })),
    }
}

fn structured_error(error: &(dyn Error + 'static)) -> CallToolResult {
    let mut source = Some(error);
    while let Some(current) = source {
        if let Some(error) = current.downcast_ref::<BeholderError>() {
            return CallToolResult::structured_error(json!({
                "error": {
                    "kind": error_kind(error.kind()),
                    "code": error.code().as_str(),
                    "message": error.message()
                }
            }));
        }
        source = current.source();
    }
    CallToolResult::structured_error(json!({
        "error": {
            "kind": "internal",
            "code": "beholder.mcp.request_failed",
            "message": "Beholder MCP request failed"
        }
    }))
}

fn error_kind(kind: BeholderErrorKind) -> &'static str {
    match kind {
        BeholderErrorKind::DeadlineExceeded => "deadline_exceeded",
        BeholderErrorKind::InvalidInput => "invalid_input",
        BeholderErrorKind::NotFound => "not_found",
        BeholderErrorKind::FailedPrecondition => "failed_precondition",
        BeholderErrorKind::Unavailable => "unavailable",
        BeholderErrorKind::Internal => "internal",
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    BeholderMcp::new()
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_the_bounded_semantic_tools() {
        let router = BeholderMcp::tool_router();
        let mut names = router
            .map
            .keys()
            .map(|name| name.as_ref())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            ["list_workspaces", "search_entities", "traverse_graph"]
        );

        assert_eq!(
            router.map["list_workspaces"].attr.input_schema["type"],
            "object"
        );
        let search = &router.map["search_entities"].attr;
        assert_eq!(search.input_schema["type"], "object");
        assert_eq!(search.input_schema["properties"]["limit"]["maximum"], 100);
        assert!(
            search.input_schema["properties"]
                .get("include_diagnostics")
                .is_some()
        );
        assert!(
            search.input_schema["properties"]["query"]["description"]
                .as_str()
                .unwrap()
                .contains("canonical entity ID")
        );
        let traversal = &router.map["traverse_graph"].attr;
        assert_eq!(traversal.input_schema["type"], "object");
        assert_eq!(
            traversal.input_schema["properties"]["max_hops"]["maximum"],
            32
        );
        assert_eq!(
            traversal.input_schema["properties"]["max_paths"]["maximum"],
            200
        );
        assert!(
            traversal.input_schema["properties"]
                .get("target_repositories")
                .is_some()
        );
        assert!(
            traversal.input_schema["properties"]
                .get("include_diagnostics")
                .is_some()
        );
        assert!(
            traversal.input_schema["properties"]["direction"]["description"]
                .as_str()
                .unwrap()
                .contains("incoming callers")
        );
        assert!(
            traversal
                .description
                .as_deref()
                .unwrap()
                .contains("partial")
        );
    }

    #[test]
    fn preserves_structured_daemon_errors() {
        let error = BeholderError::new(
            BeholderErrorKind::NotFound,
            beholder_domain::BeholderErrorCode::WorkspaceNotRegistered,
            "workspace missing",
        );
        let result = structured_error(&error);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap()["error"],
            json!({
                "kind": "not_found",
                "code": "beholder.workspace.not_registered",
                "message": "workspace missing"
            })
        );
    }
}
