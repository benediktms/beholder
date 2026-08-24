use super::schema::*;
use super::store::{EnrichmentOwner, EnrichmentPayload};
use beholder_domain::{
    DependencyOverride, DependencyRelation, EntityFact, EntityKind, EntityMetadata, FactChanges,
    GraphqlOperationKind, GraphqlTypeKind, GrpcBindingCandidate, GrpcBindingRole, Observation,
    ProtoTypeKind, RepositoryFacts, RpcCardinality, SemanticRelation, WorkspaceView,
};
use beholder_dto::{GarbageCollectionPhase, GarbageCollectionProgress};
use mnestic_engine::{DataValue, DbInstance, MultiTransaction, ScriptMutability};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    thread,
    time::Duration,
};

const FACT_BATCH_SIZE: usize = 10_000;
const GARBAGE_COLLECTION_BATCH_SIZE: usize = 10_000;
const GARBAGE_COLLECTION_TRANSACTION_RETRIES: usize = 50;
const GARBAGE_COLLECTION_TRANSACTION_RETRY_DELAY: Duration = Duration::from_millis(10);

pub(super) fn store_observations(
    transaction: &MultiTransaction,
    state: &str,
    observations: &[Observation],
) -> Result<(), Box<dyn Error>> {
    let observations = observations
        .iter()
        .map(|observation| {
            (
                (
                    observation.from.as_str(),
                    observation.relation.as_str(),
                    observation.to.as_str(),
                ),
                observation,
            )
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    for observations in observations.chunks(FACT_BATCH_SIZE) {
        let rows = observations
            .iter()
            .map(|observation| {
                DataValue::List(vec![
                    state.into(),
                    observation.from.as_str().into(),
                    observation.relation.as_str().into(),
                    observation.to.as_str().into(),
                    observation.evidence.as_str().into(),
                    observation.confidence.score().into(),
                    observation.provenance.as_str().into(),
                ])
            })
            .collect();
        let rows = DataValue::List(rows);
        transaction.run_script(
            "?[state, from, relation, to, evidence, confidence, provenance] <- $rows\n\
             :put state_observation {state, from, relation, to => evidence}",
            BTreeMap::from([("rows".into(), rows.clone())]),
        )?;
        transaction.run_script(
            "?[state, from, relation, to, evidence, confidence, provenance] <- $rows\n\
             :put state_observation_metadata {\
                 state, from, relation, to => confidence, provenance\
             }",
            BTreeMap::from([("rows".into(), rows)]),
        )?;

        let dependency_rows = observations
            .iter()
            .filter(|observation| observation.relation.dependency().is_some())
            .map(|observation| {
                DataValue::List(vec![
                    state.into(),
                    observation.from.as_str().into(),
                    observation.relation.as_str().into(),
                    observation.to.as_str().into(),
                    observation.evidence.as_str().into(),
                ])
            })
            .collect::<Vec<_>>();
        if !dependency_rows.is_empty() {
            transaction.run_script(
                "?[state, from, relation, to, evidence] <- $rows\n\
                 :put state_dependency_observation {state, from, relation, to => evidence}",
                BTreeMap::from([("rows".into(), DataValue::List(dependency_rows))]),
            )?;
        }
    }
    Ok(())
}

fn entity_kind(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Callable => "callable",
        EntityKind::GraphqlArgument => "graphql_argument",
        EntityKind::GraphqlEnumValue => "graphql_enum_value",
        EntityKind::GraphqlField => "graphql_field",
        EntityKind::GraphqlOperation => "graphql_operation",
        EntityKind::GraphqlType => "graphql_type",
        EntityKind::GrpcOperation => "grpc_operation",
        EntityKind::KafkaTopic => "kafka_topic",
        EntityKind::Namespace => "namespace",
        EntityKind::ProtoField => "proto_field",
        EntityKind::ProtoMethod => "proto_method",
        EntityKind::ProtoService => "proto_service",
        EntityKind::ProtoType => "proto_type",
        EntityKind::Service => "service",
        EntityKind::UnityPrefab => "unity_prefab",
    }
}

fn rpc_cardinality(cardinality: RpcCardinality) -> &'static str {
    match cardinality {
        RpcCardinality::BidirectionalStreaming => "bidirectional_streaming",
        RpcCardinality::ClientStreaming => "client_streaming",
        RpcCardinality::ServerStreaming => "server_streaming",
        RpcCardinality::Unary => "unary",
    }
}

fn entity_metadata(metadata: Option<EntityMetadata>) -> &'static str {
    match metadata {
        None => "",
        Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Mutation,
        }) => "graphql_operation:mutation",
        Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Query,
        }) => "graphql_operation:query",
        Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Subscription,
        }) => "graphql_operation:subscription",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Enum,
        }) => "graphql_type:enum",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Input,
        }) => "graphql_type:input",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Interface,
        }) => "graphql_type:interface",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Object,
        }) => "graphql_type:object",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Scalar,
        }) => "graphql_type:scalar",
        Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Union,
        }) => "graphql_type:union",
        Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::BidirectionalStreaming,
        }) => "rpc_cardinality:bidirectional_streaming",
        Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::ClientStreaming,
        }) => "rpc_cardinality:client_streaming",
        Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::ServerStreaming,
        }) => "rpc_cardinality:server_streaming",
        Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::Unary,
        }) => "rpc_cardinality:unary",
        Some(EntityMetadata::ProtoType {
            kind: ProtoTypeKind::Enum,
        }) => "proto_type:enum",
        Some(EntityMetadata::ProtoType {
            kind: ProtoTypeKind::Message,
        }) => "proto_type:message",
    }
}

pub(super) fn store_entities(
    transaction: &MultiTransaction,
    state: &str,
    entities: &[EntityFact],
) -> Result<(), Box<dyn Error>> {
    for entities in entities.chunks(FACT_BATCH_SIZE) {
        let rows = entities
            .iter()
            .map(|entity| {
                DataValue::List(vec![
                    state.into(),
                    entity.id.as_str().into(),
                    entity_kind(entity.kind).into(),
                    entity_metadata(entity.metadata).into(),
                ])
            })
            .collect();
        transaction.run_script(
            "?[state, id, kind, metadata] <- $rows\n\
             :put state_entity {state, id => kind, metadata}",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    Ok(())
}

pub(super) fn store_grpc_bindings(
    transaction: &MultiTransaction,
    state: &str,
    candidates: &[GrpcBindingCandidate],
) -> Result<(), Box<dyn Error>> {
    if candidates.is_empty() {
        return Ok(());
    }
    let rows = candidates
        .iter()
        .map(|candidate| {
            DataValue::List(vec![
                state.into(),
                candidate.local_symbol.as_str().into(),
                candidate.role.as_str().into(),
                candidate.service.as_str().into(),
                candidate.method.as_str().into(),
                candidate.evidence.as_str().into(),
                rpc_cardinality(candidate.cardinality).into(),
                candidate.confidence.score().into(),
                candidate.provenance.as_str().into(),
            ])
        })
        .collect();
    transaction.run_script(
        "?[state, local_symbol, role, service, method, evidence, cardinality, confidence, provenance] <- $rows\n\
         :put state_grpc_binding_candidate {\
             state, local_symbol, role, service, method, evidence => cardinality, confidence, provenance\
         }",
        BTreeMap::from([("rows".into(), DataValue::List(rows))]),
    )?;
    Ok(())
}

pub(super) fn hash_string(hash: &mut Sha256, value: &str) {
    hash.update(value.len().to_le_bytes());
    hash.update(value.as_bytes());
}

type NormalizedObservations = BTreeMap<(String, String, String), (String, u64, String)>;
type NormalizedEntities = BTreeMap<String, (String, String)>;
type NormalizedGrpcBindings =
    BTreeMap<(String, String, String, String, String), (String, u64, String)>;

fn normalized_entities(facts: &RepositoryFacts) -> NormalizedEntities {
    facts
        .entities
        .iter()
        .map(|entity| {
            (
                entity.id.as_str().into(),
                (
                    entity_kind(entity.kind).into(),
                    entity_metadata(entity.metadata).into(),
                ),
            )
        })
        .collect()
}

fn normalized_grpc_bindings(facts: &RepositoryFacts) -> NormalizedGrpcBindings {
    facts
        .grpc_bindings
        .iter()
        .map(|candidate| {
            (
                (
                    candidate.local_symbol.as_str().into(),
                    candidate.role.as_str().into(),
                    candidate.service.clone(),
                    candidate.method.clone(),
                    candidate.evidence.as_str().into(),
                ),
                (
                    rpc_cardinality(candidate.cardinality).into(),
                    candidate.confidence.score().to_bits(),
                    candidate.provenance.as_str().into(),
                ),
            )
        })
        .collect()
}

pub(super) fn normalized_observations(facts: &RepositoryFacts) -> NormalizedObservations {
    facts
        .observations
        .iter()
        .map(|observation| {
            (
                (
                    observation.from.as_str().to_owned(),
                    observation.relation.as_str().to_owned(),
                    observation.to.as_str().to_owned(),
                ),
                (
                    observation.evidence.as_str().to_owned(),
                    observation.confidence.score().to_bits(),
                    observation.provenance.as_str().to_owned(),
                ),
            )
        })
        .collect()
}

pub(super) fn analyzed_state(facts: &RepositoryFacts) -> String {
    let mut hash = Sha256::new();
    hash_string(&mut hash, &facts.state.repository.identity);
    hash_string(&mut hash, &facts.state.fingerprint);
    for ((from, relation, to), (evidence, confidence, provenance)) in normalized_observations(facts)
    {
        for value in [&from, &relation, &to, &evidence, &provenance] {
            hash_string(&mut hash, value);
        }
        hash.update(confidence.to_le_bytes());
    }
    for (id, (kind, metadata)) in normalized_entities(facts) {
        for value in [&id, &kind, &metadata] {
            hash_string(&mut hash, value);
        }
    }
    for ((local, role, service, method, evidence), (cardinality, confidence, provenance)) in
        normalized_grpc_bindings(facts)
    {
        for value in [
            &local,
            &role,
            &service,
            &method,
            &evidence,
            &cardinality,
            &provenance,
        ] {
            hash_string(&mut hash, value);
        }
        hash.update(confidence.to_le_bytes());
    }
    format!("{}:{:x}", facts.state.fingerprint, hash.finalize())
}

pub(super) fn state_exists(
    transaction: &MultiTransaction,
    state: &str,
) -> Result<bool, Box<dyn Error>> {
    Ok(!transaction
        .run_script(
            "?[stored] := *repository_state{fingerprint: $state}, stored = true",
            BTreeMap::from([("state".into(), state.into())]),
        )?
        .rows
        .is_empty())
}

pub(super) fn publish_repository(
    db: &DbInstance,
    facts: &RepositoryFacts,
) -> Result<bool, Box<dyn Error>> {
    if facts.analysis_identity.is_empty() {
        return Err("repository analysis identity must not be empty".into());
    }
    let transaction = db.multi_transaction(true);
    let analyzed_state = analyzed_state(facts);
    let params = BTreeMap::from([
        (
            "repository".into(),
            facts.state.repository.identity.clone().into(),
        ),
        (
            "source_state".into(),
            facts.state.fingerprint.clone().into(),
        ),
        ("analyzed_state".into(), analyzed_state.clone().into()),
        (
            "analysis_identity".into(),
            facts.analysis_identity.clone().into(),
        ),
        (
            "head".into(),
            facts.state.head.clone().unwrap_or_default().into(),
        ),
        ("incomplete".into(), facts.incomplete.into()),
    ]);
    let previous = transaction.run_script(
        "?[source_state, analyzed_state, analysis_identity, head, incomplete] := \
             *repository_revision{repository: $repository, source_state, analyzed_state, analysis_identity, head, incomplete}",
        params.clone(),
    )?;
    let changed = previous.rows.first().is_none_or(|row| {
        row[0].get_str() != Some(&facts.state.fingerprint)
            || row[1].get_str() != Some(&analyzed_state)
            || row[2].get_str() != Some(&facts.analysis_identity)
            || row[3].get_str() != Some(facts.state.head.as_deref().unwrap_or_default())
            || row[4] != facts.incomplete.into()
    });
    if !state_exists(&transaction, &analyzed_state)? {
        store_observations(&transaction, &analyzed_state, &facts.observations)?;
        store_entities(&transaction, &analyzed_state, &facts.entities)?;
        store_grpc_bindings(&transaction, &analyzed_state, &facts.grpc_bindings)?;
    }
    store_repository_state(&transaction, facts, &analyzed_state)?;
    transaction.run_script(
        "?[repository, code, severity, path, line] := \
             *repository_revision_diagnostic{repository: $repository, code, severity, path, line}, \
             repository = $repository\n\
         :rm repository_revision_diagnostic {repository, code, severity, path, line}",
        params.clone(),
    )?;
    let diagnostics = facts
        .diagnostics
        .iter()
        .map(|diagnostic| {
            DataValue::List(vec![
                facts.state.repository.identity.as_str().into(),
                diagnostic.code.as_str().into(),
                diagnostic.severity.as_str().into(),
                diagnostic.path.to_string_lossy().into_owned().into(),
                i64::from(diagnostic.line.unwrap_or_default()).into(),
                diagnostic.detail.as_deref().unwrap_or_default().into(),
            ])
        })
        .collect();
    transaction.run_script(
        "?[repository, code, severity, path, line, detail] <- $rows \
         :put repository_revision_diagnostic {repository, code, severity, path, line => detail}",
        BTreeMap::from([("rows".into(), DataValue::List(diagnostics))]),
    )?;
    transaction.run_script(
        "?[repository, source_state, analyzed_state, analysis_identity, head, incomplete] <- \
             [[$repository, $source_state, $analyzed_state, $analysis_identity, $head, $incomplete]] \
         :put repository_revision {repository => source_state, analyzed_state, analysis_identity, head, incomplete}",
        params,
    )?;
    transaction.commit()?;
    Ok(changed)
}

pub(super) fn delete_repository_revision(
    db: &DbInstance,
    repository: &str,
) -> Result<u64, Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    let params = BTreeMap::from([("repository".into(), repository.into())]);
    let revision = transaction.run_script(
        "?[repository, analyzed_state] := \
             *repository_revision{repository: $repository, analyzed_state}, repository = $repository",
        params.clone(),
    )?;
    transaction.run_script(
        "?[repository, code, severity, path, line] := \
             *repository_revision_diagnostic{repository: $repository, code, severity, path, line}, \
             repository = $repository\n\
         :rm repository_revision_diagnostic {repository, code, severity, path, line}",
        params.clone(),
    )?;
    transaction.run_script(
        "?[repository] := repository = $repository\n:rm repository_revision {repository}",
        params,
    )?;
    let queued = if let Some(row) = revision.rows.first() {
        let state = row[1]
            .get_str()
            .ok_or("repository revision state is not a string")?;
        let stale = transaction.run_script(
            "live_state[state] := \
                 *analysis_revision{view, revision}, \
                 *analysis_revision_state{view, revision, state}\n\
             ?[state, repository, head] := \
                 *repository_state{fingerprint: state, repository, head}, \
                 state = $state, not live_state[state]",
            BTreeMap::from([("state".into(), state.into())]),
        )?;
        if !stale.rows.is_empty() {
            transaction.run_script(
                "?[state, repository, head] <- $rows \
                 :put garbage_collection_state {state => repository, head}",
                BTreeMap::from([(
                    "rows".into(),
                    DataValue::List(
                        stale
                            .rows
                            .clone()
                            .into_iter()
                            .map(DataValue::List)
                            .collect(),
                    ),
                )]),
            )?;
            transaction.run_script(
                "?[fingerprint] := fingerprint = $state :rm repository_state {fingerprint}",
                BTreeMap::from([("state".into(), state.into())]),
            )?;
        }
        stale.rows.len().try_into()?
    } else {
        0
    };
    transaction.commit()?;
    Ok(queued)
}

pub(super) fn reusable_current_state(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
    facts: &RepositoryFacts,
) -> Result<Option<String>, Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        (
            "repository".into(),
            facts.state.repository.identity.clone().into(),
        ),
    ]);
    let current = transaction.run_script(
        "?[state] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, repository: $repository, state}",
        params,
    )?;
    let Some(state) = current
        .rows
        .first()
        .and_then(|row| row[0].get_str())
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let fingerprint = &facts.state.fingerprint;
    if state
        .strip_prefix(fingerprint)
        .is_some_and(|suffix| suffix.starts_with(':'))
    {
        return Ok(None);
    }
    if !state.ends_with(fingerprint) {
        return Ok(None);
    }
    let stored = transaction.run_script(
        "?[from, relation, to, evidence, confidence, provenance] := \
             *state_observation{state: $state, from, relation, to, evidence}, \
             *state_observation_metadata{state: $state, from, relation, to, confidence, provenance}",
        BTreeMap::from([("state".into(), state.clone().into())]),
    )?;
    let string = |row: &[DataValue], index: usize| {
        row[index]
            .get_str()
            .map(str::to_owned)
            .ok_or("stored observation contains a non-string value")
    };
    let stored = stored
        .rows
        .iter()
        .map(|row| {
            Ok((
                (string(row, 0)?, string(row, 1)?, string(row, 2)?),
                (
                    string(row, 3)?,
                    row[4]
                        .get_float()
                        .ok_or("stored observation contains a non-float confidence")?
                        .to_bits(),
                    string(row, 5)?,
                ),
            ))
        })
        .collect::<Result<NormalizedObservations, Box<dyn Error>>>()?;
    let stored_entities = transaction.run_script(
        "?[id, kind, metadata] := *state_entity{state: $state, id, kind, metadata}",
        BTreeMap::from([("state".into(), state.clone().into())]),
    )?;
    let stored_entities = stored_entities
        .rows
        .iter()
        .map(|row| Ok((string(row, 0)?, (string(row, 1)?, string(row, 2)?))))
        .collect::<Result<NormalizedEntities, Box<dyn Error>>>()?;
    let stored_bindings = transaction.run_script(
        "?[local_symbol, role, service, method, evidence, cardinality, confidence, provenance] := \
             *state_grpc_binding_candidate{\
                 state: $state, local_symbol, role, service, method, evidence, cardinality, \
                 confidence, provenance\
             }",
        BTreeMap::from([("state".into(), state.clone().into())]),
    )?;
    let stored_bindings = stored_bindings
        .rows
        .iter()
        .map(|row| {
            Ok((
                (
                    string(row, 0)?,
                    string(row, 1)?,
                    string(row, 2)?,
                    string(row, 3)?,
                    string(row, 4)?,
                ),
                (
                    string(row, 5)?,
                    row[6]
                        .get_float()
                        .ok_or("stored gRPC binding contains a non-float confidence")?
                        .to_bits(),
                    string(row, 7)?,
                ),
            ))
        })
        .collect::<Result<NormalizedGrpcBindings, Box<dyn Error>>>()?;
    Ok((stored == normalized_observations(facts)
        && stored_entities == normalized_entities(facts)
        && stored_bindings == normalized_grpc_bindings(facts))
    .then_some(state))
}

