pub const FRONTEND_VERSION: &str = "8";
pub const RESOLVER_VERSION: &str = "15";

mod analysis;
mod analyzer;
mod dotnet_di;
mod manifest;
mod model;
mod plugin;
mod project;
mod resolution;
mod unity;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use analyzer::CsharpAnalyzer;
pub use manifest::csharp_analysis_input_kind;
pub use model::CsharpAnalysis;
pub use project::{CsharpProject, parse_project, source_assemblies};
pub use resolution::{CsharpSource, resolve_repository_calls};
pub use unity::{
    UnityPrefab, parse_unity_assemblies, parse_unity_meta, parse_unity_prefab, unity_lifecycle,
    unity_prefab_dependencies,
};
