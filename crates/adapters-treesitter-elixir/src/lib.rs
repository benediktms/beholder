pub const FRONTEND_VERSION: &str = "24";
pub const RESOLVER_VERSION: &str = "13";

mod absinthe;
mod analysis;
mod analyzer;
mod grpc;
mod manifest;
mod model;
mod plugin;
mod resolution;

pub use absinthe::{GraphqlResolverBinding, bindings as graphql_resolver_bindings};
pub use analysis::analyze;
pub use analyzer::ElixirAnalyzer;
pub use grpc::bindings as grpc_bindings;
pub use manifest::elixir_analysis_input_kind;
pub use model::{ElixirAnalysis, ElixirRepository};
pub use resolution::{
    diagnostics_from_analysis, entities_from_analysis, generated_entities, generated_observations,
    observations, observations_from_analysis, resolve_repository_calls, resolve_workspace_modules,
    unresolved_endpoint_entities, workspace_dynamic_dispatch_observations,
};
