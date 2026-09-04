use super::semantic;
use super::{
    benchmark::{benchmark, benchmark_queries},
    database::{benchmark_database, memory_database, persistent_database},
    inspection::{InspectionResult, inspection_result},
    query::{
        SnapshotQueryRunner, all_entity_facts, analysis_metadata, analysis_revision, context,
        dependencies, entity_facts, impact, inspect_grpc_bindings, inspect_observations,
        inspect_relations, inspect_revisions, published_repository_head, repository_revision,
        trace, warn_on_slow_semantic_query, workspace_graph_neighborhood,
        workspace_graph_overview, workspace_topology,
    },
    storage::{
        SelectedBaselineSemantics, claim_garbage_collection, delete_repository_revision,
        enrichment_matches, enrichments_current, ensure_revision_inputs,
        garbage_collection_candidates, garbage_collection_pending, garbage_collection_queued,
        publish_enrichment, publish_observations, publish_repository, repository_contexts,
        revision_enrichment_input_fingerprint, revision_input_fingerprints,
        selected_baseline_semantics, store_verification_fingerprint, sweep_garbage_collection,
        verification_matches, view_matches,
    },
};
use beholder_domain::{
    AnalysisDiagnostic, BeholderError, BeholderErrorCode, BeholderErrorKind, DependencyOverride,
    EntityFact, EntityKind, FactChanges, FactShard, Observation, RepositoryFacts,
    SemanticCandidate, SemanticRelation, WorkspaceView,
};
use beholder_dto::{
    ContextResult, DependenciesResult, GarbageCollection, GarbageCollectionProgress,
    GraphNeighborhoodFocus, ImpactResult, RepositoryRevision, Revisioned, TraceResult,
    WorkspaceGraphNeighborhood, WorkspaceGraphOverview, WorkspaceTopology,
};
use mnestic_engine::{DataValue, DbInstance, NamedRows};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::{Path, PathBuf},
    sync::Mutex,
};

fn relevant_traversal_entities(
    result: &NamedRows,
    roots: &[&str],
    max_hops: u32,
) -> BTreeSet<String> {
    roots
        .iter()
        .map(|entity| (*entity).to_owned())
        .chain(
            result
                .rows
                .iter()
                .filter(|row| {
                    row.get(2)
                        .and_then(DataValue::get_int)
                        .is_some_and(|hops| hops < i64::from(max_hops))
                })
                .flat_map(|row| {
                    [3, 4]
                        .into_iter()
                        .filter_map(|column| row[column].get_str().map(str::to_owned))
                }),
        )
        .collect()
}

pub struct SemanticStore {
    pub(super) db: DbInstance,
    pub(super) read_db: DbInstance,
    gc_db: DbInstance,
    pub(super) database_path: Option<PathBuf>,
    engine: Mutex<()>,
}

#[derive(Clone, Copy, Default)]
pub struct EnrichmentPayload<'a> {
    pub entities: &'a [EntityFact],
    pub observations: &'a [Observation],
    pub overrides: &'a [DependencyOverride],
    pub diagnostics: &'a [(String, AnalysisDiagnostic)],
    pub diagnostic_replacements: &'a [(String, String)],
    pub fact_shards: &'a [FactShard],
}

#[derive(Clone, Copy)]
pub struct EnrichmentOwner<'a> {
    pub analyzer: &'a str,
    pub version: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrichmentPublishOutcome {
    Published,
    Unchanged,
    Superseded,
}