pub(super) fn view_matches(db: &DbInstance, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[matches] := *analysis_fingerprint{view: $view, fingerprint: stored}, \
             matches = stored == $fingerprint",
        BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("fingerprint".into(), view.fingerprint().into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows.rows.first().is_some_and(|row| row[0] == true.into()))
}

pub(super) fn verification_matches(
    db: &DbInstance,
    view: &str,
    fingerprint: &str,
) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[matches] := *analysis_verification_fingerprint{view: $view, fingerprint: stored}, \
             matches = stored == $fingerprint",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("fingerprint".into(), fingerprint.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows.rows.first().is_some_and(|row| row[0] == true.into()))
}

pub(super) fn store_verification_fingerprint(
    db: &DbInstance,
    view: &str,
    fingerprint: &str,
) -> Result<(), Box<dyn Error>> {
    db.run_script(
        "?[view, fingerprint] <- [[$view, $fingerprint]] \
         :put analysis_verification_fingerprint {view => fingerprint}",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("fingerprint".into(), fingerprint.into()),
        ]),
        ScriptMutability::Mutable,
    )?;
    Ok(())
}

#[derive(Default)]
struct GrpcResolution {
    entities: BTreeMap<String, EntityFact>,
    observations: Vec<Observation>,
    diagnostics: Vec<GrpcDiagnostic>,
}

struct GrpcDiagnostic {
    candidate: GrpcBindingCandidate,
    code: &'static str,
    detail: String,
}

