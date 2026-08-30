use super::{IndexScheduler, pipeline, refresh_workspace_snapshot, standalone_view_name};
use crate::indexing::enrichment_publication::{
    EnrichmentContribution, EnrichmentPublication, EnrichmentPublicationRequest,
    EnrichmentSnapshotRead, EnrichmentTarget,
};
use crate::{
    jobs::{
        EnrichmentJob as DurableEnrichmentJob, EnrichmentOutcome,
        EnrichmentTarget as DurableEnrichmentTarget, JobTrigger,
    },
    workspace_registry::WorkspaceRegistry,
};
use beholder_adapters_mnestic::{EnrichmentPublishOutcome, SemanticStore};
use beholder_domain::{Workspace, WorkspaceView};
#[cfg(test)]
use beholder_indexing::AnalyzerMetadata;
use beholder_indexing::{AnalyzerContribution, EnrichmentSnapshot, WorkspaceSnapshot};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::Mutex,
};

#[derive(Clone, Debug)]
pub(crate) struct ManualEnrichmentTarget {
    pub(crate) target: DurableEnrichmentTarget,
    pub(crate) worker_id: String,
    pub(crate) worker_version: String,
    pub(crate) input_fingerprint: Option<String>,
    pub(crate) current: bool,
}

impl IndexScheduler {
    pub(super) fn ensure_enrichment_inputs(
        &self,
        store: &SemanticStore,
        view: &WorkspaceView,
    ) -> Result<(), Box<dyn Error>> {
        store.ensure_revision_inputs(view)?;
        Ok(())
    }

