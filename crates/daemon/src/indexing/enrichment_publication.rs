use beholder_adapters_mnestic::{
    EnrichmentOwner as MnesticEnrichmentOwner, EnrichmentPayload as MnesticEnrichmentPayload,
    SemanticStore,
};
use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityKind, Observation, SemanticRelation,
};
use beholder_indexing::SemanticSnapshot;
use std::{collections::BTreeSet, error::Error};

#[derive(Clone, Copy)]
pub(crate) struct EnrichmentTarget<'a> {
    pub(crate) view: &'a str,
    pub(crate) repository: &'a str,
    pub(crate) analyzer: &'a str,
    pub(crate) version: &'a str,
}

pub(crate) struct EnrichmentSnapshotRead<'a> {
    pub(crate) target: EnrichmentTarget<'a>,
    pub(crate) input_fingerprint: &'a str,
    pub(crate) entity_kinds: &'a BTreeSet<EntityKind>,
    pub(crate) relations: &'a BTreeSet<SemanticRelation>,
}

pub(crate) struct EnrichmentSnapshotState {
    pub(crate) contexts: Vec<String>,
    pub(crate) baseline: SemanticSnapshot,
}

#[derive(Default)]
pub(crate) struct EnrichmentContribution<'a> {
    pub(crate) entities: &'a [EntityFact],
    pub(crate) observations: &'a [Observation],
    pub(crate) overrides: &'a [DependencyOverride],
    pub(crate) diagnostics: &'a [(String, AnalysisDiagnostic)],
}

pub(crate) struct EnrichmentPublicationRequest<'a> {
    pub(crate) target: EnrichmentTarget<'a>,
    pub(crate) input_fingerprint: &'a str,
    pub(crate) contribution: EnrichmentContribution<'a>,
}

pub(crate) trait EnrichmentPublication {
    fn enrichment_is_current(&self, target: EnrichmentTarget<'_>) -> Result<bool, Box<dyn Error>>;

    fn enrichment_snapshot(
        &self,
        read: EnrichmentSnapshotRead<'_>,
    ) -> Result<Option<EnrichmentSnapshotState>, Box<dyn Error>>;

    fn publish_enrichment(
        &self,
        request: EnrichmentPublicationRequest<'_>,
    ) -> Result<bool, Box<dyn Error>>;
}

impl EnrichmentPublication for SemanticStore {
    fn enrichment_is_current(&self, target: EnrichmentTarget<'_>) -> Result<bool, Box<dyn Error>> {
        self.enrichment_matches(
            target.view,
            target.repository,
            target.analyzer,
            target.version,
        )
    }

    fn enrichment_snapshot(
        &self,
        read: EnrichmentSnapshotRead<'_>,
    ) -> Result<Option<EnrichmentSnapshotState>, Box<dyn Error>> {
        let target = read.target;
        if self
            .revision_enrichment_input_fingerprint(target.view, target.repository, target.analyzer)?
            .as_deref()
            != Some(read.input_fingerprint)
        {
            return Ok(None);
        }
        let contexts = self.repository_contexts(target.view, target.repository, target.analyzer)?;
        let (entities, observations) = self.selected_baseline_semantics(
            target.view,
            target.repository,
            read.entity_kinds,
            read.relations,
        )?;
        Ok(Some(EnrichmentSnapshotState {
            contexts,
            baseline: SemanticSnapshot {
                entities,
                observations,
            },
        }))
    }

    fn publish_enrichment(
        &self,
        request: EnrichmentPublicationRequest<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        let target = request.target;
        SemanticStore::publish_enrichment(
            self,
            target.view,
            target.repository,
            request.input_fingerprint,
            MnesticEnrichmentOwner {
                analyzer: target.analyzer,
                version: target.version,
            },
            MnesticEnrichmentPayload {
                entities: request.contribution.entities,
                observations: request.contribution.observations,
                overrides: request.contribution.overrides,
                diagnostics: request.contribution.diagnostics,
            },
        )
    }
}
