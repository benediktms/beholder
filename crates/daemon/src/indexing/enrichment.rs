use super::{IndexScheduler, pipeline, refresh_workspace_snapshot};
use crate::indexing::enrichment_publication::{
    EnrichmentContribution, EnrichmentPublication, EnrichmentPublicationRequest,
    EnrichmentSnapshotRead, EnrichmentTarget,
};
use crate::{
    jobs::{
        EnrichmentJob as DurableEnrichmentJob, EnrichmentTarget as DurableEnrichmentTarget,
        JobTrigger,
    },
    workspace_registry::WorkspaceRegistry,
};
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::WorkspaceView;
#[cfg(test)]
use beholder_indexing::AnalyzerMetadata;
use beholder_indexing::{AnalyzerContribution, EnrichmentSnapshot, WorkspaceSnapshot};
use std::{collections::BTreeMap, error::Error, sync::Mutex};

impl IndexScheduler {
    pub(super) fn ensure_enrichment_inputs(
        &self,
        store: &SemanticStore,
        view: &WorkspaceView,
    ) -> Result<(), Box<dyn Error>> {
        store.ensure_revision_inputs(view)?;
        Ok(())
    }
}

impl IndexScheduler {
    pub(crate) fn automatic_enrichment_jobs(
        &self,
        store: &SemanticStore,
        workspaces: &Mutex<WorkspaceRegistry>,
        workspace_name: &str,
        prerequisite: crate::jobs::IndexJobId,
    ) -> Result<Vec<DurableEnrichmentJob>, String> {
        let workspace = workspaces
            .lock()
            .map_err(|_| "workspace registry lock poisoned")?
            .get(workspace_name)
            .cloned()
            .ok_or_else(|| format!("workspace is no longer registered: {workspace_name}"))?;
        let (snapshot, _) = refresh_workspace_snapshot(self, &workspace, None)
            .map_err(|error| error.to_string())?;
        let mut jobs = Vec::new();
        for repository in &snapshot.repositories {
            let repository_id = &repository.state.repository.identity;
            for worker in self.indexer.enrichment_catalog() {
                let target = EnrichmentTarget {
                    view: workspace_name,
                    repository: repository_id,
                    analyzer: &worker.id,
                    version: &worker.version,
                };
                if store
                    .enrichment_is_current(target)
                    .map_err(|error| error.to_string())?
                {
                    continue;
                }
                let input_fingerprint = store
                    .revision_enrichment_input_fingerprint(
                        workspace_name,
                        repository_id,
                        &worker.id,
                    )
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "published repository enrichment input fingerprint is missing".to_owned()
                    })?;
                jobs.push(DurableEnrichmentJob {
                    target: DurableEnrichmentTarget::WorkspaceRepository {
                        workspace: workspace_name.to_owned(),
                        repository: repository_id.clone(),
                    },
                    worker_id: worker.id,
                    expected_worker_version: worker.version,
                    trigger: JobTrigger::Automatic,
                    prerequisite_index_jobs: vec![prerequisite.clone()],
                    input_fingerprint: Some(input_fingerprint),
                });
            }
        }
        Ok(jobs)
    }

    pub(crate) async fn run_enrichment_job(
        &self,
        store: &SemanticStore,
        workspaces: &Mutex<WorkspaceRegistry>,
        job: &DurableEnrichmentJob,
    ) -> Result<bool, String> {
        let DurableEnrichmentTarget::WorkspaceRepository {
            workspace,
            repository,
        } = &job.target
        else {
            return Err("automatic enrichment jobs must target a workspace repository".into());
        };
        let input_fingerprint = job
            .input_fingerprint
            .as_deref()
            .ok_or("automatic enrichment job is missing its input fingerprint")?;
        let workspace_config = workspaces
            .lock()
            .map_err(|_| "workspace registry lock poisoned")?
            .get(workspace)
            .cloned()
            .ok_or_else(|| format!("workspace is no longer registered: {workspace}"))?;
        let (snapshot, _) = refresh_workspace_snapshot(self, &workspace_config, None)
            .map_err(|error| error.to_string())?;
        let worker = self
            .indexer
            .enrichment_catalog()
            .into_iter()
            .find(|worker| worker.id == job.worker_id);
        let Some(worker) = worker.filter(|worker| worker.version == job.expected_worker_version)
        else {
            return Ok(false);
        };
        let target_snapshot = snapshot
            .repositories
            .iter()
            .find(|candidate| candidate.state.repository.identity == *repository)
            .ok_or_else(|| {
                format!("repository is no longer in workspace {workspace}: {repository}")
            })?;
        let active = self.indexer.enricher_is_active(
            &worker.id,
            &workspace_config.enabled_plugins,
            target_snapshot,
        );
        let target = EnrichmentTarget {
            view: workspace,
            repository,
            analyzer: &worker.id,
            version: &worker.version,
        };
        let (entity_kinds, relations) = self.indexer.enrichment_semantic_inputs(&worker.id);
        let Some(state) = store
            .enrichment_snapshot(EnrichmentSnapshotRead {
                target,
                input_fingerprint,
                entity_kinds: &entity_kinds,
                relations: &relations,
            })
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if self.current_enrichment_input_fingerprint(
            &snapshot,
            workspace,
            repository,
            &worker.id,
            &state.contexts,
        )? != input_fingerprint
        {
            return Ok(false);
        }
        let contexts = state.contexts.clone();
        if !active {
            return EnrichmentPublication::publish_enrichment(
                store,
                EnrichmentPublicationRequest {
                    target,
                    input_fingerprint,
                    contribution: EnrichmentContribution::default(),
                },
            )
            .map_err(|error| error.to_string());
        }
        let contribution = self
            .indexer
            .enrich(
                EnrichmentSnapshot {
                    target_repository: repository.clone(),
                    workspace: WorkspaceSnapshot {
                        name: snapshot.name.clone(),
                        repositories: std::iter::once(target_snapshot.clone())
                            .chain(state.contexts.iter().filter_map(|identity| {
                                snapshot
                                    .repositories
                                    .iter()
                                    .find(|candidate| {
                                        candidate.state.repository.identity == *identity
                                    })
                                    .cloned()
                            }))
                            .collect(),
                    },
                    baseline: state.baseline,
                },
                &worker.id,
            )
            .await
            .map_err(|error| error.to_string())?;
        if contribution.metadata != worker {
            return Err(
                "worker contribution metadata does not match the scheduled analyzer".into(),
            );
        }
        if contribution_escapes_target(&contribution, repository) {
            return Err("worker contribution escaped its target repository".into());
        }
        if self
            .indexer
            .enrichment_catalog()
            .into_iter()
            .find(|current| current.id == worker.id)
            .as_ref()
            != Some(&worker)
        {
            return Ok(false);
        }
        let (current, _) = refresh_workspace_snapshot(self, &workspace_config, None)
            .map_err(|error| error.to_string())?;
        if self.current_enrichment_input_fingerprint(
            &current, workspace, repository, &worker.id, &contexts,
        )? != input_fingerprint
        {
            return Ok(false);
        }
        let mut diagnostics = contribution.diagnostics;
        let mut entities = Vec::new();
        let mut observations = Vec::new();
        for contribution in contribution.repositories {
            entities.extend(contribution.entities);
            observations.extend(contribution.observations);
            diagnostics.extend(
                contribution
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| (contribution.repository.clone(), diagnostic)),
            );
        }
        pipeline::report_analysis_diagnostics(workspace, &diagnostics);
        EnrichmentPublication::publish_enrichment(
            store,
            EnrichmentPublicationRequest {
                target,
                input_fingerprint,
                contribution: EnrichmentContribution {
                    entities: &entities,
                    observations: &observations,
                    overrides: &contribution.overrides,
                    diagnostics: &diagnostics,
                },
            },
        )
        .map_err(|error| error.to_string())
    }

    fn current_enrichment_input_fingerprint(
        &self,
        snapshot: &WorkspaceSnapshot,
        workspace: &str,
        repository: &str,
        worker: &str,
        contexts: &[String],
    ) -> Result<String, String> {
        let plan = self.indexer.prepare(snapshot);
        let view = WorkspaceView::new_scoped(
            workspace,
            plan.analysis_identity(),
            snapshot
                .repositories
                .iter()
                .map(|repository| repository.state.clone())
                .collect(),
            plan.repository_enrichment_identities(),
        )?
        .with_repository_contexts(BTreeMap::from([(
            worker.to_owned(),
            BTreeMap::from([(repository.to_owned(), contexts.to_vec())]),
        )]))?
        .with_repository_enrichment_inputs(BTreeMap::from([(
            worker.to_owned(),
            self.indexer
                .enrichment_input_identities(snapshot)
                .remove(worker)
                .ok_or_else(|| format!("unknown enrichment worker {worker}"))?,
        )]))?;
        let state = view
            .repository_states
            .iter()
            .find(|state| state.repository.identity == repository)
            .ok_or_else(|| {
                format!("repository is no longer in workspace {workspace}: {repository}")
            })?;
        Ok(view.repository_enrichment_input_fingerprint(state, worker))
    }
}

