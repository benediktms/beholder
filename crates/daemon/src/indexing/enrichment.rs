use super::{IndexScheduler, pipeline};
use beholder_adapters_mnestic::{EnrichmentPayload, SemanticStore};
use beholder_domain::WorkspaceView;
use beholder_indexing::{AnalyzerMetadata, WorkspaceSnapshot};
use std::{error::Error, sync::Arc, time::Instant};
use tokio::sync::watch;

type EnrichmentKey = (String, String, String);

#[derive(Clone)]
pub(super) struct EnrichmentJob {
    pub(super) analyzer: AnalyzerMetadata,
    pub(super) repository: String,
    pub(super) input_fingerprint: String,
    pub(super) queued_at: Instant,
    pub(super) snapshot: WorkspaceSnapshot,
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
                tracing::info!(
                    workspace,
                    repository,
                    analyzer,
                    queue_ms = job.queued_at.elapsed().as_secs_f64() * 1_000.0,
                    "repository enrichment started"
                );
                let result = tokio::select! {
                    result = scheduler.enrich(&store, job) => Some(result),
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
                match result {
                    Some(Ok(true)) => tracing::info!(
                        workspace,
                        repository,
                        analyzer,
                        "repository enrichment published"
                    ),
                    Some(Ok(false)) => tracing::info!(
                        workspace,
                        repository,
                        analyzer,
                        "stale repository enrichment discarded"
                    ),
                    Some(Err(error)) => tracing::error!(
                        workspace,
                        repository,
                        analyzer,
                        %error,
                        "repository enrichment failed"
                    ),
                    None => tracing::info!(
                        workspace,
                        repository,
                        analyzer,
                        "superseded repository enrichment cancelled"
                    ),
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
        if contribution.metadata.id != job.analyzer.id
            || contribution.metadata.version != job.analyzer.version
        {
            return Err(
                "worker contribution metadata does not match the scheduled analyzer".into(),
            );
        }
        if contribution
            .repositories
            .iter()
            .any(|repository| repository.repository != job.repository)
            || contribution
                .active_repositories
                .iter()
                .any(|repository| repository != &job.repository)
            || contribution
                .diagnostics
                .iter()
                .any(|(repository, _)| repository != &job.repository)
        {
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
                    &job.analyzer.id,
                    &job.analyzer.version,
                    Some(&expected_version),
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
        let catalog = self
            .indexer
            .enrichment_catalog()
            .into_iter()
            .map(|analyzer| (analyzer.id, analyzer.version))
            .collect::<Vec<_>>();
        store.enrichments_current(workspace, &catalog)
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
            let input_fingerprint = view.repository_input_fingerprint(&repository.state);
            for analyzer in self.indexer.enrichment_catalog() {
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
                            &analyzer.id,
                            &pending_version,
                            None,
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
                                snapshot: WorkspaceSnapshot {
                                    name: snapshot.name.clone(),
                                    repositories: vec![repository.clone()],
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
                        &analyzer.id,
                        &analyzer.version,
                        None,
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

fn enrichment_key(job: &EnrichmentJob) -> EnrichmentKey {
    (
        job.view.name.clone(),
        job.repository.clone(),
        job.analyzer.id.clone(),
    )
}
