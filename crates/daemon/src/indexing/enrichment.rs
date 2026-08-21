use super::{IndexScheduler, pipeline};
use beholder_adapters_mnestic::{EnrichmentPayload, SemanticStore};
use beholder_domain::WorkspaceView;
use beholder_indexing::{AnalyzerMetadata, WorkspaceSnapshot};
use std::{collections::BTreeMap, error::Error, sync::Arc};

#[derive(Clone)]
pub(super) struct EnrichmentJob {
    pub(super) analyzer: AnalyzerMetadata,
    pub(super) snapshot: WorkspaceSnapshot,
    pub(super) view: WorkspaceView,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct EnrichmentRun {
    pub(super) fingerprint: String,
    pub(super) version: String,
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
                let key = (job.view.name.clone(), job.analyzer.id.clone());
                if let Ok(mut active) = self.enriching.lock() {
                    active.insert(
                        key.clone(),
                        EnrichmentRun {
                            fingerprint: job.view.fingerprint(),
                            version: job.analyzer.version.clone(),
                        },
                    );
                }
                let scheduler = self.clone();
                let store = store.clone();
                let workspace = job.view.name.clone();
                let analyzer = job.analyzer.id.clone();
                let result = tokio::select! {
                    result = scheduler.enrich(&store, job) => result,
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
                match result {
                    Ok(true) => {
                        tracing::info!(workspace, analyzer, "workspace enrichment published")
                    }
                    Ok(false) => {
                        tracing::info!(workspace, analyzer, "stale workspace enrichment discarded")
                    }
                    Err(error) => {
                        tracing::error!(workspace, analyzer, %error, "workspace enrichment failed")
                    }
                }
            }
        }
    }

    async fn enrich(&self, store: &Arc<SemanticStore>, job: EnrichmentJob) -> Result<bool, String> {
        let contribution = self
            .indexer
            .enrich(job.snapshot, &job.analyzer.id)
            .await
            .map_err(|error| error.to_string())?;
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
        let store = Arc::clone(store);
        tokio::task::spawn_blocking(move || {
            store
                .publish_enrichment(
                    &job.view,
                    &job.analyzer.id,
                    &job.analyzer.version,
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

    pub(super) fn enrichments_current(
        &self,
        store: &SemanticStore,
        workspace: &str,
    ) -> Result<bool, Box<dyn Error>> {
        self.indexer
            .enrichment_catalog()
            .into_iter()
            .map(|analyzer| store.enrichment_matches(workspace, &analyzer.id, &analyzer.version))
            .try_fold(true, |current, matches| Ok(current && matches?))
    }

    pub(super) fn queue_enrichments(
        &self,
        store: &SemanticStore,
        snapshot: &WorkspaceSnapshot,
        view: &WorkspaceView,
    ) -> Result<(), Box<dyn Error>> {
        let active = self
            .indexer
            .active_enrichers(snapshot)
            .into_iter()
            .map(|analyzer| (analyzer.id.clone(), analyzer))
            .collect::<BTreeMap<_, _>>();
        let mut queued = false;
        for analyzer in self.indexer.enrichment_catalog() {
            if store.enrichment_matches(&view.name, &analyzer.id, &analyzer.version)? {
                continue;
            }
            if let Some(analyzer) = active.get(&analyzer.id) {
                let key = (view.name.clone(), analyzer.id.clone());
                let run = EnrichmentRun {
                    fingerprint: view.fingerprint(),
                    version: analyzer.version.clone(),
                };
                let active_runs = self
                    .enriching
                    .lock()
                    .map_err(|_| "active enrichment lock poisoned")?;
                if active_runs.get(&key) == Some(&run)
                    || store.enrichment_matches(&view.name, &analyzer.id, &analyzer.version)?
                {
                    continue;
                }
                self.enrichment_jobs
                    .lock()
                    .map_err(|_| "enrichment queue lock poisoned")?
                    .insert(
                        key,
                        EnrichmentJob {
                            analyzer: analyzer.clone(),
                            snapshot: snapshot.clone(),
                            view: view.clone(),
                        },
                    );
                queued = true;
            } else {
                store.publish_enrichment(
                    view,
                    &analyzer.id,
                    &analyzer.version,
                    EnrichmentPayload::default(),
                )?;
            }
        }
        if queued {
            self.enrichment_changed.notify_one();
        }
        Ok(())
    }
}
