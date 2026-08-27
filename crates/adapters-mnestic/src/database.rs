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
    for script in [
        CREATE_ENRICHMENT_OUTPUT_SCHEMA,
        CREATE_ENRICHMENT_ENTITY_CONTRIBUTION_SCHEMA,
        CREATE_ENRICHMENT_OBSERVATION_CONTRIBUTION_SCHEMA,
        CREATE_ENRICHMENT_OVERRIDE_CONTRIBUTION_SCHEMA,
        CREATE_ENRICHMENT_DIAGNOSTIC_CONTRIBUTION_SCHEMA,
        CREATE_BASELINE_ENTITY_SCHEMA,
        CREATE_BASELINE_OBSERVATION_SCHEMA,
        CREATE_BASELINE_OVERRIDE_SCHEMA,
        CREATE_BASELINE_DIAGNOSTIC_SCHEMA,
        CREATE_FACT_SHARD_SELECTION_SCHEMA,
        CREATE_FACT_SHARD_ENTITY_SCHEMA,
        CREATE_FACT_SHARD_OBSERVATION_SCHEMA,
        CREATE_FACT_SHARD_DEPENDENCY_SCHEMA,
        CREATE_BASELINE_FINGERPRINT_SCHEMA,
        CREATE_SCHEMA_MIGRATION_SCHEMA,
    ] {
        db.run_script(script, BTreeMap::new(), ScriptMutability::Mutable)?;
    }
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
        CREATE_REPOSITORY_REVISION_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REPOSITORY_REVISION_DIAGNOSTIC_SCHEMA,
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
    run_enrichment_migrations(&db)?;
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
        ("enrichment_output", CREATE_ENRICHMENT_OUTPUT_SCHEMA),
        (
            "enrichment_entity_contribution",
            CREATE_ENRICHMENT_ENTITY_CONTRIBUTION_SCHEMA,
        ),
        (
            "enrichment_observation_contribution",
            CREATE_ENRICHMENT_OBSERVATION_CONTRIBUTION_SCHEMA,
        ),
        (
            "enrichment_override_contribution",
            CREATE_ENRICHMENT_OVERRIDE_CONTRIBUTION_SCHEMA,
        ),
        (
            "enrichment_diagnostic_contribution",
            CREATE_ENRICHMENT_DIAGNOSTIC_CONTRIBUTION_SCHEMA,
        ),
        ("analysis_baseline_entity", CREATE_BASELINE_ENTITY_SCHEMA),
        (
            "analysis_baseline_observation",
            CREATE_BASELINE_OBSERVATION_SCHEMA,
        ),
        (
            "analysis_baseline_dependency_override",
            CREATE_BASELINE_OVERRIDE_SCHEMA,
        ),
        (
            "analysis_baseline_diagnostic",
            CREATE_BASELINE_DIAGNOSTIC_SCHEMA,
        ),
        (
            "analysis_fact_shard_selection",
            CREATE_FACT_SHARD_SELECTION_SCHEMA,
        ),
        (
            "analysis_fact_shard_entity",
            CREATE_FACT_SHARD_ENTITY_SCHEMA,
        ),
        (
            "analysis_fact_shard_observation",
            CREATE_FACT_SHARD_OBSERVATION_SCHEMA,
        ),
        (
            "analysis_fact_shard_dependency_observation",
            CREATE_FACT_SHARD_DEPENDENCY_SCHEMA,
        ),
        (
            "analysis_baseline_fingerprint",
            CREATE_BASELINE_FINGERPRINT_SCHEMA,
        ),
        ("schema_migration", CREATE_SCHEMA_MIGRATION_SCHEMA),
        ("repository_revision", CREATE_REPOSITORY_REVISION_SCHEMA),
        (
            "repository_revision_diagnostic",
            CREATE_REPOSITORY_REVISION_DIAGNOSTIC_SCHEMA,
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
    if initialize {
        run_enrichment_migrations(&db)?;
        if relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("enrichment_job"))
        {
            db.run_script(
                "::remove enrichment_job",
                BTreeMap::new(),
                ScriptMutability::Mutable,
            )?;
        }
    }
    Ok(db)
}

fn run_enrichment_migrations(db: &DbInstance) -> Result<(), Box<dyn Error>> {
    if !migration_applied(db, "enrichment-ownership", 1)? {
        migrate_enrichment_ownership_to_contributions(db)?;
    }
    if !migration_applied(db, "enrichment-baseline", 1)? {
        migrate_enrichment_baseline(db)?;
    }
    Ok(())
}