    pub(super) fn enrichment_inputs_complete(
        &self,
        store: &SemanticStore,
        view: &WorkspaceView,
    ) -> Result<bool, Box<dyn Error>> {
        for analyzer in view.enrichment_analyzers() {
            for state in &view.repository_states {
                let repository = &state.repository.identity;
                if store
                    .revision_enrichment_input_fingerprint(&view.name, repository, analyzer)?
                    .is_none()
                    || store.repository_contexts(&view.name, repository, analyzer)?
                        != view.repository_contexts(repository, analyzer)
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

impl IndexScheduler {
    pub(crate) fn manual_enrichment_targets(
        &self,
        store: &SemanticStore,
        workspaces: &Mutex<WorkspaceRegistry>,
        repository: &str,
        workspace_scope: Option<&str>,
        requested_workers: &BTreeSet<String>,
    ) -> Result<Vec<ManualEnrichmentTarget>, String> {
        let catalog = self
            .indexer
            .enrichment_catalog()
            .into_iter()
            .map(|worker| (worker.id.clone(), worker))
            .collect::<BTreeMap<_, _>>();
        if let Some(unknown) = requested_workers
            .iter()
            .find(|worker| !catalog.contains_key(*worker))
        {
            return Err(format!("unknown enrichment worker {unknown}"));
        }
        let selected = if requested_workers.is_empty() {
            catalog.keys().cloned().collect::<BTreeSet<_>>()
        } else {
            requested_workers.clone()
        };
        let targets = {
            let registry = workspaces
                .lock()
                .map_err(|_| "workspace registry lock poisoned")?;
            let registered = registry
                .repository(repository)
                .cloned()
                .ok_or_else(|| format!("repository is no longer registered: {repository}"))?;
            let mut targets = if let Some(scope) = workspace_scope {
                let workspace = registry
                    .get(scope)
                    .cloned()
                    .ok_or_else(|| format!("workspace is no longer registered: {scope}"))?;
                if !workspace
                    .repositories
                    .iter()
                    .any(|candidate| candidate.repository.identity == repository)
                {
                    return Err(format!(
                        "repository {repository} is not in workspace {scope}"
                    ));
                }
                vec![(workspace, false)]
            } else {
                registry
                    .workspaces_referencing_repository(repository)
                    .into_iter()
                    .map(|workspace| (workspace, false))
                    .collect()
            };
            if targets.is_empty() {
                let workspace =
                    Workspace::new(standalone_view_name(repository), vec![registered.selection])
                        .map_err(|error| error.to_string())?
                        .with_enabled_plugins(selected.clone())
                        .map_err(|error| error.to_string())?;
                targets.push((workspace, true));
            }
            targets
        };

        let mut planned = Vec::new();
        for (workspace, standalone) in targets {
            let (snapshot, _) = refresh_workspace_snapshot(self, &workspace, None)
                .map_err(|error| error.to_string())?;
            let target_snapshot = snapshot
                .repositories
                .iter()
                .find(|candidate| candidate.state.repository.identity == repository)
                .ok_or_else(|| {
                    format!(
                        "repository is no longer in workspace {}: {repository}",
                        workspace.name
                    )
                })?;
            for worker_id in &selected {
                let worker = catalog
                    .get(worker_id)
                    .expect("selected enrichment worker exists in catalog");
                if !self.indexer.enricher_is_active(
                    worker_id,
                    &workspace.enabled_plugins,
                    target_snapshot,
                ) {
                    continue;
                }
                let input_fingerprint = store
                    .revision_enrichment_input_fingerprint(&workspace.name, repository, worker_id)
                    .map_err(|error| error.to_string())?;
                let target = if standalone {
                    DurableEnrichmentTarget::StandaloneRepository {
                        repository: repository.to_owned(),
                    }
                } else {
                    DurableEnrichmentTarget::WorkspaceRepository {
                        workspace: workspace.name.clone(),
                        repository: repository.to_owned(),
                    }
                };
                let current = if input_fingerprint.is_some() {
                    store
                        .enrichment_is_current(EnrichmentTarget {
                            view: &workspace.name,
                            repository,
                            analyzer: worker_id,
                            version: &worker.version,
                        })
                        .map_err(|error| error.to_string())?
                } else {
                    false
                };
                planned.push(ManualEnrichmentTarget {
                    target,
                    worker_id: worker_id.clone(),
                    worker_version: worker.version.clone(),
                    input_fingerprint,
                    current,
                });
            }
        }
        Ok(planned)
    }

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
        let mut repositories = snapshot.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by_key(|repository| repository.inputs.len());
        for repository in repositories {
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
                        format!(
                            "published {workspace_name}/{repository_id} enrichment input fingerprint is missing for {}",
                            worker.id
                        )
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
    ) -> Result<EnrichmentOutcome, String> {
        let (workspace_config, repository) = {
            let registry = workspaces
                .lock()
                .map_err(|_| "workspace registry lock poisoned")?;
            match &job.target {
                DurableEnrichmentTarget::WorkspaceRepository {
                    workspace,
                    repository,
                } => (
                    registry
                        .get(workspace)
                        .cloned()
                        .ok_or_else(|| format!("workspace is no longer registered: {workspace}"))?,
                    repository.clone(),
                ),
                DurableEnrichmentTarget::StandaloneRepository { repository } => {
                    let registered = registry.repository(repository).cloned().ok_or_else(|| {
                        format!("repository is no longer registered: {repository}")
                    })?;
                    let enabled_plugins: BTreeSet<_> = self
                        .indexer
                        .enrichment_catalog()
                        .into_iter()
                        .map(|worker| worker.id)
                        .collect();
                    (
                        Workspace::new(
                            standalone_view_name(repository),
                            vec![registered.selection],
                        )
                        .map_err(|error| error.to_string())?
                        .with_enabled_plugins(enabled_plugins)
                        .map_err(|error| error.to_string())?,
                        repository.clone(),
                    )
                }
            }
        };
        let workspace = &workspace_config.name;
        let (snapshot, _) = refresh_workspace_snapshot(self, &workspace_config, None)
            .map_err(|error| error.to_string())?;
        let worker = self
            .indexer
            .enrichment_catalog()
            .into_iter()
            .find(|worker| worker.id == job.worker_id);
        let Some(worker) = worker.filter(|worker| worker.version == job.expected_worker_version)
        else {
            return Ok(EnrichmentOutcome::Superseded);
        };
        let target_snapshot = snapshot
            .repositories
            .iter()
            .find(|candidate| candidate.state.repository.identity == repository)
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
            repository: &repository,
            analyzer: &worker.id,
            version: &worker.version,
        };
        let input_fingerprint = match job.input_fingerprint.clone() {
            Some(fingerprint) => fingerprint,
            None => store
                .revision_enrichment_input_fingerprint(workspace, &repository, &worker.id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!(
                        "no successful baseline exists for repository {repository} in {workspace}"
                    )
                })?,
        };
        let (entity_kinds, relations) = self.indexer.enrichment_semantic_inputs(&worker.id);
        let Some(state) = store
            .enrichment_snapshot(EnrichmentSnapshotRead {
                target,
                input_fingerprint: &input_fingerprint,
                entity_kinds: &entity_kinds,
                relations: &relations,
            })
            .map_err(|error| error.to_string())?
        else {
            return Ok(EnrichmentOutcome::Superseded);
        };
        if !self.current_revision_inputs_match(
            &snapshot,
            workspace,
            &repository,
            &state.contexts,
            &state.revision_inputs,
        )? {
            return Ok(EnrichmentOutcome::Superseded);
        }
        if store
            .enrichment_is_current(target)
            .map_err(|error| error.to_string())?
        {
            return Ok(EnrichmentOutcome::AlreadyCurrent);
        }
        let contexts = state.contexts.clone();
        if !active {
            return EnrichmentPublication::publish_enrichment(
                store,
                EnrichmentPublicationRequest {
                    target,
                    input_fingerprint: &input_fingerprint,
                    contribution: EnrichmentContribution::default(),
                },
            )
            .map(enrichment_outcome)
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
        if contribution_escapes_target(&contribution, &repository) {
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
            return Ok(EnrichmentOutcome::Superseded);
        }
        let (current, _) = refresh_workspace_snapshot(self, &workspace_config, None)
            .map_err(|error| error.to_string())?;
        if !self.current_revision_inputs_match(
            &current,
            workspace,
            &repository,
            &contexts,
            &state.revision_inputs,
        )? {
            return Ok(EnrichmentOutcome::Superseded);
        }
        let mut diagnostics = contribution.diagnostics;
        let mut diagnostic_replacements = Vec::new();
        let mut entities = Vec::new();
        let mut observations = Vec::new();
        let mut fact_shards = Vec::new();
        for contribution in contribution.repositories {
            diagnostic_replacements.extend(
                contribution
                    .replaced_diagnostic_codes
                    .into_iter()
                    .map(|code| (contribution.repository.clone(), code)),
            );
            entities.extend(contribution.entities);
            observations.extend(contribution.observations);
            fact_shards.extend(contribution.fact_shards);
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
                input_fingerprint: &input_fingerprint,
                contribution: EnrichmentContribution {
                    entities: &entities,
                    observations: &observations,
                    overrides: &contribution.overrides,
                    diagnostics: &diagnostics,
                    diagnostic_replacements: &diagnostic_replacements,
                    fact_shards: &fact_shards,
                },
            },
        )
        .map(enrichment_outcome)
        .map_err(|error| error.to_string())
    }

    fn current_revision_inputs_match(
        &self,
        snapshot: &WorkspaceSnapshot,
        workspace: &str,
        repository: &str,
        contexts: &[String],
        expected: &BTreeMap<String, String>,
    ) -> Result<bool, String> {
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
        )?;
        Ok(std::iter::once(repository)
            .chain(contexts.iter().map(String::as_str))
            .all(|identity| {
                view.repository_states
                    .iter()
                    .find(|state| state.repository.identity == identity)
                    .zip(expected.get(identity))
                    .is_some_and(|(state, expected)| {
                        view.repository_input_fingerprint(state) == *expected
                    })
            }))
    }
}

fn enrichment_outcome(outcome: EnrichmentPublishOutcome) -> EnrichmentOutcome {
    match outcome {
        EnrichmentPublishOutcome::Published => EnrichmentOutcome::Published,
        EnrichmentPublishOutcome::Unchanged => EnrichmentOutcome::Unchanged,
        EnrichmentPublishOutcome::Superseded => EnrichmentOutcome::Superseded,
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
            || repository.fact_shards.iter().any(|shard| {
                shard.repository != target
                    || shard.producer != contribution.metadata.id
                    || !entity_belongs_to_target(shard.owner.as_str(), target)
                    || shard
                        .entities
                        .iter()
                        .any(|entity| entity_escapes_target(entity.id.as_str(), target))
                    || shard
                        .observations
                        .iter()
                        .any(|observation| entity_escapes_target(observation.from.as_str(), target))
            })
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
        Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind, FactShard,
        Provenance,
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
                        semantic_candidates: Vec::new(),
                        diagnostics: Vec::new(),
                        replaced_diagnostic_codes: BTreeSet::new(),
                        fact_shards: Vec::new(),
                    }],
                    overrides: Vec::new(),
                    candidate_overrides: Vec::new(),
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
                semantic_candidates: Vec::new(),
                diagnostics: Vec::new(),
                replaced_diagnostic_codes: BTreeSet::new(),
                fact_shards: Vec::new(),
            }],
            overrides: Vec::new(),
            candidate_overrides: Vec::new(),
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

        let mut shard_escape = contribution();
        shard_escape.repositories[0].fact_shards.push(FactShard {
            repository: "example/context".into(),
            producer: "rust".into(),
            owner: "repo://example/context/rust-source/src/lib.rs".into(),
            version: "1".into(),
            entities: Vec::new(),
            observations: Vec::new(),
        });
        assert!(contribution_escapes_target(&shard_escape, "example/target"));

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

        assert_eq!(
            scheduler
                .run_enrichment_job(&store, &registry, &job)
                .await
                .unwrap(),
            EnrichmentOutcome::Published
        );
        assert!(
            store
                .enrichment_matches("main", &repository, "semantic", "1")
                .unwrap()
        );
        fs::write(source, "fn changed() {}").unwrap();
        assert_eq!(
            scheduler
                .run_enrichment_job(&store, &registry, &job)
                .await
                .unwrap(),
            EnrichmentOutcome::Superseded
        );

        fs::remove_dir_all(state).unwrap();
    }

    #[tokio::test]
    async fn manual_job_does_not_run_without_a_successful_baseline() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-enrichment-no-baseline-{unique}"));
        let repository = state.join("repo");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(repository.join("src/lib.rs"), "fn source() {}").unwrap();
        let mut registry = WorkspaceRegistry::open(state.join("workspaces.json")).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let repository = workspace.repositories[0].repository.identity.clone();
        let scheduler = IndexScheduler::with_indexer(
            IndexerBuilder::new(state.join("frontend-cache"), 1)
                .add_enricher(FakeEnricher)
                .build()
                .unwrap(),
        );

        let error = scheduler
            .run_enrichment_job(
                &SemanticStore::memory().unwrap(),
                &Mutex::new(registry),
                &DurableEnrichmentJob {
                    target: DurableEnrichmentTarget::WorkspaceRepository {
                        workspace: "main".into(),
                        repository: repository.clone(),
                    },
                    worker_id: "semantic".into(),
                    expected_worker_version: "1".into(),
                    trigger: JobTrigger::Manual,
                    prerequisite_index_jobs: Vec::new(),
                    input_fingerprint: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            format!("no successful baseline exists for repository {repository} in main")
        );

        fs::remove_dir_all(state).unwrap();
    }
}
