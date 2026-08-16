pub const FRONTEND_VERSION: &str = "10";
pub const RESOLVER_VERSION: &str = "6";

mod analysis;
mod model;
mod resolution;

pub use analysis::analyze;
pub use model::ElixirAnalysis;
pub use resolution::{
    diagnostics_from_analysis, generated_observations, observations, observations_from_analysis,
    resolve_repository_calls, resolve_workspace_modules,
};
