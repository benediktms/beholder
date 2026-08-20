pub const FRONTEND_VERSION: &str = "7";
pub const RESOLVER_VERSION: &str = "6";

mod analysis;
mod analyzer;
mod model;
mod plugin;
mod resolution;
mod sources;
mod tonic;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations,
    observations_from_analysis, source_entity_id,
};
pub use analyzer::RustAnalyzer;
pub use model::{RustAnalysis, RustCall, RustFunction};
pub use resolution::resolve_repository_calls;
pub use sources::source_files;
pub use tonic::bindings as tonic_bindings;