fn migration_applied(db: &DbInstance, name: &str, version: i64) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[applied] := *schema_migration{name: $name, version: $version}, applied = true",
        BTreeMap::from([
            ("name".into(), name.into()),
            ("version".into(), version.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(!rows.rows.is_empty())
}

fn migrate_enrichment_ownership_to_contributions(db: &DbInstance) -> Result<(), Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    for script in [
        "?[view, owner, repository, analyzer, version, input_fingerprint] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_repository_enrichment{\
                 view, revision, owner, repository, analyzer, version, input_fingerprint\
             }, not *enrichment_output{view, owner} \
         :put enrichment_output {\
             view, owner => repository, analyzer, version, input_fingerprint\
         }",
        "?[view, owner, id, kind, metadata] := *analysis_revision{view, revision}, \
             *analysis_revision_enrichment_entity_owner{\
                 view, revision, id, analyzer: owner\
             }, *analysis_revision_entity{view, revision, id, kind, metadata} \
         :put enrichment_entity_contribution {view, owner, id => kind, metadata}",
        "?[view, owner, from, relation, to, evidence, confidence, provenance] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_enrichment_observation_owner{\
                 view, revision, from, relation, to, evidence, analyzer: owner\
             }, *analysis_revision_observation{\
                 view, revision, from, relation, to, evidence, confidence, provenance\
             } \
         :put enrichment_observation_contribution {\
             view, owner, from, relation, to, evidence => confidence, provenance\
         }",
        "?[view, owner, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_enrichment_override_owner{\
                 view, revision, from, relation, unresolved_to, analyzer: owner\
             }, *analysis_revision_dependency_override{\
                 view, revision, from, relation, unresolved_to, resolved_to, evidence\
             }, *analysis_revision_dependency_override_metadata{\
                 view, revision, from, relation, unresolved_to, confidence, provenance\
             } \
         :put enrichment_override_contribution {\
             view, owner, from, relation, unresolved_to => resolved_to, evidence, confidence, \
             provenance\
         }",
        "?[view, owner, repository, code, severity, path, line, detail] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_enrichment_diagnostic_owner{\
                 view, revision, repository, code, severity, path, line, analyzer: owner\
             }, *analysis_revision_diagnostic{\
                 view, revision, repository, code, severity, path, line, detail\
             } \
         :put enrichment_diagnostic_contribution {\
             view, owner, repository, code, severity, path, line => detail\
         }",
    ] {
        transaction.run_script(script, BTreeMap::new())?;
    }
    transaction.run_script(
        "?[name, version] <- [['enrichment-ownership', 1]] \
         :put schema_migration {name => version}",
        BTreeMap::new(),
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_enrichment_baseline(db: &DbInstance) -> Result<(), Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    for script in [
        "?[view, id, kind, metadata, revision_owned] := *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_entity{state, id, kind, metadata}, revision_owned = false \
         :put analysis_baseline_entity {view, id => kind, metadata, revision_owned}",
        "?[view, from, relation, to, evidence, confidence, provenance, revision_owned] := \
             *analysis_revision{view, revision}, *analysis_revision_state{view, revision, state}, \
             *state_observation{state, from, relation, to, evidence}, \
             *state_observation_metadata{state, from, relation, to, confidence, provenance}, \
             revision_owned = false \
         :put analysis_baseline_observation {\
             view, from, relation, to, evidence => confidence, provenance, revision_owned\
         }",
        "?[view, id, kind, metadata, revision_owned] := *analysis_revision{view, revision}, \
             *analysis_revision_entity{view, revision, id, kind, metadata}, \
             not *enrichment_entity_contribution{view, id}, revision_owned = true \
         :put analysis_baseline_entity {view, id => kind, metadata, revision_owned}",
        "?[view, from, relation, to, evidence, confidence, provenance, revision_owned] := \
             *analysis_revision{view, revision}, *analysis_revision_observation{\
                 view, revision, from, relation, to, evidence, confidence, provenance\
             }, not *enrichment_observation_contribution{view, from, relation, to, evidence}, \
             revision_owned = true \
         :put analysis_baseline_observation {\
             view, from, relation, to, evidence => confidence, provenance, revision_owned\
         }",
        "?[view, id, kind, metadata, revision_owned] := *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_entity{state, id, kind, metadata}, revision_owned = false \
         :put analysis_baseline_entity {view, id => kind, metadata, revision_owned}",
        "?[view, from, relation, to, evidence, confidence, provenance, revision_owned] := \
             *analysis_revision{view, revision}, *analysis_revision_state{view, revision, state}, \
             *state_observation{state, from, relation, to, evidence}, \
             *state_observation_metadata{state, from, relation, to, confidence, provenance}, \
             revision_owned = false \
         :put analysis_baseline_observation {\
             view, from, relation, to, evidence => confidence, provenance, revision_owned\
         }",
        "?[view, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
             *analysis_revision{view, revision}, *analysis_revision_dependency_override{\
                 view, revision, from, relation, unresolved_to, resolved_to, evidence\
             }, *analysis_revision_dependency_override_metadata{\
                 view, revision, from, relation, unresolved_to, confidence, provenance\
             }, not *enrichment_override_contribution{view, from, relation, unresolved_to} \
         :put analysis_baseline_dependency_override {\
             view, from, relation, unresolved_to => resolved_to, evidence, confidence, provenance\
         }",
        "?[view, repository, code, severity, path, line, detail] := \
             *analysis_revision{view, revision}, *analysis_revision_diagnostic{\
                 view, revision, repository, code, severity, path, line, detail\
             }, not *enrichment_diagnostic_contribution{\
                 view, repository, code, severity, path, line\
             } \
         :put analysis_baseline_diagnostic {\
             view, repository, code, severity, path, line => detail\
         }",
    ] {
        transaction.run_script(script, BTreeMap::new())?;
    }
    transaction.run_script(
        "?[name, version] <- [['enrichment-baseline', 1]] \
         :put schema_migration {name => version}",
        BTreeMap::new(),
    )?;
    transaction.commit()?;
    Ok(())
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

    #[test]
    fn interrupted_enrichment_migration_resumes_until_the_marker_is_committed() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-migration-resume-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join("beholder.db");
        let store = SemanticStore::persistent(&path, true).unwrap();
        let view = WorkspaceView::new(
            "legacy-enrichment",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repo".into(),
                },
                head: None,
                fingerprint: "source".into(),
            }],
        )
        .unwrap();
        store
            .publish(&view, &[facts(&view, Vec::new())], &[])
            .unwrap();
        for script in [
            "?[view, revision, owner, repository, analyzer, version, input_fingerprint] <- \
                 [['legacy-enrichment', 1, 'owner', 'example/repo', 'rust', '1', 'input']] \
             :put analysis_revision_repository_enrichment {\
                 view, revision, owner => repository, analyzer, version, input_fingerprint\
             }",
            "?[view, revision, id, kind, metadata] <- \
                 [['legacy-enrichment', 1, 'generated', 'callable', '']] \
             :put analysis_revision_entity {view, revision, id => kind, metadata}",
            "?[view, revision, id, analyzer] <- \
                 [['legacy-enrichment', 1, 'generated', 'owner']] \
             :put analysis_revision_enrichment_entity_owner {view, revision, id => analyzer}",
            "?[view, owner, repository, analyzer, version, input_fingerprint] <- \
                 [['legacy-enrichment', 'owner', 'example/repo', 'rust', '1', 'input']] \
             :put enrichment_output {\
                 view, owner => repository, analyzer, version, input_fingerprint\
             }",
            "?[name] <- [['enrichment-ownership']] :rm schema_migration {name}",
        ] {
            store
                .db
                .run_script(script, BTreeMap::new(), ScriptMutability::Mutable)
                .unwrap();
        }
        drop(store);

        let store = SemanticStore::persistent(&path, true).unwrap();
        let migrated = store
            .db
            .run_script(
                "?[kind, version] := *enrichment_entity_contribution{\
                     view: 'legacy-enrichment', owner: 'owner', id: 'generated', kind\
                 }, *schema_migration{name: 'enrichment-ownership', version}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(migrated.rows.len(), 1);
        assert_eq!(migrated.rows[0][0].get_str(), Some("callable"));
        assert_eq!(migrated.rows[0][1].get_int(), Some(1));
        assert!(
            store
                .db
                .run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)
                .unwrap()
                .rows
                .iter()
                .all(|row| row[0].get_str() != Some("enrichment_job"))
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
