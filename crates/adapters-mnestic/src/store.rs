use super::semantic;
use super::{
    benchmark::{benchmark, benchmark_queries},
    database::{benchmark_database, memory_database, persistent_database},
    inspection::{InspectionResult, inspection_result},
    query::{
        analysis_metadata, analysis_revision, context, dependencies, entity_facts, impact,
        inspect_grpc_bindings, inspect_observations, inspect_relations, inspect_revisions,
        repository_revision, trace,
    },
    storage::{
        claim_garbage_collection, delete_repository_revision, enrichment_matches,
        enrichment_retry_failed, enrichment_retry_started, enrichments_current,
        ensure_revision_inputs, garbage_collection_candidates, garbage_collection_pending,
        garbage_collection_queued, prepare_enrichment, publish_enrichment, publish_observations,
        publish_repository, repository_contexts, revision_enrichment_input_fingerprint,
        store_verification_fingerprint, sweep_garbage_collection, verification_matches,
        view_matches,
    },
};
use beholder_domain::{
    AnalysisDiagnostic, BeholderError, BeholderErrorCode, BeholderErrorKind, DependencyOverride,
    EntityFact, FactChanges, Observation, RepositoryFacts, WorkspaceView,
};
use beholder_dto::{
    ContextResult, DependenciesResult, GarbageCollection, GarbageCollectionProgress, ImpactResult,
    RepositoryRevision, Revisioned, TraceResult,
};
use mnestic_engine::{DbInstance, MultiTransaction, NamedRows};
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
    time::Duration,
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

#[derive(Clone, Copy)]
pub struct EnrichmentOwner<'a> {
    pub analyzer: &'a str,
    pub version: &'a str,
    pub expected_version: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrichmentSchedule {
    Current,
    Queue,
    Running,
    RetryAfter(Duration),
    Exhausted,
    Superseded,
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
        {
            let fresh = initialize && !path.exists();
            let connection = sqlite::open(path)?;
            connection.execute("PRAGMA busy_timeout = 5000")?;
            if fresh {
                connection.execute("PRAGMA auto_vacuum = INCREMENTAL")?;
            }
            connection.execute("PRAGMA journal_mode = WAL")?;
        }
        let db = persistent_database(path, initialize)?;
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
        view_matches(&self.db, view)
    }

    pub fn verification_matches(
        &self,
        view: &str,
        fingerprint: &str,
    ) -> Result<bool, Box<dyn Error>> {
        verification_matches(&self.db, view, fingerprint)
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

    pub fn publish_repository(&self, facts: &RepositoryFacts) -> Result<bool, Box<dyn Error>> {
        publish_repository(&self.db, facts)
    }

    pub fn repository_revision(
        &self,
        repository: &str,
    ) -> Result<Option<RepositoryRevision>, Box<dyn Error>> {
        repository_revision(&self.read_db, repository)
    }

    pub fn delete_repository_revision(&self, repository: &str) -> Result<u64, Box<dyn Error>> {
        delete_repository_revision(&self.db, repository)
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
        repository: &str,
        analyzer: &str,
        version: &str,
    ) -> Result<bool, Box<dyn Error>> {
        enrichment_matches(&self.db, view, repository, analyzer, version)
    }

    pub fn enrichments_current(
        &self,
        view: &str,
        catalog: &[(String, String)],
    ) -> Result<bool, Box<dyn Error>> {
        enrichments_current(&self.db, view, catalog)
    }

    pub fn ensure_revision_inputs(&self, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
        ensure_revision_inputs(&self.db, view)
    }

    pub fn revision_enrichment_input_fingerprint(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
    ) -> Result<Option<String>, Box<dyn Error>> {
        revision_enrichment_input_fingerprint(&self.db, view, repository, analyzer)
    }

    pub fn repository_contexts(
        &self,
        view: &str,
        target: &str,
        analyzer: &str,
    ) -> Result<Vec<String>, Box<dyn Error>> {
        repository_contexts(&self.db, view, target, analyzer)
    }

    pub fn publish_enrichment(
        &self,
        view: &WorkspaceView,
        repository: &str,
        input_fingerprint: &str,
        owner: EnrichmentOwner<'_>,
        payload: EnrichmentPayload<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        publish_enrichment(
            &self.db,
            view,
            repository,
            input_fingerprint,
            owner,
            payload,
        )
    }

    pub fn prepare_enrichment(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
        version: &str,
        input_fingerprint: &str,
    ) -> Result<EnrichmentSchedule, Box<dyn Error>> {
        prepare_enrichment(
            &self.db,
            view,
            repository,
            analyzer,
            version,
            input_fingerprint,
        )
    }

    pub fn enrichment_retry_started(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
        version: &str,
        input_fingerprint: &str,
    ) -> Result<bool, Box<dyn Error>> {
        enrichment_retry_started(
            &self.db,
            view,
            repository,
            analyzer,
            version,
            input_fingerprint,
        )
    }

    pub fn enrichment_retry_failed(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
        version: &str,
        input_fingerprint: &str,
        error: &str,
    ) -> Result<Option<Duration>, Box<dyn Error>> {
        enrichment_retry_failed(
            &self.db,
            view,
            repository,
            analyzer,
            version,
            input_fingerprint,
            error,
        )
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
        mut progress: impl FnMut(GarbageCollectionProgress) -> bool,
    ) -> Result<u64, Box<dyn Error>> {
        sweep_garbage_collection(&self.db, &mut progress)
    }

    pub fn garbage_collection_pending(&self) -> Result<bool, Box<dyn Error>> {
        garbage_collection_pending(&self.read_db)
    }

    pub fn garbage_collection_candidates(&self) -> Result<u64, Box<dyn Error>> {
        garbage_collection_candidates(&self.read_db)
    }

    pub fn garbage_collection_queued(&self) -> Result<u64, Box<dyn Error>> {
        garbage_collection_queued(&self.read_db)
    }

    pub fn reclaimable_database_pages(&self) -> Result<u64, Box<dyn Error>> {
        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            return sqlite_pragma(path, "PRAGMA freelist_count");
        }
        Ok(0)
    }

    pub fn reclaim_database_pages(&self, pages: u32) -> Result<u64, Box<dyn Error>> {
        if pages == 0 {
            return Ok(0);
        }
        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            let before = sqlite_pragma(path, "PRAGMA freelist_count")?;
            if before == 0 {
                return Ok(0);
            }
            let connection = sqlite::open(path)?;
            connection.execute("PRAGMA busy_timeout = 5000")?;
            connection.execute(format!("PRAGMA incremental_vacuum({pages})"))?;
            return Ok(before.saturating_sub(sqlite_pragma(path, "PRAGMA freelist_count")?));
        }
        Ok(0)
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
        let analysis_revision = analysis_revision(&transaction, view)?;
        if analysis_revision == 0 {
            transaction.abort()?;
            return Err(Box::new(BeholderError::new(
                BeholderErrorKind::Unavailable,
                BeholderErrorCode::WorkspaceRevisionUnavailable,
                format!("workspace has no completed analysis revision: {view}"),
            )));
        }
        let result = read(&transaction)?;
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

