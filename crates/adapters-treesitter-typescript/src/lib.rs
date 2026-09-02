pub const FRONTEND_VERSION: &str = "46";
pub const RESOLVER_VERSION: &str = "29";

mod analysis;
mod analyzer;
mod graphql;
mod grats;
mod grpc;
mod manifest;
mod model;
mod nestjs;
mod nestjs_di;
mod nestjs_graphql;
mod plugin;
mod resolution;
mod ts_proto;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
    unresolved_endpoint_entities,
};
pub use analyzer::TypescriptAnalyzer;
pub use graphql::{
    GraphqlFactInput, GraphqlResolverInput, GraphqlResolverSource, collect_graphql_facts,
    collect_graphql_resolvers,
};
pub use grpc::{GrpcBindingInput, bindings as grpc_bindings};
pub use manifest::{typescript_analysis_input_kind, typescript_config_chains};
pub use model::{SourceLanguage, TypescriptAnalysis, TypescriptRepository};
pub use resolution::{
    resolve_repository_calls, resolve_workspace_calls, unresolved_call_diagnostics,
};
