use beholder_adapters_mnestic::{
    EnrichmentOwner as MnesticEnrichmentOwner, EnrichmentPayload as MnesticEnrichmentPayload,
    EnrichmentPublishOutcome, SemanticStore,
};
use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityKind, FactShard, Observation,
    SemanticRelation,
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
    pub(crate) fact_shards: &'a [FactShard],
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
        let (entities, mut observations, candidates) = self.selected_baseline_semantics(
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
            let (context_entities, context_observations, _) = self.selected_baseline_semantics(
                target.view,
                context,
                read.entity_kinds,
                read.relations,
            )?;
            entities.extend(
                context_entities
                    .into_iter()
                    .map(|entity| (entity.id.clone(), entity)),
            );
            observations.extend(context_observations);
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
                fact_shards: request.contribution.fact_shards,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        EntityFact, LogicalRepository, RepositoryFacts, RepositoryState, StructuralRelation,
        WorkspaceView,
    };

    fn facts(state: RepositoryState, observations: Vec<Observation>) -> RepositoryFacts {
        let entities = observations
            .iter()
            .flat_map(|observation| [&observation.from, &observation.to])
            .map(|id| EntityFact::new(id.clone(), EntityKind::Callable, None).unwrap())
            .collect();
        RepositoryFacts {
            state,
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities,
            grpc_bindings: Vec::new(),
            observations,
        }
    }

    #[test]
    fn context_definitions_reach_enrichment_snapshot() {
        let store = SemanticStore::memory().unwrap();
        let target = RepositoryState {
            repository: LogicalRepository {
                identity: "example/target".into(),
            },
            head: None,
            fingerprint: "target".into(),
        };
        let context = RepositoryState {
            repository: LogicalRepository {
                identity: "example/context".into(),
            },
            head: None,
            fingerprint: "context".into(),
        };
        let view = WorkspaceView::new("main", "analysis", vec![target.clone(), context.clone()])
            .unwrap()
            .with_repository_contexts(BTreeMap::from([(
                "typescript".into(),
                BTreeMap::from([("example/target".into(), vec!["example/context".into()])]),
            )]))
            .unwrap();
        let definition = Observation::structural(
            "repo://example/context/typescript/service",
            StructuralRelation::Defines,
            "repo://example/context/typescript/service/selected",
            "src/service.ts:1",
        );
        store
            .publish(
                &view,
                &[
                    facts(target.clone(), Vec::new()),
                    facts(context, vec![definition.clone()]),
                ],
                &[],
            )
            .unwrap();
        let input = view.repository_enrichment_input_fingerprint(&target, "typescript");

        let snapshot = store
            .enrichment_snapshot(EnrichmentSnapshotRead {
                target: EnrichmentTarget {
                    view: "main",
                    repository: "example/target",
                    analyzer: "typescript",
                    version: "1",
                },
                input_fingerprint: &input,
                entity_kinds: &BTreeSet::new(),
                relations: &BTreeSet::from([SemanticRelation::Structural(
                    StructuralRelation::Defines,
                )]),
            })
            .unwrap()
            .unwrap();

        assert!(snapshot.baseline.observations.contains(&definition));
    }
}
