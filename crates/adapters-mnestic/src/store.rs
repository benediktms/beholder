use super::semantic;
use super::{
    benchmark::{benchmark, benchmark_queries},
    database::{benchmark_database, memory_database, persistent_database},
    inspection::{InspectionResult, inspection_result},
    query::{
        analysis_metadata, analysis_revision, context, dependencies, entity_facts, impact,
        inspect_grpc_bindings, inspect_observations, inspect_relations, inspect_revisions, trace,
    },
    storage::{
        claim_garbage_collection, enrichment_matches, garbage_collection_pending,
        garbage_collection_queued, publish_enrichment, publish_observations,
        store_verification_fingerprint, sweep_garbage_collection, verification_matches,
        view_matches,
    },
};
use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, FactChanges, Observation, RepositoryFacts,
    WorkspaceView,
};
use beholder_dto::{
    ContextResult, DependenciesResult, GarbageCollection, GarbageCollectionProgress, ImpactResult,
    Revisioned, TraceResult,
};
use mnestic_engine::{DbInstance, MultiTransaction, NamedRows};
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
};

fn relevant_entities(
    result: &NamedRows,
    roots: &[&str],
    entity_columns: &[usize],
) -> BTreeSet<String> {
    roots
        .iter()
        .map(|entity| (*entity).to_owned())
        .chain(result.rows.iter().flat_map(|row| {
            entity_columns
                .iter()
                .filter_map(|&column| row.get(column)?.get_str().map(str::to_owned))
        }))
        .collect()
}

pub struct SemanticStore {
    pub(super) db: DbInstance,
    pub(super) read_db: DbInstance,
    pub(super) database_path: Option<PathBuf>,
}

#[derive(Default)]
pub struct EnrichmentPayload<'a> {
    pub entities: &'a [EntityFact],
    pub observations: &'a [Observation],
    pub overrides: &'a [DependencyOverride],
    pub diagnostics: &'a [(String, AnalysisDiagnostic)],
}

impl SemanticStore {
    pub fn memory() -> Result<Self, Box<dyn Error>> {
        let db = memory_database()?;
        Ok(Self {
            read_db: db.clone(),
            db,
            database_path: None,
        })
    }

    pub fn persistent(path: &Path, initialize: bool) -> Result<Self, Box<dyn Error>> {
        #[cfg(feature = "sqlite")]
        if initialize && !path.exists() {
            sqlite::open(path)?.execute("PRAGMA auto_vacuum = INCREMENTAL")?;
        }
        let db = persistent_database(path, initialize)?;
        #[cfg(feature = "sqlite")]
        sqlite::open(path)?.execute("PRAGMA journal_mode=WAL")?;
        let read_db = persistent_database(path, false)?;
        Ok(Self {
            db,
            read_db,
            database_path: Some(path.into()),
        })
    }

    pub fn benchmark_store(storage: &str, path: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let db = benchmark_database(storage, path)?;
        Ok(Self {
            read_db: db.clone(),
            db,
            database_path: (storage == "sqlite")
                .then_some(path)
                .flatten()
                .map(PathBuf::from),
        })
    }

    pub fn view_matches(&self, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
        view_matches(&self.read_db, view)
    }

    pub fn verification_matches(
        &self,
        view: &str,
        fingerprint: &str,
    ) -> Result<bool, Box<dyn Error>> {
        verification_matches(&self.read_db, view, fingerprint)
    }

    pub fn store_verification_fingerprint(
        &self,
        view: &str,
        fingerprint: &str,
    ) -> Result<(), Box<dyn Error>> {
        store_verification_fingerprint(&self.db, view, fingerprint)
    }

    pub fn publish(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
    ) -> Result<FactChanges, Box<dyn Error>> {
        publish_observations(&self.db, view, repositories, overrides, None)
    }

    pub fn publish_verified(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
        verification_fingerprint: &str,
    ) -> Result<FactChanges, Box<dyn Error>> {
        publish_observations(
            &self.db,
            view,
            repositories,
            overrides,
            Some(verification_fingerprint),
        )
    }

    pub fn enrichment_matches(
        &self,
        view: &str,
        analyzer: &str,
        version: &str,
    ) -> Result<bool, Box<dyn Error>> {
        enrichment_matches(&self.read_db, view, analyzer, version)
    }

