pub const FRONTEND_VERSION: &str = "3";
pub const RESOLVER_VERSION: &str = "5";

mod analysis;
mod model;
mod resolution;
mod sources;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations, observations_from_analysis,
};
pub use model::RustAnalysis;
pub use resolution::resolve_repository_calls;
pub use sources::source_files;