impl SemanticStore {
    pub fn memory() -> Result<Self, Box<dyn Error>> {
        let db = memory_database()?;
        Ok(Self {
            read_db: db.clone(),
            gc_db: db.clone(),
            db,
            database_path: None,
            engine: Mutex::new(()),
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
        let gc_db = persistent_database(path, false)?;
        Ok(Self {
            db,
            read_db,
            gc_db,
            database_path: Some(path.into()),
            engine: Mutex::new(()),
        })
    }

    pub fn benchmark_store(storage: &str, path: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let db = benchmark_database(storage, path)?;
        Ok(Self {
            read_db: db.clone(),
            gc_db: db.clone(),
            db,
            database_path: (storage == "sqlite")
                .then_some(path)
                .flatten()
                .map(PathBuf::from),
            engine: Mutex::new(()),
        })
    }

    fn access<T>(
        &self,
        operation: impl FnOnce() -> Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        let _engine = self
            .engine
            .lock()
            .map_err(|_| "semantic store engine lock poisoned")?;
        operation()
    }

    pub fn view_matches(&self, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
        view_matches(&self.read_db, view)
    }

    pub fn verification_matches(
        &self,
        view: &str,
        fingerprint: &str,
    ) -> Result<bool, Box<dyn Error>> {
        self.access(|| verification_matches(&self.db, view, fingerprint))
    }

    pub fn store_verification_fingerprint(
        &self,
        view: &str,
        fingerprint: &str,
    ) -> Result<(), Box<dyn Error>> {
        self.access(|| store_verification_fingerprint(&self.db, view, fingerprint))
    }

    pub fn publish(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
    ) -> Result<FactChanges, Box<dyn Error>> {
        self.access(|| {
            publish_observations(
                &self.db,
                view,
                repositories,
                overrides,
                &[],
                &[],
                None,
                false,
            )
        })
    }

    pub fn publish_repository(&self, facts: &RepositoryFacts) -> Result<bool, Box<dyn Error>> {
        self.access(|| publish_repository(&self.db, facts))
    }

    pub fn repository_revision(
        &self,
        repository: &str,
    ) -> Result<Option<RepositoryRevision>, Box<dyn Error>> {
        repository_revision(&self.read_db, repository)
    }

    pub fn published_repository_head(
        &self,
        view: &str,
        repository: &str,
    ) -> Result<Option<String>, Box<dyn Error>> {
        published_repository_head(&self.read_db, view, repository)
    }

    pub fn delete_repository_revision(&self, repository: &str) -> Result<u64, Box<dyn Error>> {
        self.access(|| delete_repository_revision(&self.db, repository, None))
    }

    pub fn delete_standalone_repository_revision(
        &self,
        repository: &str,
        view: &str,
    ) -> Result<u64, Box<dyn Error>> {
        self.access(|| delete_repository_revision(&self.db, repository, Some(view)))
    }

    pub fn publish_verified(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
        verification_fingerprint: &str,
    ) -> Result<FactChanges, Box<dyn Error>> {
        self.access(|| {
            publish_observations(
                &self.db,
                view,
                repositories,
                overrides,
                &[],
                &[],
                Some(verification_fingerprint),
                false,
            )
        })
    }

    pub fn publish_verified_sharded(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
        fact_shards: &[FactShard],
        semantic_candidates: &[SemanticCandidate],
        verification_fingerprint: &str,
    ) -> Result<FactChanges, Box<dyn Error>> {
        self.access(|| {
            publish_observations(
                &self.db,
                view,
                repositories,
                overrides,
                fact_shards,
                semantic_candidates,
                Some(verification_fingerprint),
                true,
            )
        })
    }

    pub fn enrichment_matches(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
        version: &str,
    ) -> Result<bool, Box<dyn Error>> {
        self.access(|| enrichment_matches(&self.db, view, repository, analyzer, version))
    }

    pub fn enrichments_current(
        &self,
        view: &str,
        catalog: &[(String, String)],
    ) -> Result<bool, Box<dyn Error>> {
        self.access(|| enrichments_current(&self.db, view, catalog))
    }

    pub fn ensure_revision_inputs(&self, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
        self.access(|| ensure_revision_inputs(&self.db, view))
    }

    pub fn revision_enrichment_input_fingerprint(
        &self,
        view: &str,
        repository: &str,
        analyzer: &str,
    ) -> Result<Option<String>, Box<dyn Error>> {
        self.access(|| revision_enrichment_input_fingerprint(&self.db, view, repository, analyzer))
    }

    pub fn revision_input_fingerprints(
        &self,
        view: &str,
    ) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
        self.access(|| revision_input_fingerprints(&self.db, view))
    }

    pub fn repository_contexts(
        &self,
        view: &str,
        target: &str,
        analyzer: &str,
    ) -> Result<Vec<String>, Box<dyn Error>> {
        self.access(|| repository_contexts(&self.db, view, target, analyzer))
    }

    pub fn selected_baseline_semantics(
        &self,
        view: &str,
        repository: &str,
        entity_kinds: &BTreeSet<EntityKind>,
        relations: &BTreeSet<SemanticRelation>,
    ) -> Result<SelectedBaselineSemantics, Box<dyn Error>> {
        self.access(|| {
            selected_baseline_semantics(&self.db, view, repository, entity_kinds, relations)
        })
    }

    pub fn publish_enrichment(
        &self,
        view: &str,
        repository: &str,
        input_fingerprint: &str,
        owner: EnrichmentOwner<'_>,
        payload: EnrichmentPayload<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(
            self.publish_enrichment_outcome(view, repository, input_fingerprint, owner, payload)?
                != EnrichmentPublishOutcome::Superseded,
        )
    }

    pub fn publish_enrichment_outcome(
        &self,
        view: &str,
        repository: &str,
        input_fingerprint: &str,
        owner: EnrichmentOwner<'_>,
        payload: EnrichmentPayload<'_>,
    ) -> Result<EnrichmentPublishOutcome, Box<dyn Error>> {
        self.access(|| {
            publish_enrichment(
                &self.db,
                view,
                repository,
                input_fingerprint,
                owner,
                payload,
            )
        })
    }

    pub fn checkpoint(&self) -> Result<(), Box<dyn Error>> {
        let _engine = self
            .engine
            .lock()
            .map_err(|_| "semantic store engine lock poisoned")?;
        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            let connection = sqlite::open(path)?;
            connection.execute("PRAGMA busy_timeout = 5000")?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let mut checkpoint = connection.prepare("PRAGMA wal_checkpoint(TRUNCATE)")?;
                if checkpoint.next()? == sqlite::State::Row && checkpoint.read::<i64, _>(0)? == 0 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err("SQLite WAL checkpoint remained busy".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Ok(())
    }

    pub fn checkpoint_passive(&self) -> Result<(), Box<dyn Error>> {
        let _engine = self
            .engine
            .lock()
            .map_err(|_| "semantic store engine lock poisoned")?;
        #[cfg(feature = "sqlite")]
        if let Some(path) = &self.database_path {
            sqlite::open(path)?.execute("PRAGMA wal_checkpoint(PASSIVE)")?;
        }
        Ok(())
    }

    pub fn garbage_collect(&self) -> Result<GarbageCollection, Box<dyn Error>> {
        self.access(|| {
            Ok(GarbageCollection {
                repository_states_queued: claim_garbage_collection(&self.db)?,
            })
        })
    }

    pub fn sweep_garbage_collection(
        &self,
        mut progress: impl FnMut(GarbageCollectionProgress) -> bool,
    ) -> Result<u64, Box<dyn Error>> {
        sweep_garbage_collection(&self.gc_db, &self.engine, &mut progress)
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
        let _engine = self
            .engine
            .lock()
            .map_err(|_| "semantic store engine lock poisoned")?;
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
        warn_on_slow_semantic_query(|| {
            let result = context(&self.read_db, view, entity)?;
            let entities = std::iter::once(entity.to_owned())
                .chain(result.iter().map(|row| row.related.clone()))
                .collect();
            semantic::context(
                view,
                entity,
                result,
                inspection_result(entity_facts(&self.read_db, view, &entities)?),
            )
        })
    }

    pub fn context_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<ContextResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = context(transaction, view, entity)?;
            let entities = std::iter::once(entity.to_owned())
                .chain(result.iter().map(|row| row.related.clone()))
                .collect();
            semantic::context(
                view,
                entity,
                result,
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    pub fn workspace_topology_snapshot(
        &self,
        view: &str,
    ) -> Result<Revisioned<WorkspaceTopology>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::workspace_topology(
                view,
                inspection_result(workspace_topology(transaction, view)?),
                inspection_result(all_entity_facts(transaction, view)?),
            )
        })
    }