fn resolve_grpc_bindings(
    repositories: &[RepositoryFacts],
) -> Result<GrpcResolution, Box<dyn Error>> {
    let contracts = repositories
        .iter()
        .flat_map(|facts| &facts.entities)
        .filter_map(|entity| {
            let EntityMetadata::ProtoMethod { cardinality } = entity.metadata? else {
                return None;
            };
            let (service, method) = entity
                .id
                .as_str()
                .strip_prefix("proto-method://")?
                .split_once('/')?;
            Some((
                (
                    service.to_owned(),
                    method.to_owned(),
                    rpc_cardinality(cardinality),
                ),
                entity.id.clone(),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut resolution = GrpcResolution::default();
    for candidate in repositories.iter().flat_map(|facts| &facts.grpc_bindings) {
        if candidate.cardinality != RpcCardinality::Unary {
            resolution.diagnostics.push(GrpcDiagnostic {
                candidate: candidate.clone(),
                code: "grpc.cardinality_unsupported",
                detail: format!(
                    "{} streaming is not supported in Phase 5",
                    rpc_cardinality(candidate.cardinality)
                ),
            });
            continue;
        }
        let Some(contract) = contracts.get(&(
            candidate.service.clone(),
            candidate.method.clone(),
            rpc_cardinality(candidate.cardinality),
        )) else {
            resolution.diagnostics.push(GrpcDiagnostic {
                candidate: candidate.clone(),
                code: "grpc.contract_unmatched",
                detail: "no matching Protobuf method in the workspace view".into(),
            });
            continue;
        };
        let operation = format!("grpc://{}/{}", candidate.service, candidate.method);
        resolution.entities.insert(
            operation.clone(),
            EntityFact::new(operation.as_str(), EntityKind::GrpcOperation, None)?,
        );
        let observation = |from: &str, relation, to: &str| Observation {
            from: from.into(),
            relation: SemanticRelation::Dependency(relation),
            to: to.into(),
            evidence: candidate.evidence.clone(),
            confidence: candidate.confidence,
            provenance: candidate.provenance,
        };
        resolution.observations.push(observation(
            &operation,
            DependencyRelation::BindsContract,
            contract.as_str(),
        ));
        resolution.observations.push(match candidate.role {
            GrpcBindingRole::Client => observation(
                candidate.local_symbol.as_str(),
                DependencyRelation::CallsRpc,
                &operation,
            ),
            GrpcBindingRole::Server => observation(
                &operation,
                DependencyRelation::ImplementedBy,
                candidate.local_symbol.as_str(),
            ),
        });
    }
    Ok(resolution)
}

fn store_grpc_resolution(
    transaction: &MultiTransaction,
    view: &str,
    resolution: &GrpcResolution,
) -> Result<(), Box<dyn Error>> {
    let revision = transaction
        .run_script(
            "?[revision] := *analysis_revision{view: $view, revision}",
            BTreeMap::from([("view".into(), view.into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .ok_or("published analysis revision is missing")?;
    let entity_rows = resolution
        .entities
        .values()
        .map(|entity| {
            DataValue::List(vec![
                view.into(),
                revision.into(),
                entity.id.as_str().into(),
                entity_kind(entity.kind).into(),
                entity_metadata(entity.metadata).into(),
            ])
        })
        .collect::<Vec<_>>();
    if !entity_rows.is_empty() {
        transaction.run_script(
            "?[view, revision, id, kind, metadata] <- $rows\n\
             :put analysis_revision_entity {view, revision, id => kind, metadata}",
            BTreeMap::from([("rows".into(), DataValue::List(entity_rows))]),
        )?;
    }
    let observation_rows = resolution
        .observations
        .iter()
        .map(|observation| {
            DataValue::List(vec![
                view.into(),
                revision.into(),
                observation.from.as_str().into(),
                observation.relation.as_str().into(),
                observation.to.as_str().into(),
                observation.evidence.as_str().into(),
                observation.confidence.score().into(),
                observation.provenance.as_str().into(),
            ])
        })
        .collect::<Vec<_>>();
    if !observation_rows.is_empty() {
        transaction.run_script(
            "?[view, revision, from, relation, to, evidence, confidence, provenance] <- $rows\n\
             :put analysis_revision_observation {\
                 view, revision, from, relation, to, evidence => confidence, provenance\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(observation_rows))]),
        )?;
    }
    let diagnostic_rows = resolution
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let candidate = &diagnostic.candidate;
            DataValue::List(vec![
                view.into(),
                revision.into(),
                candidate.local_symbol.as_str().into(),
                candidate.role.as_str().into(),
                candidate.service.as_str().into(),
                candidate.method.as_str().into(),
                candidate.evidence.as_str().into(),
                diagnostic.code.into(),
                diagnostic.detail.as_str().into(),
            ])
        })
        .collect::<Vec<_>>();
    if !diagnostic_rows.is_empty() {
        transaction.run_script(
            "?[view, revision, local_symbol, role, service, method, evidence, code, detail] <- $rows\n\
             :put analysis_revision_grpc_diagnostic {\
                 view, revision, local_symbol, role, service, method, evidence => code, detail\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(diagnostic_rows))]),
        )?;
    }
    Ok(())
}

pub(super) fn publish_observations(
    db: &DbInstance,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
    overrides: &[DependencyOverride],
    verification_fingerprint: Option<&str>,
) -> Result<FactChanges, Box<dyn Error>> {
    if repositories
        .iter()
        .any(|facts| facts.analysis_identity.is_empty())
        || repositories.len() != view.repository_states.len()
        || view.repository_states.iter().any(|state| {
            repositories
                .iter()
                .filter(|facts| facts.state == *state)
                .count()
                != 1
        })
    {
        return Err("repository facts do not match the workspace view".into());
    }
    let resolution = resolve_grpc_bindings(repositories)?;
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        ("fingerprint".into(), view.fingerprint().into()),
    ]);
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        &format!(
            "{DIRECT_RULES}\n\
             ?[from, relation, to, evidence] := \
                 effective_observation[from, to, relation, evidence, _, _]"
        ),
        BTreeMap::from([("view".into(), view.name.clone().into())]),
    )?;
    let current = current
        .rows
        .into_iter()
        .map(|row| {
            let value = |index: usize| {
                row[index]
                    .get_str()
                    .map(str::to_owned)
                    .ok_or("observation contains a non-string value")
            };
            Ok(((value(0)?, value(1)?, value(2)?), value(3)?))
        })
        .collect::<Result<BTreeMap<_, _>, Box<dyn Error>>>()?;
    let override_targets = overrides
        .iter()
        .map(|override_| {
            (
                (
                    override_.from.as_str().to_owned(),
                    override_.relation.as_str().to_owned(),
                    override_.unresolved_to.as_str().to_owned(),
                    override_.evidence.as_str().to_owned(),
                ),
                override_.resolved_to.as_str().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let next = repositories
        .iter()
        .flat_map(|facts| &facts.observations)
        .chain(&resolution.observations)
        .map(|observation| {
            let from = observation.from.as_str().to_owned();
            let relation = observation.relation.as_str().to_owned();
            let unresolved_to = observation.to.as_str().to_owned();
            let evidence = observation.evidence.as_str().to_owned();
            let to = override_targets
                .get(&(
                    from.clone(),
                    relation.clone(),
                    unresolved_to.clone(),
                    evidence.clone(),
                ))
                .cloned()
                .unwrap_or(unresolved_to);
            ((from, relation, to), evidence)
        })
        .collect::<BTreeMap<_, _>>();
    let mut changes = FactChanges::default();
    for (key, evidence) in &next {
        match current.get(key) {
            None => changes.inserted += 1,
            Some(current) if current == evidence => changes.unchanged += 1,
            Some(_) => changes.updated += 1,
        }
    }
    changes.removed = current
        .keys()
        .filter(|key| !next.contains_key(*key))
        .count();

    let mut analyzed_states = Vec::with_capacity(repositories.len());
    for facts in repositories {
        let desired = analyzed_state(facts);
        let state = if state_exists(&transaction, &desired)? {
            desired
        } else if let Some(current) = reusable_current_state(&transaction, view, facts)? {
            current
        } else {
            store_observations(&transaction, &desired, &facts.observations)?;
            store_entities(&transaction, &desired, &facts.entities)?;
            store_grpc_bindings(&transaction, &desired, &facts.grpc_bindings)?;
            desired
        };
        analyzed_states.push(state);
    }
    transaction.run_script(
        "?[view, revision] := \
             *analysis_revision{view: $view, revision: previous}, \
             view = $view, revision = previous + 1\n\
         ?[view, revision] := \
             not *analysis_revision{view: $view}, view = $view, revision = 1\n\
         :put analysis_revision {view => revision}",
        params.clone(),
    )?;
    transaction.run_script(
        "?[view, fingerprint] <- [[$view, $fingerprint]] \
         :put analysis_fingerprint {view => fingerprint}",
        params,
    )?;
    if let Some(fingerprint) = verification_fingerprint {
        transaction.run_script(
            "?[view, fingerprint] <- [[$view, $fingerprint]] \
             :put analysis_verification_fingerprint {view => fingerprint}",
            BTreeMap::from([
                ("view".into(), view.name.clone().into()),
                ("fingerprint".into(), fingerprint.into()),
            ]),
        )?;
    } else {
        transaction.run_script(
            "?[view] <- [[$view]] :rm analysis_verification_fingerprint {view}",
            BTreeMap::from([("view".into(), view.name.clone().into())]),
        )?;
    }
    store_repository_states(&transaction, view, repositories, &analyzed_states)?;
    store_revision_inputs(&transaction, view)?;
    store_analysis_metadata(&transaction, view, repositories)?;
    store_grpc_resolution(&transaction, &view.name, &resolution)?;
    for override_ in overrides {
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("from".into(), override_.from.as_str().into()),
            ("relation".into(), override_.relation.as_str().into()),
            (
                "unresolved_to".into(),
                override_.unresolved_to.as_str().into(),
            ),
            ("resolved_to".into(), override_.resolved_to.as_str().into()),
            ("evidence".into(), override_.evidence.as_str().into()),
            ("confidence".into(), override_.confidence.score().into()),
            ("provenance".into(), override_.provenance.as_str().into()),
        ]);
        transaction.run_script(
            "?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, from = $from, relation = $relation, unresolved_to = $unresolved_to, \
                 resolved_to = $resolved_to, evidence = $evidence\n\
             :put analysis_revision_dependency_override {\
                 view, revision, from, relation, unresolved_to => resolved_to, evidence\
             }",
            params.clone(),
        )?;
        transaction.run_script(
            "?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, from = $from, relation = $relation, \
                 unresolved_to = $unresolved_to, confidence = $confidence, \
                 provenance = $provenance\n\
             :put analysis_revision_dependency_override_metadata {\
                 view, revision, from, relation, unresolved_to => confidence, provenance\
             }",
            params,
        )?;
    }
    carry_forward_enrichments(&transaction, &view.name)?;
    transaction.commit()?;
    Ok(changes)
}

fn store_revision_inputs(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
) -> Result<(), Box<dyn Error>> {
    let revision = transaction
        .run_script(
            "?[revision] := *analysis_revision{view: $view, revision}",
            BTreeMap::from([("view".into(), view.name.clone().into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .ok_or("published analysis revision is missing")?;
    let rows = view
        .repository_states
        .iter()
        .map(|state| {
            DataValue::List(vec![
                view.name.as_str().into(),
                revision.into(),
                state.repository.identity.as_str().into(),
                view.repository_input_fingerprint(state).into(),
            ])
        })
        .collect::<Vec<_>>();
    transaction.run_script(
        "?[view, revision, repository, fingerprint] <- $rows \
         :put analysis_revision_input {view, revision, repository => fingerprint}",
        BTreeMap::from([("rows".into(), DataValue::List(rows))]),
    )?;
    let rows = view
        .enrichment_analyzers()
        .flat_map(|analyzer| {
            view.repository_states.iter().map(move |state| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    state.repository.identity.as_str().into(),
                    analyzer.into(),
                    view.repository_enrichment_input_fingerprint(state, analyzer)
                        .into(),
                ])
            })
        })
        .collect::<Vec<_>>();
    transaction.run_script(
        "?[view, revision, repository, analyzer, fingerprint] <- $rows \
         :put analysis_revision_enrichment_input {\
             view, revision, repository, analyzer => fingerprint\
         }",
        BTreeMap::from([("rows".into(), DataValue::List(rows))]),
    )?;
    let rows = view
        .enrichment_analyzers()
        .flat_map(|analyzer| {
            view.repository_states.iter().flat_map(move |state| {
                view.repository_contexts(&state.repository.identity, analyzer)
                    .iter()
                    .map(move |context| {
                        DataValue::List(vec![
                            view.name.as_str().into(),
                            revision.into(),
                            state.repository.identity.as_str().into(),
                            analyzer.into(),
                            context.as_str().into(),
                        ])
                    })
            })
        })
        .collect::<Vec<_>>();
    transaction.run_script(
        "?[view, revision, target, analyzer, context] <- $rows \
         :put analysis_revision_context {view, revision, target, analyzer, context}",
        BTreeMap::from([("rows".into(), DataValue::List(rows))]),
    )?;
    Ok(())
}

pub(super) fn ensure_revision_inputs(
    db: &DbInstance,
    view: &WorkspaceView,
) -> Result<bool, Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        "?[fingerprint] := *analysis_fingerprint{view: $view, fingerprint}",
        BTreeMap::from([("view".into(), view.name.clone().into())]),
    )?;
    let expected = view.fingerprint();
    if current.rows.first().and_then(|row| row[0].get_str()) != Some(expected.as_str()) {
        transaction.abort()?;
        return Ok(false);
    }
    transaction.run_script(
        "?[view, revision, repository, analyzer] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{view, revision, repository, analyzer} \
         :rm analysis_revision_enrichment_input {view, revision, repository, analyzer}",
        BTreeMap::from([("view".into(), view.name.clone().into())]),
    )?;
    transaction.run_script(
        "?[view, revision, target, analyzer, context] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_context{view, revision, target, analyzer, context} \
         :rm analysis_revision_context {view, revision, target, analyzer, context}",
        BTreeMap::from([("view".into(), view.name.clone().into())]),
    )?;
    store_revision_inputs(&transaction, view)?;
    transaction.commit()?;
    Ok(true)
}

pub(super) fn revision_enrichment_input_fingerprint(
    db: &DbInstance,
    view: &str,
    repository: &str,
    analyzer: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    let rows = db.run_script(
        "?[fingerprint] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, fingerprint\
             }",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("repository".into(), repository.into()),
            ("analyzer".into(), analyzer.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| row[0].get_str())
        .map(str::to_owned))
}

pub(super) fn repository_contexts(
    db: &DbInstance,
    view: &str,
    target: &str,
    analyzer: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let rows = db.run_script(
        "?[context] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_context{\
                 view: $view, revision, target: $target, analyzer: $analyzer, context\
             } \
         :sort context",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("target".into(), target.into()),
            ("analyzer".into(), analyzer.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows
        .rows
        .into_iter()
        .filter_map(|row| row[0].get_str().map(str::to_owned))
        .collect())
}

fn carry_forward_enrichments(
    transaction: &MultiTransaction,
    view: &str,
) -> Result<(), Box<dyn Error>> {
    let valid_owner = "valid_owner[owner] := \
         *analysis_revision{view: $view, revision}, previous = revision - 1, \
         *analysis_revision_repository_enrichment{\
             view: $view, revision: previous, owner, repository, analyzer, input_fingerprint\
         }, \
         *analysis_revision_enrichment_input{\
             view: $view, revision, repository, analyzer, fingerprint: input_fingerprint\
         }\n";
    let scripts = [
        format!(
            "{valid_owner}\
             ?[view, revision, id, analyzer] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[analyzer], \
                 *analysis_revision_enrichment_entity_owner{{\
                     view: $view, revision: previous, id, analyzer\
                 }}, \
                 not *analysis_revision_entity{{view: $view, revision, id}}, view = $view \
             :put analysis_revision_enrichment_entity_owner {{view, revision, id => analyzer}}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, from, relation, to, evidence, analyzer] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[analyzer], \
                 *analysis_revision_enrichment_observation_owner{{\
                     view: $view, revision: previous, from, relation, to, evidence, analyzer\
                 }}, \
                 not *analysis_revision_observation{{\
                     view: $view, revision, from, relation, to, evidence\
                 }}, view = $view \
             :put analysis_revision_enrichment_observation_owner {{\
                 view, revision, from, relation, to, evidence => analyzer\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, from, relation, unresolved_to, analyzer] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[analyzer], \
                 *analysis_revision_enrichment_override_owner{{\
                     view: $view, revision: previous, from, relation, unresolved_to, analyzer\
                 }}, \
                 not *analysis_revision_dependency_override{{\
                     view: $view, revision, from, relation, unresolved_to\
                 }}, view = $view \
             :put analysis_revision_enrichment_override_owner {{\
                 view, revision, from, relation, unresolved_to => analyzer\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, repository, code, severity, path, line, analyzer] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[analyzer], \
                 *analysis_revision_enrichment_diagnostic_owner{{\
                     view: $view, revision: previous, repository, code, severity, path, line, \
                     analyzer\
                 }}, \
                 not *analysis_revision_diagnostic{{\
                     view: $view, revision, repository, code, severity, path, line\
                 }}, view = $view \
             :put analysis_revision_enrichment_diagnostic_owner {{\
                 view, revision, repository, code, severity, path, line => analyzer\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, id, kind, metadata] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_enrichment_entity_owner{{\
                     view: $view, revision, id, analyzer: owner\
                 }}, \
                 *analysis_revision_entity{{\
                     view: $view, revision: previous, id, kind, metadata\
                 }}, view = $view \
             :put analysis_revision_entity {{view, revision, id => kind, metadata}}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, from, relation, to, evidence, confidence, provenance] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_enrichment_observation_owner{{\
                     view: $view, revision, from, relation, to, evidence, analyzer: owner\
                 }}, \
                 *analysis_revision_observation{{\
                     view: $view, revision: previous, from, relation, to, evidence, confidence, \
                     provenance\
                 }}, view = $view \
             :put analysis_revision_observation {{\
                 view, revision, from, relation, to, evidence => confidence, provenance\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_enrichment_override_owner{{\
                     view: $view, revision, from, relation, unresolved_to, analyzer: owner\
                 }}, \
                 *analysis_revision_dependency_override{{\
                     view: $view, revision: previous, from, relation, unresolved_to, resolved_to, \
                     evidence\
                 }}, view = $view \
             :put analysis_revision_dependency_override {{\
                 view, revision, from, relation, unresolved_to => resolved_to, evidence\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_enrichment_override_owner{{\
                     view: $view, revision, from, relation, unresolved_to, analyzer: owner\
                 }}, \
                 *analysis_revision_dependency_override_metadata{{\
                     view: $view, revision: previous, from, relation, unresolved_to, confidence, \
                     provenance\
                 }}, view = $view \
             :put analysis_revision_dependency_override_metadata {{\
                 view, revision, from, relation, unresolved_to => confidence, provenance\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, repository, code, severity, path, line, detail] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_enrichment_diagnostic_owner{{\
                     view: $view, revision, repository, code, severity, path, line, analyzer: owner\
                 }}, \
                 *analysis_revision_diagnostic{{\
                     view: $view, revision: previous, repository, code, severity, path, line, detail\
                 }}, view = $view \
             :put analysis_revision_diagnostic {{\
                 view, revision, repository, code, severity, path, line => detail\
             }}"
        ),
        format!(
            "{valid_owner}\
             ?[view, revision, owner, repository, analyzer, version, input_fingerprint] := \
                 *analysis_revision{{view: $view, revision}}, previous = revision - 1, \
                 valid_owner[owner], \
                 *analysis_revision_repository_enrichment{{\
                     view: $view, revision: previous, owner, repository, analyzer, version, \
                     input_fingerprint\
                 }}, view = $view \
             :put analysis_revision_repository_enrichment {{\
                 view, revision, owner => repository, analyzer, version, input_fingerprint\
             }}"
        ),
    ];
    for script in scripts {
        transaction.run_script(&script, BTreeMap::from([("view".into(), view.into())]))?;
    }
    Ok(())
}

pub(super) fn enrichment_matches(
    db: &DbInstance,
    view: &str,
    repository: &str,
    analyzer: &str,
    version: &str,
) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[version] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, fingerprint\
             }, \
             *analysis_revision_repository_enrichment{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, \
                 version, input_fingerprint: fingerprint\
             }",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("repository".into(), repository.into()),
            ("analyzer".into(), analyzer.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows.rows.first().and_then(|row| row[0].get_str()) == Some(version))
}

pub(super) fn enrichments_current(
    db: &DbInstance,
    view: &str,
    catalog: &[(String, String)],
) -> Result<bool, Box<dyn Error>> {
    if catalog.is_empty() {
        return Ok(true);
    }
    for (analyzer, version) in catalog {
        let missing = db.run_script(
            "matching[repository] := \
                 *analysis_revision{view: $view, revision}, \
                 *analysis_revision_enrichment_input{\
                     view: $view, revision, repository, analyzer: $analyzer, fingerprint\
                 }, \
                 *analysis_revision_repository_enrichment{\
                     view: $view, revision, repository, analyzer: $analyzer, version: $version, \
                     input_fingerprint: fingerprint\
                 }\n\
             ?[repository] := \
                 *analysis_revision{view: $view, revision}, \
                 *analysis_revision_state{view: $view, revision, repository}, \
                 not matching[repository]",
            BTreeMap::from([
                ("view".into(), view.into()),
                ("analyzer".into(), analyzer.as_str().into()),
                ("version".into(), version.as_str().into()),
            ]),
            ScriptMutability::Immutable,
        )?;
        if !missing.rows.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn publish_enrichment(
    db: &DbInstance,
    view: &WorkspaceView,
    repository: &str,
    input_fingerprint: &str,
    owner: EnrichmentOwner<'_>,
    payload: EnrichmentPayload<'_>,
) -> Result<bool, Box<dyn Error>> {
    let EnrichmentOwner {
        analyzer,
        version,
        expected_version,
    } = owner;
    let EnrichmentPayload {
        entities,
        observations,
        overrides,
        diagnostics,
    } = payload;
    if diagnostics
        .iter()
        .any(|(diagnostic_repository, _)| diagnostic_repository != repository)
    {
        return Err("enrichment diagnostic belongs to a different target repository".into());
    }
    let owner = format!("{}:{analyzer}{repository}", analyzer.len());
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        "?[revision, fingerprint] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, fingerprint\
             }",
        BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("repository".into(), repository.into()),
            ("analyzer".into(), analyzer.into()),
        ]),
    )?;
    let Some(row) = current.rows.first() else {
        return Err("published analysis revision is missing".into());
    };
    if row[1].get_str() != Some(input_fingerprint) {
        return Ok(false);
    }
    if let Some(expected_version) = expected_version {
        let expected = transaction.run_script(
            "?[repository] := *analysis_revision{view: $view, revision}, \
                 *analysis_revision_repository_enrichment{\
                     view: $view, revision, owner: $owner, repository, \
                     version: $expected_version, \
                     input_fingerprint: $input_fingerprint\
                 }",
            BTreeMap::from([
                ("view".into(), view.name.clone().into()),
                ("owner".into(), owner.as_str().into()),
                ("expected_version".into(), expected_version.into()),
                ("input_fingerprint".into(), input_fingerprint.into()),
            ]),
        )?;
        if expected.rows.is_empty() {
            return Ok(false);
        }
    }
    let previous = row[0]
        .get_int()
        .ok_or("published analysis revision is invalid")?;
    let revision = previous + 1;
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        ("previous".into(), previous.into()),
        ("revision".into(), revision.into()),
        ("owner".into(), owner.as_str().into()),
    ]);
    transaction.run_script(
        "?[view, revision] <- [[$view, $revision]] \
         :put analysis_revision {view => revision}",
        params.clone(),
    )?;
    for script in [
        "?[view, revision, repository, state] := \
             *analysis_revision_state{view: $view, revision: $previous, repository, state}, \
             view = $view, revision = $revision \
         :put analysis_revision_state {view, revision, repository => state}",
        "?[view, revision, repository, fingerprint] := \
             *analysis_revision_input{view: $view, revision: $previous, repository, fingerprint}, \
             view = $view, revision = $revision \
         :put analysis_revision_input {view, revision, repository => fingerprint}",
        "?[view, revision, repository, analyzer, fingerprint] := \
             *analysis_revision_enrichment_input{\
                 view: $view, revision: $previous, repository, analyzer, fingerprint\
             }, \
             view = $view, revision = $revision \
         :put analysis_revision_enrichment_input {\
             view, revision, repository, analyzer => fingerprint\
         }",
        "?[view, revision, target, analyzer, context] := \
             *analysis_revision_context{\
                 view: $view, revision: $previous, target, analyzer, context\
             }, \
             view = $view, revision = $revision \
         :put analysis_revision_context {view, revision, target, analyzer, context}",
        "?[view, revision, id, kind, metadata] := \
             *analysis_revision_entity{view: $view, revision: $previous, id, kind, metadata}, \
             not *analysis_revision_enrichment_entity_owner{view: $view, revision: $previous, id, analyzer: $owner}, \
             view = $view, revision = $revision \
         :put analysis_revision_entity {view, revision, id => kind, metadata}",
        "?[view, revision, from, relation, to, evidence, confidence, provenance] := \
             *analysis_revision_observation{view: $view, revision: $previous, from, relation, to, evidence, confidence, provenance}, \
             not *analysis_revision_enrichment_observation_owner{view: $view, revision: $previous, from, relation, to, evidence, analyzer: $owner}, \
             view = $view, revision = $revision \
         :put analysis_revision_observation {view, revision, from, relation, to, evidence => confidence, provenance}",
        "?[view, revision, incomplete] := \
             *analysis_revision_metadata{view: $view, revision: $previous, incomplete}, \
             view = $view, revision = $revision \
         :put analysis_revision_metadata {view, revision => incomplete}",
        "?[view, revision, local_symbol, role, service, method, evidence, code, detail] := \
             *analysis_revision_grpc_diagnostic{view: $view, revision: $previous, local_symbol, role, service, method, evidence, code, detail}, \
             view = $view, revision = $revision \
         :put analysis_revision_grpc_diagnostic {view, revision, local_symbol, role, service, method, evidence => code, detail}",
        "?[view, revision, repository, code, severity, path, line, detail] := \
             *analysis_revision_diagnostic{view: $view, revision: $previous, repository, code, severity, path, line, detail}, \
             not *analysis_revision_enrichment_diagnostic_owner{view: $view, revision: $previous, repository, code, severity, path, line, analyzer: $owner}, \
             view = $view, revision = $revision \
         :put analysis_revision_diagnostic {view, revision, repository, code, severity, path, line => detail}",
        "?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
             *analysis_revision_dependency_override{view: $view, revision: $previous, from, relation, unresolved_to, resolved_to, evidence}, \
             not *analysis_revision_enrichment_override_owner{view: $view, revision: $previous, from, relation, unresolved_to, analyzer: $owner}, \
             view = $view, revision = $revision \
         :put analysis_revision_dependency_override {view, revision, from, relation, unresolved_to => resolved_to, evidence}",
        "?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
             *analysis_revision_dependency_override_metadata{view: $view, revision: $previous, from, relation, unresolved_to, confidence, provenance}, \
             not *analysis_revision_enrichment_override_owner{view: $view, revision: $previous, from, relation, unresolved_to, analyzer: $owner}, \
             view = $view, revision = $revision \
         :put analysis_revision_dependency_override_metadata {view, revision, from, relation, unresolved_to => confidence, provenance}",
        "?[view, revision, analyzer, version] := \
             *analysis_revision_enrichment{view: $view, revision: $previous, analyzer, version}, \
             view = $view, revision = $revision \
         :put analysis_revision_enrichment {view, revision, analyzer => version}",
        "?[view, revision, owner, repository, analyzer, version, input_fingerprint] := \
             *analysis_revision_repository_enrichment{\
                 view: $view, revision: $previous, owner, repository, analyzer, version, \
                 input_fingerprint\
             }, owner != $owner, view = $view, revision = $revision \
         :put analysis_revision_repository_enrichment {\
             view, revision, owner => repository, analyzer, version, input_fingerprint\
         }",
        "?[view, revision, from, relation, unresolved_to, analyzer] := \
             *analysis_revision_enrichment_override_owner{view: $view, revision: $previous, from, relation, unresolved_to, analyzer}, \
             analyzer != $owner, view = $view, revision = $revision \
         :put analysis_revision_enrichment_override_owner {view, revision, from, relation, unresolved_to => analyzer}",
        "?[view, revision, repository, code, severity, path, line, analyzer] := \
             *analysis_revision_enrichment_diagnostic_owner{view: $view, revision: $previous, repository, code, severity, path, line, analyzer}, \
             analyzer != $owner, view = $view, revision = $revision \
         :put analysis_revision_enrichment_diagnostic_owner {view, revision, repository, code, severity, path, line => analyzer}",
        "?[view, revision, id, analyzer] := \
             *analysis_revision_enrichment_entity_owner{view: $view, revision: $previous, id, analyzer}, \
             analyzer != $owner, view = $view, revision = $revision \
         :put analysis_revision_enrichment_entity_owner {view, revision, id => analyzer}",
        "?[view, revision, from, relation, to, evidence, analyzer] := \
             *analysis_revision_enrichment_observation_owner{view: $view, revision: $previous, from, relation, to, evidence, analyzer}, \
             analyzer != $owner, view = $view, revision = $revision \
         :put analysis_revision_enrichment_observation_owner {view, revision, from, relation, to, evidence => analyzer}",
    ] {
        transaction.run_script(script, params.clone())?;
    }
    for entities in entities.chunks(FACT_BATCH_SIZE) {
        let rows = entities
            .iter()
            .map(|entity| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    entity.id.as_str().into(),
                    entity_kind(entity.kind).into(),
                    entity_metadata(entity.metadata).into(),
                ])
            })
            .collect::<Vec<_>>();
        let values = BTreeMap::from([
            ("rows".into(), DataValue::List(rows)),
            ("view".into(), view.name.clone().into()),
            ("previous".into(), previous.into()),
            ("analyzer".into(), owner.as_str().into()),
        ]);
        transaction.run_script(
            "rows[view, revision, id, kind, metadata] <- $rows \
             incoming[view, revision, id, kind, metadata] := \
                 rows[view, revision, id, kind, metadata], \
                 not *analysis_revision_entity{view: $view, revision: $previous, id} \
             incoming[view, revision, id, kind, metadata] := \
                 rows[view, revision, id, kind, metadata], \
                 *analysis_revision_enrichment_entity_owner{view: $view, revision: $previous, id, analyzer: $analyzer} \
             ?[view, revision, id, kind, metadata] := incoming[view, revision, id, kind, metadata] \
             :put analysis_revision_entity {view, revision, id => kind, metadata}",
            values.clone(),
        )?;
        transaction.run_script(
            "rows[view, revision, id, kind, metadata] <- $rows \
             incoming[view, revision, id] := \
                 rows[view, revision, id, _, _], \
                 not *analysis_revision_entity{view: $view, revision: $previous, id} \
             incoming[view, revision, id] := \
                 rows[view, revision, id, _, _], \
                 *analysis_revision_enrichment_entity_owner{view: $view, revision: $previous, id, analyzer: $analyzer} \
             ?[view, revision, id, analyzer] := \
                 incoming[view, revision, id], analyzer = $analyzer \
             :put analysis_revision_enrichment_entity_owner {view, revision, id => analyzer}",
            values,
        )?;
    }
    for observations in observations.chunks(FACT_BATCH_SIZE) {
        let rows = observations
            .iter()
            .map(|observation| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    observation.from.as_str().into(),
                    observation.relation.as_str().into(),
                    observation.to.as_str().into(),
                    observation.evidence.as_str().into(),
                    observation.confidence.score().into(),
                    observation.provenance.as_str().into(),
                ])
            })
            .collect::<Vec<_>>();
        let values = BTreeMap::from([
            ("rows".into(), DataValue::List(rows)),
            ("view".into(), view.name.clone().into()),
            ("previous".into(), previous.into()),
            ("analyzer".into(), owner.as_str().into()),
        ]);
        transaction.run_script(
            "rows[view, revision, from, relation, to, evidence, confidence, provenance] <- $rows \
             incoming[view, revision, from, relation, to, evidence, confidence, provenance] := \
                 rows[view, revision, from, relation, to, evidence, confidence, provenance], \
                 not *analysis_revision_observation{view: $view, revision: $previous, from, relation, to, evidence} \
             incoming[view, revision, from, relation, to, evidence, confidence, provenance] := \
                 rows[view, revision, from, relation, to, evidence, confidence, provenance], \
                 *analysis_revision_enrichment_observation_owner{view: $view, revision: $previous, from, relation, to, evidence, analyzer: $analyzer} \
             ?[view, revision, from, relation, to, evidence, confidence, provenance] := \
                 incoming[view, revision, from, relation, to, evidence, confidence, provenance] \
             :put analysis_revision_observation {view, revision, from, relation, to, evidence => confidence, provenance}",
            values.clone(),
        )?;
        transaction.run_script(
            "rows[view, revision, from, relation, to, evidence, confidence, provenance] <- $rows \
             incoming[view, revision, from, relation, to, evidence] := \
                 rows[view, revision, from, relation, to, evidence, _, _], \
                 not *analysis_revision_observation{view: $view, revision: $previous, from, relation, to, evidence} \
             incoming[view, revision, from, relation, to, evidence] := \
                 rows[view, revision, from, relation, to, evidence, _, _], \
                 *analysis_revision_enrichment_observation_owner{view: $view, revision: $previous, from, relation, to, evidence, analyzer: $analyzer} \
             ?[view, revision, from, relation, to, evidence, analyzer] := \
                 incoming[view, revision, from, relation, to, evidence], analyzer = $analyzer \
             :put analysis_revision_enrichment_observation_owner {view, revision, from, relation, to, evidence => analyzer}",
            values,
        )?;
    }
    for overrides in overrides.chunks(FACT_BATCH_SIZE) {
        let rows = overrides
            .iter()
            .map(|override_| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    override_.from.as_str().into(),
                    override_.relation.as_str().into(),
                    override_.unresolved_to.as_str().into(),
                    override_.resolved_to.as_str().into(),
                    override_.evidence.as_str().into(),
                    override_.confidence.score().into(),
                    override_.provenance.as_str().into(),
                ])
            })
            .collect();
        let values = BTreeMap::from([("rows".into(), DataValue::List(rows))]);
        transaction.run_script(
            "rows[view, revision, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows \
             ?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
                 rows[view, revision, from, relation, unresolved_to, resolved_to, evidence, _, _] \
             :put analysis_revision_dependency_override {view, revision, from, relation, unresolved_to => resolved_to, evidence}",
            values.clone(),
        )?;
        transaction.run_script(
            "rows[view, revision, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows \
             ?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
                 rows[view, revision, from, relation, unresolved_to, _, _, confidence, provenance] \
             :put analysis_revision_dependency_override_metadata {view, revision, from, relation, unresolved_to => confidence, provenance}",
            values.clone(),
        )?;
        transaction.run_script(
            "rows[view, revision, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows \
             ?[view, revision, from, relation, unresolved_to, analyzer] := \
                 rows[view, revision, from, relation, unresolved_to, _, _, _, _], analyzer = $analyzer \
             :put analysis_revision_enrichment_override_owner {view, revision, from, relation, unresolved_to => analyzer}",
            BTreeMap::from([
                ("rows".into(), values["rows"].clone()),
                ("analyzer".into(), owner.as_str().into()),
            ]),
        )?;
    }
    if !diagnostics.is_empty() {
        let rows: Vec<DataValue> = diagnostics
            .iter()
            .map(|(repository, diagnostic)| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    repository.as_str().into(),
                    diagnostic.code.as_str().into(),
                    diagnostic.severity.as_str().into(),
                    diagnostic.path.to_string_lossy().into_owned().into(),
                    i64::from(diagnostic.line.unwrap_or_default()).into(),
                    diagnostic.detail.as_deref().unwrap_or_default().into(),
                ])
            })
            .collect();
        transaction.run_script(
            "?[view, revision, repository, code, severity, path, line, detail] <- $rows \
             :put analysis_revision_diagnostic {view, revision, repository, code, severity, path, line => detail}",
            BTreeMap::from([("rows".into(), DataValue::List(rows.clone()))]),
        )?;
        transaction.run_script(
            "rows[view, revision, repository, code, severity, path, line, detail] <- $rows \
             ?[view, revision, repository, code, severity, path, line, analyzer] := \
                 rows[view, revision, repository, code, severity, path, line, _], analyzer = $analyzer \
             :put analysis_revision_enrichment_diagnostic_owner {view, revision, repository, code, severity, path, line => analyzer}",
            BTreeMap::from([
                ("rows".into(), DataValue::List(rows)),
                ("analyzer".into(), owner.as_str().into()),
            ]),
        )?;
    }
    transaction.run_script(
        "?[view, revision, analyzer, version] <- [[$view, $revision, $analyzer, $version]] \
         :put analysis_revision_enrichment {view, revision, analyzer => version}",
        BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("revision".into(), revision.into()),
            ("analyzer".into(), owner.as_str().into()),
            ("version".into(), version.into()),
        ]),
    )?;
    transaction.run_script(
        "?[view, revision, owner, repository, analyzer, version, input_fingerprint] <- \
             [[$view, $revision, $owner, $repository, $analyzer, $version, $input_fingerprint]] \
         :put analysis_revision_repository_enrichment {\
             view, revision, owner => repository, analyzer, version, input_fingerprint\
         }",
        BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("revision".into(), revision.into()),
            ("owner".into(), owner.into()),
            ("repository".into(), repository.into()),
            ("analyzer".into(), analyzer.into()),
            ("version".into(), version.into()),
            ("input_fingerprint".into(), input_fingerprint.into()),
        ]),
    )?;
    transaction.commit()?;
    Ok(true)
}

