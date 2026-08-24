use super::schema::*;
use mnestic_engine::{DbInstance, ScriptMutability};
use std::{collections::BTreeMap, error::Error, path::Path};

pub(super) fn memory_database() -> Result<DbInstance, Box<dyn Error>> {
    let db = DbInstance::new("mem", "", Default::default())?;
    db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(
        CREATE_DEPENDENCY_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_METADATA_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENTITY_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_GRPC_BINDING_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_OBSERVATION_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_ENTITY_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_GRPC_DIAGNOSTIC_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ANALYSIS_METADATA_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ANALYSIS_DIAGNOSTIC_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_INPUT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_CONTEXT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_INPUT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REPOSITORY_ENRICHMENT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_OVERRIDE_OWNER_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_DIAGNOSTIC_OWNER_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_ENTITY_OWNER_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_ENRICHMENT_OBSERVATION_OWNER_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OBSERVATION_TO_INDEX,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_METADATA_TO_INDEX,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_OBSERVATION_TO_INDEX,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OVERRIDE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OVERRIDE_METADATA_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_STATE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_FINGERPRINT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_VERIFICATION_FINGERPRINT_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REPOSITORY_STATE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_GARBAGE_COLLECTION_STATE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(SEED, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(
        SEED_DEPENDENCIES,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(SEED_METADATA, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(SEED_REVISIONS, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(SEED_STATES, BTreeMap::new(), ScriptMutability::Mutable)?;
    Ok(db)
}

pub(super) fn benchmark_database(
    storage: &str,
    path: Option<&str>,
) -> Result<DbInstance, Box<dyn Error>> {
    #[cfg(not(feature = "sqlite"))]
    let _ = path;
    match storage {
        "mem" => Ok(DbInstance::new("mem", "", Default::default())?),
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(DbInstance::new(
            "sqlite",
            path.ok_or("sqlite benchmark requires a database path")?,
            Default::default(),
        )?),
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => Err("build with --features sqlite to benchmark SQLite".into()),
        _ => Err("storage must be mem or sqlite".into()),
    }
}

pub(super) fn persistent_database(
    path: &Path,
    initialize: bool,
) -> Result<DbInstance, Box<dyn Error>> {
    if path.as_os_str().is_empty() {
        return Err("database path must not be empty".into());
    }
    let is_new = !path.exists();
    if is_new && !initialize {
        return Err(format!("database does not exist: {}", path.display()).into());
    }
    let db = benchmark_database("sqlite", path.to_str())?;
    if is_new {
        db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    }
    let relations = db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?;
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_observation"))
    {
        db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_observation_metadata"))
    {
        db.run_script(
            CREATE_METADATA_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("garbage_collection_state"))
    {
        db.run_script(
            CREATE_GARBAGE_COLLECTION_STATE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_verification_fingerprint"))
    {
        db.run_script(
            CREATE_VERIFICATION_FINGERPRINT_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_entity"))
    {
        db.run_script(
            CREATE_ENTITY_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    for (name, script) in [
        ("state_grpc_binding_candidate", CREATE_GRPC_BINDING_SCHEMA),
        (
            "analysis_revision_observation",
            CREATE_REVISION_OBSERVATION_SCHEMA,
        ),
        ("analysis_revision_entity", CREATE_REVISION_ENTITY_SCHEMA),
        (
            "analysis_revision_grpc_diagnostic",
            CREATE_GRPC_DIAGNOSTIC_SCHEMA,
        ),
        (
            "analysis_revision_metadata",
            CREATE_ANALYSIS_METADATA_SCHEMA,
        ),
        (
            "analysis_revision_diagnostic",
            CREATE_ANALYSIS_DIAGNOSTIC_SCHEMA,
        ),
        ("analysis_revision_enrichment", CREATE_ENRICHMENT_SCHEMA),
        ("analysis_revision_input", CREATE_REVISION_INPUT_SCHEMA),
        ("analysis_revision_context", CREATE_REVISION_CONTEXT_SCHEMA),
        (
            "analysis_revision_enrichment_input",
            CREATE_ENRICHMENT_INPUT_SCHEMA,
        ),
        (
            "analysis_revision_repository_enrichment",
            CREATE_REPOSITORY_ENRICHMENT_SCHEMA,
        ),
        (
            "analysis_revision_enrichment_override_owner",
            CREATE_ENRICHMENT_OVERRIDE_OWNER_SCHEMA,
        ),
        (
            "analysis_revision_enrichment_diagnostic_owner",
            CREATE_ENRICHMENT_DIAGNOSTIC_OWNER_SCHEMA,
        ),
        (
            "analysis_revision_enrichment_entity_owner",
            CREATE_ENRICHMENT_ENTITY_OWNER_SCHEMA,
        ),
        (
            "analysis_revision_enrichment_observation_owner",
            CREATE_ENRICHMENT_OBSERVATION_OWNER_SCHEMA,
        ),
    ] {
        if initialize
            && !relations
                .rows
                .iter()
                .any(|row| row[0].get_str() == Some(name))
        {
            db.run_script(script, BTreeMap::new(), ScriptMutability::Mutable)?;
        }
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_dependency_observation"))
    {
        db.run_script(
            CREATE_DEPENDENCY_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_dependency_override_metadata"))
    {
        db.run_script(
            CREATE_OVERRIDE_METADATA_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_dependency_override"))
    {
        db.run_script(
            CREATE_OVERRIDE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision"))
    {
        db.run_script(
            CREATE_REVISION_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
        db.run_script(
            "?[view, revision] <- [['main', 0]] \
             :put analysis_revision {view => revision}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_fingerprint"))
    {
        db.run_script(
            CREATE_FINGERPRINT_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
        db.run_script(
            "?[view, fingerprint] <- [['main', '']] \
             :put analysis_fingerprint {view => fingerprint}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("repository_state"))
    {
        db.run_script(
            CREATE_REPOSITORY_STATE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_state"))
    {
        db.run_script(
            CREATE_REVISION_STATE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize {
        let relations =
            db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?;
        for (name, script) in [
            ("state_observation:by_to", CREATE_OBSERVATION_TO_INDEX),
            ("state_observation_metadata:by_to", CREATE_METADATA_TO_INDEX),
            (
                "analysis_revision_observation:by_to",
                CREATE_REVISION_OBSERVATION_TO_INDEX,
            ),
        ] {
            if !relations
                .rows
                .iter()
                .any(|row| row[0].get_str() == Some(name))
            {
                db.run_script(script, BTreeMap::new(), ScriptMutability::Mutable)?;
            }
        }
    }
    Ok(db)
}

#[cfg(test)]
mod tests {
    use crate::{SemanticStore, database::benchmark_database};
    use beholder_domain::{
        DependencyRelation, LogicalRepository, Observation, RepositoryFacts, RepositoryState,
        StructuralRelation, WorkspaceView,
    };
    use mnestic_engine::ScriptMutability;
    use std::{collections::BTreeMap, fs, time::SystemTime};
    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations,
        }
    }
    #[test]
    fn existing_database_accepts_repository_state_facts() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-backfill-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join("beholder.db");
        let db = benchmark_database("sqlite", path.to_str()).unwrap();
        db.run_script(
            ":create observation {\
                     view: String, from: String, relation: String, to: String => evidence: String\
                 }",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        drop(db);

        let store = SemanticStore::persistent(&path, true).unwrap();
        assert!(!store.garbage_collection_pending().unwrap());
        let view = WorkspaceView::new(
            "legacy",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "repo".into(),
                },
                head: Some("head".into()),
                fingerprint: "state".into(),
            }],
        )
        .unwrap();
        store
            .publish(
                &view,
                &[facts(
                    &view,
                    vec![
                        Observation::structural(
                            "repo/file",
                            StructuralRelation::Defines,
                            "repo/caller",
                            "src/lib.rs:1",
                        ),
                        Observation::dependency(
                            "repo/caller",
                            DependencyRelation::Calls,
                            "repo/target",
                            "src/lib.rs:2",
                        ),
                    ],
                )],
                &[],
            )
            .unwrap();
        assert_eq!(
            store
                .trace(
                    "legacy",
                    "repo/caller",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .len(),
            1
        );
        assert!(
            store
                .trace(
                    "legacy",
                    "repo/file",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .is_empty()
        );
        assert_eq!(
            store.context("legacy", "repo/target").unwrap().edges.len(),
            1
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
