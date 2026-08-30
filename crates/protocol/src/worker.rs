use crate::{v1, worker_v1 as wire};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, CandidateOverride, Confidence,
    DependencyOverride, DependencyRelation, EntityFact, EntityKind, EntityMetadata, FactShard,
    GraphqlOperationKind, GraphqlTypeKind, GrpcBindingCandidate, GrpcBindingRole,
    LogicalRepository, Observation, ProtoTypeKind, Provenance, RepositoryState, RpcCardinality,
    SemanticCandidate, SemanticRelation, SourcePosition, SourceSpan, StructuralRelation,
};
use beholder_indexing::{
    AnalysisCompleteness, AnalysisInputKind, AnalyzerContribution, AnalyzerMetadata,
    CacheStatistics, EnrichmentSnapshot, GraphqlResolverCandidate, InputKind, PluginDescriptor,
    PluginInputScope, PluginInputSelector, PluginPathMatcher, RepositoryContribution,
    RepositoryInput, RepositorySnapshot, SemanticSnapshot, WorkspaceSnapshot,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

const CONTRIBUTION_CHUNK_ITEMS: usize = 2_048;

pub fn analyze_requests(snapshot: EnrichmentSnapshot) -> Result<Vec<wire::AnalyzeRequest>, String> {
    let target_repository = snapshot.target_repository;
    let baseline = snapshot.baseline;
    let snapshot = snapshot.workspace;
    let start = wire::AnalyzeRequest {
        request: Some(wire::analyze_request::Request::Start(wire::AnalysisStart {
            workspace: snapshot.name,
        })),
    };
    let repositories = snapshot
        .repositories
        .into_iter()
        .flat_map(move |repository| {
            let identity = repository.state.repository.identity;
            let start = wire::AnalyzeRequest {
                request: Some(wire::analyze_request::Request::Repository(
                    wire::RepositoryStart {
                        identity: identity.clone(),
                        base: repository.base.to_string_lossy().into_owned(),
                        head: repository.state.head,
                        fingerprint: repository.state.fingerprint,
                        target: identity == target_repository,
                    },
                )),
            };
            std::iter::once(start).chain(repository.inputs.into_iter().map(move |input| {
                wire::AnalyzeRequest {
                    request: Some(wire::analyze_request::Request::Input(
                        wire::RepositoryInput {
                            repository: identity.clone(),
                            path: input.path.to_string_lossy().into_owned(),
                            content: input.content.to_vec(),
                            content_hash: Sha256::digest(input.content.as_ref()).to_vec(),
                            kind: match input.kind {
                                InputKind::Source => wire::InputKind::Source as i32,
                                InputKind::ProtobufDescriptor => {
                                    wire::InputKind::ProtobufDescriptor as i32
                                }
                            },
                        },
                    )),
                }
            }))
        })
        .collect::<Vec<_>>();
    let baseline_entities = baseline
        .entities
        .into_iter()
        .map(|entity| {
            Ok(wire::AnalyzeRequest {
                request: Some(wire::analyze_request::Request::BaselineEntity(
                    wire::BaselineEntity {
                        entity: Some(entity_to_wire(entity)?),
                    },
                )),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let baseline_observations = baseline
        .observations
        .into_iter()
        .map(|observation| {
            Ok(wire::AnalyzeRequest {
                request: Some(wire::analyze_request::Request::BaselineObservation(
                    wire::BaselineObservation {
                        observation: Some(observation_to_wire(observation)?),
                    },
                )),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let baseline_candidates = baseline
        .candidates
        .into_iter()
        .map(|candidate| {
            Ok(wire::AnalyzeRequest {
                request: Some(wire::analyze_request::Request::BaselineCandidate(
                    wire::BaselineCandidate {
                        candidate: Some(candidate_to_wire(candidate)?),
                    },
                )),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let finish = wire::AnalyzeRequest {
        request: Some(wire::analyze_request::Request::Finish(
            wire::AnalysisFinish {},
        )),
    };
    Ok(std::iter::once(start)
        .chain(repositories)
        .chain(baseline_entities)
        .chain(baseline_observations)
        .chain(baseline_candidates)
        .chain(std::iter::once(finish))
        .collect())
}

pub fn workspace_snapshot(
    requests: impl IntoIterator<Item = wire::AnalyzeRequest>,
) -> Result<EnrichmentSnapshot, String> {
    let mut builder = WorkspaceSnapshotBuilder::default();
    for request in requests {
        builder.push(request)?;
    }
    builder.finish()
}

#[derive(Default)]
pub struct WorkspaceSnapshotBuilder {
    name: Option<String>,
    repositories: BTreeMap<String, RepositorySnapshot>,
    target_repository: Option<String>,
    baseline: SemanticSnapshot,
    finished: bool,
}

impl WorkspaceSnapshotBuilder {
    pub fn push(&mut self, request: wire::AnalyzeRequest) -> Result<(), String> {
        if self.finished {
            return Err("worker request followed analysis finish".into());
        }
        match request.request.ok_or("worker request is empty")? {
            wire::analyze_request::Request::Start(start) => {
                if self.name.replace(start.workspace).is_some() {
                    return Err("worker analysis started more than once".into());
                }
            }
            wire::analyze_request::Request::Repository(repository) => {
                if self.name.is_none() {
                    return Err("worker repository preceded analysis start".into());
                }
                let identity = repository.identity;
                if repository.target && self.target_repository.replace(identity.clone()).is_some() {
                    return Err("worker analysis identified more than one target repository".into());
                }
                if self
                    .repositories
                    .insert(
                        identity.clone(),
                        RepositorySnapshot {
                            base: PathBuf::from(repository.base),
                            state: RepositoryState {
                                repository: LogicalRepository { identity },
                                head: repository.head,
                                fingerprint: repository.fingerprint,
                            },
                            inputs: Vec::new(),
                        },
                    )
                    .is_some()
                {
                    return Err("worker repository appeared more than once".into());
                }
            }
            wire::analyze_request::Request::Input(input) => {
                let repository = self
                    .repositories
                    .get_mut(&input.repository)
                    .ok_or("worker input references an unknown repository")?;
                repository.inputs.push(RepositoryInput {
                    path: PathBuf::from(input.path),
                    content: Arc::from(input.content),
                    kind: match wire::InputKind::try_from(input.kind)
                        .map_err(|_| "worker input kind is unknown")?
                    {
                        wire::InputKind::Unspecified => {
                            return Err("worker input kind is missing".into());
                        }
                        wire::InputKind::Source => InputKind::Source,
                        wire::InputKind::ProtobufDescriptor => InputKind::ProtobufDescriptor,
                    },
                });
            }
            wire::analyze_request::Request::BaselineEntity(entity) => {
                self.baseline.entities.push(entity_from_wire(
                    entity.entity.ok_or("worker baseline entity is missing")?,
                )?);
            }
            wire::analyze_request::Request::BaselineObservation(observation) => {
                self.baseline.observations.push(observation_from_wire(
                    observation
                        .observation
                        .ok_or("worker baseline observation is missing")?,
                )?);
            }
            wire::analyze_request::Request::BaselineCandidate(candidate) => {
                self.baseline.candidates.push(candidate_from_wire(
                    candidate
                        .candidate
                        .ok_or("worker baseline candidate is missing")?,
                )?);
            }
            wire::analyze_request::Request::Finish(_) => self.finished = true,
        }
        Ok(())
    }

    pub fn finish(self) -> Result<EnrichmentSnapshot, String> {
        if !self.finished {
            return Err("worker request stream ended before analysis finish".into());
        }
        let target_repository = self
            .target_repository
            .ok_or("worker request stream omitted its target repository")?;
        let mut repositories = self.repositories;
        let target = repositories
            .remove(&target_repository)
            .ok_or("worker target repository is missing from its snapshot")?;
        Ok(EnrichmentSnapshot {
            target_repository,
            workspace: WorkspaceSnapshot {
                name: self
                    .name
                    .ok_or("worker request stream omitted analysis start")?,
                repositories: std::iter::once(target)
                    .chain(repositories.into_values())
                    .collect(),
            },
            baseline: self.baseline,
        })
    }
}

pub fn descriptor_to_wire(descriptor: PluginDescriptor) -> wire::PluginDescriptor {
    wire::PluginDescriptor {
        id: descriptor.id,
        api_version: descriptor.api_version,
        inputs: descriptor
            .inputs
            .into_iter()
            .map(selector_to_wire)
            .collect(),
        semantic_entities: descriptor
            .semantic_entities
            .into_iter()
            .map(plugin_entity_kind_to_wire)
            .map(|kind| kind as i32)
            .collect(),
        semantic_relations: descriptor
            .semantic_relations
            .into_iter()
            .map(relation_to_wire)
            .map(|kind| kind as i32)
            .collect(),
        produces_entities: descriptor
            .produces_entities
            .into_iter()
            .map(plugin_entity_kind_to_wire)
            .map(|kind| kind as i32)
            .collect(),
        produces_relations: descriptor
            .produces_relations
            .into_iter()
            .map(relation_to_wire)
            .map(|kind| kind as i32)
            .collect(),
    }
}

pub fn descriptor_from_wire(
    descriptor: wire::PluginDescriptor,
) -> Result<PluginDescriptor, String> {
    let descriptor = PluginDescriptor {
        id: descriptor.id,
        api_version: descriptor.api_version,
        inputs: descriptor
            .inputs
            .into_iter()
            .map(selector_from_wire)
            .collect::<Result<_, _>>()?,
        semantic_entities: descriptor
            .semantic_entities
            .into_iter()
            .map(plugin_entity_kind_from_wire)
            .collect::<Result<BTreeSet<_>, _>>()?,
        semantic_relations: descriptor
            .semantic_relations
            .into_iter()
            .map(relation_from_wire)
            .collect::<Result<BTreeSet<_>, _>>()?,
        produces_entities: descriptor
            .produces_entities
            .into_iter()
            .map(plugin_entity_kind_from_wire)
            .collect::<Result<BTreeSet<_>, _>>()?,
        produces_relations: descriptor
            .produces_relations
            .into_iter()
            .map(relation_from_wire)
            .collect::<Result<BTreeSet<_>, _>>()?,
    };
    descriptor.validate()?;
    Ok(descriptor)
}

fn selector_to_wire(selector: PluginInputSelector) -> wire::PluginInputSelector {
    use wire::plugin_input_selector::Matcher;
    wire::PluginInputSelector {
        scope: match selector.scope {
            PluginInputScope::Target => wire::PluginInputScope::Target,
            PluginInputScope::Context => wire::PluginInputScope::Context,
        } as i32,
        kind: match selector.kind {
            AnalysisInputKind::Source => wire::PluginInputKind::Source,
            AnalysisInputKind::Configuration => wire::PluginInputKind::Configuration,
            AnalysisInputKind::Dependency => wire::PluginInputKind::Dependency,
            AnalysisInputKind::Toolchain => wire::PluginInputKind::Toolchain,
            AnalysisInputKind::Environment => wire::PluginInputKind::Environment,
        } as i32,
        matcher: Some(match selector.matcher {
            PluginPathMatcher::Extension(value) => Matcher::Extension(value),
            PluginPathMatcher::FileName(value) => Matcher::FileName(value),
            PluginPathMatcher::PathSuffix(value) => {
                Matcher::PathSuffix(value.to_string_lossy().into_owned())
            }
        }),
    }
}

fn selector_from_wire(selector: wire::PluginInputSelector) -> Result<PluginInputSelector, String> {
    use wire::plugin_input_selector::Matcher;
    Ok(PluginInputSelector {
        scope: match wire::PluginInputScope::try_from(selector.scope)
            .map_err(|_| "plugin input scope is unknown")?
        {
            wire::PluginInputScope::Unspecified => {
                return Err("plugin input scope is missing".into());
            }
            wire::PluginInputScope::Target => PluginInputScope::Target,
            wire::PluginInputScope::Context => PluginInputScope::Context,
        },
        kind: match wire::PluginInputKind::try_from(selector.kind)
            .map_err(|_| "plugin input kind is unknown")?
        {
            wire::PluginInputKind::Unspecified => {
                return Err("plugin input kind is missing".into());
            }
            wire::PluginInputKind::Source => AnalysisInputKind::Source,
            wire::PluginInputKind::Configuration => AnalysisInputKind::Configuration,
            wire::PluginInputKind::Dependency => AnalysisInputKind::Dependency,
            wire::PluginInputKind::Toolchain => AnalysisInputKind::Toolchain,
            wire::PluginInputKind::Environment => AnalysisInputKind::Environment,
        },
        matcher: match selector.matcher.ok_or("plugin input matcher is missing")? {
            Matcher::Extension(value) => PluginPathMatcher::Extension(value),
            Matcher::FileName(value) => PluginPathMatcher::FileName(value),
            Matcher::PathSuffix(value) => PluginPathMatcher::PathSuffix(value.into()),
        },
    })
}

fn plugin_entity_kind_to_wire(kind: EntityKind) -> wire::PluginEntityKind {
    match kind {
        EntityKind::Callable => wire::PluginEntityKind::Callable,
        EntityKind::GraphqlArgument => wire::PluginEntityKind::GraphqlArgument,
        EntityKind::GraphqlEnumValue => wire::PluginEntityKind::GraphqlEnumValue,
        EntityKind::GraphqlField => wire::PluginEntityKind::GraphqlField,
        EntityKind::GraphqlOperation => wire::PluginEntityKind::GraphqlOperation,
        EntityKind::GraphqlType => wire::PluginEntityKind::GraphqlType,
        EntityKind::GrpcOperation => wire::PluginEntityKind::GrpcOperation,
        EntityKind::KafkaTopic => wire::PluginEntityKind::KafkaTopic,
        EntityKind::Namespace => wire::PluginEntityKind::Namespace,
        EntityKind::ProtoField => wire::PluginEntityKind::ProtoField,
        EntityKind::ProtoMethod => wire::PluginEntityKind::ProtoMethod,
        EntityKind::ProtoService => wire::PluginEntityKind::ProtoService,
        EntityKind::ProtoType => wire::PluginEntityKind::ProtoType,
        EntityKind::Service => wire::PluginEntityKind::Service,
        EntityKind::UnityPrefab => wire::PluginEntityKind::UnityPrefab,
    }
}

fn plugin_entity_kind_from_wire(kind: i32) -> Result<EntityKind, String> {
    Ok(
        match wire::PluginEntityKind::try_from(kind).map_err(|_| "plugin entity kind is unknown")? {
            wire::PluginEntityKind::Unspecified => {
                return Err("plugin entity kind is missing".into());
            }
            wire::PluginEntityKind::Callable => EntityKind::Callable,
            wire::PluginEntityKind::GraphqlArgument => EntityKind::GraphqlArgument,
            wire::PluginEntityKind::GraphqlEnumValue => EntityKind::GraphqlEnumValue,
            wire::PluginEntityKind::GraphqlField => EntityKind::GraphqlField,
            wire::PluginEntityKind::GraphqlOperation => EntityKind::GraphqlOperation,
            wire::PluginEntityKind::GraphqlType => EntityKind::GraphqlType,
            wire::PluginEntityKind::GrpcOperation => EntityKind::GrpcOperation,
            wire::PluginEntityKind::KafkaTopic => EntityKind::KafkaTopic,
            wire::PluginEntityKind::Namespace => EntityKind::Namespace,
            wire::PluginEntityKind::ProtoField => EntityKind::ProtoField,
            wire::PluginEntityKind::ProtoMethod => EntityKind::ProtoMethod,
            wire::PluginEntityKind::ProtoService => EntityKind::ProtoService,
            wire::PluginEntityKind::ProtoType => EntityKind::ProtoType,
            wire::PluginEntityKind::Service => EntityKind::Service,
            wire::PluginEntityKind::UnityPrefab => EntityKind::UnityPrefab,
        },
    )
}

pub fn analyze_events(
    contribution: AnalyzerContribution,
) -> Result<Vec<wire::AnalyzeEvent>, String> {
    let AnalyzerContribution {
        metadata,
        active_repositories,
        repositories,
        overrides,
        candidate_overrides,
        graphql_resolvers,
        diagnostics,
        cache,
    } = contribution;
    let mut events = Vec::new();
    for repository in repositories {
        events.extend(repository_events(repository)?);
    }
    events.extend(analysis_contribution_events(
        overrides,
        candidate_overrides,
        graphql_resolvers,
        diagnostics,
    )?);
    events.push(wire::AnalyzeEvent {
        event: Some(wire::analyze_event::Event::Completed(
            wire::AnalysisCompleted {
                metadata: Some(wire::AnalyzerMetadata {
                    id: metadata.id,
                    version: metadata.version,
                }),
                active_repositories,
                cache: Some(wire::CacheStatistics {
                    memory_hits: cache
                        .memory_hits
                        .try_into()
                        .map_err(|_| "cache hit overflow")?,
                    disk_hits: cache
                        .disk_hits
                        .try_into()
                        .map_err(|_| "cache hit overflow")?,
                    misses: cache.misses.try_into().map_err(|_| "cache miss overflow")?,
                }),
            },
        )),
    });
    Ok(events)
}

#[derive(Default)]
pub struct ContributionAccumulator {
    repositories: Vec<RepositoryContribution>,
    overrides: Vec<DependencyOverride>,
    candidate_overrides: Vec<CandidateOverride>,
    graphql_resolvers: Vec<GraphqlResolverCandidate>,
    diagnostics: Vec<(String, AnalysisDiagnostic)>,
    completed: Option<wire::AnalysisCompleted>,
}

impl ContributionAccumulator {
    pub fn push(&mut self, event: wire::AnalyzeEvent) -> Result<(), String> {
        if self.completed.is_some() {
            return Err("worker event followed analysis completion".into());
        }
        match event.event.ok_or("worker event is empty")? {
            wire::analyze_event::Event::Progress(progress) => {
                match wire::AnalysisPhase::try_from(progress.phase)
                    .map_err(|_| "worker progress phase is unknown")?
                {
                    wire::AnalysisPhase::Unspecified => {
                        return Err("worker progress phase is missing".into());
                    }
                    wire::AnalysisPhase::ReceivingSnapshot | wire::AnalysisPhase::Analyzing => {}
                }
            }
            wire::analyze_event::Event::Repository(repository) => {
                let mut chunk = repository_from_wire(repository)?;
                if let Some(repository) = self
                    .repositories
                    .iter_mut()
                    .find(|repository| repository.repository == chunk.repository)
                {
                    if repository.completeness != chunk.completeness {
                        return Err("worker repository chunks disagree on completeness".into());
                    }
                    repository.entities.append(&mut chunk.entities);
                    repository.grpc_bindings.append(&mut chunk.grpc_bindings);
                    repository.observations.append(&mut chunk.observations);
                    repository.diagnostics.append(&mut chunk.diagnostics);
                    repository
                        .replaced_diagnostic_codes
                        .append(&mut chunk.replaced_diagnostic_codes);
                    for mut shard in chunk.fact_shards {
                        if let Some(existing) = repository.fact_shards.iter_mut().find(|existing| {
                            existing.repository == shard.repository
                                && existing.producer == shard.producer
                                && existing.owner == shard.owner
                        }) {
                            if existing.version != shard.version {
                                return Err("worker fact shard chunks disagree on version".into());
                            }
                            existing.entities.append(&mut shard.entities);
                            existing.observations.append(&mut shard.observations);
                        } else {
                            repository.fact_shards.push(shard);
                        }
                    }
                } else {
                    self.repositories.push(chunk);
                }
            }
            wire::analyze_event::Event::Completed(value) => {
                if self.completed.replace(value).is_some() {
                    return Err("worker completed more than once".into());
                }
            }
            wire::analyze_event::Event::Failure(failure) => {
                return Err(format!("{}: {}", failure.code, failure.message));
            }
            wire::analyze_event::Event::Contribution(contribution) => {
                for r#override in contribution.overrides {
                    self.overrides.push(override_from_wire(r#override)?);
                }
                self.candidate_overrides
                    .extend(
                        contribution
                            .candidate_overrides
                            .into_iter()
                            .map(|override_| CandidateOverride {
                                candidate_id: override_.candidate_id,
                                resolved_to: override_.resolved_to.into(),
                                evidence: override_.evidence.into(),
                            }),
                    );
                self.graphql_resolvers
                    .extend(contribution.graphql_resolvers.into_iter().map(|resolver| {
                        GraphqlResolverCandidate {
                            repository: resolver.repository,
                            field: resolver.field,
                            parent: resolver.parent,
                            resolver: resolver.resolver.into(),
                            evidence: resolver.evidence.into(),
                        }
                    }));
                for diagnostic in contribution.diagnostics {
                    self.diagnostics.push((
                        diagnostic.repository,
                        diagnostic_from_wire(
                            diagnostic
                                .diagnostic
                                .ok_or("worker diagnostic is missing")?,
                        )?,
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<AnalyzerContribution, String> {
        let completed = self
            .completed
            .ok_or("worker stream ended without completion")?;
        let metadata = completed.metadata.ok_or("worker metadata is missing")?;
        let cache = completed
            .cache
            .ok_or("worker cache statistics are missing")?;
        Ok(AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: metadata.id,
                version: metadata.version,
            },
            active_repositories: completed.active_repositories,
            repositories: self.repositories,
            overrides: self.overrides,
            candidate_overrides: self.candidate_overrides,
            graphql_resolvers: self.graphql_resolvers,
            diagnostics: self.diagnostics,
            cache: CacheStatistics {
                memory_hits: cache
                    .memory_hits
                    .try_into()
                    .map_err(|_| "cache hit overflow")?,
                disk_hits: cache
                    .disk_hits
                    .try_into()
                    .map_err(|_| "cache hit overflow")?,
                misses: cache.misses.try_into().map_err(|_| "cache miss overflow")?,
            },
        })
    }
}

pub fn contribution_from_events(
    events: impl IntoIterator<Item = wire::AnalyzeEvent>,
) -> Result<AnalyzerContribution, String> {
    let mut accumulator = ContributionAccumulator::default();
    for event in events {
        accumulator.push(event)?;
    }
    accumulator.finish()
}

fn analysis_contribution_events(
    overrides: Vec<DependencyOverride>,
    candidate_overrides: Vec<CandidateOverride>,
    graphql_resolvers: Vec<GraphqlResolverCandidate>,
    diagnostics: Vec<(String, AnalysisDiagnostic)>,
) -> Result<Vec<wire::AnalyzeEvent>, String> {
    let mut overrides = overrides
        .into_iter()
        .map(override_to_wire)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .peekable();
    let mut candidate_overrides = candidate_overrides.into_iter().peekable();
    let mut graphql_resolvers = graphql_resolvers
        .into_iter()
        .map(|resolver| wire::GraphqlResolverCandidate {
            repository: resolver.repository,
            field: resolver.field,
            parent: resolver.parent,
            resolver: resolver.resolver.to_string(),
            evidence: resolver.evidence.as_str().into(),
        })
        .peekable();
    let mut diagnostics = diagnostics
        .into_iter()
        .map(|(repository, diagnostic)| wire::RepositoryDiagnostic {
            repository,
            diagnostic: Some(diagnostic_to_wire(diagnostic)),
        })
        .peekable();
    let mut events = Vec::new();
    while overrides.peek().is_some()
        || candidate_overrides.peek().is_some()
        || graphql_resolvers.peek().is_some()
        || diagnostics.peek().is_some()
    {
        events.push(wire::AnalyzeEvent {
            event: Some(wire::analyze_event::Event::Contribution(
                wire::AnalysisContribution {
                    overrides: overrides.by_ref().take(CONTRIBUTION_CHUNK_ITEMS).collect(),
                    candidate_overrides: candidate_overrides
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .map(|override_| wire::CandidateOverride {
                            candidate_id: override_.candidate_id,
                            resolved_to: override_.resolved_to.to_string(),
                            evidence: override_.evidence.as_str().into(),
                        })
                        .collect(),
                    graphql_resolvers: graphql_resolvers
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                    diagnostics: diagnostics
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                },
            )),
        });
    }
    Ok(events)
}

fn repository_events(
    repository: RepositoryContribution,
) -> Result<Vec<wire::AnalyzeEvent>, String> {
    let repository = repository_to_wire(repository)?;
    let mut entities = repository.entities.into_iter().peekable();
    let mut grpc_bindings = repository.grpc_bindings.into_iter().peekable();
    let mut observations = repository.observations.into_iter().peekable();
    let mut diagnostics = repository.diagnostics.into_iter().peekable();
    let mut replaced_diagnostic_codes = repository.replaced_diagnostic_codes.into_iter().peekable();
    let mut fact_shards = repository.fact_shards.into_iter().peekable();
    let mut events = Vec::new();
    loop {
        events.push(wire::AnalyzeEvent {
            event: Some(wire::analyze_event::Event::Repository(
                wire::RepositoryContribution {
                    repository: repository.repository.clone(),
                    completeness: repository.completeness,
                    entities: entities.by_ref().take(CONTRIBUTION_CHUNK_ITEMS).collect(),
                    grpc_bindings: grpc_bindings
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                    observations: observations
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                    diagnostics: diagnostics
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                    replaced_diagnostic_codes: replaced_diagnostic_codes
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                    fact_shards: fact_shards
                        .by_ref()
                        .take(CONTRIBUTION_CHUNK_ITEMS)
                        .collect(),
                },
            )),
        });
        if entities.peek().is_none()
            && grpc_bindings.peek().is_none()
            && observations.peek().is_none()
            && diagnostics.peek().is_none()
            && replaced_diagnostic_codes.peek().is_none()
            && fact_shards.peek().is_none()
        {
            break;
        }
    }
    Ok(events)
}

fn repository_to_wire(
    repository: RepositoryContribution,
) -> Result<wire::RepositoryContribution, String> {
    Ok(wire::RepositoryContribution {
        repository: repository.repository,
        completeness: match repository.completeness {
            AnalysisCompleteness::Complete => wire::AnalysisCompleteness::Complete as i32,
            AnalysisCompleteness::Incomplete => wire::AnalysisCompleteness::Incomplete as i32,
        },
        entities: repository
            .entities
            .into_iter()
            .map(entity_to_wire)
            .collect::<Result<_, _>>()?,
        grpc_bindings: repository
            .grpc_bindings
            .into_iter()
            .map(binding_to_wire)
            .collect(),
        observations: repository
            .observations
            .into_iter()
            .map(observation_to_wire)
            .collect::<Result<_, _>>()?,
        diagnostics: repository
            .diagnostics
            .into_iter()
            .map(diagnostic_to_wire)
            .collect(),
        replaced_diagnostic_codes: repository.replaced_diagnostic_codes.into_iter().collect(),
        fact_shards: repository
            .fact_shards
            .into_iter()
            .map(fact_shard_to_wire)
            .collect::<Result<_, _>>()?,
    })
}

fn repository_from_wire(
    repository: wire::RepositoryContribution,
) -> Result<RepositoryContribution, String> {
    Ok(RepositoryContribution {
        repository: repository.repository,
        completeness: match wire::AnalysisCompleteness::try_from(repository.completeness)
            .map_err(|_| "worker analysis completeness is unknown")?
        {
            wire::AnalysisCompleteness::Unspecified => {
                return Err("worker analysis completeness is missing".into());
            }
            wire::AnalysisCompleteness::Complete => AnalysisCompleteness::Complete,
            wire::AnalysisCompleteness::Incomplete => AnalysisCompleteness::Incomplete,
        },
        entities: repository
            .entities
            .into_iter()
            .map(entity_from_wire)
            .collect::<Result<_, _>>()?,
        fact_shards: repository
            .fact_shards
            .into_iter()
            .map(fact_shard_from_wire)
            .collect::<Result<_, _>>()?,
        grpc_bindings: repository
            .grpc_bindings
            .into_iter()
            .map(binding_from_wire)
            .collect::<Result<_, _>>()?,
        observations: repository
            .observations
            .into_iter()
            .map(observation_from_wire)
            .collect::<Result<_, _>>()?,
        semantic_candidates: Vec::new(),
        diagnostics: repository
            .diagnostics
            .into_iter()
            .map(diagnostic_from_wire)
            .collect::<Result<_, _>>()?,
        replaced_diagnostic_codes: repository.replaced_diagnostic_codes.into_iter().collect(),
    })
}

fn candidate_to_wire(value: SemanticCandidate) -> Result<wire::SemanticCandidate, String> {
    Ok(wire::SemanticCandidate {
        id: value.id,
        repository: value.repository,
        from: value.from.to_string(),
        relation: relation_to_wire(SemanticRelation::Dependency(value.relation)) as i32,
        unresolved_to: value.unresolved_to.to_string(),
        span: Some(wire::SourceSpan {
            path: value.span.path.to_string_lossy().into_owned(),
            start: Some(wire::SourcePosition {
                line: value.span.start.line,
                character: value.span.start.character,
            }),
            end: Some(wire::SourcePosition {
                line: value.span.end.line,
                character: value.span.end.character,
            }),
        }),
        evidence: value.evidence.as_str().into(),
    })
}

fn candidate_from_wire(value: wire::SemanticCandidate) -> Result<SemanticCandidate, String> {
    let relation = relation_from_wire(value.relation)?
        .dependency()
        .ok_or("worker semantic candidate used a structural relation")?;
    let span = value
        .span
        .ok_or("worker semantic candidate span is missing")?;
    let start = span
        .start
        .ok_or("worker semantic candidate start is missing")?;
    let end = span.end.ok_or("worker semantic candidate end is missing")?;
    Ok(SemanticCandidate {
        id: value.id,
        repository: value.repository,
        from: value.from.into(),
        relation,
        unresolved_to: value.unresolved_to.into(),
        span: SourceSpan {
            path: span.path.into(),
            start: SourcePosition {
                line: start.line,
                character: start.character,
            },
            end: SourcePosition {
                line: end.line,
                character: end.character,
            },
        },
        evidence: value.evidence.into(),
    })
}

fn fact_shard_to_wire(shard: FactShard) -> Result<wire::FactShard, String> {
    Ok(wire::FactShard {
        repository: shard.repository,
        producer: shard.producer,
        owner: shard.owner.to_string(),
        version: shard.version,
        entities: shard
            .entities
            .into_iter()
            .map(entity_to_wire)
            .collect::<Result<_, _>>()?,
        observations: shard
            .observations
            .into_iter()
            .map(observation_to_wire)
            .collect::<Result<_, _>>()?,
    })
}

fn fact_shard_from_wire(shard: wire::FactShard) -> Result<FactShard, String> {
    Ok(FactShard {
        repository: shard.repository,
        producer: shard.producer,
        owner: shard.owner.into(),
        version: shard.version,
        entities: shard
            .entities
            .into_iter()
            .map(entity_from_wire)
            .collect::<Result<_, _>>()?,
        observations: shard
            .observations
            .into_iter()
            .map(observation_from_wire)
            .collect::<Result<_, _>>()?,
    })
}

fn entity_to_wire(entity: EntityFact) -> Result<wire::EntityFact, String> {
    let kind = match entity.kind {
        EntityKind::Callable => v1::EntityKind::Callable,
        EntityKind::GraphqlArgument => v1::EntityKind::GraphqlArgument,
        EntityKind::GraphqlEnumValue => v1::EntityKind::GraphqlEnumValue,
        EntityKind::GraphqlField => v1::EntityKind::GraphqlField,
        EntityKind::GraphqlOperation => v1::EntityKind::GraphqlOperation,
        EntityKind::GraphqlType => v1::EntityKind::GraphqlType,
        EntityKind::GrpcOperation | EntityKind::ProtoMethod => v1::EntityKind::Rpc,
        EntityKind::KafkaTopic => v1::EntityKind::KafkaTopic,
        EntityKind::Namespace => v1::EntityKind::Namespace,
        EntityKind::ProtoField => v1::EntityKind::ProtoField,
        EntityKind::ProtoService => v1::EntityKind::ProtoService,
        EntityKind::ProtoType => match entity.metadata {
            Some(EntityMetadata::ProtoType {
                kind: ProtoTypeKind::Enum,
            }) => v1::EntityKind::ProtoEnum,
            Some(EntityMetadata::ProtoType {
                kind: ProtoTypeKind::Message,
            }) => v1::EntityKind::ProtoMessage,
            _ => return Err("Protobuf type entity metadata is missing".into()),
        },
        EntityKind::Service => v1::EntityKind::Service,
        EntityKind::UnityPrefab => v1::EntityKind::UnityPrefab,
    };
    Ok(wire::EntityFact {
        id: entity.id.to_string(),
        kind: kind as i32,
        metadata: entity.metadata.map(metadata_to_wire),
    })
}

fn entity_from_wire(entity: wire::EntityFact) -> Result<EntityFact, String> {
    let metadata = entity.metadata.map(metadata_from_wire).transpose()?;
    let kind =
        match v1::EntityKind::try_from(entity.kind).map_err(|_| "worker entity kind is unknown")? {
            v1::EntityKind::Unspecified => return Err("worker entity kind is missing".into()),
            v1::EntityKind::Callable => EntityKind::Callable,
            v1::EntityKind::GraphqlArgument => EntityKind::GraphqlArgument,
            v1::EntityKind::GraphqlEnumValue => EntityKind::GraphqlEnumValue,
            v1::EntityKind::GraphqlField => EntityKind::GraphqlField,
            v1::EntityKind::GraphqlOperation => EntityKind::GraphqlOperation,
            v1::EntityKind::GraphqlType => EntityKind::GraphqlType,
            v1::EntityKind::KafkaTopic => EntityKind::KafkaTopic,
            v1::EntityKind::Namespace | v1::EntityKind::ProtoFile => EntityKind::Namespace,
            v1::EntityKind::ProtoEnum | v1::EntityKind::ProtoMessage => EntityKind::ProtoType,
            v1::EntityKind::ProtoField => EntityKind::ProtoField,
            v1::EntityKind::ProtoService => EntityKind::ProtoService,
            v1::EntityKind::Rpc if metadata.is_some() => EntityKind::ProtoMethod,
            v1::EntityKind::Rpc => EntityKind::GrpcOperation,
            v1::EntityKind::Service => EntityKind::Service,
            v1::EntityKind::UnityPrefab => EntityKind::UnityPrefab,
        };
    EntityFact::new(entity.id, kind, metadata).map_err(str::to_owned)
}

fn metadata_to_wire(metadata: EntityMetadata) -> v1::EntityMetadata {
    use v1::entity_metadata::Metadata;
    v1::EntityMetadata {
        metadata: Some(match metadata {
            EntityMetadata::GraphqlOperation { kind } => {
                Metadata::GraphqlOperationKind(match kind {
                    GraphqlOperationKind::Mutation => v1::GraphqlOperationKind::Mutation,
                    GraphqlOperationKind::Query => v1::GraphqlOperationKind::Query,
                    GraphqlOperationKind::Subscription => v1::GraphqlOperationKind::Subscription,
                } as i32)
            }
            EntityMetadata::GraphqlType { kind } => Metadata::GraphqlTypeKind(match kind {
                GraphqlTypeKind::Enum => v1::GraphqlTypeKind::Enum,
                GraphqlTypeKind::Input => v1::GraphqlTypeKind::Input,
                GraphqlTypeKind::Interface => v1::GraphqlTypeKind::Interface,
                GraphqlTypeKind::Object => v1::GraphqlTypeKind::Object,
                GraphqlTypeKind::Scalar => v1::GraphqlTypeKind::Scalar,
                GraphqlTypeKind::Union => v1::GraphqlTypeKind::Union,
            } as i32),
            EntityMetadata::ProtoMethod { cardinality } => {
                Metadata::RpcCardinality(cardinality_to_wire(cardinality) as i32)
            }
            EntityMetadata::ProtoType { kind } => Metadata::ProtoTypeKind(match kind {
                ProtoTypeKind::Enum => v1::ProtoTypeKind::Enum,
                ProtoTypeKind::Message => v1::ProtoTypeKind::Message,
            } as i32),
        }),
    }
}

fn metadata_from_wire(metadata: v1::EntityMetadata) -> Result<EntityMetadata, String> {
    use v1::entity_metadata::Metadata;
    match metadata.metadata.ok_or("worker entity metadata is empty")? {
        Metadata::GraphqlOperationKind(kind) => Ok(EntityMetadata::GraphqlOperation {
            kind: match v1::GraphqlOperationKind::try_from(kind)
                .map_err(|_| "worker GraphQL operation kind is unknown")?
            {
                v1::GraphqlOperationKind::Unspecified => {
                    return Err("worker GraphQL operation kind is missing".into());
                }
                v1::GraphqlOperationKind::Mutation => GraphqlOperationKind::Mutation,
                v1::GraphqlOperationKind::Query => GraphqlOperationKind::Query,
                v1::GraphqlOperationKind::Subscription => GraphqlOperationKind::Subscription,
            },
        }),
        Metadata::GraphqlTypeKind(kind) => Ok(EntityMetadata::GraphqlType {
            kind: match v1::GraphqlTypeKind::try_from(kind)
                .map_err(|_| "worker GraphQL type kind is unknown")?
            {
                v1::GraphqlTypeKind::Unspecified => {
                    return Err("worker GraphQL type kind is missing".into());
                }
                v1::GraphqlTypeKind::Enum => GraphqlTypeKind::Enum,
                v1::GraphqlTypeKind::Input => GraphqlTypeKind::Input,
                v1::GraphqlTypeKind::Interface => GraphqlTypeKind::Interface,
                v1::GraphqlTypeKind::Object => GraphqlTypeKind::Object,
                v1::GraphqlTypeKind::Scalar => GraphqlTypeKind::Scalar,
                v1::GraphqlTypeKind::Union => GraphqlTypeKind::Union,
            },
        }),
        Metadata::RpcCardinality(cardinality) => Ok(EntityMetadata::ProtoMethod {
            cardinality: cardinality_from_wire(cardinality)?,
        }),
        Metadata::ProtoTypeKind(kind) => Ok(EntityMetadata::ProtoType {
            kind: match v1::ProtoTypeKind::try_from(kind)
                .map_err(|_| "worker Protobuf type kind is unknown")?
            {
                v1::ProtoTypeKind::Unspecified => {
                    return Err("worker Protobuf type kind is missing".into());
                }
                v1::ProtoTypeKind::Enum => ProtoTypeKind::Enum,
                v1::ProtoTypeKind::Message => ProtoTypeKind::Message,
            },
        }),
    }
}

fn observation_to_wire(observation: Observation) -> Result<wire::Observation, String> {
    Ok(wire::Observation {
        from: observation.from.to_string(),
        relation: relation_to_wire(observation.relation) as i32,
        to: observation.to.to_string(),
        evidence: observation.evidence.as_str().into(),
        confidence: confidence_to_wire(observation.confidence) as i32,
        provenance: provenance_to_wire(observation.provenance) as i32,
    })
}

fn observation_from_wire(observation: wire::Observation) -> Result<Observation, String> {
    Ok(Observation {
        from: observation.from.into(),
        relation: relation_from_wire(observation.relation)?,
        to: observation.to.into(),
        evidence: observation.evidence.into(),
        confidence: confidence_from_wire(observation.confidence)?,
        provenance: provenance_from_wire(observation.provenance)?,
    })
}

fn binding_to_wire(binding: GrpcBindingCandidate) -> wire::GrpcBindingCandidate {
    wire::GrpcBindingCandidate {
        local_symbol: binding.local_symbol.to_string(),
        role: match binding.role {
            GrpcBindingRole::Client => wire::GrpcBindingRole::Client,
            GrpcBindingRole::Server => wire::GrpcBindingRole::Server,
        } as i32,
        service: binding.service,
        method: binding.method,
        cardinality: cardinality_to_wire(binding.cardinality) as i32,
        evidence: binding.evidence.as_str().into(),
        confidence: confidence_to_wire(binding.confidence) as i32,
        provenance: provenance_to_wire(binding.provenance) as i32,
    }
}

fn binding_from_wire(binding: wire::GrpcBindingCandidate) -> Result<GrpcBindingCandidate, String> {
    Ok(GrpcBindingCandidate {
        local_symbol: binding.local_symbol.into(),
        role: match wire::GrpcBindingRole::try_from(binding.role)
            .map_err(|_| "worker gRPC binding role is unknown")?
        {
            wire::GrpcBindingRole::Unspecified => {
                return Err("worker gRPC binding role is missing".into());
            }
            wire::GrpcBindingRole::Client => GrpcBindingRole::Client,
            wire::GrpcBindingRole::Server => GrpcBindingRole::Server,
        },
        service: binding.service,
        method: binding.method,
        cardinality: cardinality_from_wire(binding.cardinality)?,
        evidence: binding.evidence.into(),
        confidence: confidence_from_wire(binding.confidence)?,
        provenance: provenance_from_wire(binding.provenance)?,
    })
}

fn override_to_wire(value: DependencyOverride) -> Result<wire::DependencyOverride, String> {
    Ok(wire::DependencyOverride {
        from: value.from.to_string(),
        relation: relation_to_wire(SemanticRelation::Dependency(value.relation)) as i32,
        unresolved_to: value.unresolved_to.to_string(),
        resolved_to: value.resolved_to.to_string(),
        evidence: value.evidence.as_str().into(),
        confidence: confidence_to_wire(value.confidence) as i32,
        provenance: provenance_to_wire(value.provenance) as i32,
    })
}

fn override_from_wire(value: wire::DependencyOverride) -> Result<DependencyOverride, String> {
    let relation = relation_from_wire(value.relation)?
        .dependency()
        .ok_or("worker dependency override used a structural relation")?;
    Ok(DependencyOverride {
        from: value.from.into(),
        relation,
        unresolved_to: value.unresolved_to.into(),
        resolved_to: value.resolved_to.into(),
        evidence: value.evidence.into(),
        confidence: confidence_from_wire(value.confidence)?,
        provenance: provenance_from_wire(value.provenance)?,
    })
}

fn diagnostic_to_wire(diagnostic: AnalysisDiagnostic) -> wire::AnalysisDiagnostic {
    wire::AnalysisDiagnostic {
        code: diagnostic.code,
        severity: match diagnostic.severity {
            AnalysisDiagnosticSeverity::KnownLimitation => {
                v1::AnalysisDiagnosticSeverity::KnownLimitation
            }
            AnalysisDiagnosticSeverity::Warning => v1::AnalysisDiagnosticSeverity::Warning,
        } as i32,
        path: diagnostic.path.to_string_lossy().into_owned(),
        line: diagnostic.line,
        detail: diagnostic.detail,
    }
}

fn diagnostic_from_wire(
    diagnostic: wire::AnalysisDiagnostic,
) -> Result<AnalysisDiagnostic, String> {
    Ok(AnalysisDiagnostic {
        code: diagnostic.code,
        severity: match v1::AnalysisDiagnosticSeverity::try_from(diagnostic.severity)
            .map_err(|_| "worker diagnostic severity is unknown")?
        {
            v1::AnalysisDiagnosticSeverity::Unspecified => {
                return Err("worker diagnostic severity is missing".into());
            }
            v1::AnalysisDiagnosticSeverity::KnownLimitation => {
                AnalysisDiagnosticSeverity::KnownLimitation
            }
            v1::AnalysisDiagnosticSeverity::Warning => AnalysisDiagnosticSeverity::Warning,
        },
        path: diagnostic.path.into(),
        line: diagnostic.line,
        detail: diagnostic.detail,
    })
}

fn confidence_to_wire(value: Confidence) -> wire::Confidence {
    match value {
        Confidence::Exact => wire::Confidence::Exact,
        Confidence::Inferred => wire::Confidence::Inferred,
    }
}

fn confidence_from_wire(value: i32) -> Result<Confidence, String> {
    match wire::Confidence::try_from(value).map_err(|_| "worker confidence is unknown")? {
        wire::Confidence::Unspecified => Err("worker confidence is missing".into()),
        wire::Confidence::Exact => Ok(Confidence::Exact),
        wire::Confidence::Inferred => Ok(Confidence::Inferred),
    }
}

fn provenance_to_wire(value: Provenance) -> wire::Provenance {
    match value {
        Provenance::Ast => wire::Provenance::Ast,
        Provenance::Compiler => wire::Provenance::Compiler,
        Provenance::Descriptor => wire::Provenance::Descriptor,
        Provenance::Generated => wire::Provenance::Generated,
        Provenance::UniqueNameHeuristic => wire::Provenance::UniqueNameHeuristic,
    }
}

fn provenance_from_wire(value: i32) -> Result<Provenance, String> {
    match wire::Provenance::try_from(value).map_err(|_| "worker provenance is unknown")? {
        wire::Provenance::Unspecified => Err("worker provenance is missing".into()),
        wire::Provenance::Ast => Ok(Provenance::Ast),
        wire::Provenance::Compiler => Ok(Provenance::Compiler),
        wire::Provenance::Descriptor => Ok(Provenance::Descriptor),
        wire::Provenance::Generated => Ok(Provenance::Generated),
        wire::Provenance::UniqueNameHeuristic => Ok(Provenance::UniqueNameHeuristic),
    }
}

fn cardinality_to_wire(value: RpcCardinality) -> v1::RpcCardinality {
    match value {
        RpcCardinality::BidirectionalStreaming => v1::RpcCardinality::BidirectionalStreaming,
        RpcCardinality::ClientStreaming => v1::RpcCardinality::ClientStreaming,
        RpcCardinality::ServerStreaming => v1::RpcCardinality::ServerStreaming,
        RpcCardinality::Unary => v1::RpcCardinality::Unary,
    }
}

fn cardinality_from_wire(value: i32) -> Result<RpcCardinality, String> {
    match v1::RpcCardinality::try_from(value).map_err(|_| "worker RPC cardinality is unknown")? {
        v1::RpcCardinality::Unspecified => Err("worker RPC cardinality is missing".into()),
        v1::RpcCardinality::BidirectionalStreaming => Ok(RpcCardinality::BidirectionalStreaming),
        v1::RpcCardinality::ClientStreaming => Ok(RpcCardinality::ClientStreaming),
        v1::RpcCardinality::ServerStreaming => Ok(RpcCardinality::ServerStreaming),
        v1::RpcCardinality::Unary => Ok(RpcCardinality::Unary),
    }
}

fn relation_to_wire(value: SemanticRelation) -> v1::RelationKind {
    match value {
        SemanticRelation::Structural(StructuralRelation::Defines) => v1::RelationKind::Defines,
        SemanticRelation::Structural(StructuralRelation::FieldOf) => v1::RelationKind::FieldOf,
        SemanticRelation::Structural(StructuralRelation::RequestType) => {
            v1::RelationKind::RequestType
        }
        SemanticRelation::Structural(StructuralRelation::ResponseType) => {
            v1::RelationKind::ResponseType
        }
        SemanticRelation::Dependency(DependencyRelation::BindsContract) => {
            v1::RelationKind::BindsContract
        }
        SemanticRelation::Dependency(DependencyRelation::Calls) => v1::RelationKind::Calls,
        SemanticRelation::Dependency(DependencyRelation::CallsGraphql) => {
            v1::RelationKind::CallsGraphql
        }
        SemanticRelation::Dependency(DependencyRelation::CallsRpc) => v1::RelationKind::CallsRpc,
        SemanticRelation::Dependency(DependencyRelation::ConsumedBy) => {
            v1::RelationKind::ConsumedBy
        }
        SemanticRelation::Dependency(DependencyRelation::Implements) => {
            v1::RelationKind::Implements
        }
        SemanticRelation::Dependency(DependencyRelation::ImplementedBy) => {
            v1::RelationKind::ImplementedBy
        }
        SemanticRelation::Dependency(DependencyRelation::Imports) => v1::RelationKind::Imports,
        SemanticRelation::Dependency(DependencyRelation::Publishes) => v1::RelationKind::Publishes,
        SemanticRelation::Dependency(DependencyRelation::Requires) => v1::RelationKind::Requires,
        SemanticRelation::Dependency(DependencyRelation::ResolvedBy) => {
            v1::RelationKind::ResolvedBy
        }
        SemanticRelation::Dependency(DependencyRelation::Selects) => v1::RelationKind::Selects,
        SemanticRelation::Dependency(DependencyRelation::Uses) => v1::RelationKind::Uses,
    }
}

fn relation_from_wire(value: i32) -> Result<SemanticRelation, String> {
    Ok(
        match v1::RelationKind::try_from(value).map_err(|_| "worker relation is unknown")? {
            v1::RelationKind::Unspecified => return Err("worker relation is missing".into()),
            v1::RelationKind::Defines => SemanticRelation::Structural(StructuralRelation::Defines),
            v1::RelationKind::FieldOf => SemanticRelation::Structural(StructuralRelation::FieldOf),
            v1::RelationKind::RequestType => {
                SemanticRelation::Structural(StructuralRelation::RequestType)
            }
            v1::RelationKind::ResponseType => {
                SemanticRelation::Structural(StructuralRelation::ResponseType)
            }
            v1::RelationKind::BindsContract => {
                SemanticRelation::Dependency(DependencyRelation::BindsContract)
            }
            v1::RelationKind::Calls => SemanticRelation::Dependency(DependencyRelation::Calls),
            v1::RelationKind::CallsGraphql => {
                SemanticRelation::Dependency(DependencyRelation::CallsGraphql)
            }
            v1::RelationKind::CallsRpc => {
                SemanticRelation::Dependency(DependencyRelation::CallsRpc)
            }
            v1::RelationKind::ConsumedBy => {
                SemanticRelation::Dependency(DependencyRelation::ConsumedBy)
            }
            v1::RelationKind::Implements => {
                SemanticRelation::Dependency(DependencyRelation::Implements)
            }
            v1::RelationKind::ImplementedBy => {
                SemanticRelation::Dependency(DependencyRelation::ImplementedBy)
            }
            v1::RelationKind::Imports => SemanticRelation::Dependency(DependencyRelation::Imports),
            v1::RelationKind::Publishes => {
                SemanticRelation::Dependency(DependencyRelation::Publishes)
            }
            v1::RelationKind::Requires => {
                SemanticRelation::Dependency(DependencyRelation::Requires)
            }
            v1::RelationKind::ResolvedBy => {
                SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
            }
            v1::RelationKind::Selects => SemanticRelation::Dependency(DependencyRelation::Selects),
            v1::RelationKind::Uses => SemanticRelation::Dependency(DependencyRelation::Uses),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trip_preserves_target_and_contexts() {
        let repository = |identity: &str| RepositorySnapshot {
            base: PathBuf::from(identity),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: identity.into(),
                },
                head: None,
                fingerprint: format!("{identity}-state"),
            },
            inputs: Vec::new(),
        };
        let snapshot = EnrichmentSnapshot {
            target_repository: "example/target".into(),
            workspace: WorkspaceSnapshot {
                name: "main".into(),
                repositories: vec![repository("example/target"), repository("example/context")],
            },
            baseline: SemanticSnapshot::default(),
        };

        assert_eq!(
            workspace_snapshot(analyze_requests(snapshot.clone()).unwrap()).unwrap(),
            snapshot
        );
    }

    #[test]
    fn plugin_descriptor_round_trip_preserves_generic_contract() {
        let descriptor = PluginDescriptor {
            id: "example.kafka".into(),
            api_version: beholder_indexing::PLUGIN_API_VERSION,
            inputs: vec![PluginInputSelector {
                scope: PluginInputScope::Target,
                matcher: PluginPathMatcher::PathSuffix("config/topics.exs".into()),
                kind: AnalysisInputKind::Configuration,
            }],
            semantic_entities: BTreeSet::from([EntityKind::ProtoType]),
            semantic_relations: BTreeSet::from([SemanticRelation::Dependency(
                DependencyRelation::BindsContract,
            )]),
            produces_entities: BTreeSet::from([EntityKind::KafkaTopic]),
            produces_relations: BTreeSet::from([
                SemanticRelation::Dependency(DependencyRelation::Publishes),
                SemanticRelation::Dependency(DependencyRelation::ConsumedBy),
            ]),
        };

        assert_eq!(
            descriptor_from_wire(descriptor_to_wire(descriptor.clone())).unwrap(),
            descriptor
        );
    }

    #[test]
    fn preserves_grpc_operation_entity_kind() {
        let entity =
            EntityFact::new("grpc-operation://example", EntityKind::GrpcOperation, None).unwrap();

        let decoded = entity_from_wire(entity_to_wire(entity.clone()).unwrap()).unwrap();

        assert_eq!(decoded, entity);
    }

    #[test]
    fn streams_and_merges_large_repository_contributions() {
        let contribution = AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: "rust".into(),
                version: "1".into(),
            },
            active_repositories: vec!["example/repo".into()],
            repositories: vec![RepositoryContribution {
                repository: "example/repo".into(),
                completeness: AnalysisCompleteness::Complete,
                entities: Vec::new(),
                grpc_bindings: Vec::new(),
                observations: (0..5_000)
                    .map(|index| {
                        Observation::dependency(
                            format!("repo://example/repo/rust/from-{index}"),
                            DependencyRelation::Calls,
                            format!("repo://example/repo/rust/to-{index}"),
                            format!("src/lib.rs:{index}"),
                        )
                    })
                    .collect(),
                semantic_candidates: Vec::new(),
                diagnostics: Vec::new(),
                replaced_diagnostic_codes: BTreeSet::from(["syntax.unresolved".into()]),
                fact_shards: vec![FactShard {
                    repository: "example/repo".into(),
                    producer: "rust".into(),
                    owner: "repo://example/repo/rust/src/lib.rs".into(),
                    version: "source-v1".into(),
                    entities: vec![
                        EntityFact::new(
                            "repo://example/repo/rust/lib",
                            EntityKind::Namespace,
                            None,
                        )
                        .unwrap(),
                    ],
                    observations: vec![Observation::dependency(
                        "repo://example/repo/rust/lib",
                        DependencyRelation::Calls,
                        "rust-call://target",
                        "src/lib.rs:1",
                    )],
                }],
            }],
            overrides: (0..5_000)
                .map(|index| DependencyOverride {
                    from: format!("repo://example/repo/rust/from-{index}").into(),
                    relation: DependencyRelation::Calls,
                    unresolved_to: format!("rust-call://target-{index}").into(),
                    resolved_to: format!("repo://example/repo/rust/to-{index}").into(),
                    evidence: format!("src/lib.rs:{index}").into(),
                    confidence: Confidence::Inferred,
                    provenance: Provenance::UniqueNameHeuristic,
                })
                .collect(),
            candidate_overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        };

        let events = analyze_events(contribution.clone()).unwrap();

        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(event.event, Some(wire::analyze_event::Event::Repository(_)))
                })
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event.event,
                        Some(wire::analyze_event::Event::Contribution(_))
                    )
                })
                .count(),
            3
        );
        assert_eq!(contribution_from_events(events).unwrap(), contribution);
    }

    #[test]
    fn rejects_events_after_completion() {
        let contribution = AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: "rust".into(),
                version: "1".into(),
            },
            active_repositories: Vec::new(),
            repositories: Vec::new(),
            overrides: Vec::new(),
            candidate_overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        };
        let mut events = analyze_events(contribution).unwrap();
        events.push(wire::AnalyzeEvent {
            event: Some(wire::analyze_event::Event::Progress(
                wire::AnalysisProgress::default(),
            )),
        });

        assert_eq!(
            contribution_from_events(events).unwrap_err(),
            "worker event followed analysis completion"
        );
    }

    #[test]
    fn rejects_unspecified_progress() {
        let events = [wire::AnalyzeEvent {
            event: Some(wire::analyze_event::Event::Progress(
                wire::AnalysisProgress::default(),
            )),
        }];

        assert_eq!(
            contribution_from_events(events).unwrap_err(),
            "worker progress phase is missing"
        );
    }
}
