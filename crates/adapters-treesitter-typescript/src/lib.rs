pub const FRONTEND_VERSION: &str = "5";
pub const RESOLVER_VERSION: &str = "4";

mod analysis;
mod model;
mod resolution;

pub use analysis::{analyze, entities_from_analysis, observations_from_analysis};
pub use model::{SourceLanguage, TypescriptAnalysis};
pub use resolution::resolve_repository_calls;
