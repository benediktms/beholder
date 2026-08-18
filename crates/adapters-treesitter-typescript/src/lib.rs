pub const FRONTEND_VERSION: &str = "9";
pub const RESOLVER_VERSION: &str = "7";

mod analysis;
mod model;
mod resolution;

pub use analysis::{analyze, entities_from_analysis, observations_from_analysis};
pub use model::{SourceLanguage, TypescriptAnalysis, TypescriptRepository};
pub use resolution::{resolve_repository_calls, resolve_workspace_calls};
