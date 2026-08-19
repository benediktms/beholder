pub const FRONTEND_VERSION: &str = "18";
pub const RESOLVER_VERSION: &str = "9";

mod absinthe;
mod analysis;
mod grpc;
mod model;
mod resolution;

pub use absinthe::{GraphqlResolverBinding, bindings as graphql_resolver_bindings};
pub use analysis::analyze;
pub use grpc::bindings as grpc_bindings;
pub use model::ElixirAnalysis;
pub use resolution::{
    diagnostics_from_analysis, entities_from_analysis, generated_entities, generated_observations,
    observations, observations_from_analysis, resolve_repository_calls, resolve_workspace_modules,
};
