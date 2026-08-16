use super::semantic;
use super::{
    benchmark::{benchmark, benchmark_queries},
    database::{benchmark_database, memory_database, persistent_database},
    inspection::{InspectionResult, inspection_result},
    query::{
        analysis_revision, context, dependencies, impact, inspect_observations, inspect_relations,
        inspect_revisions, trace,
    },
    storage::{publish_observations, view_matches},
};
use beholder_domain::{DependencyOverride, FactChanges, RepositoryFacts, WorkspaceView};
use beholder_dto::{ContextResult, DependenciesResult, ImpactResult, Revisioned, TraceResult};
use mnestic_engine::{DbInstance, MultiTransaction};
use std::{
    error::Error,
    path::{Path, PathBuf},
};

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

    pub fn context(&self, view: &str, entity: &str) -> Result<ContextResult, Box<dyn Error>> {
        semantic::context(
            view,
            entity,
            inspection_result(context(&self.read_db, view, entity)?),
        )
    }

    pub fn context_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<ContextResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::context(
                view,
                entity,
                inspection_result(context(transaction, view, entity)?),
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
        semantic::trace(
            view,
            from,
            to,
            max_hops,
            inspection_result(trace(&self.read_db, view, from, to)?),
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
            semantic::trace(
                view,
                from,
                to,
                max_hops,
                inspection_result(trace(transaction, view, from, to)?),
            )
        })
    }

    pub fn impact(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<ImpactResult, Box<dyn Error>> {
        semantic::impact(
            view,
            entity,
            max_hops,
            inspection_result(impact(&self.read_db, view, entity)?),
        )
    }

    pub fn impact_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<ImpactResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::impact(
                view,
                entity,
                max_hops,
                inspection_result(impact(transaction, view, entity)?),
            )
        })
    }

    pub fn dependencies(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<DependenciesResult, Box<dyn Error>> {
        semantic::dependencies(
            view,
            entity,
            max_hops,
            inspection_result(dependencies(&self.read_db, view, entity)?),
        )
    }

    pub fn dependencies_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<DependenciesResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::dependencies(
                view,
                entity,
                max_hops,
                inspection_result(dependencies(transaction, view, entity)?),
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
        transaction.abort()?;
        Ok(Revisioned {
            result,
            analysis_revision,
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
        DependencyRelation, LogicalRepository, Observation, RepositoryFacts, RepositoryState,
        WorkspaceView,
    };
    use std::{
        collections::BTreeMap,
        fs,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, SystemTime},
    };
    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            observations,
        }
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