#[cfg(feature = "sqlite")]
fn sqlite_pragma(path: &Path, pragma: &str) -> Result<u64, Box<dyn Error>> {
    let connection = sqlite::open(path)?;
    connection.execute("PRAGMA busy_timeout = 5000")?;
    let mut statement = connection.prepare(pragma)?;
    if statement.next()? != sqlite::State::Row {
        return Err(format!("SQLite pragma returned no row: {pragma}").into());
    }
    statement.read::<i64, _>(0)?.try_into().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use crate::SemanticStore;
    use beholder_domain::{
        AnalysisDiagnostic, AnalysisDiagnosticSeverity, BeholderError, BeholderErrorCode,
        DependencyRelation, LogicalRepository, Observation, RepositoryFacts, RepositoryState,
        WorkspaceView,
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
    fn standalone_repository_facts_are_reused_by_a_workspace() {
        let store = SemanticStore::memory().unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "example/repository".into(),
            },
            head: Some("head".into()),
            fingerprint: "source-state".into(),
        };
        let repository = RepositoryFacts {
            state: state.clone(),
            analysis_identity: "analysis-v1".into(),
            incomplete: false,
            diagnostics: vec![AnalysisDiagnostic {
                code: "typescript.syntax_recovered".into(),
                severity: AnalysisDiagnosticSeverity::Warning,
                path: "src/lib.ts".into(),
                line: Some(3),
                detail: Some("recovered".into()),
            }],
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations: vec![Observation::dependency(
                "repo/source",
                DependencyRelation::Calls,
                "repo/target",
                "src/lib.rs:1",
            )],
        };

        assert!(store.publish_repository(&repository).unwrap());
        assert!(!store.publish_repository(&repository).unwrap());
        let standalone = store
            .db
            .run_script(
                "?[source_state, analyzed_state] := \
                     *repository_revision{repository: 'example/repository', source_state, analyzed_state}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(standalone.rows[0][0].get_str(), Some("source-state"));
        let analyzed_state = standalone.rows[0][1].get_str().unwrap().to_owned();
        let revision = store
            .repository_revision("example/repository")
            .unwrap()
            .unwrap();
        assert_eq!(revision.source_state, "source-state");
        assert_eq!(
            revision.analysis.diagnostics[0].detail.as_deref(),
            Some("recovered")
        );
        assert_eq!(store.garbage_collect().unwrap().repository_states_queued, 0);

        let view = WorkspaceView::new("standalone-reuse", "rules", vec![state]).unwrap();
        store.publish(&view, &[repository], &[]).unwrap();
        let selected = store
            .db
            .run_script(
                "?[state] := *analysis_revision{view: 'standalone-reuse', revision}, \
                     *analysis_revision_state{view: 'standalone-reuse', revision, repository: 'example/repository', state}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(selected.rows[0][0].get_str(), Some(analyzed_state.as_str()));
        let observations = store
            .db
            .run_script(
                "?[state] := *state_observation{state, from: 'repo/source', to: 'repo/target'}",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(observations.rows.len(), 1);
        assert_eq!(
            store
                .delete_repository_revision("example/repository")
                .unwrap(),
            0
        );
        assert_eq!(store.garbage_collection_queued().unwrap(), 0);
        assert_eq!(
            store
                .context("standalone-reuse", "repo/source")
                .unwrap()
                .edges
                .len(),
            1
        );
    }

    #[test]
    fn deleting_a_standalone_revision_queues_its_unreferenced_state() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "standalone",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repository".into(),
                },
                head: Some("head".into()),
                fingerprint: "source-state".into(),
            }],
        )
        .unwrap();
        let repository = facts(
            &view,
            vec![Observation::dependency(
                "repo/source",
                DependencyRelation::Calls,
                "repo/target",
                "src/lib.rs:1",
            )],
        );

        store.publish_repository(&repository).unwrap();
        assert_eq!(
            store
                .delete_repository_revision("example/repository")
                .unwrap(),
            1
        );
        assert!(
            store
                .repository_revision("example/repository")
                .unwrap()
                .is_none()
        );
        assert_eq!(store.garbage_collection_queued().unwrap(), 1);
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
    fn snapshot_requires_a_completed_revision() {
        let store = SemanticStore::memory().unwrap();
        let error = store
            .context_snapshot("pending", "repo/source")
            .unwrap_err();
        let error = error.downcast_ref::<BeholderError>().unwrap();

        assert_eq!(
            error.code(),
            BeholderErrorCode::WorkspaceRevisionUnavailable
        );
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
        for fingerprint in ["standalone-old", "standalone-current"] {
            let standalone_state = RepositoryState {
                repository: LogicalRepository {
                    identity: "standalone".into(),
                },
                head: Some(fingerprint.into()),
                fingerprint: fingerprint.into(),
            };
            assert!(
                store
                    .publish_repository(&RepositoryFacts {
                        state: standalone_state,
                        analysis_identity: "analysis".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: vec![Observation::dependency(
                            format!("standalone/{fingerprint}"),
                            DependencyRelation::Calls,
                            "standalone/target",
                            "src/lib.rs:1",
                        )],
                    })
                    .unwrap()
            );
        }

        assert_eq!(store.garbage_collection_candidates().unwrap(), 3);
        let collected = store.garbage_collect().unwrap();
        assert_eq!(collected.repository_states_queued, 3);
        assert_eq!(store.garbage_collection_candidates().unwrap(), 0);
        assert!(store.garbage_collection_pending().unwrap());
        assert_eq!(store.garbage_collection_queued().unwrap(), 3);
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
                .sweep_garbage_collection(|event| {
                    progress.push(event);
                    true
                })
                .unwrap(),
            3
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
        assert_eq!(observations.stale_states, Some(3));
        assert_eq!(observations.repositories, Some(2));
        assert!(observations.completed_steps < 3);
        assert_eq!(
            observation_updates
                .iter()
                .filter(|event| event.step == observations.step)
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
        assert_eq!(store.garbage_collection_candidates().unwrap(), 1);
        assert_eq!(store.garbage_collect().unwrap().repository_states_queued, 1);
        assert_eq!(store.sweep_garbage_collection(|_| true).unwrap(), 1);
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
        store.checkpoint().unwrap();
        let size_before_reclaim = fs::metadata(&database).unwrap().len();
        let pages_before_reclaim = store.reclaimable_database_pages().unwrap();
        assert!(pages_before_reclaim > 0);
        while store.reclaim_database_pages(1_024).unwrap() > 0 {}
        assert_eq!(store.reclaimable_database_pages().unwrap(), 0);
        assert!(fs::metadata(&database).unwrap().len() < size_before_reclaim);
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
