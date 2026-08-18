pub const FRONTEND_VERSION: &str = "1";
pub const RESOLVER_VERSION: &str = "2";

mod analysis;
mod model;

pub use analysis::{
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
};
pub use model::CsharpAnalysis;