    pub fn publish_enrichment(
        &self,
        view: &WorkspaceView,
        analyzer: &str,
        version: &str,
        payload: EnrichmentPayload<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        publish_enrichment(&self.db, view, analyzer, version, payload)
    }

    pub fn checkpoint(&self) -> Result<(), Box<dyn Error>> {
        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            let connection = sqlite::open(path)?;
            connection.execute("PRAGMA busy_timeout = 5000")?;
            let mut checkpoint = connection.prepare("PRAGMA wal_checkpoint(TRUNCATE)")?;
            if checkpoint.next()? != sqlite::State::Row || checkpoint.read::<i64, _>(0)? != 0 {
                return Err("SQLite WAL checkpoint remained busy".into());
            }
        }
        Ok(())
    }

    pub fn garbage_collect(&self) -> Result<GarbageCollection, Box<dyn Error>> {
        Ok(GarbageCollection {
            repository_states_queued: claim_garbage_collection(&self.db)?,
        })
    }

    pub fn sweep_garbage_collection(
        &self,
        mut progress: impl FnMut(GarbageCollectionProgress),
    ) -> Result<u64, Box<dyn Error>> {
        sweep_garbage_collection(&self.db, &mut progress)
    }

    pub fn garbage_collection_pending(&self) -> Result<bool, Box<dyn Error>> {
        garbage_collection_pending(&self.read_db)
    }

    pub fn garbage_collection_queued(&self) -> Result<u64, Box<dyn Error>> {
        garbage_collection_queued(&self.read_db)
    }

    pub fn inspect_relations(&self) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_relations(&self.read_db).map(inspection_result)
    }

    pub fn inspect_revisions(&self) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_revisions(&self.read_db).map(inspection_result)
    }

    pub fn inspect_observations(
        &self,
        relation: Option<&str>,
    ) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_observations(&self.read_db, relation).map(inspection_result)
    }

    pub fn inspect_grpc_bindings(&self) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_grpc_bindings(&self.read_db).map(inspection_result)
    }

    pub fn context(&self, view: &str, entity: &str) -> Result<ContextResult, Box<dyn Error>> {
        let result = context(&self.read_db, view, entity)?;
        let entities = relevant_entities(&result, &[entity], &[2]);
        semantic::context(
            view,
            entity,
            inspection_result(result),
            inspection_result(entity_facts(&self.read_db, view, &entities)?),
        )
    }

    pub fn context_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<ContextResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = context(transaction, view, entity)?;
            let entities = relevant_entities(&result, &[entity], &[2]);
            semantic::context(
                view,
                entity,
                inspection_result(result),
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    pub fn trace(
        &self,
        view: &str,
        from: &str,
        to: &str,
        max_hops: u32,
    ) -> Result<TraceResult, Box<dyn Error>> {
        let result = trace(&self.read_db, view, from, to)?;
        let entities = relevant_entities(&result, &[from, to], &[3, 4]);
        semantic::trace(
            view,
            from,
            to,
            max_hops,
            inspection_result(result),
            inspection_result(entity_facts(&self.read_db, view, &entities)?),
        )
    }

    pub fn trace_snapshot(
        &self,
        view: &str,
        from: &str,
        to: &str,
        max_hops: u32,
    ) -> Result<Revisioned<TraceResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = trace(transaction, view, from, to)?;
            let entities = relevant_entities(&result, &[from, to], &[3, 4]);
            semantic::trace(
                view,
                from,
                to,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    pub fn impact(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<ImpactResult, Box<dyn Error>> {
        let result = impact(&self.read_db, view, entity)?;
        let entities = relevant_entities(&result, &[entity], &[3, 4]);
        semantic::impact(
            view,
            entity,
            max_hops,
            inspection_result(result),
            inspection_result(entity_facts(&self.read_db, view, &entities)?),
        )
    }

    pub fn impact_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<ImpactResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = impact(transaction, view, entity)?;
            let entities = relevant_entities(&result, &[entity], &[3, 4]);
            semantic::impact(
                view,
                entity,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    pub fn dependencies(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<DependenciesResult, Box<dyn Error>> {
        let result = dependencies(&self.read_db, view, entity)?;
        let entities = relevant_entities(&result, &[entity], &[3, 4]);
        semantic::dependencies(
            view,
            entity,
            max_hops,
            inspection_result(result),
            inspection_result(entity_facts(&self.read_db, view, &entities)?),
        )
    }

    pub fn dependencies_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<DependenciesResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = dependencies(transaction, view, entity)?;
            let entities = relevant_entities(&result, &[entity], &[3, 4]);
            semantic::dependencies(
                view,
                entity,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    fn snapshot<T>(
        &self,
        view: &str,
        read: impl FnOnce(&MultiTransaction) -> Result<T, Box<dyn Error>>,
    ) -> Result<Revisioned<T>, Box<dyn Error>> {
        let transaction = self.read_db.multi_transaction(false);
        let result = read(&transaction)?;
        let analysis_revision = analysis_revision(&transaction, view)?;
        let analysis = analysis_metadata(&transaction, view, analysis_revision)?;
        transaction.abort()?;
        Ok(Revisioned {
            result,
            analysis_revision,
            analysis,
        })
    }

    pub fn benchmark(
        &self,
        topology: &str,
        entities: i64,
        fanout: i64,
        depth: i64,
    ) -> Result<String, Box<dyn Error>> {
        benchmark(&self.db, topology, entities, fanout, depth)
    }

    pub fn benchmark_queries(&self, topology: &str, entities: i64, depth: i64) -> String {
        benchmark_queries(&self.db, topology, entities, depth)
    }
}

#[cfg(test)]
mod tests {
    use crate::SemanticStore;
    use beholder_domain::{
        AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, LogicalRepository,
        Observation, RepositoryFacts, RepositoryState, WorkspaceView,
    };
    use beholder_dto::{AnalysisCompleteness, AnalysisDiagnosticSeverity as DtoSeverity};
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, SystemTime},
    };
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
    fn snapshot_preserves_incomplete_analysis_metadata() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "main",
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
        let mut repository = facts(&view, Vec::new());
        repository.incomplete = true;
        repository.diagnostics.push(AnalysisDiagnostic {
            code: "typescript.syntax_recovered".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: "src/broken.ts".into(),
            line: Some(7),
            detail: Some("unexpected token".into()),
        });
        store.publish(&view, &[repository], &[]).unwrap();

        let snapshot = store.context_snapshot("main", "missing").unwrap();

        assert_eq!(
            snapshot.analysis.completeness,
            AnalysisCompleteness::Incomplete
        );
        assert_eq!(snapshot.analysis.diagnostics.len(), 1);
        let diagnostic = &snapshot.analysis.diagnostics[0];
        assert_eq!(diagnostic.code, "typescript.syntax_recovered");
        assert_eq!(diagnostic.severity, DtoSeverity::Warning);
        assert_eq!(diagnostic.repository, "repo");
        assert_eq!(diagnostic.path, PathBuf::from("src/broken.ts"));
        assert_eq!(diagnostic.line, Some(7));
    }
    #[test]
    fn persistent_reads_serve_the_completed_revision_during_a_write() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-concurrent-read-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store =
            Arc::new(SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap());
        let view = WorkspaceView::new(
            "main",
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
                    vec![Observation::dependency(
                        "repo/caller",
                        DependencyRelation::Calls,
                        "repo/target",
                        "src/lib.rs:1",
                    )],
                )],
                &[],
            )
            .unwrap();

        let writer = store.db.multi_transaction(true);
        writer
            .run_script(
                "?[state, from, relation, to, evidence] := \
                         i in int_range(10000), state = 'pending', from = to_string(i), \
                         relation = 'calls', to = 'target', evidence = 'probe' \
                     :put state_observation {state, from, relation, to => evidence}",
                BTreeMap::new(),
            )
            .unwrap();
        let (sent, received) = mpsc::channel();
        let reader = store.clone();
        let reader_thread = thread::spawn(move || {
            let snapshot = reader.context_snapshot("main", "repo/caller").unwrap();
            sent.send((snapshot.analysis_revision, snapshot.result.edges.len()))
                .unwrap();
        });

        let (revision, edge_count) = received
            .recv_timeout(Duration::from_secs(1))
            .expect("read blocked behind the uncommitted writer");
        assert_eq!(revision, 1);
        assert_eq!(edge_count, 1);
        writer.abort().unwrap();
        reader_thread.join().unwrap();
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn garbage_collection_queues_stale_states_and_sweeps_them_in_restart_safe_batches() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-gc-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        let store = SemanticStore::persistent(&database, true).unwrap();
        let state = |fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some(fingerprint.into()),
            fingerprint: fingerprint.into(),
        };

        for fingerprint in ["old", "resurrected", "current"] {
            let view = WorkspaceView::new("main", "analysis", vec![state(fingerprint)]).unwrap();
            let observations = if fingerprint == "old" {
                (0..10_001)
                    .map(|index| {
                        Observation::dependency(
                            format!("repo/old/{index}"),
                            DependencyRelation::Calls,
                            "repo/target",
                            format!("src/lib.rs:{index}"),
                        )
                    })
                    .collect()
            } else if fingerprint == "current" {
                vec![Observation::dependency(
                    "repo/source",
                    DependencyRelation::Calls,
                    "repo/target",
                    "src/lib.rs:current",
                )]
            } else {
                vec![Observation::dependency(
                    "repo/resurrected",
                    DependencyRelation::Calls,
                    "repo/target",
                    "src/lib.rs:resurrected",
                )]
            };
            store
                .publish(&view, &[facts(&view, observations)], &[])
                .unwrap();
        }

        let collected = store.garbage_collect().unwrap();
        assert_eq!(collected.repository_states_queued, 2);
        assert!(store.garbage_collection_pending().unwrap());
        assert_eq!(store.garbage_collection_queued().unwrap(), 2);
        drop(store);
        let store = SemanticStore::persistent(&database, true).unwrap();
        assert!(store.garbage_collection_pending().unwrap());

        let resurrected =
            WorkspaceView::new("main", "analysis", vec![state("resurrected")]).unwrap();
        store
            .publish(
                &resurrected,
                &[facts(
                    &resurrected,
                    vec![Observation::dependency(
                        "repo/resurrected",
                        DependencyRelation::Calls,
                        "repo/target",
                        "src/lib.rs:resurrected",
                    )],
                )],
                &[],
            )
            .unwrap();

        let mut progress = Vec::new();
        assert_eq!(
            store
                .sweep_garbage_collection(|event| progress.push(event))
                .unwrap(),
            2
        );
        let observation_updates = progress
            .iter()
            .filter(|event| {
                event
                    .step
                    .as_deref()
                    .is_some_and(|step| step.starts_with("stale observations "))
            })
            .collect::<Vec<_>>();
        let observations = observation_updates
            .iter()
            .find(|event| event.completed_rows == Some(10_001))
            .unwrap();
        assert_eq!(observations.rows, None);
        assert_eq!(observations.stale_states, Some(2));
        assert_eq!(observations.repositories, Some(1));
        assert!(observations.completed_steps < 2);
        assert_eq!(
            observation_updates
                .iter()
                .filter_map(|event| event.completed_rows.filter(|rows| *rows > 0))
                .collect::<Vec<_>>(),
            [10_000, 10_001]
        );
        assert!(!store.garbage_collection_pending().unwrap());
        assert_eq!(store.garbage_collection_queued().unwrap(), 0);
        assert_eq!(
            store.context("main", "repo/resurrected").unwrap().root.id,
            "repo/resurrected"
        );
        let states = store
            .db
            .run_script(
                "?[state] := *repository_state{fingerprint: state}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(states.rows.len(), 2);
        let old_observations = store
            .db
            .run_script(
                "?[state] := *state_observation{state, from: 'repo/old/0'}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert!(old_observations.rows.is_empty());
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn query_snapshot_keeps_rows_and_revision_consistent() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-query-snapshot-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let view = WorkspaceView::new(
            "snapshot",
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
                    vec![Observation::dependency(
                        "repo/source",
                        DependencyRelation::Calls,
                        "repo/target",
                        "source.rs:1",
                    )],
                )],
                &[],
            )
            .unwrap();
        let snapshot = store.context_snapshot("snapshot", "repo/source").unwrap();

        assert_eq!(snapshot.analysis_revision, 1);
        assert!(
            snapshot
                .result
                .nodes
                .iter()
                .any(|value| value.id == "repo/target")
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
