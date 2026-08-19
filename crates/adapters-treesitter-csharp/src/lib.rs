pub const FRONTEND_VERSION: &str = "8";
pub const RESOLVER_VERSION: &str = "13";

mod analysis;
mod dotnet_di;
mod model;
mod project;
mod resolution;
mod unity;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use model::CsharpAnalysis;
pub use project::{CsharpProject, parse_project, source_assemblies};
pub use resolution::{CsharpSource, resolve_repository_calls};
pub use unity::{
    UnityPrefab, parse_unity_assemblies, parse_unity_meta, parse_unity_prefab, unity_lifecycle,
    unity_prefab_dependencies,
};
