pub const FRONTEND_VERSION: &str = "4";
pub const RESOLVER_VERSION: &str = "8";

mod analysis;
mod dotnet_di;
mod model;
mod project;
mod resolution;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use model::CsharpAnalysis;
pub use project::{CsharpProject, parse_project, source_assemblies};
pub use resolution::{CsharpSource, resolve_repository_calls};