fn contribution_escapes_target(contribution: &AnalyzerContribution, target: &str) -> bool {
    contribution.repositories.iter().any(|repository| {
        repository.repository != target
            || repository
                .entities
                .iter()
                .any(|entity| entity_escapes_target(entity.id.as_str(), target))
            || repository
                .grpc_bindings
                .iter()
                .any(|binding| !entity_belongs_to_target(binding.local_symbol.as_str(), target))
            || repository
                .observations
                .iter()
                .any(|observation| entity_escapes_target(observation.from.as_str(), target))
    }) || contribution
        .active_repositories
        .iter()
        .any(|repository| repository != target)
        || contribution
            .diagnostics
            .iter()
            .any(|(repository, _)| repository != target)
        || contribution.graphql_resolvers.iter().any(|resolver| {
            resolver.repository != target
                || !entity_belongs_to_target(resolver.resolver.as_str(), target)
        })
        || contribution
            .overrides
            .iter()
            .any(|override_| !entity_belongs_to_target(override_.from.as_str(), target))
}

fn entity_belongs_to_target(entity: &str, target: &str) -> bool {
    entity.starts_with(&format!("repo://{target}/"))
}

fn entity_escapes_target(entity: &str, target: &str) -> bool {
    entity.starts_with("repo://") && !entity_belongs_to_target(entity, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind, Provenance,
    };
    use beholder_indexing::{
        AnalysisCompleteness, CacheStatistics, EnrichmentFuture, IndexerBuilder,
        RepositoryContribution, WorkspaceEnricher,
    };
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct FakeEnricher;

    impl WorkspaceEnricher for FakeEnricher {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: "semantic".into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            path.extension().is_some_and(|extension| extension == "rs")
        }

        fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
            Box::pin(async move {
                let repository = snapshot.target_repository;
                Ok(AnalyzerContribution {
                    metadata: self.metadata(),
                    active_repositories: vec![repository.clone()],
                    repositories: vec![RepositoryContribution {
                        repository: repository.clone(),
                        completeness: AnalysisCompleteness::Complete,
                        entities: vec![EntityFact::new(
                            format!("repo://{repository}/semantic/generated"),
                            EntityKind::Callable,
                            None,
                        )?],
                        grpc_bindings: Vec::new(),
                        observations: Vec::new(),
                        diagnostics: Vec::new(),
                    }],
                    overrides: Vec::new(),
                    graphql_resolvers: Vec::new(),
                    diagnostics: Vec::new(),
                    cache: CacheStatistics::default(),
                })
            })
        }
    }

    fn contribution() -> AnalyzerContribution {
        AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: "rust".into(),
                version: "1".into(),
            },
            active_repositories: vec!["example/target".into()],
            repositories: vec![RepositoryContribution {
                repository: "example/target".into(),
                completeness: AnalysisCompleteness::Complete,
                entities: vec![
                    EntityFact::new(
                        "repo://example/target/rust/lib/local",
                        EntityKind::Callable,
                        None,
                    )
                    .unwrap(),
                ],
                grpc_bindings: Vec::new(),
                observations: Vec::new(),
                diagnostics: Vec::new(),
            }],
            overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        }
    }

    #[test]
    fn rejects_repository_qualified_facts_owned_by_context() {
        let mut entity_escape = contribution();
        entity_escape.repositories[0].entities[0] = EntityFact::new(
            "repo://example/context/rust/lib/foreign",
            EntityKind::Callable,
            None,
        )
        .unwrap();
        assert!(contribution_escapes_target(
            &entity_escape,
            "example/target"
        ));

        let mut override_escape = contribution();
        override_escape.overrides.push(DependencyOverride {
            from: "repo://example/context/rust/lib/caller".into(),
            relation: DependencyRelation::Calls,
            unresolved_to: "rust-call://callee".into(),
            resolved_to: "repo://example/target/rust/lib/callee".into(),
            evidence: "src/lib.rs:1".into(),
            confidence: Confidence::Exact,
            provenance: Provenance::Compiler,
        });
        assert!(contribution_escapes_target(
            &override_escape,
            "example/target"
        ));

        assert!(!contribution_escapes_target(
            &contribution(),
            "example/target"
        ));
    }

    #[tokio::test]
    async fn automatic_job_rebuilds_and_publishes_current_enrichment() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-durable-enrichment-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        let source = repository.join("src/lib.rs");
        fs::write(&source, "fn source() {}").unwrap();
        let registry_path = state.join("workspaces.json");
        let mut registry = WorkspaceRegistry::open(registry_path).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let repository = workspace.repositories[0].repository.identity.clone();
        let registry = Mutex::new(registry);
        let store = SemanticStore::memory().unwrap();
        let scheduler = IndexScheduler::with_indexer(
            IndexerBuilder::new(state.join("frontend-cache"), 1)
                .add_enricher(FakeEnricher)
                .build()
                .unwrap(),
        );
        scheduler.index(&store, &workspace).unwrap();
        let job = scheduler
            .automatic_enrichment_jobs(
                &store,
                &registry,
                "main",
                crate::jobs::IndexJobId("01J00000000000000000000000".into()),
            )
            .unwrap()
            .pop()
            .unwrap();

        assert!(
            scheduler
                .run_enrichment_job(&store, &registry, &job)
                .await
                .unwrap()
        );
        assert!(
            store
                .enrichment_matches("main", &repository, "semantic", "1")
                .unwrap()
        );
        fs::write(source, "fn changed() {}").unwrap();
        assert!(
            !scheduler
                .run_enrichment_job(&store, &registry, &job)
                .await
                .unwrap()
        );

        fs::remove_dir_all(state).unwrap();
    }
}
