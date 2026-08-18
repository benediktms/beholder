pub const FRONTEND_VERSION: &str = "26";
pub const RESOLVER_VERSION: &str = "15";

mod analysis;
mod grpc;
mod model;
mod nestjs;
mod nestjs_di;
mod resolution;
mod ts_proto;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use grpc::{GrpcBindingInput, bindings as grpc_bindings};
pub use model::{SourceLanguage, TypescriptAnalysis, TypescriptRepository};
pub use resolution::{
    resolve_repository_calls, resolve_workspace_calls, unresolved_call_diagnostics,
};
