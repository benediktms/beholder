mod benchmark;
mod database;
mod inspection;
mod query;
mod schema;
mod semantic;
mod storage;
mod store;

pub use inspection::{InspectionResult, InspectionValue};
pub use store::{EnrichmentOwner, EnrichmentPayload, EnrichmentPublishOutcome, SemanticStore};

#[cfg(feature = "devtools")]
use database::persistent_database;
#[cfg(feature = "devtools")]
use inspection::inspection_result;
#[cfg(feature = "devtools")]
use mnestic_engine::ScriptMutability;
#[cfg(feature = "devtools")]
use std::{collections::BTreeMap, error::Error, path::Path};

#[cfg(feature = "devtools")]
pub fn explain(path: &Path, query: &str) -> Result<InspectionResult, Box<dyn Error>> {
    let db = persistent_database(path, false)?;
    db.run_script(
        &format!("::explain {{\n{query}\n}}"),
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )
    .map(inspection_result)
    .map_err(Into::into)
}

#[cfg(feature = "devtools")]
pub fn execute(path: &Path, query: &str) -> Result<InspectionResult, Box<dyn Error>> {
    let db = persistent_database(path, false)?;
    db.run_script(query, BTreeMap::new(), ScriptMutability::Immutable)
        .map(inspection_result)
        .map_err(Into::into)
}
