pub const FRONTEND_VERSION: &str = "28";
pub const RESOLVER_VERSION: &str = "20";

mod analysis;
mod graphql;
mod grats;
mod grpc;
mod model;
mod nestjs;
mod nestjs_di;
mod nestjs_graphql;
mod resolution;
mod ts_proto;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use graphql::{
    GraphqlFactInput, GraphqlResolverInput, GraphqlResolverSource, collect_graphql_facts,
    collect_graphql_resolvers,
};
pub use grpc::{GrpcBindingInput, bindings as grpc_bindings};
pub use model::{SourceLanguage, TypescriptAnalysis, TypescriptRepository};
pub use resolution::{
    resolve_repository_calls, resolve_workspace_calls, unresolved_call_diagnostics,
};
