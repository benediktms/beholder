use super::{IndexScheduler, pipeline};
use beholder_adapters_mnestic::{EnrichmentOwner, EnrichmentPayload, SemanticStore};
use beholder_domain::WorkspaceView;
use beholder_indexing::{
    AnalyzerContribution, AnalyzerMetadata, EnrichmentSnapshot, WorkspaceSnapshot,
};
use std::{error::Error, sync::Arc, time::Instant};
use tokio::sync::watch;
use tracing::Instrument;

type EnrichmentKey = (String, String, String);

#[derive(Clone)]
pub(super) struct EnrichmentJob {
    pub(super) analyzer: AnalyzerMetadata,
    pub(super) repository: String,
    pub(super) input_fingerprint: String,
    pub(super) queued_at: Instant,
    pub(super) snapshot: EnrichmentSnapshot,
    pub(super) view: WorkspaceView,
}

pub(super) struct EnrichmentRun {
    pub(super) input_fingerprint: String,
    pub(super) version: String,
    pub(super) cancel: watch::Sender<()>,
}

impl EnrichmentRun {
    fn matches(&self, input_fingerprint: &str, version: &str) -> bool {
        self.input_fingerprint == input_fingerprint && self.version == version
    }
}

impl IndexScheduler {
    pub(super) async fn run_enrichments(self: Arc<Self>, store: Arc<SemanticStore>) {
        loop {
            tokio::select! {
                _ = self.enrichment_changed.notified() => {}
                _ = self.enrichment_shutdown.notified() => break,
            }
            while let Some(job) = self
                .enrichment_jobs
                .lock()
                .ok()
                .and_then(|mut jobs| jobs.pop_first().map(|(_, job)| job))
            {
                let key = enrichment_key(&job);
                let (cancel, mut cancelled) = watch::channel(());
                if let Ok(mut active) = self.enriching.lock() {
                    active.insert(
                        key.clone(),
                        EnrichmentRun {
                            input_fingerprint: job.input_fingerprint.clone(),
                            version: job.analyzer.version.clone(),
                            cancel,
                        },
                    );
                }
                let scheduler = self.clone();
                let store = store.clone();
                let workspace = job.view.name.clone();
                let repository = job.repository.clone();
                let analyzer = job.analyzer.id.clone();
                let span = tracing::info_span!("index.enrichment", workspace, repository, analyzer);
                span.in_scope(|| {
                    tracing::info!(
                        queue_ms = job.queued_at.elapsed().as_secs_f64() * 1_000.0,
                        "repository enrichment started"
                    );
                });
                let result = tokio::select! {
                    result = scheduler.enrich(&store, job).instrument(span.clone()) => Some(result),
                    changed = cancelled.changed() => {
                        let _ = changed;
                        None
                    }
                    _ = self.enrichment_shutdown.notified() => {
                        if let Ok(mut active) = self.enriching.lock() {
                            active.remove(&key);
                        }
                        return;
                    }
                };
                if let Ok(mut active) = self.enriching.lock() {
                    active.remove(&key);
                }
                span.in_scope(|| match result {
                    Some(Ok(true)) => tracing::info!("repository enrichment published"),
                    Some(Ok(false)) => tracing::info!("stale repository enrichment discarded"),
                    Some(Err(error)) => {
                        tracing::error!(%error, "repository enrichment failed")
                    }
                    None => tracing::info!("superseded repository enrichment cancelled"),
                });
            }
        }
    }