    pub fn workspace_graph_overview_snapshot(
        &self,
        view: &str,
    ) -> Result<Revisioned<WorkspaceGraphOverview>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let (communities, edges) = workspace_graph_overview(transaction, view)?;
            semantic::workspace_graph_overview(
                view,
                inspection_result(communities),
                inspection_result(edges),
            )
        })
    }

    pub fn workspace_graph_neighborhood_snapshot(
        &self,
        view: &str,
        focus: GraphNeighborhoodFocus,
        max_edges: u32,
    ) -> Result<Revisioned<WorkspaceGraphNeighborhood>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let (focus_kind, focus_id) = match &focus {
                GraphNeighborhoodFocus::Repository(repository) => ("repository", repository.as_str()),
                GraphNeighborhoodFocus::Entity(entity) => ("entity", entity.as_str()),
                GraphNeighborhoodFocus::External => ("external", ""),
            };
            let result = workspace_graph_neighborhood(transaction, view, focus_kind, focus_id)?;
            let entities = result
                .rows
                .iter()
                .flat_map(|row| [0, 1].into_iter().filter_map(|column| row[column].get_str()))
                .map(str::to_owned)
                .collect();
            semantic::workspace_graph_neighborhood(
                view,
                focus.clone(),
                max_edges,
                inspection_result(result),
                inspection_result(entity_facts(transaction, view, &entities)?),
            )
        })
    }

    pub fn workspace_topology_status(&self, view: &str) -> Result<Revisioned<()>, Box<dyn Error>> {
        self.snapshot(view, |_| Ok(()))
    }

    pub fn trace(
        &self,
        view: &str,
        from: &str,
        to: &str,
        max_hops: u32,
    ) -> Result<TraceResult, Box<dyn Error>> {
        warn_on_slow_semantic_query(|| {
            let result = trace(&self.read_db, view, from, to, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[from, to], max_hops);
            semantic::trace(
                view,
                from,
                to,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(&self.read_db, view, &entities)?),
            )
        })
    }

    pub fn trace_snapshot(
        &self,
        view: &str,
        from: &str,
        to: &str,
        max_hops: u32,
    ) -> Result<Revisioned<TraceResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = trace(transaction, view, from, to, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[from, to], max_hops);
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
        warn_on_slow_semantic_query(|| {
            let result = impact(&self.read_db, view, entity, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[entity], max_hops);
            semantic::impact(
                view,
                entity,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(&self.read_db, view, &entities)?),
            )
        })
    }

    pub fn impact_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<ImpactResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = impact(transaction, view, entity, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[entity], max_hops);
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
        warn_on_slow_semantic_query(|| {
            let result = dependencies(&self.read_db, view, entity, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[entity], max_hops);
            semantic::dependencies(
                view,
                entity,
                max_hops,
                inspection_result(result),
                inspection_result(entity_facts(&self.read_db, view, &entities)?),
            )
        })
    }

    pub fn dependencies_snapshot(
        &self,
        view: &str,
        entity: &str,
        max_hops: u32,
    ) -> Result<Revisioned<DependenciesResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            let result = dependencies(transaction, view, entity, max_hops)?;
            let entities = relevant_traversal_entities(&result, &[entity], max_hops);
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
        read: impl FnOnce(&SnapshotQueryRunner<'_>) -> Result<T, Box<dyn Error>>,
    ) -> Result<Revisioned<T>, Box<dyn Error>> {
        warn_on_slow_semantic_query(|| {
            let transaction = self.read_db.multi_transaction(false);
            let query_runner = SnapshotQueryRunner::new(&transaction, &self.read_db);
            let analysis_revision = analysis_revision(&query_runner, view)?;
            if analysis_revision == 0 {
                transaction.abort()?;
                return Err(Box::new(BeholderError::new(
                    BeholderErrorKind::Unavailable,
                    BeholderErrorCode::WorkspaceRevisionUnavailable,
                    format!("workspace has no completed analysis revision: {view}"),
                )) as Box<dyn Error>);
            }
            let result = read(&query_runner)?;
            let analysis = analysis_metadata(&query_runner, view, analysis_revision)?;
            transaction.abort()?;
            Ok(Revisioned {
                result,
                analysis_revision,
                analysis,
            })
        })
    }

    pub fn benchmark(
        &self,
        topology: &str,
        entities: i64,
        fanout: i64,
        depth: i64,
    ) -> Result<String, Box<dyn Error>> {
        self.access(|| benchmark(&self.db, topology, entities, fanout, depth))
    }

    pub fn benchmark_queries(&self, topology: &str, entities: i64, depth: i64) -> String {
        let _engine = self
            .engine
            .lock()
            .expect("semantic store engine lock poisoned");
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
    use super::{EnrichmentOwner, EnrichmentPayload, relevant_traversal_entities};
    use crate::SemanticStore;
    use beholder_domain::{
        AnalysisDiagnostic, AnalysisDiagnosticSeverity, BeholderError, BeholderErrorCode,
        Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind, FactShard,
        LogicalRepository, Observation, Provenance, RepositoryFacts, RepositoryState,
        StructuralRelation, WorkspaceView,
    };
    use beholder_dto::{AnalysisCompleteness, AnalysisDiagnosticSeverity as DtoSeverity};
    use mnestic_engine::NamedRows;
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::PathBuf,
        sync::{Arc, mpsc},
        thread,
        time::SystemTime,
    };
    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        let entities = observations
            .iter()
            .flat_map(|observation| [&observation.from, &observation.to])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| EntityFact::new(id.clone(), EntityKind::Callable, None).unwrap())
            .collect();
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities,
            grpc_bindings: Vec::new(),
            observations,
        }
    }

    #[test]
    fn boundary_probe_entities_are_not_hydrated() {
        let rows = NamedRows::new(
            Vec::new(),
            vec![
                vec![
                    "edge".into(),
                    "".into(),
                    0_i64.into(),
                    "root".into(),
                    "near".into(),
                ],
                vec![
                    "edge".into(),
                    "".into(),
                    1_i64.into(),
                    "near".into(),
                    "far".into(),
                ],
            ],
        );

        assert_eq!(
            relevant_traversal_entities(&rows, &["root"], 1),
            BTreeSet::from(["near".to_owned(), "root".to_owned()])
        );
    }

    #[test]
    fn fact_shard_selection_reuses_unchanged_versions_and_replaces_one_owner() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "incremental",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repository".into(),
                },
                head: None,
                fingerprint: "state".into(),
            }],
        )
        .unwrap();
        let repository = facts(&view, Vec::new());
        let owner = "repo://example/repository/rust/lib/run";
        let shard = |version: &str, target: &str| FactShard {
            repository: "example/repository".into(),
            producer: "rust".into(),
            owner: owner.into(),
            version: version.into(),
            entities: vec![
                EntityFact::new(owner, EntityKind::Callable, None).unwrap(),
                EntityFact::new(target, EntityKind::Callable, None).unwrap(),
            ],
            observations: vec![Observation::dependency(
                owner,
                DependencyRelation::Calls,
                target,
                "src/lib.rs:1",
            )],
        };

        let first = shard("body-1", "rust-call://first");
        assert_eq!(
            store
                .publish_verified_sharded(
                    &view,
                    std::slice::from_ref(&repository),
                    &[],
                    std::slice::from_ref(&first),
                    &[],
                    "verified-1",
                )
                .unwrap()
                .inserted,
            1
        );
        assert_eq!(
            store.context("incremental", owner).unwrap().edges[0].to,
            "rust-call://first"
        );
        assert!(
            store
                .impact("incremental", "rust-call://first", 1)
                .unwrap()
                .affected
                .iter()
                .any(|affected| affected.entity == owner)
        );

        assert_eq!(
            store
                .publish_verified_sharded(
                    &view,
                    std::slice::from_ref(&repository),
                    &[],
                    std::slice::from_ref(&first),
                    &[],
                    "verified-1",
                )
                .unwrap()
                .unchanged,
            1
        );

        let second = shard("body-2", "rust-call://second");
        let changes = store
            .publish_verified_sharded(
                &view,
                std::slice::from_ref(&repository),
                &[],
                std::slice::from_ref(&second),
                &[],
                "verified-2",
            )
            .unwrap();
        assert_eq!(changes.updated, 1);
        assert_eq!(changes.inserted, 0);
        assert_eq!(changes.removed, 0);
        let context = store.context("incremental", owner).unwrap();
        assert_eq!(context.edges.len(), 1);
        assert_eq!(context.edges[0].to, "rust-call://second");
        let dependencies = store.dependencies("incremental", owner, 1).unwrap();
        assert!(
            dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == "rust-call://second")
        );
        assert!(
            !dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == "rust-call://first")
        );

        assert_eq!(
            store
                .publish_verified_sharded(
                    &view,
                    std::slice::from_ref(&repository),
                    &[],
                    &[],
                    &[],
                    "verified-3",
                )
                .unwrap()
                .removed,
            1
        );
        assert!(
            store
                .context("incremental", owner)
                .unwrap()
                .edges
                .is_empty()
        );
        assert!(
            store
                .dependencies("incremental", owner, 1)
                .unwrap()
                .dependencies
                .is_empty()
        );
    }

    #[test]
    fn fact_shard_overrides_apply_to_bounded_traversals() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "overridden-shard",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repository".into(),
                },
                head: None,
                fingerprint: "state".into(),
            }],
        )
        .unwrap()
        .with_repository_contexts(BTreeMap::from([("rust".into(), BTreeMap::new())]))
        .unwrap()
        .with_repository_enrichment_inputs(BTreeMap::from([(
            "rust".into(),
            BTreeMap::from([("example/repository".into(), "semantic".into())]),
        )]))
        .unwrap();
        let state_owner = "repo://example/repository/rust/lib/state_run";
        let state_unresolved = "rust-call://state-target";
        let state_resolved = "repo://example/repository/rust/lib/state-target";
        let repository = facts(
            &view,
            vec![Observation::dependency(
                state_owner,
                DependencyRelation::Calls,
                state_unresolved,
                "src/lib.rs:3",
            )],
        );
        let shard_owner = "repo://example/repository/rust/lib/shard";
        let source = "repo://example/repository/rust/lib/run";
        let base_unresolved = "rust-call://base-target";
        let base_resolved = "repo://example/repository/rust/lib/base-target";
        let losing_enrichment_resolved =
            "repo://example/repository/rust/lib/losing-enrichment-target";
        let enrichment_unresolved = "rust-call://enrichment-target";
        let enrichment_resolved = "repo://example/repository/rust/lib/enrichment-target";
        let duplicate_target = "rust-call://duplicate-target";
        let structural_target = "repo://example/repository/rust/lib/structural-target";
        let shard = FactShard {
            repository: "example/repository".into(),
            producer: "rust".into(),
            owner: shard_owner.into(),
            version: "body-1".into(),
            entities: [
                shard_owner,
                source,
                state_owner,
                state_unresolved,
                base_resolved,
                base_unresolved,
                losing_enrichment_resolved,
                enrichment_resolved,
                enrichment_unresolved,
                duplicate_target,
                state_resolved,
                structural_target,
            ]
            .into_iter()
            .map(|id| EntityFact::new(id, EntityKind::Callable, None).unwrap())
            .collect(),
            observations: vec![
                Observation::dependency(
                    source,
                    DependencyRelation::Calls,
                    base_unresolved,
                    "src/lib.rs:1",
                ),
                Observation::dependency(
                    source,
                    DependencyRelation::Calls,
                    enrichment_unresolved,
                    "src/lib.rs:2",
                ),
                Observation::dependency(
                    source,
                    DependencyRelation::Calls,
                    enrichment_unresolved,
                    "src/lib.rs:4",
                ),
                Observation::dependency(
                    source,
                    DependencyRelation::Calls,
                    duplicate_target,
                    "src/lib.rs:5",
                ),
                Observation::structural(
                    source,
                    StructuralRelation::Defines,
                    structural_target,
                    "src/lib.rs:8",
                ),
            ],
        };
        let base_override = DependencyOverride {
            from: source.into(),
            relation: DependencyRelation::Calls,
            unresolved_to: base_unresolved.into(),
            resolved_to: base_resolved.into(),
            evidence: "src/lib.rs:1".into(),
            confidence: Confidence::Inferred,
            provenance: Provenance::UniqueNameHeuristic,
        };
        let enrichment_override = DependencyOverride {
            from: source.into(),
            relation: DependencyRelation::Calls,
            unresolved_to: enrichment_unresolved.into(),
            resolved_to: enrichment_resolved.into(),
            evidence: "src/lib.rs:2".into(),
            confidence: Confidence::Inferred,
            provenance: Provenance::UniqueNameHeuristic,
        };
        let losing_enrichment_override = DependencyOverride {
            from: source.into(),
            relation: DependencyRelation::Calls,
            unresolved_to: base_unresolved.into(),
            resolved_to: losing_enrichment_resolved.into(),
            evidence: "src/lib.rs:1".into(),
            confidence: Confidence::Inferred,
            provenance: Provenance::UniqueNameHeuristic,
        };
        let state_override = DependencyOverride {
            from: state_owner.into(),
            relation: DependencyRelation::Calls,
            unresolved_to: state_unresolved.into(),
            resolved_to: state_resolved.into(),
            evidence: "src/lib.rs:3".into(),
            confidence: Confidence::Inferred,
            provenance: Provenance::UniqueNameHeuristic,
        };
        let duplicate_override_observation = Observation::dependency(
            source,
            DependencyRelation::Calls,
            enrichment_unresolved,
            "src/lib.rs:2",
        );
        let mut duplicate_plain_observation = Observation::dependency(
            source,
            DependencyRelation::Calls,
            duplicate_target,
            "src/lib.rs:5",
        );
        duplicate_plain_observation.confidence = Confidence::Inferred;
        duplicate_plain_observation.provenance = Provenance::UniqueNameHeuristic;
        let mut duplicate_structural_observation = Observation::structural(
            source,
            StructuralRelation::Defines,
            structural_target,
            "src/lib.rs:8",
        );
        duplicate_structural_observation.confidence = Confidence::Inferred;
        duplicate_structural_observation.provenance = Provenance::UniqueNameHeuristic;
        store
            .publish_verified_sharded(
                &view,
                &[repository],
                &[base_override],
                &[shard],
                &[],
                "verified",
            )
            .unwrap();
        let input =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "rust");
        store
            .publish_enrichment(
                &view.name,
                "example/repository",
                &input,
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "1",
                },
                EnrichmentPayload {
                    observations: &[
                        duplicate_override_observation,
                        duplicate_plain_observation,
                        duplicate_structural_observation,
                    ],
                    overrides: &[
                        enrichment_override,
                        losing_enrichment_override,
                        state_override,
                    ],
                    ..EnrichmentPayload::default()
                },
            )
            .unwrap();
        store
            .db
            .run_script(
                "?[view, revision, from, relation, to, evidence, confidence, provenance] := \
                     *analysis_revision{view: 'overridden-shard', revision}, \
                     view = 'overridden-shard', \
                     from = 'repo://example/repository/rust/lib/state_run', \
                     relation = 'calls', to = 'rust-call://state-target', \
                     evidence in ['src/lib.rs:6', 'src/lib.rs:7'], \
                     confidence = 1.0, provenance = 'ast' \
                 :put analysis_revision_observation {\
                     view, revision, from, relation, to, evidence => confidence, provenance\
                 }",
                BTreeMap::new(),
                mnestic_engine::ScriptMutability::Mutable,
            )
            .unwrap();
        let transaction = store.db.multi_transaction(true);
        crate::storage::rebuild_resolved_dependencies(&transaction, &view.name).unwrap();
        transaction.commit().unwrap();

        let dependencies = store.dependencies(&view.name, source, 1).unwrap();
        assert!(
            !dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == structural_target)
        );
        assert!(
            store
                .impact(&view.name, structural_target, 1)
                .unwrap()
                .affected
                .is_empty()
        );
        assert!(
            dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == base_resolved)
        );
        assert!(
            !dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == losing_enrichment_resolved)
        );
        assert!(
            dependencies
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == enrichment_resolved)
        );
        assert_eq!(
            dependencies
                .edges
                .iter()
                .find(|edge| edge.to == enrichment_resolved)
                .unwrap()
                .evidence
                .len(),
            2
        );
        for unresolved in [base_unresolved, enrichment_unresolved] {
            assert!(
                !dependencies
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.entity == unresolved)
            );
            assert!(
                store
                    .impact(&view.name, unresolved, 1)
                    .unwrap()
                    .affected
                    .is_empty()
            );
        }
        for resolved in [base_resolved, enrichment_resolved] {
            let impact = store.impact(&view.name, resolved, 1).unwrap();
            assert!(
                impact
                    .affected
                    .iter()
                    .any(|affected| affected.entity == source)
            );
            if resolved == enrichment_resolved {
                assert_eq!(
                    impact
                        .edges
                        .iter()
                        .find(|edge| edge.to == enrichment_resolved)
                        .unwrap()
                        .evidence
                        .len(),
                    2
                );
            }
        }
        assert!(
            store
                .impact(&view.name, enrichment_resolved, 0)
                .unwrap()
                .traversal
                .truncated
        );
        assert_eq!(
            store
                .impact(&view.name, duplicate_target, 1)
                .unwrap()
                .edges
                .iter()
                .find(|edge| edge.to == duplicate_target)
                .unwrap()
                .evidence
                .len(),
            1
        );
        let state_impact = store.impact(&view.name, state_resolved, 1).unwrap();
        assert!(
            state_impact
                .affected
                .iter()
                .any(|affected| affected.entity == state_owner)
        );
        assert_eq!(
            state_impact
                .edges
                .iter()
                .find(|edge| edge.to == state_resolved)
                .unwrap()
                .evidence
                .len(),
            3
        );
        assert!(
            store
                .impact(&view.name, losing_enrichment_resolved, 1)
                .unwrap()
                .affected
                .is_empty()
        );
    }

    #[test]
    fn semantic_noop_refreshes_inputs_without_advancing_the_graph_revision() {
        let store = SemanticStore::memory().unwrap();
        let view = |name: &str, fingerprint: &str, head: &str| {
            WorkspaceView::new(
                name,
                "analysis",
                vec![RepositoryState {
                    repository: LogicalRepository {
                        identity: "example/repository".into(),
                    },
                    head: Some(head.into()),
                    fingerprint: fingerprint.into(),
                }],
            )
            .unwrap()
        };
        let initial = view("semantic-noop", "source-1", "head-1");
        let owner = "repo://example/repository/typescript/run";
        let shard = FactShard {
            repository: "example/repository".into(),
            producer: "typescript".into(),
            owner: owner.into(),
            version: "semantic-1".into(),
            entities: vec![
                EntityFact::new(owner, EntityKind::Callable, None).unwrap(),
                EntityFact::new("typescript-call://first", EntityKind::Callable, None).unwrap(),
            ],
            observations: vec![Observation::dependency(
                owner,
                DependencyRelation::Calls,
                "typescript-call://first",
                "src/run.ts:1",
            )],
        };
        store
            .publish_verified_sharded(
                &initial,
                &[facts(&initial, Vec::new())],
                &[],
                std::slice::from_ref(&shard),
                &[],
                "verified-1",
            )
            .unwrap();
        let other = view("semantic-noop-other", "source-1", "head-1");
        store
            .publish_verified_sharded(
                &other,
                &[facts(&other, Vec::new())],
                &[],
                std::slice::from_ref(&shard),
                &[],
                "verified-other",
            )
            .unwrap();
        let updated = view("semantic-noop", "source-2", "head-2");

        let changes = store
            .publish_verified_sharded(
                &updated,
                &[facts(&updated, Vec::new())],
                &[],
                std::slice::from_ref(&shard),
                &[],
                "verified-2",
            )
            .unwrap();

        assert_eq!(changes.unchanged, 1);
        assert_eq!(
            store
                .db
                .run_script(
                    "?[revision] := *analysis_revision{view: 'semantic-noop', revision}",
                    BTreeMap::new(),
                    mnestic_engine::ScriptMutability::Immutable,
                )
                .unwrap()
                .rows[0][0]
                .get_int(),
            Some(1)
        );
        assert!(store.view_matches(&updated).unwrap());
        assert!(
            store
                .verification_matches("semantic-noop", "verified-2")
                .unwrap()
        );
        assert_eq!(
            store
                .published_repository_head("semantic-noop", "example/repository")
                .unwrap()
                .as_deref(),
            Some("head-2")
        );
        assert_eq!(
            store
                .published_repository_head("semantic-noop-other", "example/repository")
                .unwrap()
                .as_deref(),
            Some("head-1")
        );
        assert_eq!(
            store.revision_input_fingerprints("semantic-noop").unwrap()["example/repository"],
            updated.repository_input_fingerprint(&updated.repository_states[0])
        );
        assert_eq!(
            store.context("semantic-noop", owner).unwrap().edges[0].to,
            "typescript-call://first"
        );
    }

    #[test]
    fn semantic_noop_rejects_obsolete_entity_used_by_retained_enrichment() {
        let store = SemanticStore::memory().unwrap();
        let current_revision = || {
            store
                .db
                .run_script(
                    "?[revision] := *analysis_revision{view: 'semantic-noop-validation', revision}",
                    BTreeMap::new(),
                    mnestic_engine::ScriptMutability::Immutable,
                )
                .unwrap()
                .rows[0][0]
                .get_int()
                .unwrap()
        };
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "example/repository".into(),
            },
            head: None,
            fingerprint: "source".into(),
        };
        let view = |analyzers: &[&str]| {
            WorkspaceView::new("semantic-noop-validation", "analysis", vec![state.clone()])
                .unwrap()
                .with_repository_contexts(
                    analyzers
                        .iter()
                        .map(|analyzer| ((*analyzer).into(), BTreeMap::new()))
                        .collect(),
                )
                .unwrap()
        };
        let initial = view(&["compiler-a", "compiler-b"]);
        let source = EntityFact::new("repo/source", EntityKind::Callable, None).unwrap();
        let baseline = RepositoryFacts {
            entities: vec![source.clone()],
            ..facts(&initial, Vec::new())
        };
        store
            .publish_verified_sharded(&initial, &[baseline], &[], &[], &[], "verified-1")
            .unwrap();
        let target = EntityFact::new("repo/target", EntityKind::Callable, None).unwrap();
        let retained_observation = Observation::dependency(
            source.id.as_str(),
            DependencyRelation::Calls,
            target.id.as_str(),
            "src/lib.rs:1",
        );
        for (analyzer, payload) in [
            (
                "compiler-a",
                EnrichmentPayload {
                    entities: std::slice::from_ref(&target),
                    ..EnrichmentPayload::default()
                },
            ),
            (
                "compiler-b",
                EnrichmentPayload {
                    observations: std::slice::from_ref(&retained_observation),
                    ..EnrichmentPayload::default()
                },
            ),
        ] {
            let input = initial.repository_enrichment_input_fingerprint(&state, analyzer);
            store
                .publish_enrichment(
                    &initial.name,
                    "example/repository",
                    &input,
                    EnrichmentOwner {
                        analyzer,
                        version: "1",
                    },
                    payload,
                )
                .unwrap();
        }
        assert_eq!(current_revision(), 3);

        let updated = view(&["compiler-b"]);
        let baseline = RepositoryFacts {
            entities: vec![source],
            ..facts(&updated, Vec::new())
        };
        assert!(
            store
                .publish_verified_sharded(&updated, &[baseline], &[], &[], &[], "verified-2",)
                .is_err()
        );
        assert_eq!(current_revision(), 3);
        assert_eq!(
            store
                .context(&initial.name, target.id.as_str())
                .unwrap()
                .root
                .id,
            target.id.as_str()
        );
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
            entities: vec![
                EntityFact::new("repo/source", EntityKind::Callable, None).unwrap(),
                EntityFact::new("repo/target", EntityKind::Callable, None).unwrap(),
            ],
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
        store
            .publish(&view, std::slice::from_ref(&repository), &[])
            .unwrap();
        assert_eq!(
            store
                .delete_standalone_repository_revision("example/repository", "standalone")
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
        assert!(
            store
                .db
                .run_script(
                    "?[revision] := *analysis_revision_state{view: 'standalone', revision}",
                    BTreeMap::new(),
                    mnestic_engine::ScriptMutability::Immutable,
                )
                .unwrap()
                .rows
                .is_empty()
        );
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
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("read blocked behind the uncommitted writer");
        assert_eq!(revision, 1);
        assert_eq!(edge_count, 1);
        writer.abort().unwrap();
        reader_thread.join().unwrap();

        let reader = store.read_db.multi_transaction(false);
        reader
            .run_script(
                "?[revision] := *analysis_revision{view: 'main', revision}",
                BTreeMap::new(),
            )
            .unwrap();
        let (sent, received) = mpsc::channel();
        let checkpoint = store.clone();
        let checkpoint_thread =
            thread::spawn(move || sent.send(checkpoint.checkpoint_passive().is_ok()).unwrap());
        assert!(
            received
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("checkpoint blocked behind the active read snapshot")
        );
        reader.abort().unwrap();
        checkpoint_thread.join().unwrap();
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn view_match_reads_use_the_reserved_read_engine() {
        let store = Arc::new(SemanticStore::memory().unwrap());
        let access = store.clone();
        let view = WorkspaceView::new(
            "missing",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repository".into(),
                },
                head: None,
                fingerprint: "input".into(),
            }],
        )
        .unwrap();
        let engine = store.engine.lock().unwrap();
        let (sent, received) = mpsc::channel();
        let access_thread = thread::spawn(move || {
            let result = access.view_matches(&view);
            sent.send(result.is_ok()).unwrap();
        });

        received
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("view match read blocked behind the primary engine");
        drop(engine);
        access_thread.join().unwrap();
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
        let mut writer_interleaved = false;
        assert_eq!(
            store
                .sweep_garbage_collection(|event| {
                    if event.completed_rows == Some(10_000) && !writer_interleaved {
                        store
                            .store_verification_fingerprint("main", "during-gc")
                            .unwrap();
                        writer_interleaved = true;
                    }
                    progress.push(event);
                    true
                })
                .unwrap(),
            3
        );
        assert!(writer_interleaved);
        assert!(store.verification_matches("main", "during-gc").unwrap());
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
