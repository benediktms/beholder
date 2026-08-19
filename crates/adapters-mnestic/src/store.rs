use super::semantic;
use super::{
    benchmark::{benchmark, benchmark_queries},
    database::{benchmark_database, memory_database, persistent_database},
    inspection::{InspectionResult, inspection_result},
    query::{
        analysis_metadata, analysis_revision, context, dependencies, entity_facts, impact,
        inspect_grpc_bindings, inspect_observations, inspect_relations, inspect_revisions, trace,
    },
    storage::{garbage_collect, publish_observations, view_matches},
};
use beholder_domain::{DependencyOverride, FactChanges, RepositoryFacts, WorkspaceView};
use beholder_dto::{
    ContextResult, DependenciesResult, GarbageCollection, ImpactResult, Revisioned, TraceResult,
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

    pub fn publish(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
    ) -> Result<FactChanges, Box<dyn Error>> {
        publish_observations(&self.db, view, repositories, overrides)
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
        let bytes_before = self
            .database_path
            .as_deref()
            .map(std::fs::metadata)
            .transpose()?
            .map_or(0, |metadata| metadata.len());
        let repository_states_removed = garbage_collect(&self.db)?;
        self.checkpoint()?;

        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            let connection = sqlite::open(path)?;
            connection.execute("PRAGMA busy_timeout = 5000")?;
            let mut mode = connection.prepare("PRAGMA auto_vacuum")?;
            if mode.next()? != sqlite::State::Row {
                return Err("SQLite did not report its auto-vacuum mode".into());
            }
            if mode.read::<i64, _>(0)? == 0 {
                drop(mode);
                connection.execute("PRAGMA auto_vacuum = INCREMENTAL")?;
                connection.execute("VACUUM")?;
            } else {
                drop(mode);
                connection.execute("PRAGMA incremental_vacuum")?;
            }
        }
        self.checkpoint()?;

        let bytes_after = self
            .database_path
            .as_deref()
            .map(std::fs::metadata)
            .transpose()?
            .map_or(0, |metadata| metadata.len());
        Ok(GarbageCollection {
            repository_states_removed,
            bytes_before,
            bytes_after,
        })
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
    fn garbage_collection_keeps_only_current_states_and_enables_incremental_vacuum() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-gc-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        sqlite::open(&database).unwrap();
        let store = SemanticStore::persistent(&database, true).unwrap();
        let state = |fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some(fingerprint.into()),
            fingerprint: fingerprint.into(),
        };

        for fingerprint in ["old", "current"] {
            let view = WorkspaceView::new("main", "analysis", vec![state(fingerprint)]).unwrap();
            store
                .publish(
                    &view,
                    &[facts(
                        &view,
                        vec![Observation::dependency(
                            "repo/source",
                            DependencyRelation::Calls,
                            "repo/target",
                            format!("src/lib.rs:{fingerprint}"),
                        )],
                    )],
                    &[],
                )
                .unwrap();
        }

        let before = store.context("main", "repo/source").unwrap();
        let collected = store.garbage_collect().unwrap();
        assert_eq!(collected.repository_states_removed, 1);
        assert_eq!(store.context("main", "repo/source").unwrap(), before);
        let states = store
            .db
            .run_script(
                "?[state] := *repository_state{fingerprint: state}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(states.rows.len(), 1);

        let connection = sqlite::open(&database).unwrap();
        let mut mode = connection.prepare("PRAGMA auto_vacuum").unwrap();
        assert_eq!(mode.next().unwrap(), sqlite::State::Row);
        assert_eq!(mode.read::<i64, _>(0).unwrap(), 2);
        drop(store);

        let new_database = state_dir.join("new.db");
        let new_store = SemanticStore::persistent(&new_database, true).unwrap();
        let connection = sqlite::open(new_database).unwrap();
        let mut mode = connection.prepare("PRAGMA auto_vacuum").unwrap();
        assert_eq!(mode.next().unwrap(), sqlite::State::Row);
        assert_eq!(mode.read::<i64, _>(0).unwrap(), 2);
        drop(new_store);
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