    async fn enrich(&self, store: &Arc<SemanticStore>, job: EnrichmentJob) -> Result<bool, String> {
        let contribution = self
            .indexer
            .enrich(job.snapshot, &job.analyzer.id)
            .await
            .map_err(|error| error.to_string())?;
        if contribution.metadata.id != job.analyzer.id
            || contribution.metadata.version != job.analyzer.version
        {
            return Err(
                "worker contribution metadata does not match the scheduled analyzer".into(),
            );
        }
        if contribution_escapes_target(&contribution, &job.repository) {
            return Err("worker contribution escaped its target repository".into());
        }
        let mut diagnostics = contribution.diagnostics;
        let mut entities = Vec::new();
        let mut observations = Vec::new();
        for repository in contribution.repositories {
            entities.extend(repository.entities);
            observations.extend(repository.observations);
            diagnostics.extend(
                repository
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| (repository.repository.clone(), diagnostic)),
            );
        }
        pipeline::report_analysis_diagnostics(&job.view.name, &diagnostics);
        let expected_version = format!("pending:{}", job.analyzer.version);
        let store = Arc::clone(store);
        tokio::task::spawn_blocking(move || {
            store
                .publish_enrichment(
                    &job.view,
                    &job.repository,
                    &job.input_fingerprint,
                    EnrichmentOwner {
                        analyzer: &job.analyzer.id,
                        version: &job.analyzer.version,
                        expected_version: Some(&expected_version),
                    },
                    EnrichmentPayload {
                        entities: &entities,
                        observations: &observations,
                        overrides: &contribution.overrides,
                        diagnostics: &diagnostics,
                    },
                )
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(super) fn queue_enrichments(
        &self,
        store: &SemanticStore,
        snapshot: &WorkspaceSnapshot,
        view: &WorkspaceView,
    ) -> Result<(), Box<dyn Error>> {
        if !store.ensure_revision_inputs(view)? {
            return Ok(());
        }
        let mut queued = false;
        for repository in &snapshot.repositories {
            let repository_id = repository.state.repository.identity.clone();
            for analyzer in self.indexer.enrichment_catalog() {
                let input_fingerprint = store
                    .revision_enrichment_input_fingerprint(
                        &view.name,
                        &repository_id,
                        &analyzer.id,
                    )?
                    .ok_or("published repository enrichment input fingerprint is missing")?;
                let contexts =
                    store.repository_contexts(&view.name, &repository_id, &analyzer.id)?;
                if store.enrichment_matches(
                    &view.name,
                    &repository_id,
                    &analyzer.id,
                    &analyzer.version,
                )? {
                    continue;
                }
                if self.indexer.enricher_is_active(&analyzer.id, repository) {
                    let pending_version = format!("pending:{}", analyzer.version);
                    if !store.enrichment_matches(
                        &view.name,
                        &repository_id,
                        &analyzer.id,
                        &pending_version,
                    )? {
                        store.publish_enrichment(
                            view,
                            &repository_id,
                            &input_fingerprint,
                            EnrichmentOwner {
                                analyzer: &analyzer.id,
                                version: &pending_version,
                                expected_version: None,
                            },
                            EnrichmentPayload::default(),
                        )?;
                        tracing::info!(
                            workspace = %view.name,
                            repository = %repository_id,
                            analyzer = %analyzer.id,
                            reason = "input_or_version_changed",
                            "repository enrichment invalidated"
                        );
                    }
                    let key = (
                        view.name.clone(),
                        repository_id.clone(),
                        analyzer.id.clone(),
                    );
                    {
                        let active = self
                            .enriching
                            .lock()
                            .map_err(|_| "active enrichment lock poisoned")?;
                        if let Some(run) = active.get(&key) {
                            if run.matches(&input_fingerprint, &analyzer.version) {
                                continue;
                            }
                            let _ = run.cancel.send(());
                        }
                    }
                    self.enrichment_jobs
                        .lock()
                        .map_err(|_| "enrichment queue lock poisoned")?
                        .insert(
                            key,
                            EnrichmentJob {
                                analyzer: analyzer.clone(),
                                repository: repository_id.clone(),
                                input_fingerprint: input_fingerprint.clone(),
                                queued_at: Instant::now(),
                                snapshot: EnrichmentSnapshot {
                                    target_repository: repository_id.clone(),
                                    workspace: WorkspaceSnapshot {
                                        name: snapshot.name.clone(),
                                        repositories: std::iter::once(repository.clone())
                                            .chain(contexts.iter().filter_map(|identity| {
                                                snapshot
                                                    .repositories
                                                    .iter()
                                                    .find(|candidate| {
                                                        candidate.state.repository.identity
                                                            == *identity
                                                    })
                                                    .cloned()
                                            }))
                                            .collect(),
                                    },
                                },
                                view: view.clone(),
                            },
                        );
                    queued = true;
                } else {
                    store.publish_enrichment(
                        view,
                        &repository_id,
                        &input_fingerprint,
                        EnrichmentOwner {
                            analyzer: &analyzer.id,
                            version: &analyzer.version,
                            expected_version: None,
                        },
                        EnrichmentPayload::default(),
                    )?;
                }
            }
        }
        if queued {
            self.enrichment_changed.notify_one();
        }
        Ok(())
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

fn enrichment_key(job: &EnrichmentJob) -> EnrichmentKey {
    (
        job.view.name.clone(),
        job.repository.clone(),
        job.analyzer.id.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind, Provenance,
    };
    use beholder_indexing::{AnalysisCompleteness, CacheStatistics, RepositoryContribution};

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
}
