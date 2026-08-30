use beholder_adapters_mnestic::{
    EnrichmentOwner as MnesticEnrichmentOwner, EnrichmentPayload as MnesticEnrichmentPayload,
    EnrichmentPublishOutcome, SemanticStore,
};
use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityKind, Observation, SemanticRelation,
};
use beholder_indexing::SemanticSnapshot;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

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
    pub(crate) revision_inputs: BTreeMap<String, String>,
}

#[derive(Default)]
pub(crate) struct EnrichmentContribution<'a> {
    pub(crate) entities: &'a [EntityFact],
    pub(crate) observations: &'a [Observation],
    pub(crate) overrides: &'a [DependencyOverride],
    pub(crate) diagnostics: &'a [(String, AnalysisDiagnostic)],
    pub(crate) diagnostic_replacements: &'a [(String, String)],
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
    ) -> Result<EnrichmentPublishOutcome, Box<dyn Error>>;
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
        let (entities, observations, candidates) = self.selected_baseline_semantics(
            target.view,
            target.repository,
            read.entity_kinds,
            read.relations,
        )?;
        let mut entities = entities
            .into_iter()
            .map(|entity| (entity.id.clone(), entity))
            .collect::<BTreeMap<_, _>>();
        for context in &contexts {
            let (context_entities, _, _) = self.selected_baseline_semantics(
                target.view,
                context,
                read.entity_kinds,
                &BTreeSet::new(),
            )?;
            entities.extend(
                context_entities
                    .into_iter()
                    .map(|entity| (entity.id.clone(), entity)),
            );
        }
        Ok(Some(EnrichmentSnapshotState {
            contexts,
            revision_inputs: self.revision_input_fingerprints(target.view)?,
            baseline: SemanticSnapshot {
                entities: entities.into_values().collect(),
                observations,
                candidates,
            },
        }))
    }

    fn publish_enrichment(
        &self,
        request: EnrichmentPublicationRequest<'_>,
    ) -> Result<EnrichmentPublishOutcome, Box<dyn Error>> {
        let target = request.target;
        SemanticStore::publish_enrichment_outcome(
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
                diagnostic_replacements: request.contribution.diagnostic_replacements,
            },
        )
    }
}