fn store_analysis_metadata(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
) -> Result<(), Box<dyn Error>> {
    let revision = transaction
        .run_script(
            "?[revision] := *analysis_revision{view: $view, revision}",
            BTreeMap::from([("view".into(), view.name.clone().into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .ok_or("published analysis revision is missing")?;
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        (
            "incomplete".into(),
            repositories.iter().any(|facts| facts.incomplete).into(),
        ),
    ]);
    transaction.run_script(
        "?[view, revision, incomplete] := \
             *analysis_revision{view: $view, revision}, \
             view = $view, incomplete = $incomplete\n\
         :put analysis_revision_metadata {view, revision => incomplete}",
        params,
    )?;

    let rows = repositories
        .iter()
        .flat_map(|facts| {
            facts.diagnostics.iter().map(|diagnostic| {
                DataValue::List(vec![
                    view.name.as_str().into(),
                    revision.into(),
                    facts.state.repository.identity.as_str().into(),
                    diagnostic.code.as_str().into(),
                    diagnostic.severity.as_str().into(),
                    diagnostic.path.to_string_lossy().into_owned().into(),
                    i64::from(diagnostic.line.unwrap_or_default()).into(),
                    diagnostic.detail.as_deref().unwrap_or_default().into(),
                ])
            })
        })
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        transaction.run_script(
            "?[view, revision, repository, code, severity, path, line, detail] <- $rows\n\
             :put analysis_revision_diagnostic {\
                 view, revision, repository, code, severity, path, line => detail\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    Ok(())
}

pub(super) fn store_repository_states(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
    analyzed_states: &[String],
) -> Result<(), Box<dyn Error>> {
    for (facts, analyzed_state) in repositories.iter().zip(analyzed_states) {
        store_repository_state(transaction, facts, analyzed_state)?;
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            (
                "repository".into(),
                facts.state.repository.identity.clone().into(),
            ),
            ("state".into(), analyzed_state.as_str().into()),
        ]);
        transaction.run_script(
            "?[view, revision, repository, state] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, repository = $repository, state = $state\n\
             :put analysis_revision_state {view, revision, repository => state}",
            params,
        )?;
    }
    Ok(())
}

fn store_repository_state(
    transaction: &MultiTransaction,
    facts: &RepositoryFacts,
    analyzed_state: &str,
) -> Result<(), Box<dyn Error>> {
    transaction.run_script(
        "?[fingerprint, repository, head] <- [[$state, $repository, $head]]\n\
         :put repository_state {fingerprint => repository, head}",
        BTreeMap::from([
            ("state".into(), analyzed_state.into()),
            (
                "repository".into(),
                facts.state.repository.identity.clone().into(),
            ),
            (
                "head".into(),
                facts.state.head.clone().unwrap_or_default().into(),
            ),
        ]),
    )?;
    Ok(())
}

pub(super) fn claim_garbage_collection(db: &DbInstance) -> Result<u64, Box<dyn Error>> {
    for attempt in 0..=GARBAGE_COLLECTION_TRANSACTION_RETRIES {
        match claim_garbage_collection_once(db) {
            Ok(claimed) => return Ok(claimed),
            Err(_) if attempt < GARBAGE_COLLECTION_TRANSACTION_RETRIES => {
                thread::sleep(GARBAGE_COLLECTION_TRANSACTION_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn claim_garbage_collection_once(db: &DbInstance) -> Result<u64, Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    let stale = transaction.run_script(
        "live_state[state] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}\n\
         live_state[state] := *repository_revision{analyzed_state: state}\n\
         ?[state, repository, head] := \
             *repository_state{fingerprint: state, repository, head}, not live_state[state]",
        BTreeMap::new(),
    )?;
    if stale.rows.is_empty() {
        transaction.abort()?;
        return Ok(0);
    }
    let states = DataValue::List(
        stale
            .rows
            .iter()
            .map(|row| DataValue::List(vec![row[0].clone()]))
            .collect(),
    );
    transaction.run_script(
        "?[state, repository, head] <- $rows \
         :put garbage_collection_state {state => repository, head}",
        BTreeMap::from([(
            "rows".into(),
            DataValue::List(
                stale
                    .rows
                    .clone()
                    .into_iter()
                    .map(DataValue::List)
                    .collect(),
            ),
        )]),
    )?;
    transaction.run_script(
        "?[fingerprint] <- $states :rm repository_state {fingerprint}",
        BTreeMap::from([("states".into(), states)]),
    )?;
    transaction.commit()?;
    Ok(stale.rows.len().try_into()?)
}

pub(super) fn garbage_collection_pending(db: &DbInstance) -> Result<bool, Box<dyn Error>> {
    Ok(!db
        .run_script(
            "?[state] := *garbage_collection_state{state}\n:limit 1",
            BTreeMap::new(),
            ScriptMutability::Immutable,
        )?
        .rows
        .is_empty())
}

pub(super) fn garbage_collection_candidates(db: &DbInstance) -> Result<u64, Box<dyn Error>> {
    db.run_script(
        "live_state[state] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}\n\
         live_state[state] := *repository_revision{analyzed_state: state}\n\
         ?[count(state)] := \
             *repository_state{fingerprint: state}, not live_state[state]",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?
    .rows
    .first()
    .and_then(|row| row[0].get_int())
    .unwrap_or_default()
    .try_into()
    .map_err(Into::into)
}

pub(super) fn garbage_collection_queued(db: &DbInstance) -> Result<u64, Box<dyn Error>> {
    db.run_script(
        "?[count(state)] := *garbage_collection_state{state}",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?
    .rows
    .first()
    .and_then(|row| row[0].get_int())
    .unwrap_or_default()
    .try_into()
    .map_err(Into::into)
}

struct RelationCleanup<'a> {
    step: String,
    select_script: String,
    relation: &'a str,
    keys: &'a str,
    parameters: BTreeMap<String, DataValue>,
    guard_repository_state: bool,
    stale_states: u32,
    repositories: u32,
    completed_steps: u32,
    total_steps: u32,
}

fn sweep_relation(
    db: &DbInstance,
    progress: &mut impl FnMut(GarbageCollectionProgress) -> bool,
    cleanup: RelationCleanup<'_>,
) -> Result<bool, Box<dyn Error>> {
    let mut completed_rows = 0;
    if !progress(GarbageCollectionProgress {
        phase: GarbageCollectionPhase::SweepingObsoleteStates,
        step: Some(cleanup.step.clone()),
        rows: None,
        completed_rows: Some(completed_rows),
        stale_states: Some(cleanup.stale_states),
        repositories: Some(cleanup.repositories),
        completed_steps: cleanup.completed_steps,
        total_steps: cleanup.total_steps,
    }) {
        return Ok(false);
    }
    loop {
        let batch = db
            .run_script(
                &format!(
                    "{}\n:limit {GARBAGE_COLLECTION_BATCH_SIZE}",
                    cleanup.select_script
                ),
                cleanup.parameters.clone(),
                ScriptMutability::Immutable,
            )?
            .rows;
        if batch.is_empty() {
            return Ok(true);
        }
        if !remove_garbage_collection_batch(db, &cleanup, &batch)? {
            return Ok(true);
        }
        let batch_size: u64 = batch.len().try_into()?;
        completed_rows += batch_size;
        if !progress(GarbageCollectionProgress {
            phase: GarbageCollectionPhase::SweepingObsoleteStates,
            step: Some(cleanup.step.clone()),
            rows: None,
            completed_rows: Some(completed_rows),
            stale_states: Some(cleanup.stale_states),
            repositories: Some(cleanup.repositories),
            completed_steps: cleanup.completed_steps,
            total_steps: cleanup.total_steps,
        }) {
            return Ok(false);
        }
    }
}

fn remove_garbage_collection_batch(
    db: &DbInstance,
    cleanup: &RelationCleanup<'_>,
    batch: &[Vec<DataValue>],
) -> Result<bool, Box<dyn Error>> {
    for attempt in 0..=GARBAGE_COLLECTION_TRANSACTION_RETRIES {
        match remove_garbage_collection_batch_once(db, cleanup, batch) {
            Ok(removed) => return Ok(removed),
            Err(_) if attempt < GARBAGE_COLLECTION_TRANSACTION_RETRIES => {
                thread::sleep(GARBAGE_COLLECTION_TRANSACTION_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn remove_garbage_collection_batch_once(
    db: &DbInstance,
    cleanup: &RelationCleanup<'_>,
    batch: &[Vec<DataValue>],
) -> Result<bool, Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    if cleanup.guard_repository_state
        && !transaction
            .run_script(
                "?[fingerprint] := \
                     *repository_state{fingerprint: $state}, fingerprint = $state\n\
                 :limit 1",
                cleanup.parameters.clone(),
            )?
            .rows
            .is_empty()
    {
        transaction.abort()?;
        return Ok(false);
    }
    transaction.run_script(
        &format!(
            "?[{}] <- $rows\n:rm {} {{{}}}",
            cleanup.keys, cleanup.relation, cleanup.keys
        ),
        BTreeMap::from([(
            "rows".into(),
            DataValue::List(batch.iter().cloned().map(DataValue::List).collect()),
        )]),
    )?;
    transaction.commit()?;
    Ok(true)
}

pub(super) fn sweep_garbage_collection(
    db: &DbInstance,
    progress: &mut impl FnMut(GarbageCollectionProgress) -> bool,
) -> Result<u64, Box<dyn Error>> {
    let queued = db.run_script(
        "?[state, repository] := *garbage_collection_state{state, repository}",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?;
    let stale_states = queued.rows.len().try_into()?;
    let repositories = queued
        .rows
        .iter()
        .map(|row| {
            row[1]
                .get_str()
                .ok_or("stale repository state has a non-string repository")
        })
        .collect::<Result<BTreeSet<_>, _>>()?
        .len()
        .try_into()?;
    let state_steps = [
        (
            "stale observations",
            "?[state, from, relation, to] := \
                 *state_observation{state: $state, from, relation, to}, state = $state",
            "state_observation",
            "state, from, relation, to",
        ),
        (
            "stale entities",
            "?[state, id] := *state_entity{state: $state, id}, state = $state",
            "state_entity",
            "state, id",
        ),
        (
            "stale gRPC binding candidates",
            "?[state, local_symbol, role, service, method, evidence] := \
                 *state_grpc_binding_candidate{\
                     state: $state, local_symbol, role, service, method, evidence\
                 }, state = $state",
            "state_grpc_binding_candidate",
            "state, local_symbol, role, service, method, evidence",
        ),
        (
            "stale dependency observations",
            "?[state, from, relation, to] := \
                 *state_dependency_observation{\
                     state: $state, from, relation, to\
                 }, state = $state",
            "state_dependency_observation",
            "state, from, relation, to",
        ),
        (
            "stale observation metadata",
            "?[state, from, relation, to] := \
                 *state_observation_metadata{\
                     state: $state, from, relation, to\
                 }, state = $state",
            "state_observation_metadata",
            "state, from, relation, to",
        ),
    ];

    let revision_steps = [
        (
            "superseded revision repository states",
            "analysis_revision_state",
            "view, revision, repository",
        ),
        (
            "superseded revision inputs",
            "analysis_revision_input",
            "view, revision, repository",
        ),
        (
            "superseded revision enrichment inputs",
            "analysis_revision_enrichment_input",
            "view, revision, repository, analyzer",
        ),
        (
            "superseded revision contexts",
            "analysis_revision_context",
            "view, revision, target, analyzer, context",
        ),
        (
            "superseded revision metadata",
            "analysis_revision_metadata",
            "view, revision",
        ),
        (
            "superseded revision diagnostics",
            "analysis_revision_diagnostic",
            "view, revision, repository, code, severity, path, line",
        ),
        (
            "superseded revision enrichments",
            "analysis_revision_enrichment",
            "view, revision, analyzer",
        ),
        (
            "superseded repository enrichments",
            "analysis_revision_repository_enrichment",
            "view, revision, owner",
        ),
        (
            "superseded revision enrichment override owners",
            "analysis_revision_enrichment_override_owner",
            "view, revision, from, relation, unresolved_to",
        ),
        (
            "superseded revision enrichment diagnostic owners",
            "analysis_revision_enrichment_diagnostic_owner",
            "view, revision, repository, code, severity, path, line",
        ),
        (
            "superseded revision enrichment entity owners",
            "analysis_revision_enrichment_entity_owner",
            "view, revision, id",
        ),
        (
            "superseded revision enrichment observation owners",
            "analysis_revision_enrichment_observation_owner",
            "view, revision, from, relation, to, evidence",
        ),
        (
            "superseded revision dependency overrides",
            "analysis_revision_dependency_override",
            "view, revision, from, relation, unresolved_to",
        ),
        (
            "superseded revision dependency override metadata",
            "analysis_revision_dependency_override_metadata",
            "view, revision, from, relation, unresolved_to",
        ),
        (
            "superseded revision observations",
            "analysis_revision_observation",
            "view, revision, from, relation, to, evidence",
        ),
        (
            "superseded revision entities",
            "analysis_revision_entity",
            "view, revision, id",
        ),
        (
            "superseded revision gRPC diagnostics",
            "analysis_revision_grpc_diagnostic",
            "view, revision, local_symbol, role, service, method, evidence",
        ),
    ];
    let total_states = stale_states;
    let mut states_resolved = 0;
    for row in queued.rows {
        let state = row[0]
            .get_str()
            .ok_or("garbage collection state has a non-string fingerprint")?;
        let repository = row[1]
            .get_str()
            .ok_or("garbage collection state has a non-string repository")?;
        for (step, select_script, relation, keys) in state_steps {
            if !sweep_relation(
                db,
                progress,
                RelationCleanup {
                    step: format!("{step} from {repository} state {state}"),
                    select_script: select_script.into(),
                    relation,
                    keys,
                    parameters: BTreeMap::from([("state".into(), state.into())]),
                    guard_repository_state: true,
                    stale_states,
                    repositories,
                    completed_steps: states_resolved,
                    total_steps: total_states,
                },
            )? {
                return Ok(states_resolved.into());
            }
        }
        db.run_script(
            "?[state] := state = $state\n:rm garbage_collection_state {state}",
            BTreeMap::from([("state".into(), state.into())]),
            ScriptMutability::Mutable,
        )?;
        states_resolved += 1;
    }
    let current_revisions = db.run_script(
        "?[view, revision] := *analysis_revision{view, revision}",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?;
    let revision_steps_per_view: u32 = revision_steps.len().try_into()?;
    let total_revision_steps = u32::try_from(current_revisions.rows.len())?
        .checked_mul(revision_steps_per_view)
        .ok_or("garbage collection revision step count overflow")?;
    for (view_index, row) in current_revisions.rows.into_iter().enumerate() {
        let view = row[0]
            .get_str()
            .ok_or("analysis revision has a non-string view")?;
        let current = row[1].clone();
        for (relation_index, (step, relation, keys)) in revision_steps.into_iter().enumerate() {
            let completed_steps = u32::try_from(view_index)?
                .checked_mul(revision_steps_per_view)
                .and_then(|steps| steps.checked_add(u32::try_from(relation_index).ok()?))
                .ok_or("garbage collection revision step count overflow")?;
            if !sweep_relation(
                db,
                progress,
                RelationCleanup {
                    step: format!("{step} from {view}"),
                    select_script: format!(
                        "?[{keys}] := \
                             *{relation}{{{keys}}}, \
                             view = $view, revision < $current"
                    ),
                    relation,
                    keys,
                    parameters: BTreeMap::from([
                        ("view".into(), view.into()),
                        ("current".into(), current.clone()),
                    ]),
                    guard_repository_state: false,
                    stale_states,
                    repositories,
                    completed_steps,
                    total_steps: total_revision_steps,
                },
            )? {
                return Ok(states_resolved.into());
            }
        }
    }
    Ok(states_resolved.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnrichmentOwner, EnrichmentPayload, SemanticStore};
    use beholder_domain::{
        AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
        DependencyRelation, EntityFact, EntityKind, EntityMetadata, FactChanges,
        GrpcBindingCandidate, GrpcBindingRole, LogicalRepository, Observation, ProtoTypeKind,
        Provenance, RepositoryFacts, RepositoryState, RpcCardinality, StructuralRelation,
        WorkspaceView,
    };
    use mnestic_engine::ScriptMutability;
    use std::{
        collections::BTreeMap,
        fs,
        path::Path,
        time::{Instant, SystemTime},
    };
    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations,
        }
    }

    fn with_enrichment_analyzers(view: WorkspaceView, analyzers: &[&str]) -> WorkspaceView {
        view.with_repository_contexts(
            analyzers
                .iter()
                .map(|analyzer| ((*analyzer).into(), BTreeMap::new()))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn persists_typed_entity_facts_with_repository_state() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "main",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "contracts".into(),
                },
                head: Some("head".into()),
                fingerprint: "descriptor".into(),
            }],
        )
        .unwrap();
        let mut facts = facts(&view, Vec::new());
        facts.entities.push(
            EntityFact::new(
                "proto-type://pricing.v1.Quote",
                EntityKind::ProtoType,
                Some(EntityMetadata::ProtoType {
                    kind: ProtoTypeKind::Message,
                }),
            )
            .unwrap(),
        );
        facts.entities.push(
            EntityFact::new("repo://example/rust/unrelated", EntityKind::Callable, None).unwrap(),
        );
        facts.entities.push(
            EntityFact::new(
                "graphql-argument://Mutation/createOrder/input",
                EntityKind::GraphqlArgument,
                None,
            )
            .unwrap(),
        );
        facts.entities.push(
            EntityFact::new(
                "graphql-enum-value://OrderMode/PREVIEW",
                EntityKind::GraphqlEnumValue,
                None,
            )
            .unwrap(),
        );
        facts.entities.push(
            EntityFact::new(
                "graphql-operation://CreateOrder",
                EntityKind::GraphqlOperation,
                Some(EntityMetadata::GraphqlOperation {
                    kind: GraphqlOperationKind::Mutation,
                }),
            )
            .unwrap(),
        );
        let state = analyzed_state(&facts);
        store.publish(&view, &[facts], &[]).unwrap();

        let rows = store
            .db
            .run_script(
                "?[kind, metadata] := *state_entity{state: $state, id: 'proto-type://pricing.v1.Quote', kind, metadata}",
                BTreeMap::from([("state".into(), state.into())]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![["proto_type".into(), "proto_type:message".into()]]
        );
        let context = store
            .context("main", "proto-type://pricing.v1.Quote")
            .unwrap();
        let root = context.root;
        assert_eq!(root.kind, beholder_dto::EntityKind::ProtoMessage);
        assert_eq!(
            root.metadata,
            Some(beholder_dto::EntityMetadata::ProtoType {
                type_kind: beholder_dto::ProtoTypeKind::Message,
            })
        );
        assert_eq!(context.nodes.len(), 1);
        assert_eq!(
            store
                .context("main", "graphql-argument://Mutation/createOrder/input")
                .unwrap()
                .root
                .kind,
            beholder_dto::EntityKind::GraphqlArgument
        );
        assert_eq!(
            store
                .context("main", "graphql-enum-value://OrderMode/PREVIEW")
                .unwrap()
                .root
                .kind,
            beholder_dto::EntityKind::GraphqlEnumValue
        );
        assert_eq!(
            store
                .context("main", "graphql-operation://CreateOrder")
                .unwrap()
                .root
                .metadata,
            Some(beholder_dto::EntityMetadata::GraphqlOperation {
                operation_kind: beholder_dto::GraphqlOperationKind::Mutation,
            })
        );
    }

    #[test]
    fn stores_entities_across_batch_boundaries() {
        let store = SemanticStore::memory().unwrap();
        let transaction = store.db.multi_transaction(true);
        let entities = (0..=FACT_BATCH_SIZE)
            .map(|index| {
                EntityFact::new(
                    format!("repo://example/entity/{index}"),
                    EntityKind::Callable,
                    None,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        store_entities(&transaction, "state", &entities).unwrap();
        transaction.commit().unwrap();

        let rows = store
            .db
            .run_script(
                "?[count(id)] := *state_entity{state: 'state', id}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(
            rows.rows[0][0].get_int(),
            Some((FACT_BATCH_SIZE + 1) as i64)
        );
    }

    #[test]
    fn stores_one_row_for_duplicate_semantic_edges() {
        let store = SemanticStore::memory().unwrap();
        let transaction = store.db.multi_transaction(true);
        let observations = vec![
            Observation::dependency(
                "repo/source",
                DependencyRelation::Calls,
                "repo/target",
                "src/lib.rs:1",
            ),
            Observation::dependency(
                "repo/source",
                DependencyRelation::Calls,
                "repo/target",
                "src/lib.rs:2",
            ),
        ];

        store_observations(&transaction, "state", &observations).unwrap();
        transaction.commit().unwrap();

        let rows = store
            .db
            .run_script(
                "?[evidence] := *state_observation{state: 'state', evidence}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(rows.rows, vec![vec!["src/lib.rs:2".into()]]);
    }

    #[test]
    fn resolves_grpc_candidates_at_publication_and_republishes_unmatched() {
        let store = SemanticStore::memory().unwrap();
        let application_state = RepositoryState {
            repository: LogicalRepository {
                identity: "application".into(),
            },
            head: Some("app-head".into()),
            fingerprint: "app-state".into(),
        };
        let contract_state = RepositoryState {
            repository: LogicalRepository {
                identity: "contracts".into(),
            },
            head: Some("contract-head".into()),
            fingerprint: "contract-state".into(),
        };
        let view = WorkspaceView::new(
            "grpc",
            "analysis",
            vec![application_state.clone(), contract_state.clone()],
        )
        .unwrap();
        let candidate = |local_symbol: &str,
                         role: GrpcBindingRole,
                         evidence: &str,
                         confidence: Confidence| GrpcBindingCandidate {
            local_symbol: local_symbol.into(),
            role,
            service: "pricing.v1.Pricing".into(),
            method: "GetQuote".into(),
            cardinality: RpcCardinality::Unary,
            evidence: evidence.into(),
            confidence,
            provenance: Provenance::Ast,
        };
        let application = RepositoryFacts {
            state: application_state.clone(),
            analysis_identity: "rust".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: vec![
                candidate(
                    "repo://application/rust/client/get_quote",
                    GrpcBindingRole::Client,
                    "src/client.rs:12",
                    Confidence::Inferred,
                ),
                candidate(
                    "repo://application/rust/server/get_quote",
                    GrpcBindingRole::Server,
                    "src/server.rs:24",
                    Confidence::Exact,
                ),
            ],
            observations: Vec::new(),
        };
        let contracts = RepositoryFacts {
            state: contract_state,
            analysis_identity: "protobuf".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: vec![
                EntityFact::new(
                    "proto-method://pricing.v1.Pricing/GetQuote",
                    EntityKind::ProtoMethod,
                    Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::Unary,
                    }),
                )
                .unwrap(),
            ],
            grpc_bindings: Vec::new(),
            observations: Vec::new(),
        };
        store
            .publish(&view, &[application.clone(), contracts], &[])
            .unwrap();

        let operation = "grpc://pricing.v1.Pricing/GetQuote";
        let context = store.context("grpc", operation).unwrap();
        assert_eq!(context.root.kind, beholder_dto::EntityKind::Rpc);
        for kind in [
            beholder_dto::RelationKind::BindsContract,
            beholder_dto::RelationKind::CallsRpc,
            beholder_dto::RelationKind::ImplementedBy,
        ] {
            assert!(context.edges.iter().any(|edge| edge.kind == kind));
        }
        let binding = context
            .edges
            .iter()
            .find(|edge| edge.kind == beholder_dto::RelationKind::BindsContract)
            .unwrap();
        assert_eq!(binding.confidence, 1.0);
        assert_eq!(binding.evidence.len(), 2);
        let impacted = store
            .impact("grpc", "proto-method://pricing.v1.Pricing/GetQuote", 32)
            .unwrap();
        for expected in [
            operation,
            "repo://application/rust/client/get_quote",
            "repo://application/rust/server/get_quote",
        ] {
            assert!(
                impacted.nodes.iter().any(|node| node.id == expected),
                "missing {expected} from {:#?}",
                impacted.nodes
            );
        }

        let replacement_contract_state = RepositoryState {
            fingerprint: "contract-without-method".into(),
            ..view.repository_states[1].clone()
        };
        let replacement_view = WorkspaceView::new(
            "grpc",
            "analysis",
            vec![application_state, replacement_contract_state.clone()],
        )
        .unwrap();
        store
            .publish(
                &replacement_view,
                &[
                    application,
                    RepositoryFacts {
                        state: replacement_contract_state,
                        analysis_identity: "protobuf".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: Vec::new(),
                    },
                ],
                &[],
            )
            .unwrap();

        assert!(store.context("grpc", operation).unwrap().edges.is_empty());
        let bindings = format!("{:?}", store.inspect_grpc_bindings().unwrap());
        assert!(bindings.contains("grpc.contract_unmatched"));
        assert!(bindings.contains("src/client.rs:12"));
        assert!(bindings.contains("src/server.rs:24"));
    }
    #[test]
    fn publish_replaces_only_changed_facts() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-fact-replacement-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let repository_state = |fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: fingerprint.into(),
        };
        let observation = |from: &str, to: &str, evidence: &str| {
            Observation::dependency(from, DependencyRelation::Calls, to, evidence)
        };

        let first = WorkspaceView::new("main", "analysis", vec![repository_state("one")]).unwrap();
        assert_eq!(
            store
                .publish(
                    &first,
                    &[facts(
                        &first,
                        vec![
                            observation("repo/a", "repo/b", "a.rs:1"),
                            observation("repo/removed", "repo/b", "removed.rs:1"),
                        ],
                    )],
                    &[],
                )
                .unwrap(),
            FactChanges {
                inserted: 2,
                updated: 0,
                removed: 0,
                unchanged: 0,
            }
        );

        let second = WorkspaceView::new("main", "analysis", vec![repository_state("two")]).unwrap();
        assert_eq!(
            store
                .publish(
                    &second,
                    &[facts(
                        &second,
                        vec![
                            observation("repo/a", "repo/b", "a.rs:2"),
                            observation("repo/new", "repo/b", "new.rs:1"),
                        ],
                    )],
                    &[],
                )
                .unwrap(),
            FactChanges {
                inserted: 1,
                updated: 1,
                removed: 1,
                unchanged: 0,
            }
        );
        let observations = store.inspect_observations(None).unwrap();
        assert_eq!(observations.rows.len(), 4);
        assert!(format!("{observations:?}").contains("repo/removed"));
        assert!(format!("{observations:?}").contains("a.rs:2"));
        assert!(
            store
                .context("main", "repo/removed")
                .unwrap()
                .edges
                .is_empty()
        );
        assert!(
            store
                .inspect_revisions()
                .unwrap()
                .rows
                .iter()
                .any(|row| row[1].as_i64() == Some(2))
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn repository_state_facts_are_reused_across_views_and_analysis_versions() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-state-reuse-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "shared".into(),
        };
        let observation = Observation::dependency(
            "repo/source",
            DependencyRelation::Calls,
            "repo/target",
            "src/lib.rs:1",
        );
        for (name, analysis_identity) in [("first", "analysis-v1"), ("second", "analysis-v2")] {
            let view =
                WorkspaceView::new(name, format!("workspace-rules:{name}"), vec![state.clone()])
                    .unwrap();
            store
                .publish(
                    &view,
                    &[RepositoryFacts {
                        state: state.clone(),
                        analysis_identity: analysis_identity.into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: vec![observation.clone()],
                    }],
                    &[],
                )
                .unwrap();
            assert_eq!(store.context(name, "repo/source").unwrap().edges.len(), 1);
        }

        assert_eq!(store.inspect_observations(None).unwrap().rows.len(), 1);
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn legacy_analysis_state_is_reused_after_analyzer_invalidation() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-legacy-reuse-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "shared".into(),
        };
        let observation = Observation::dependency(
            "repo/source",
            DependencyRelation::Calls,
            "repo/target",
            "src/lib.rs:1",
        );
        let legacy_state = "8:analysisshared";
        let transaction = store.db.multi_transaction(true);
        store_observations(
            &transaction,
            legacy_state,
            std::slice::from_ref(&observation),
        )
        .unwrap();
        transaction
            .run_script(
                "?[fingerprint, repository, head] <- [[$state, 'repo', 'head']] \
                     :put repository_state {fingerprint => repository, head}",
                BTreeMap::from([("state".into(), legacy_state.into())]),
            )
            .unwrap();
        transaction
            .run_script(
                "?[view, revision] <- [['main', 1]] \
                     :put analysis_revision {view => revision}",
                BTreeMap::new(),
            )
            .unwrap();
        transaction
            .run_script(
                "?[view, revision, repository, state] <- [['main', 1, 'repo', $state]] \
                     :put analysis_revision_state {view, revision, repository => state}",
                BTreeMap::from([("state".into(), legacy_state.into())]),
            )
            .unwrap();
        transaction.commit().unwrap();

        let view = WorkspaceView::new("main", "analysis-v2", vec![state.clone()]).unwrap();
        store
            .publish(
                &view,
                &[RepositoryFacts {
                    state,
                    analysis_identity: "analysis-v2".into(),
                    incomplete: false,
                    diagnostics: Vec::new(),
                    entities: Vec::new(),
                    grpc_bindings: Vec::new(),
                    observations: vec![observation],
                }],
                &[],
            )
            .unwrap();
        store.checkpoint().unwrap();

        assert_eq!(store.inspect_observations(None).unwrap().rows.len(), 1);
        assert_eq!(
            fs::metadata(state_dir.join("beholder.db-wal")).map_or(0, |m| m.len()),
            0
        );
        let selected = store
                .db
                .run_script(
                    "?[state] := *analysis_revision{view: 'main', revision}, \
                         *analysis_revision_state{view: 'main', revision, repository: 'repo', state}",
                    BTreeMap::new(),
                    ScriptMutability::Immutable,
                )
                .unwrap();
        assert_eq!(selected.rows[0][0].get_str(), Some(legacy_state));
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    #[ignore = "manual production-scale publish regression benchmark"]
    fn production_scale_analyzer_invalidation_benchmark() {
        const FACTS: usize = 21_000;
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-publish-bench-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        let wal = state_dir.join("beholder.db-wal");
        let store = SemanticStore::persistent(&database, true).unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "example/repository".into(),
            },
            head: Some("head".into()),
            fingerprint: "unchanged-source-state".into(),
        };
        let observations = (0..FACTS)
            .map(|index| {
                Observation::dependency(
                    format!("repo://example/source/{index}"),
                    DependencyRelation::Calls,
                    format!("repo://example/target/{index}"),
                    format!("lib/source.ex:{}", index + 1),
                )
            })
            .collect::<Vec<_>>();
        let publish = |analysis_identity: &str| {
            let view = WorkspaceView::new("main", analysis_identity, vec![state.clone()]).unwrap();
            let started = Instant::now();
            store
                .publish(
                    &view,
                    &[RepositoryFacts {
                        state: state.clone(),
                        analysis_identity: analysis_identity.into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: observations.clone(),
                    }],
                    &[],
                )
                .unwrap();
            store.checkpoint().unwrap();
            started.elapsed()
        };
        let size = |path: &Path| fs::metadata(path).map_or(0, |metadata| metadata.len());

        let first = publish("analysis-v1");
        let database_after_first = size(&database);
        let wal_after_first = size(&wal);
        let second = publish("analysis-v2");
        let database_after_second = size(&database);
        let wal_after_second = size(&wal);
        let stored = store
            .db
            .run_script(
                "?[count(from)] := *state_observation{from}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();

        eprintln!(
            "facts={FACTS} first_ms={} second_ms={} database_first_bytes={database_after_first} \
                 database_growth_bytes={} wal_first_bytes={wal_after_first} wal_growth_bytes={}",
            first.as_millis(),
            second.as_millis(),
            database_after_second.saturating_sub(database_after_first),
            wal_after_second.saturating_sub(wal_after_first),
        );
        assert_eq!(stored.rows[0][0].get_int(), Some(FACTS as i64));
        assert!(database_after_second.saturating_sub(database_after_first) < 1024 * 1024);
        assert!(wal_after_second.saturating_sub(wal_after_first) < 1024 * 1024);
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn workspace_override_connects_selected_repository_states() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-state-join-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let source = RepositoryState {
            repository: LogicalRepository {
                identity: "source".into(),
            },
            head: Some("source-head".into()),
            fingerprint: "source-state".into(),
        };
        let target = RepositoryState {
            repository: LogicalRepository {
                identity: "target".into(),
            },
            head: Some("target-head".into()),
            fingerprint: "target-state".into(),
        };
        let view =
            WorkspaceView::new("joined", "analysis", vec![source.clone(), target.clone()]).unwrap();
        let unresolved = Observation::dependency(
            "repo://source/rust/lib/caller",
            DependencyRelation::Calls,
            "rust-call://helper",
            "src/lib.rs:1",
        );
        let resolved = "repo://target/rust/lib/helper";
        store
            .publish(
                &view,
                &[
                    RepositoryFacts {
                        state: source,
                        analysis_identity: "analysis".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: vec![unresolved.clone()],
                    },
                    RepositoryFacts {
                        state: target,
                        analysis_identity: "analysis".into(),
                        incomplete: false,
                        diagnostics: Vec::new(),
                        entities: Vec::new(),
                        grpc_bindings: Vec::new(),
                        observations: vec![Observation::structural(
                            "repo://target/rust/lib",
                            StructuralRelation::Defines,
                            resolved,
                            "src/lib.rs:1",
                        )],
                    },
                ],
                &[DependencyOverride {
                    from: unresolved.from,
                    relation: DependencyRelation::Calls,
                    unresolved_to: unresolved.to,
                    resolved_to: resolved.into(),
                    evidence: unresolved.evidence,
                    confidence: Confidence::Inferred,
                    provenance: Provenance::UniqueNameHeuristic,
                }],
            )
            .unwrap();

        let context = store
            .context("joined", "repo://source/rust/lib/caller")
            .unwrap();
        let edge = context
            .edges
            .iter()
            .find(|edge| edge.to == resolved)
            .unwrap();
        assert_eq!(edge.confidence, 0.6);
        assert_eq!(
            edge.evidence[0].source_kind,
            beholder_dto::EvidenceKind::Inference
        );
        assert_eq!(
            edge.evidence[0].detail.as_deref(),
            Some("unique_name_heuristic")
        );
        let context = format!("{context:?}");
        assert!(context.contains(resolved));
        assert!(!context.contains("rust-call://helper"));
        assert_eq!(
            store
                .trace(
                    "joined",
                    "repo://source/rust/lib/caller",
                    resolved,
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .len(),
            1
        );
        assert!(
            store
                .dependencies(
                    "joined",
                    "repo://source/rust/lib/caller",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .dependencies
                .iter()
                .any(|dependency| dependency.entity == resolved)
        );
        assert!(
            store
                .impact("joined", resolved, beholder_dto::DEFAULT_MAX_HOPS)
                .unwrap()
                .affected
                .iter()
                .any(|affected| affected.entity == "repo://source/rust/lib/caller")
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn carries_forward_only_unchanged_repository_enrichments() {
        let store = SemanticStore::memory().unwrap();
        let state = |repository: &str, fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: repository.into(),
            },
            head: Some("head".into()),
            fingerprint: fingerprint.into(),
        };
        let initial = with_enrichment_analyzers(
            WorkspaceView::new(
                "incremental",
                "syntax",
                vec![state("example/a", "a-1"), state("example/b", "b-1")],
            )
            .unwrap(),
            &["rust"],
        );
        let repository_facts = |state: RepositoryState| RepositoryFacts {
            state,
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations: Vec::new(),
        };
        store
            .publish(
                &initial,
                &initial
                    .repository_states
                    .iter()
                    .cloned()
                    .map(repository_facts)
                    .collect::<Vec<_>>(),
                &[],
            )
            .unwrap();
        let entity_a = EntityFact::new(
            "repo://example/a/rust/lib/generated",
            EntityKind::Callable,
            None,
        )
        .unwrap();
        let entity_b = EntityFact::new(
            "repo://example/b/rust/lib/generated",
            EntityKind::Callable,
            None,
        )
        .unwrap();
        store
            .publish_enrichment(
                &initial,
                "example/a",
                &initial
                    .repository_enrichment_input_fingerprint(&initial.repository_states[0], "rust"),
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "1",
                    expected_version: None,
                },
                EnrichmentPayload {
                    entities: std::slice::from_ref(&entity_a),
                    ..EnrichmentPayload::default()
                },
            )
            .unwrap();
        store
            .publish_enrichment(
                &initial,
                "example/b",
                &initial
                    .repository_enrichment_input_fingerprint(&initial.repository_states[1], "rust"),
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "1",
                    expected_version: None,
                },
                EnrichmentPayload {
                    entities: std::slice::from_ref(&entity_b),
                    ..EnrichmentPayload::default()
                },
            )
            .unwrap();
        assert!(
            store
                .enrichments_current("incremental", &[("rust".into(), "1".into())])
                .unwrap()
        );

        let updated = with_enrichment_analyzers(
            WorkspaceView::new(
                "incremental",
                "syntax",
                vec![state("example/a", "a-2"), state("example/b", "b-1")],
            )
            .unwrap(),
            &["rust"],
        );
        store
            .publish(
                &updated,
                &updated
                    .repository_states
                    .iter()
                    .cloned()
                    .map(repository_facts)
                    .collect::<Vec<_>>(),
                &[],
            )
            .unwrap();

        assert!(
            !store
                .enrichment_matches("incremental", "example/a", "rust", "1")
                .unwrap()
        );
        assert!(
            !store
                .enrichments_current("incremental", &[("rust".into(), "1".into())])
                .unwrap()
        );
        assert!(
            store
                .enrichment_matches("incremental", "example/b", "rust", "1")
                .unwrap()
        );
        let current_entities = store
            .db
            .run_script(
                "?[id] := *analysis_revision{view: 'incremental', revision}, \
                     *analysis_revision_entity{view: 'incremental', revision, id}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();
        let current_entities = current_entities
            .rows
            .iter()
            .filter_map(|row| row[0].get_str())
            .collect::<BTreeSet<_>>();
        assert!(current_entities.contains(entity_b.id.as_str()));
        assert!(!current_entities.contains(entity_a.id.as_str()));
    }

    #[test]
    fn publishes_a_target_result_after_an_unrelated_repository_changes() {
        let repository_state = |identity: &str, fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: identity.into(),
            },
            head: None,
            fingerprint: fingerprint.into(),
        };
        let facts = |state: RepositoryState| RepositoryFacts {
            state,
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations: Vec::new(),
        };
        let store = SemanticStore::memory().unwrap();
        let contexts = BTreeMap::from([(
            "rust".into(),
            BTreeMap::from([("example/a".into(), vec!["example/b".into()])]),
        )]);
        let started = WorkspaceView::new(
            "concurrent",
            "syntax",
            vec![
                repository_state("example/a", "a-1"),
                repository_state("example/b", "b-1"),
                repository_state("example/c", "c-1"),
            ],
        )
        .unwrap()
        .with_repository_contexts(contexts.clone())
        .unwrap();
        store
            .publish(
                &started,
                &started
                    .repository_states
                    .iter()
                    .cloned()
                    .map(facts)
                    .collect::<Vec<_>>(),
                &[],
            )
            .unwrap();
        let target_input =
            started.repository_enrichment_input_fingerprint(&started.repository_states[0], "rust");

        let current = WorkspaceView::new(
            "concurrent",
            "syntax",
            vec![
                repository_state("example/a", "a-1"),
                repository_state("example/b", "b-1"),
                repository_state("example/c", "c-2"),
            ],
        )
        .unwrap()
        .with_repository_contexts(contexts.clone())
        .unwrap();
        store
            .publish(
                &current,
                &current
                    .repository_states
                    .iter()
                    .cloned()
                    .map(facts)
                    .collect::<Vec<_>>(),
                &[],
            )
            .unwrap();

        assert!(
            store
                .publish_enrichment(
                    &started,
                    "example/a",
                    &target_input,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
                        expected_version: None,
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert!(
            store
                .enrichment_matches("concurrent", "example/a", "rust", "1")
                .unwrap()
        );

        let changed_context = WorkspaceView::new(
            "concurrent",
            "syntax",
            vec![
                repository_state("example/a", "a-1"),
                repository_state("example/b", "b-2"),
                repository_state("example/c", "c-2"),
            ],
        )
        .unwrap()
        .with_repository_contexts(contexts)
        .unwrap();
        store
            .publish(
                &changed_context,
                &changed_context
                    .repository_states
                    .iter()
                    .cloned()
                    .map(facts)
                    .collect::<Vec<_>>(),
                &[],
            )
            .unwrap();
        assert!(
            !store
                .publish_enrichment(
                    &started,
                    "example/a",
                    &target_input,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "2",
                        expected_version: None,
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
    }

    #[test]
    fn persists_analyzer_scoped_context_identity_across_restart() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-context-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        let state = |identity: &str| RepositoryState {
            repository: LogicalRepository {
                identity: identity.into(),
            },
            head: None,
            fingerprint: format!("{identity}-source"),
        };
        let view = WorkspaceView::new(
            "persistent-context",
            "syntax",
            vec![state("example/target"), state("example/context")],
        )
        .unwrap()
        .with_repository_contexts(BTreeMap::from([(
            "rust".into(),
            BTreeMap::from([("example/target".into(), vec!["example/context".into()])]),
        )]))
        .unwrap();
        let expected = view.repository_enrichment_input_fingerprint(
            view.repository_states
                .iter()
                .find(|state| state.repository.identity == "example/target")
                .unwrap(),
            "rust",
        );
        let repositories = view
            .repository_states
            .iter()
            .cloned()
            .map(|state| RepositoryFacts {
                state,
                analysis_identity: "syntax".into(),
                incomplete: false,
                diagnostics: Vec::new(),
                entities: Vec::new(),
                grpc_bindings: Vec::new(),
                observations: Vec::new(),
            })
            .collect::<Vec<_>>();

        let store = SemanticStore::persistent(&database, true).unwrap();
        store.publish(&view, &repositories, &[]).unwrap();
        store.checkpoint().unwrap();
        drop(store);

        let store = SemanticStore::persistent(&database, false).unwrap();
        assert_eq!(
            store
                .revision_enrichment_input_fingerprint(
                    "persistent-context",
                    "example/target",
                    "rust",
                )
                .unwrap()
                .as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            store
                .repository_contexts("persistent-context", "example/target", "rust")
                .unwrap(),
            ["example/context"]
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn rejects_a_result_after_a_newer_analyzer_run_supersedes_it() {
        let store = SemanticStore::memory().unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "superseded",
                "syntax",
                vec![RepositoryState {
                    repository: LogicalRepository {
                        identity: "example/repo".into(),
                    },
                    head: None,
                    fingerprint: "source".into(),
                }],
            )
            .unwrap(),
            &["rust"],
        );
        store
            .publish(&view, &[facts(&view, Vec::new())], &[])
            .unwrap();
        let input_fingerprint =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "rust");
        store
            .publish_enrichment(
                &view,
                "example/repo",
                &input_fingerprint,
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "pending:2",
                    expected_version: None,
                },
                EnrichmentPayload::default(),
            )
            .unwrap();

        assert!(
            !store
                .publish_enrichment(
                    &view,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
                        expected_version: Some("pending:1"),
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert!(
            store
                .enrichment_matches("superseded", "example/repo", "rust", "pending:2")
                .unwrap()
        );
    }

    #[test]
    fn publishes_enrichment_only_for_the_current_baseline() {
        let store = SemanticStore::memory().unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "example/repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "source".into(),
        };
        let view = with_enrichment_analyzers(
            WorkspaceView::new("enriched", "syntax", vec![state.clone()]).unwrap(),
            &["rust"],
        );
        let input_fingerprint = view.repository_enrichment_input_fingerprint(&state, "rust");
        let call = Observation::dependency(
            "repo://example/repo/rust/lib/caller",
            DependencyRelation::Calls,
            "rust-call://helper",
            "src/lib.rs:2",
        );
        let mut baseline = facts(&view, vec![call.clone()]);
        baseline.diagnostics.push(AnalysisDiagnostic {
            code: "rust.syntax_recovered".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: "src/lib.rs".into(),
            line: Some(1),
            detail: None,
        });
        store.publish(&view, &[baseline], &[]).unwrap();
        let resolved = "repo://example/repo/rust/lib/helper";
        let override_ = DependencyOverride {
            from: call.from,
            relation: DependencyRelation::Calls,
            unresolved_to: call.to,
            resolved_to: resolved.into(),
            evidence: call.evidence,
            confidence: Confidence::Exact,
            provenance: Provenance::Compiler,
        };

        let compiler_diagnostic = AnalysisDiagnostic {
            code: "rust.semantic_resolution_unavailable".into(),
            severity: AnalysisDiagnosticSeverity::KnownLimitation,
            path: "Cargo.toml".into(),
            line: None,
            detail: Some("compiler analysis was partial".into()),
        };
        assert!(
            store
                .publish_enrichment(
                    &view,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
                        expected_version: None,
                    },
                    EnrichmentPayload {
                        overrides: &[override_],
                        diagnostics: &[("example/repo".into(), compiler_diagnostic)],
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap()
        );
        assert!(
            store
                .enrichment_matches("enriched", "example/repo", "rust", "1")
                .unwrap()
        );
        let context = store
            .context("enriched", "repo://example/repo/rust/lib/caller")
            .unwrap();
        let edge = context
            .edges
            .iter()
            .find(|edge| edge.to == resolved)
            .unwrap();
        assert_eq!(edge.confidence, 1.0);
        assert_eq!(
            edge.evidence[0].source_kind,
            beholder_dto::EvidenceKind::Compiler
        );
        assert_eq!(
            store
                .context_snapshot("enriched", "missing")
                .unwrap()
                .analysis
                .diagnostics
                .len(),
            2
        );
        assert!(
            store
                .publish_enrichment(
                    &view,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "2",
                        expected_version: None,
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        let context = store
            .context("enriched", "repo://example/repo/rust/lib/caller")
            .unwrap();
        assert!(
            context
                .edges
                .iter()
                .any(|edge| edge.to == "rust-call://helper")
        );
        assert!(context.edges.iter().all(|edge| edge.to != resolved));
        let diagnostics = store
            .context_snapshot("enriched", "missing")
            .unwrap()
            .analysis
            .diagnostics;
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "rust.syntax_recovered");

        let stale = with_enrichment_analyzers(
            WorkspaceView::new(
                "enriched",
                "syntax",
                vec![RepositoryState {
                    fingerprint: "new-source".into(),
                    ..state
                }],
            )
            .unwrap(),
            &["rust"],
        );
        assert!(
            !store
                .publish_enrichment(
                    &stale,
                    "example/repo",
                    &stale.repository_enrichment_input_fingerprint(
                        &stale.repository_states[0],
                        "rust",
                    ),
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
                        expected_version: None,
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
    }

    #[test]
    fn publishes_and_retracts_additive_enrichment_facts() {
        let store = SemanticStore::memory().unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "elixir-enriched",
                "syntax",
                vec![RepositoryState {
                    repository: LogicalRepository {
                        identity: "example/repo".into(),
                    },
                    head: Some("head".into()),
                    fingerprint: "source".into(),
                }],
            )
            .unwrap(),
            &["elixir"],
        );
        store
            .publish(&view, &[facts(&view, Vec::new())], &[])
            .unwrap();
        let input_fingerprint =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "elixir");

        let generated = "repo://example/repo/elixir/Example/generated/0";
        let entity = EntityFact::new(generated, EntityKind::Callable, None).unwrap();
        let mut observation = Observation::dependency(
            generated,
            DependencyRelation::Calls,
            "elixir-call://External/run/0",
            "lib/example.ex:2 (compiler remote_function via macro expansion)",
        );
        observation.confidence = Confidence::Inferred;
        observation.provenance = Provenance::Compiler;

        assert!(
            store
                .publish_enrichment(
                    &view,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "elixir",
                        version: "1",
                        expected_version: None,
                    },
                    EnrichmentPayload {
                        entities: &[entity],
                        observations: &[observation],
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap()
        );
        let context = store.context("elixir-enriched", generated).unwrap();
        assert_eq!(context.edges.len(), 1);
        assert_eq!(context.edges[0].confidence, 0.6);
        assert_eq!(
            context.edges[0].evidence[0].source_kind,
            beholder_dto::EvidenceKind::Compiler
        );

        assert!(
            store
                .publish_enrichment(
                    &view,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "elixir",
                        version: "2",
                        expected_version: None,
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert!(
            store
                .context("elixir-enriched", generated)
                .unwrap()
                .edges
                .is_empty()
        );
        let rows = store
            .db
            .run_script(
                "?[id] := *analysis_revision{view: $view, revision}, \
                     *analysis_revision_entity{view: $view, revision, id}, id = $id",
                BTreeMap::from([
                    ("view".into(), "elixir-enriched".into()),
                    ("id".into(), generated.into()),
                ]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert!(rows.rows.is_empty());
    }
}
