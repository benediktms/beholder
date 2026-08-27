use super::schema::*;
use super::store::{EnrichmentOwner, EnrichmentPayload};
use beholder_domain::{
    AnalysisDiagnostic, Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind,
    EntityMetadata, FactChanges, FactShard, GraphqlOperationKind, GraphqlTypeKind,
    GrpcBindingCandidate, GrpcBindingRole, Observation, ProtoTypeKind, Provenance, RepositoryFacts,
    RpcCardinality, SemanticRelation, StructuralRelation, WorkspaceView,
};
use beholder_dto::{GarbageCollectionPhase, GarbageCollectionProgress};
use mnestic_engine::{DataValue, DbInstance, MultiTransaction, ScriptMutability};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    thread,
    time::{Duration, Instant},
};

const FACT_BATCH_SIZE: usize = 10_000;
const GARBAGE_COLLECTION_BATCH_SIZE: usize = 10_000;
const GARBAGE_COLLECTION_TRANSACTION_RETRIES: usize = 50;
const GARBAGE_COLLECTION_TRANSACTION_RETRY_DELAY: Duration = Duration::from_millis(10);

fn replace_fact_shards(
    transaction: &MultiTransaction,
    view: &str,
    shards: &[FactShard],
) -> Result<FactChanges, Box<dyn Error>> {
    if shards.iter().any(|shard| {
        shard.producer.is_empty() || shard.repository.is_empty() || shard.version.is_empty()
    }) {
        return Err("fact shard publication scope and version must not be empty".into());
    }
    let incoming = shards
        .iter()
        .map(|shard| {
            (
                (
                    shard.producer.as_str(),
                    shard.repository.as_str(),
                    shard.owner.as_str(),
                ),
                shard,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if incoming.len() != shards.len() {
        let mut owners = BTreeMap::new();
        for shard in shards {
            *owners
                .entry((
                    shard.producer.as_str(),
                    shard.repository.as_str(),
                    shard.owner.as_str(),
                ))
                .or_insert(0usize) += 1;
        }
        let duplicates = owners
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|((producer, repository, owner), count)| {
                format!("{producer}/{repository}/{owner} ({count})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(
            format!("fact shard owners must be unique within a publication: {duplicates}").into(),
        );
    }

    let current = transaction.run_script(
        "?[producer, repository, owner, version] := *analysis_fact_shard_selection{\
             view: $view, producer, repository, owner, version\
         }",
        BTreeMap::from([("view".into(), view.into())]),
    )?;
    let current = current
        .rows
        .into_iter()
        .map(|row| {
            let string = |index: usize, name: &str| {
                row[index]
                    .get_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("fact shard {name} is not a string"))
            };
            Ok((
                (
                    string(0, "producer")?,
                    string(1, "repository")?,
                    string(2, "owner")?,
                ),
                string(3, "version")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Box<dyn Error>>>()?;

    let mut changes = FactChanges::default();
    let removed = current
        .keys()
        .filter(|(producer, repository, owner)| {
            !incoming.contains_key(&(producer.as_str(), repository.as_str(), owner.as_str()))
        })
        .map(|(producer, repository, owner)| {
            DataValue::List(vec![
                view.into(),
                producer.as_str().into(),
                repository.as_str().into(),
                owner.as_str().into(),
            ])
        })
        .collect::<Vec<_>>();
    changes.removed = removed.len();
    if !removed.is_empty() {
        transaction.run_script(
            "?[view, producer, repository, owner] <- $rows \
             :rm analysis_fact_shard_selection {view, producer, repository, owner}",
            BTreeMap::from([("rows".into(), DataValue::List(removed))]),
        )?;
    }

    let mut entity_rows = Vec::new();
    let mut observation_rows = Vec::new();
    let mut dependency_rows = Vec::new();
    let mut selection_rows = Vec::new();
    for shard in shards {
        let key = (
            shard.producer.clone(),
            shard.repository.clone(),
            shard.owner.as_str().to_owned(),
        );
        match current.get(&key) {
            Some(version) if version == &shard.version => {
                changes.unchanged += shard.observations.len();
                continue;
            }
            Some(_) => changes.updated += shard.observations.len(),
            None => changes.inserted += shard.observations.len(),
        }
        let scope = [
            shard.producer.as_str().into(),
            shard.owner.as_str().into(),
            shard.version.as_str().into(),
        ];
        entity_rows.extend(shard.entities.iter().map(|entity| {
            let mut row = scope.to_vec();
            row.extend([
                entity.id.as_str().into(),
                entity_kind(entity.kind).into(),
                entity_metadata(entity.metadata).into(),
            ]);
            DataValue::List(row)
        }));
        observation_rows.extend(shard.observations.iter().map(|observation| {
            let mut row = scope.to_vec();
            row.extend([
                observation.from.as_str().into(),
                observation.relation.as_str().into(),
                observation.to.as_str().into(),
                observation.evidence.as_str().into(),
                observation.confidence.score().into(),
                observation.provenance.as_str().into(),
            ]);
            DataValue::List(row)
        }));
        dependency_rows.extend(
            shard
                .observations
                .iter()
                .filter(|observation| observation.relation.dependency().is_some())
                .map(|observation| {
                    let mut row = scope.to_vec();
                    row.extend([
                        observation.from.as_str().into(),
                        observation.relation.as_str().into(),
                        observation.to.as_str().into(),
                        observation.evidence.as_str().into(),
                    ]);
                    DataValue::List(row)
                }),
        );
        selection_rows.push(DataValue::List(vec![
            view.into(),
            shard.producer.as_str().into(),
            shard.repository.as_str().into(),
            shard.owner.as_str().into(),
            shard.version.as_str().into(),
        ]));
    }
    for (script, rows) in [
        (
            "?[producer, owner, version, id, kind, metadata] <- $rows \
             :put analysis_fact_shard_entity {\
                 producer, owner, version, id => kind, metadata\
             }",
            entity_rows,
        ),
        (
            "?[producer, owner, version, from, relation, to, evidence, confidence, provenance] \
                 <- $rows \
             :put analysis_fact_shard_observation {\
                 producer, owner, version, from, relation, to, evidence \
                 => confidence, provenance\
             }",
            observation_rows,
        ),
        (
            "?[producer, owner, version, from, relation, to, evidence] <- $rows \
             :put analysis_fact_shard_dependency_observation {\
                 producer, owner, version, from, relation, to, evidence\
             }",
            dependency_rows,
        ),
        (
            "?[view, producer, repository, owner, version] <- $rows \
             :put analysis_fact_shard_selection {\
                 view, producer, repository, owner => version\
             }",
            selection_rows,
        ),
    ] {
        for rows in rows.chunks(FACT_BATCH_SIZE) {
            transaction.run_script(
                script,
                BTreeMap::from([("rows".into(), DataValue::List(rows.to_vec()))]),
            )?;
        }
    }
    Ok(changes)
}

fn enrichment_owner_key(analyzer: &str, repository: &str) -> String {
    format!("{}:{analyzer}{repository}", analyzer.len())
}

pub(super) fn selected_baseline_semantics(
    db: &DbInstance,
    view: &str,
    repository: &str,
    entity_kinds: &BTreeSet<EntityKind>,
    relations: &BTreeSet<SemanticRelation>,
) -> Result<(Vec<EntityFact>, Vec<Observation>), Box<dyn Error>> {
    if entity_kinds.is_empty() && relations.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let state = db.run_script(
        "?[state] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, repository: $repository, state}",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("repository".into(), repository.into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    let Some(state) = state.rows.first().and_then(|row| row[0].get_str()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let params = BTreeMap::from([
        ("state".into(), state.into()),
        ("view".into(), view.into()),
        ("repository".into(), repository.into()),
    ]);
    let stored_entities = db.run_script(
        "stored[id, kind, metadata] := *state_entity{state: $state, id, kind, metadata}\n\
         stored[id, kind, metadata] := *analysis_fact_shard_selection{\
             view: $view, repository: $repository, producer, owner, version\
         }, *analysis_fact_shard_entity{producer, owner, version, id, kind, metadata}\n\
         ?[id, kind, metadata] := stored[id, kind, metadata]",
        params.clone(),
        ScriptMutability::Immutable,
    )?;
    let mut entities = BTreeMap::new();
    for row in stored_entities.rows {
        let id = stored_string(&row, 0, "entity ID")?.to_owned();
        let kind = parse_entity_kind(stored_string(&row, 1, "entity kind")?)?;
        let metadata = parse_entity_metadata(stored_string(&row, 2, "entity metadata")?)?;
        entities.insert(id.clone(), EntityFact::new(id, kind, metadata)?);
    }
    let mut selected_ids = entities
        .iter()
        .filter(|(_, entity)| entity_kinds.contains(&entity.kind))
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut observations = Vec::new();
    if !relations.is_empty() {
        let stored = db.run_script(
            "stored[from, relation, to, evidence, confidence, provenance] := \
                 *state_observation{state: $state, from, relation, to, evidence}, \
                 *state_observation_metadata{state: $state, from, relation, to, confidence, provenance}\n\
             stored[from, relation, to, evidence, confidence, provenance] := \
                 *analysis_fact_shard_selection{\
                     view: $view, repository: $repository, producer, owner, version\
                 }, *analysis_fact_shard_observation{\
                     producer, owner, version, from, relation, to, evidence, confidence, provenance\
                 }\n\
             ?[from, relation, to, evidence, confidence, provenance] := \
                 stored[from, relation, to, evidence, confidence, provenance]",
            params,
            ScriptMutability::Immutable,
        )?;
        for row in stored.rows {
            let relation = parse_relation(stored_string(&row, 1, "relation")?)?;
            if !relations.contains(&relation) {
                continue;
            }
            let from = stored_string(&row, 0, "source entity")?.to_owned();
            let to = stored_string(&row, 2, "destination entity")?.to_owned();
            selected_ids.insert(from.clone());
            selected_ids.insert(to.clone());
            observations.push(Observation {
                from: from.into(),
                relation,
                to: to.into(),
                evidence: stored_string(&row, 3, "evidence")?.into(),
                confidence: parse_confidence(
                    row[4]
                        .get_float()
                        .ok_or("stored confidence is not a float")?,
                )?,
                provenance: parse_provenance(stored_string(&row, 5, "provenance")?)?,
            });
        }
    }
    let entities = selected_ids
        .into_iter()
        .map(|id| {
            entities.remove(&id).ok_or_else(|| {
                format!("baseline relationship references missing entity {id}").into()
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok((entities, observations))
}

fn stored_string<'a>(
    row: &'a [DataValue],
    index: usize,
    field: &str,
) -> Result<&'a str, Box<dyn Error>> {
    row.get(index)
        .and_then(DataValue::get_str)
        .ok_or_else(|| format!("stored {field} is not a string").into())
}

fn parse_entity_kind(value: &str) -> Result<EntityKind, Box<dyn Error>> {
    Ok(match value {
        "callable" => EntityKind::Callable,
        "graphql_argument" => EntityKind::GraphqlArgument,
        "graphql_enum_value" => EntityKind::GraphqlEnumValue,
        "graphql_field" => EntityKind::GraphqlField,
        "graphql_operation" => EntityKind::GraphqlOperation,
        "graphql_type" => EntityKind::GraphqlType,
        "grpc_operation" => EntityKind::GrpcOperation,
        "kafka_topic" => EntityKind::KafkaTopic,
        "namespace" => EntityKind::Namespace,
        "proto_field" => EntityKind::ProtoField,
        "proto_method" => EntityKind::ProtoMethod,
        "proto_service" => EntityKind::ProtoService,
        "proto_type" => EntityKind::ProtoType,
        "service" => EntityKind::Service,
        "unity_prefab" => EntityKind::UnityPrefab,
        _ => return Err(format!("unknown stored entity kind {value}").into()),
    })
}

fn parse_entity_metadata(value: &str) -> Result<Option<EntityMetadata>, Box<dyn Error>> {
    Ok(match value {
        "" => None,
        "graphql_operation:mutation" => Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Mutation,
        }),
        "graphql_operation:query" => Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Query,
        }),
        "graphql_operation:subscription" => Some(EntityMetadata::GraphqlOperation {
            kind: GraphqlOperationKind::Subscription,
        }),
        "graphql_type:enum" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Enum,
        }),
        "graphql_type:input" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Input,
        }),
        "graphql_type:interface" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Interface,
        }),
        "graphql_type:object" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Object,
        }),
        "graphql_type:scalar" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Scalar,
        }),
        "graphql_type:union" => Some(EntityMetadata::GraphqlType {
            kind: GraphqlTypeKind::Union,
        }),
        "rpc_cardinality:bidirectional_streaming" => Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::BidirectionalStreaming,
        }),
        "rpc_cardinality:client_streaming" => Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::ClientStreaming,
        }),
        "rpc_cardinality:server_streaming" => Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::ServerStreaming,
        }),
        "rpc_cardinality:unary" => Some(EntityMetadata::ProtoMethod {
            cardinality: RpcCardinality::Unary,
        }),
        "proto_type:enum" => Some(EntityMetadata::ProtoType {
            kind: ProtoTypeKind::Enum,
        }),
        "proto_type:message" => Some(EntityMetadata::ProtoType {
            kind: ProtoTypeKind::Message,
        }),
        _ => return Err(format!("unknown stored entity metadata {value}").into()),
    })
}

fn parse_relation(value: &str) -> Result<SemanticRelation, Box<dyn Error>> {
    Ok(match value {
        "defines" => SemanticRelation::Structural(StructuralRelation::Defines),
        "field_of" => SemanticRelation::Structural(StructuralRelation::FieldOf),
        "request_type" => SemanticRelation::Structural(StructuralRelation::RequestType),
        "response_type" => SemanticRelation::Structural(StructuralRelation::ResponseType),
        "binds_contract" => SemanticRelation::Dependency(DependencyRelation::BindsContract),
        "calls" => SemanticRelation::Dependency(DependencyRelation::Calls),
        "calls_graphql" => SemanticRelation::Dependency(DependencyRelation::CallsGraphql),
        "calls_rpc" => SemanticRelation::Dependency(DependencyRelation::CallsRpc),
        "consumed_by" => SemanticRelation::Dependency(DependencyRelation::ConsumedBy),
        "implements" => SemanticRelation::Dependency(DependencyRelation::Implements),
        "implemented_by" => SemanticRelation::Dependency(DependencyRelation::ImplementedBy),
        "imports" => SemanticRelation::Dependency(DependencyRelation::Imports),
        "publishes" => SemanticRelation::Dependency(DependencyRelation::Publishes),
        "requires" => SemanticRelation::Dependency(DependencyRelation::Requires),
        "resolved_by" => SemanticRelation::Dependency(DependencyRelation::ResolvedBy),
        "selects" => SemanticRelation::Dependency(DependencyRelation::Selects),
        "uses" => SemanticRelation::Dependency(DependencyRelation::Uses),
        _ => return Err(format!("unknown stored semantic relation {value}").into()),
    })
}

fn parse_confidence(value: f64) -> Result<Confidence, Box<dyn Error>> {
    if value.to_bits() == 1.0f64.to_bits() {
        Ok(Confidence::Exact)
    } else if value.to_bits() == 0.6f64.to_bits() {
        Ok(Confidence::Inferred)
    } else {
        Err(format!("unknown stored confidence {value}").into())
    }
}

fn parse_provenance(value: &str) -> Result<Provenance, Box<dyn Error>> {
    Ok(match value {
        "ast" => Provenance::Ast,
        "compiler" => Provenance::Compiler,
        "descriptor" => Provenance::Descriptor,
        "generated" => Provenance::Generated,
        "unique_name_heuristic" => Provenance::UniqueNameHeuristic,
        _ => return Err(format!("unknown stored provenance {value}").into()),
    })
}

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

fn baseline_fingerprint(
    repositories: &[RepositoryFacts],
    resolution: &GrpcResolution,
    overrides: &[DependencyOverride],
) -> String {
    let mut hash = Sha256::new();
    for facts in repositories {
        hash_string(&mut hash, &facts.state.repository.identity);
        for (id, (kind, metadata)) in normalized_entities(facts) {
            for value in [&id, &kind, &metadata] {
                hash_string(&mut hash, value);
            }
        }
        for ((from, relation, to), (evidence, confidence, provenance)) in
            normalized_observations(facts)
        {
            for value in [&from, &relation, &to, &evidence, &provenance] {
                hash_string(&mut hash, value);
            }
            hash.update(confidence.to_le_bytes());
        }
        let diagnostics = facts
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    (
                        diagnostic.code.as_str(),
                        diagnostic.severity.as_str(),
                        diagnostic.path.to_string_lossy(),
                        diagnostic.line.unwrap_or_default(),
                    ),
                    diagnostic.detail.as_deref().unwrap_or_default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for ((code, severity, path, line), detail) in diagnostics {
            for value in [code, severity, path.as_ref(), detail] {
                hash_string(&mut hash, value);
            }
            hash.update(line.to_le_bytes());
        }
    }
    for entity in resolution.entities.values() {
        for value in [
            entity.id.as_str(),
            entity_kind(entity.kind),
            entity_metadata(entity.metadata),
        ] {
            hash_string(&mut hash, value);
        }
    }
    for observation in &resolution.observations {
        for value in [
            observation.from.as_str(),
            observation.relation.as_str(),
            observation.to.as_str(),
            observation.evidence.as_str(),
            observation.provenance.as_str(),
        ] {
            hash_string(&mut hash, value);
        }
        hash.update(observation.confidence.score().to_bits().to_le_bytes());
    }
    let overrides = overrides
        .iter()
        .map(|override_| {
            (
                (
                    override_.from.as_str(),
                    override_.relation.as_str(),
                    override_.unresolved_to.as_str(),
                ),
                override_,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for ((from, relation, unresolved_to), override_) in overrides {
        for value in [
            from,
            relation,
            unresolved_to,
            override_.resolved_to.as_str(),
            override_.evidence.as_str(),
            override_.provenance.as_str(),
        ] {
            hash_string(&mut hash, value);
        }
        hash.update(override_.confidence.score().to_bits().to_le_bytes());
    }
    for diagnostic in &resolution.diagnostics {
        for value in [
            diagnostic.candidate.local_symbol.as_str(),
            diagnostic.candidate.role.as_str(),
            diagnostic.candidate.service.as_str(),
            diagnostic.candidate.method.as_str(),
            diagnostic.candidate.evidence.as_str(),
            diagnostic.code,
            diagnostic.detail.as_str(),
        ] {
            hash_string(&mut hash, value);
        }
    }
    format!("{:x}", hash.finalize())
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
    fact_shards: &[FactShard],
    verification_fingerprint: Option<&str>,
) -> Result<FactChanges, Box<dyn Error>> {
    let publication_started = Instant::now();
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
    let started = Instant::now();
    let resolution = resolve_grpc_bindings(repositories)?;
    tracing::info!(
        target: "beholder::publication",
        stage = "resolve_bindings",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        observations = resolution.observations.len(),
        "Mnestic publication stage completed"
    );
    let baseline_fingerprint = baseline_fingerprint(repositories, &resolution, overrides);
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        ("fingerprint".into(), view.fingerprint().into()),
    ]);
    let transaction = db.multi_transaction(true);
    let started = Instant::now();
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
    tracing::info!(
        target: "beholder::publication",
        stage = "read_effective_observations",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        rows_read = current.len(),
        "Mnestic publication stage completed"
    );
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
    let started = Instant::now();
    let next = repositories
        .iter()
        .flat_map(|facts| &facts.observations)
        .chain(&resolution.observations)
        .chain(
            fact_shards
                .iter()
                .flat_map(|shard| shard.observations.iter()),
        )
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
    tracing::info!(
        target: "beholder::publication",
        stage = "build_next_observations",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        rows = next.len(),
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
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
    tracing::info!(
        target: "beholder::publication",
        stage = "diff_effective_observations",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        rows_inserted = changes.inserted,
        rows_updated = changes.updated,
        rows_removed = changes.removed,
        rows_unchanged = changes.unchanged,
        "Mnestic publication stage completed"
    );

    let started = Instant::now();
    let shard_changes = replace_fact_shards(&transaction, &view.name, fact_shards)?;
    tracing::info!(
        target: "beholder::publication",
        stage = "replace_fact_shards",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        shards = fact_shards.len(),
        rows_inserted = shard_changes.inserted,
        rows_updated = shard_changes.updated,
        rows_removed = shard_changes.removed,
        rows_unchanged = shard_changes.unchanged,
        "Mnestic publication stage completed"
    );

    let started = Instant::now();
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
    tracing::info!(
        target: "beholder::publication",
        stage = "store_repository_states",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        repositories = repositories.len(),
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
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
    tracing::info!(
        target: "beholder::publication",
        stage = "store_revision_manifest",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        repositories = repositories.len(),
        overrides = overrides.len(),
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
    let baseline_changed = transaction
        .run_script(
            "?[fingerprint] := *analysis_baseline_fingerprint{view: $view, fingerprint}",
            BTreeMap::from([("view".into(), view.name.clone().into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_str())
        != Some(baseline_fingerprint.as_str());
    tracing::info!(
        target: "beholder::publication",
        stage = "read_baseline_fingerprint",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        baseline_changed,
        "Mnestic publication stage completed"
    );
    if baseline_changed {
        let started = Instant::now();
        replace_baseline_snapshot(&transaction, &view.name)?;
        tracing::info!(
            target: "beholder::publication",
            stage = "replace_baseline_snapshot",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "Mnestic publication stage completed"
        );
        transaction.run_script(
            "?[view, fingerprint] <- [[$view, $fingerprint]] \
             :put analysis_baseline_fingerprint {view => fingerprint}",
            BTreeMap::from([
                ("view".into(), view.name.clone().into()),
                ("fingerprint".into(), baseline_fingerprint.into()),
            ]),
        )?;
    }
    let started = Instant::now();
    carry_forward_enrichments(&transaction, &view.name)?;
    tracing::info!(
        target: "beholder::publication",
        stage = "carry_forward_enrichments",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
    let obsolete = obsolete_enrichment_owners(&transaction, &view.name)?;
    tracing::info!(
        target: "beholder::publication",
        stage = "find_obsolete_enrichments",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        obsolete_owners = obsolete.len(),
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
    remove_enrichment_state(&transaction, &view.name, &obsolete)?;
    tracing::info!(
        target: "beholder::publication",
        stage = "remove_obsolete_enrichments",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        obsolete_owners = obsolete.len(),
        "Mnestic publication stage completed"
    );
    let started = Instant::now();
    transaction.commit()?;
    tracing::info!(
        target: "beholder::publication",
        stage = "transaction_commit",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "Mnestic publication stage completed"
    );
    tracing::info!(
        target: "beholder::publication",
        stage = "total",
        elapsed_ms = publication_started.elapsed().as_secs_f64() * 1000.0,
        "Mnestic publication completed"
    );
    Ok(changes)
}

fn replace_baseline_snapshot(
    transaction: &MultiTransaction,
    view: &str,
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([("view".into(), view.into())]);
    for (relation, script) in [
        (
            "entities",
            "?[view, id] := *analysis_baseline_entity{view: $view, id}, view = $view \
         :rm analysis_baseline_entity {view, id}",
        ),
        (
            "observations",
            "?[view, from, relation, to, evidence] := *analysis_baseline_observation{\
             view: $view, from, relation, to, evidence\
         }, view = $view \
         :rm analysis_baseline_observation {view, from, relation, to, evidence}",
        ),
        (
            "overrides",
            "?[view, from, relation, unresolved_to] := *analysis_baseline_dependency_override{\
             view: $view, from, relation, unresolved_to\
         }, view = $view \
         :rm analysis_baseline_dependency_override {view, from, relation, unresolved_to}",
        ),
        (
            "diagnostics",
            "?[view, repository, code, severity, path, line] := *analysis_baseline_diagnostic{\
             view: $view, repository, code, severity, path, line\
         }, view = $view \
         :rm analysis_baseline_diagnostic {view, repository, code, severity, path, line}",
        ),
    ] {
        let started = Instant::now();
        transaction.run_script(script, params.clone())?;
        tracing::info!(
            target: "beholder::publication",
            stage = "replace_baseline.delete",
            relation,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "Mnestic publication stage completed"
        );
    }
    for (relation, script) in [
        (
            "state_entities",
            "?[view, id, kind, metadata, revision_owned] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_entity{state, id, kind, metadata}, revision_owned = false \
         :put analysis_baseline_entity {view, id => kind, metadata, revision_owned}",
        ),
        (
            "state_observations",
            "?[view, from, relation, to, evidence, confidence, provenance, revision_owned] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_observation{state, from, relation, to, evidence}, \
             *state_observation_metadata{state, from, relation, to, confidence, provenance}, \
             revision_owned = false \
         :put analysis_baseline_observation {\
             view, from, relation, to, evidence => confidence, provenance, revision_owned\
         }",
        ),
        (
            "revision_entities",
            "?[view, id, kind, metadata, revision_owned] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view, revision, id, kind, metadata}, \
             revision_owned = true \
         :put analysis_baseline_entity {view, id => kind, metadata, revision_owned}",
        ),
        (
            "revision_observations",
            "?[view, from, relation, to, evidence, confidence, provenance, revision_owned] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_observation{\
                 view, revision, from, relation, to, evidence, confidence, provenance\
             }, revision_owned = true \
         :put analysis_baseline_observation {\
             view, from, relation, to, evidence => confidence, provenance, revision_owned\
         }",
        ),
        (
            "overrides",
            "?[view, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_dependency_override{\
                 view, revision, from, relation, unresolved_to, resolved_to, evidence\
             }, *analysis_revision_dependency_override_metadata{\
                 view, revision, from, relation, unresolved_to, confidence, provenance\
             } \
         :put analysis_baseline_dependency_override {\
             view, from, relation, unresolved_to => resolved_to, evidence, confidence, provenance\
         }",
        ),
        (
            "diagnostics",
            "?[view, repository, code, severity, path, line, detail] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_diagnostic{\
                 view, revision, repository, code, severity, path, line, detail\
             } \
         :put analysis_baseline_diagnostic {\
             view, repository, code, severity, path, line => detail\
         }",
        ),
    ] {
        let started = Instant::now();
        transaction.run_script(script, params.clone())?;
        tracing::info!(
            target: "beholder::publication",
            stage = "replace_baseline.populate",
            relation,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "Mnestic publication stage completed"
        );
    }
    Ok(())
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
    reconcile_obsolete_enrichments(&transaction, &view.name)?;
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
    let revision = transaction
        .run_script(
            "?[revision] := *analysis_revision{view: $view, revision}",
            BTreeMap::from([("view".into(), view.into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .ok_or("published analysis revision is missing")?;
    materialize_enrichment_contributions(transaction, view, revision)
}

fn obsolete_enrichment_owners(
    transaction: &MultiTransaction,
    view: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let rows = transaction.run_script(
        "current[repository, analyzer] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{view, revision, repository, analyzer}\n\
         obsolete[owner] := *enrichment_output{\
             view: $view, owner, repository, analyzer\
         }, not current[repository, analyzer]\n\
         ?[owner] := obsolete[owner]",
        BTreeMap::from([("view".into(), view.into())]),
    )?;
    rows.rows
        .into_iter()
        .map(|row| {
            row[0]
                .get_str()
                .map(str::to_owned)
                .ok_or_else(|| "enrichment owner is not a string".into())
        })
        .collect()
}

fn reconcile_obsolete_enrichments(
    transaction: &MultiTransaction,
    view: &str,
) -> Result<(), Box<dyn Error>> {
    let obsolete = obsolete_enrichment_owners(transaction, view)?;
    if obsolete.is_empty() {
        return Ok(());
    }
    let previous = transaction
        .run_script(
            "?[revision] := *analysis_revision{view: $view, revision}",
            BTreeMap::from([("view".into(), view.into())]),
        )?
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .ok_or("published analysis revision is missing")?;
    let revision = previous + 1;
    copy_revision(transaction, view, previous, revision)?;
    for owner in &obsolete {
        remove_owner_contribution_keys(transaction, view, owner, revision)?;
    }
    remove_enrichment_state(transaction, view, &obsolete)?;
    materialize_enrichment_contributions(transaction, view, revision)?;
    if effective_revision_changed(transaction, view, previous, revision)? {
        transaction.run_script(
            "?[view, revision] <- [[$view, $revision]] \
             :put analysis_revision {view => revision}",
            BTreeMap::from([
                ("view".into(), view.into()),
                ("revision".into(), revision.into()),
            ]),
        )?;
    } else {
        remove_revision_candidate(transaction, view, revision)?;
    }
    Ok(())
}

fn remove_enrichment_state(
    transaction: &MultiTransaction,
    view: &str,
    owners: &[String],
) -> Result<(), Box<dyn Error>> {
    if owners.is_empty() {
        return Ok(());
    }
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        (
            "owners".into(),
            DataValue::List(
                owners
                    .iter()
                    .map(|owner| DataValue::List(vec![owner.as_str().into()]))
                    .collect(),
            ),
        ),
    ]);
    for (relation, keys) in [
        ("enrichment_entity_contribution", "view, owner, id"),
        (
            "enrichment_observation_contribution",
            "view, owner, from, relation, to, evidence",
        ),
        (
            "enrichment_override_contribution",
            "view, owner, from, relation, unresolved_to",
        ),
        (
            "enrichment_diagnostic_contribution",
            "view, owner, repository, code, severity, path, line",
        ),
        ("enrichment_output", "view, owner"),
    ] {
        transaction.run_script(
            &format!(
                "obsolete[owner] <- $owners\n\
                 ?[{keys}] := obsolete[owner], *{relation}{{{keys}}}, view = $view \
                 :rm {relation} {{{keys}}}"
            ),
            params.clone(),
        )?;
    }
    Ok(())
}

fn valid_enrichment_owner_rule() -> &'static str {
    "valid_owner[owner] := \
         *enrichment_output{\
             view: $view, owner, repository, analyzer, input_fingerprint\
         }, \
         *analysis_revision_enrichment_input{\
             view: $view, revision: $revision, repository, analyzer, \
             fingerprint: input_fingerprint\
         }\n"
}

fn materialize_enrichment_contributions(
    transaction: &MultiTransaction,
    view: &str,
    revision: i64,
) -> Result<(), Box<dyn Error>> {
    // Baseline facts always win collisions. Analyzer conflicts are deterministic: observations
    // and overrides prefer higher confidence, then the lexicographically smallest owner; entities
    // and diagnostics use the owner tie-break directly. Existing rows are left untouched so an
    // enrichment publication only rematerializes keys removed from its candidate revision.
    let valid_owner = valid_enrichment_owner_rule();
    let scripts = [
        (
            "entities",
            format!(
                "{valid_owner}\
             state_baseline[id] := *analysis_baseline_entity{{\
                 view: $view, id, revision_owned: false\
             }}\n\
             state_baseline[id] := *analysis_fact_shard_selection{{\
                 view: $view, producer, owner, version\
             }}, *analysis_fact_shard_entity{{producer, owner, version, id}}\n\
             candidate[id, cost, kind, metadata] := *analysis_baseline_entity{{\
                 view: $view, id, kind, metadata, revision_owned: true\
             }}, cost = [0, '']\n\
             candidate[id, cost, kind, metadata] := valid_owner[owner], \
                 *enrichment_entity_contribution{{view: $view, owner, id, kind, metadata}}, \
                 cost = [1, owner]\n\
             winner[id, smallest_by(kind_pair), smallest_by(metadata_pair)] := \
                 candidate[id, cost, kind, metadata], kind_pair = [kind, cost], \
                 metadata_pair = [metadata, cost]\n\
             ?[view, revision, id, kind, metadata] := winner[id, kind, metadata], \
                 not state_baseline[id], \
                 not *analysis_revision_entity{{view: $view, revision: $revision, id}}, \
                 view = $view, revision = $revision \
             :put analysis_revision_entity {{view, revision, id => kind, metadata}}"
            ),
        ),
        (
            "observations",
            format!(
                "{valid_owner}\
             state_baseline[from, relation, to, evidence] := *analysis_baseline_observation{{\
                 view: $view, from, relation, to, evidence, revision_owned: false\
             }}\n\
             state_baseline[from, relation, to, evidence] := \
                 *analysis_fact_shard_selection{{view: $view, producer, owner, version}}, \
                 *analysis_fact_shard_observation{{\
                     producer, owner, version, from, relation, to, evidence\
                 }}\n\
             candidate[from, relation, to, evidence, cost, confidence, provenance] := \
                 *analysis_baseline_observation{{\
                     view: $view, from, relation, to, evidence, confidence, provenance, \
                     revision_owned: true\
                 }}, cost = [0, 0.0, '']\n\
             candidate[from, relation, to, evidence, cost, confidence, provenance] := \
                 valid_owner[owner], *enrichment_observation_contribution{{\
                     view: $view, owner, from, relation, to, evidence, confidence, provenance\
                 }}, cost = [1, -confidence, owner]\n\
             winner[from, relation, to, evidence, smallest_by(confidence_pair), \
                     smallest_by(provenance_pair)] := \
                 candidate[from, relation, to, evidence, cost, confidence, provenance], \
                 confidence_pair = [confidence, cost], \
                 provenance_pair = [provenance, cost]\n\
             ?[view, revision, from, relation, to, evidence, confidence, provenance] := \
                 winner[from, relation, to, evidence, confidence, provenance], \
                 not state_baseline[from, relation, to, evidence], \
                 not *analysis_revision_observation{{\
                     view: $view, revision: $revision, from, relation, to, evidence\
                 }}, \
                 view = $view, revision = $revision \
             :put analysis_revision_observation {{\
                 view, revision, from, relation, to, evidence => confidence, provenance\
             }}"
            ),
        ),
        (
            "overrides",
            format!(
                "{valid_owner}\
             candidate[from, relation, unresolved_to, cost, resolved_to, evidence, confidence, provenance] := \
                 *analysis_baseline_dependency_override{{\
                     view: $view, from, relation, unresolved_to, resolved_to, evidence, \
                     confidence, provenance\
                 }}, cost = [0, 0.0, '']\n\
             candidate[from, relation, unresolved_to, cost, resolved_to, evidence, confidence, provenance] := \
                 valid_owner[owner], *enrichment_override_contribution{{\
                     view: $view, owner, from, relation, unresolved_to, resolved_to, evidence, \
                     confidence, provenance\
                 }}, cost = [1, -confidence, owner]\n\
             winner[from, relation, unresolved_to, smallest_by(resolved_pair), \
                     smallest_by(evidence_pair)] := \
                 candidate[from, relation, unresolved_to, cost, resolved_to, evidence, \
                     _, _], \
                 resolved_pair = [resolved_to, cost], evidence_pair = [evidence, cost]\n\
             ?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
                 winner[from, relation, unresolved_to, resolved_to, evidence], \
                 not *analysis_revision_dependency_override{{\
                     view: $view, revision: $revision, from, relation, unresolved_to\
                 }}, \
                 view = $view, revision = $revision \
             :put analysis_revision_dependency_override {{\
                 view, revision, from, relation, unresolved_to => resolved_to, evidence\
             }}"
            ),
        ),
        (
            "override_metadata",
            format!(
                "{valid_owner}\
             candidate[from, relation, unresolved_to, cost, confidence, provenance] := \
                 *analysis_baseline_dependency_override{{\
                     view: $view, from, relation, unresolved_to, confidence, provenance\
                 }}, cost = [0, 0.0, '']\n\
             candidate[from, relation, unresolved_to, cost, confidence, provenance] := \
                 valid_owner[owner], *enrichment_override_contribution{{\
                     view: $view, owner, from, relation, unresolved_to, confidence, provenance\
                 }}, cost = [1, -confidence, owner]\n\
             winner[from, relation, unresolved_to, smallest_by(confidence_pair), \
                     smallest_by(provenance_pair)] := \
                 candidate[from, relation, unresolved_to, cost, confidence, provenance], \
                 confidence_pair = [confidence, cost], \
                 provenance_pair = [provenance, cost]\n\
             ?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
                 winner[from, relation, unresolved_to, confidence, provenance], \
                 not *analysis_revision_dependency_override_metadata{{\
                     view: $view, revision: $revision, from, relation, unresolved_to\
                 }}, \
                 view = $view, revision = $revision \
             :put analysis_revision_dependency_override_metadata {{\
                 view, revision, from, relation, unresolved_to => confidence, provenance\
             }}"
            ),
        ),
        (
            "diagnostics",
            format!(
                "{valid_owner}\
             candidate[repository, code, severity, path, line, cost, detail] := \
                 *analysis_baseline_diagnostic{{\
                     view: $view, repository, code, severity, path, line, detail\
                 }}, cost = [0, '']\n\
             candidate[repository, code, severity, path, line, cost, detail] := \
                 valid_owner[owner], *enrichment_diagnostic_contribution{{\
                     view: $view, owner, repository, code, severity, path, line, detail\
                 }}, cost = [1, owner]\n\
             winner[repository, code, severity, path, line, smallest_by(detail_pair)] := \
                 candidate[repository, code, severity, path, line, cost, detail], \
                 detail_pair = [detail, cost]\n\
             ?[view, revision, repository, code, severity, path, line, detail] := \
                 winner[repository, code, severity, path, line, detail], \
                 not *analysis_revision_diagnostic{{\
                     view: $view, revision: $revision, repository, code, severity, path, line\
                 }}, \
                 view = $view, revision = $revision \
             :put analysis_revision_diagnostic {{\
                 view, revision, repository, code, severity, path, line => detail\
             }}"
            ),
        ),
    ];
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("revision".into(), revision.into()),
    ]);
    for (relation, script) in scripts {
        let started = Instant::now();
        transaction
            .run_script(&script, params.clone())
            .map_err(|error| {
                format!("failed to materialize enrichment relation {relation}: {error}")
            })?;
        tracing::info!(
            target: "beholder::publication",
            stage = "materialize_enrichment_contributions",
            relation,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "Mnestic publication stage completed"
        );
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
    let owner = enrichment_owner_key(analyzer, repository);
    let rows = db.run_script(
        "?[version] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, fingerprint\
             }, \
             *enrichment_output{\
                 view: $view, owner: $owner, repository: $repository, analyzer: $analyzer, \
                 version, input_fingerprint: fingerprint\
             }",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("owner".into(), owner.into()),
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
                 *enrichment_output{\
                     view: $view, repository, analyzer: $analyzer, version: $version, \
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
    view: &str,
    repository: &str,
    input_fingerprint: &str,
    owner: EnrichmentOwner<'_>,
    payload: EnrichmentPayload<'_>,
) -> Result<bool, Box<dyn Error>> {
    let EnrichmentOwner { analyzer, version } = owner;
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

    let owner = enrichment_owner_key(analyzer, repository);
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        "?[revision, fingerprint] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_enrichment_input{\
                 view: $view, revision, repository: $repository, analyzer: $analyzer, fingerprint\
             }",
        BTreeMap::from([
            ("view".into(), view.into()),
            ("repository".into(), repository.into()),
            ("analyzer".into(), analyzer.into()),
        ]),
    )?;
    let Some(row) = current.rows.first() else {
        transaction.abort()?;
        return Err("published analysis revision is missing".into());
    };
    if row[1].get_str() != Some(input_fingerprint) {
        transaction.abort()?;
        return Ok(false);
    }
    let output_is_current = !transaction
        .run_script(
            "?[owner] := *enrichment_output{\
                 view: $view, owner: $owner, repository: $repository, analyzer: $analyzer, \
                 input_fingerprint: $input_fingerprint\
             }, owner = $owner",
            BTreeMap::from([
                ("view".into(), view.into()),
                ("owner".into(), owner.as_str().into()),
                ("repository".into(), repository.into()),
                ("analyzer".into(), analyzer.into()),
                ("input_fingerprint".into(), input_fingerprint.into()),
            ]),
        )?
        .rows
        .is_empty();
    if output_is_current
        && owner_contributions_match(
            &transaction,
            view,
            &owner,
            entities,
            observations,
            overrides,
            diagnostics,
        )?
    {
        complete_enrichment(
            &transaction,
            view,
            &owner,
            repository,
            analyzer,
            version,
            input_fingerprint,
        )?;
        transaction.commit()?;
        return Ok(true);
    }
    if owner_contributions_are_empty(&transaction, view, &owner)?
        && incoming_contributions_are_effective(
            &transaction,
            view,
            entities,
            observations,
            overrides,
            diagnostics,
        )?
    {
        replace_enrichment_contributions(
            &transaction,
            view,
            &owner,
            entities,
            observations,
            overrides,
            diagnostics,
        )?;
        complete_enrichment(
            &transaction,
            view,
            &owner,
            repository,
            analyzer,
            version,
            input_fingerprint,
        )?;
        transaction.commit()?;
        return Ok(true);
    }

    let previous = row[0]
        .get_int()
        .ok_or("published analysis revision is invalid")?;
    let revision = previous + 1;
    copy_revision(&transaction, view, previous, revision)?;
    remove_owner_contribution_keys(&transaction, view, &owner, revision)?;
    replace_enrichment_contributions(
        &transaction,
        view,
        &owner,
        entities,
        observations,
        overrides,
        diagnostics,
    )?;
    remove_owner_contribution_keys(&transaction, view, &owner, revision)?;
    complete_enrichment(
        &transaction,
        view,
        &owner,
        repository,
        analyzer,
        version,
        input_fingerprint,
    )?;
    materialize_enrichment_contributions(&transaction, view, revision)?;

    let changed = effective_revision_changed(&transaction, view, previous, revision)?;
    if changed {
        transaction.run_script(
            "?[view, revision] <- [[$view, $revision]] \
             :put analysis_revision {view => revision}",
            BTreeMap::from([
                ("view".into(), view.into()),
                ("revision".into(), revision.into()),
            ]),
        )?;
    } else {
        remove_revision_candidate(&transaction, view, revision)?;
    }
    transaction.commit()?;
    Ok(true)
}

fn complete_enrichment(
    transaction: &MultiTransaction,
    view: &str,
    owner: &str,
    repository: &str,
    analyzer: &str,
    version: &str,
    input_fingerprint: &str,
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("owner".into(), owner.into()),
        ("repository".into(), repository.into()),
        ("analyzer".into(), analyzer.into()),
        ("version".into(), version.into()),
        ("input_fingerprint".into(), input_fingerprint.into()),
    ]);
    transaction.run_script(
        "?[view, owner, repository, analyzer, version, input_fingerprint] <- \
             [[$view, $owner, $repository, $analyzer, $version, $input_fingerprint]] \
         :put enrichment_output {\
             view, owner => repository, analyzer, version, input_fingerprint\
         }",
        params,
    )?;
    Ok(())
}

fn owner_contributions_match(
    transaction: &MultiTransaction,
    view: &str,
    owner: &str,
    entities: &[EntityFact],
    observations: &[Observation],
    overrides: &[DependencyOverride],
    diagnostics: &[(String, AnalysisDiagnostic)],
) -> Result<bool, Box<dyn Error>> {
    let entity_rows = entities
        .iter()
        .map(|entity| {
            DataValue::List(vec![
                entity.id.as_str().into(),
                entity_kind(entity.kind).into(),
                entity_metadata(entity.metadata).into(),
            ])
        })
        .collect();
    if relation_differs(
        transaction,
        "incoming[id, kind, metadata] <- $rows\n\
         different[id] := *enrichment_entity_contribution{\
             view: $view, owner: $owner, id, kind, metadata\
         }, not incoming[id, kind, metadata]\n\
         different[id] := incoming[id, kind, metadata], \
             not *enrichment_entity_contribution{\
                 view: $view, owner: $owner, id, kind, metadata\
             }\n\
         ?[id] := different[id] :limit 1",
        entity_rows,
        view,
        owner,
    )? {
        return Ok(false);
    }
    let observation_rows = observations
        .iter()
        .map(|observation| {
            DataValue::List(vec![
                observation.from.as_str().into(),
                observation.relation.as_str().into(),
                observation.to.as_str().into(),
                observation.evidence.as_str().into(),
                observation.confidence.score().into(),
                observation.provenance.as_str().into(),
            ])
        })
        .collect();
    if relation_differs(
        transaction,
        "incoming[from, relation, to, evidence, confidence, provenance] <- $rows\n\
         different[from, relation, to, evidence] := \
             *enrichment_observation_contribution{\
                 view: $view, owner: $owner, from, relation, to, evidence, confidence, provenance\
             }, not incoming[from, relation, to, evidence, confidence, provenance]\n\
         different[from, relation, to, evidence] := \
             incoming[from, relation, to, evidence, confidence, provenance], \
             not *enrichment_observation_contribution{\
                 view: $view, owner: $owner, from, relation, to, evidence, confidence, provenance\
             }\n\
         ?[from, relation, to, evidence] := different[from, relation, to, evidence] :limit 1",
        observation_rows,
        view,
        owner,
    )? {
        return Ok(false);
    }
    let override_rows = overrides
        .iter()
        .map(|override_| {
            DataValue::List(vec![
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
    if relation_differs(
        transaction,
        "incoming[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows\n\
         different[from, relation, unresolved_to] := *enrichment_override_contribution{\
             view: $view, owner: $owner, from, relation, unresolved_to, resolved_to, evidence, \
             confidence, provenance\
         }, not incoming[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance]\n\
         different[from, relation, unresolved_to] := incoming[\
             from, relation, unresolved_to, resolved_to, evidence, confidence, provenance\
         ], not *enrichment_override_contribution{\
             view: $view, owner: $owner, from, relation, unresolved_to, resolved_to, evidence, \
             confidence, provenance\
         }\n\
         ?[from, relation, unresolved_to] := different[from, relation, unresolved_to] :limit 1",
        override_rows,
        view,
        owner,
    )? {
        return Ok(false);
    }
    let diagnostic_rows = diagnostics
        .iter()
        .map(|(repository, diagnostic)| {
            DataValue::List(vec![
                repository.as_str().into(),
                diagnostic.code.as_str().into(),
                diagnostic.severity.as_str().into(),
                diagnostic.path.to_string_lossy().into_owned().into(),
                i64::from(diagnostic.line.unwrap_or_default()).into(),
                diagnostic.detail.as_deref().unwrap_or_default().into(),
            ])
        })
        .collect();
    Ok(!relation_differs(
        transaction,
        "incoming[repository, code, severity, path, line, detail] <- $rows\n\
         different[repository, code, severity, path, line] := \
             *enrichment_diagnostic_contribution{\
                 view: $view, owner: $owner, repository, code, severity, path, line, detail\
             }, not incoming[repository, code, severity, path, line, detail]\n\
         different[repository, code, severity, path, line] := \
             incoming[repository, code, severity, path, line, detail], \
             not *enrichment_diagnostic_contribution{\
                 view: $view, owner: $owner, repository, code, severity, path, line, detail\
             }\n\
         ?[repository, code, severity, path, line] := \
             different[repository, code, severity, path, line] :limit 1",
        diagnostic_rows,
        view,
        owner,
    )?)
}

fn owner_contributions_are_empty(
    transaction: &MultiTransaction,
    view: &str,
    owner: &str,
) -> Result<bool, Box<dyn Error>> {
    let rows = transaction
        .run_script(
            "present[key] := *enrichment_entity_contribution{\
             view: $view, owner: $owner, id: key\
         }\n\
         present[key] := *enrichment_observation_contribution{\
             view: $view, owner: $owner, from: key\
         }\n\
         present[key] := *enrichment_override_contribution{\
             view: $view, owner: $owner, from: key\
         }\n\
         present[key] := *enrichment_diagnostic_contribution{\
             view: $view, owner: $owner, repository: key\
         }\n\
         ?[key] := present[key] :limit 1",
            BTreeMap::from([("view".into(), view.into()), ("owner".into(), owner.into())]),
        )
        .map_err(|error| format!("failed to inspect owner contributions: {error}"))?;
    Ok(rows.rows.is_empty())
}

fn incoming_contributions_are_effective(
    transaction: &MultiTransaction,
    view: &str,
    entities: &[EntityFact],
    observations: &[Observation],
    overrides: &[DependencyOverride],
    diagnostics: &[(String, AnalysisDiagnostic)],
) -> Result<bool, Box<dyn Error>> {
    let entity_rows = entities
        .iter()
        .map(|entity| {
            DataValue::List(vec![
                entity.id.as_str().into(),
                entity_kind(entity.kind).into(),
                entity_metadata(entity.metadata).into(),
            ])
        })
        .collect();
    if incoming_relation_is_missing(
        transaction,
        "incoming[id, kind, metadata] <- $rows\n\
         effective[id, kind, metadata] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id, kind, metadata}\n\
         effective[id, kind, metadata] := *analysis_baseline_entity{\
             view: $view, id, kind, metadata\
         }\n\
         effective[id, kind, metadata] := *analysis_fact_shard_selection{\
             view: $view, producer, owner, version\
         }, *analysis_fact_shard_entity{producer, owner, version, id, kind, metadata}\n\
         ?[id] := incoming[id, kind, metadata], not effective[id, kind, metadata] :limit 1",
        entity_rows,
        view,
    )? {
        return Ok(false);
    }
    let observation_rows = observations
        .iter()
        .map(|observation| {
            DataValue::List(vec![
                observation.from.as_str().into(),
                observation.relation.as_str().into(),
                observation.to.as_str().into(),
                observation.evidence.as_str().into(),
                observation.confidence.score().into(),
                observation.provenance.as_str().into(),
            ])
        })
        .collect();
    if incoming_relation_is_missing(
        transaction,
        "incoming[from, relation, to, evidence, confidence, provenance] <- $rows\n\
         effective[from, relation, to, evidence, confidence, provenance] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_observation{\
                 view: $view, revision, from, relation, to, evidence, confidence, provenance\
             }\n\
         effective[from, relation, to, evidence, confidence, provenance] := \
             *analysis_baseline_observation{\
                 view: $view, from, relation, to, evidence, confidence, provenance\
             }\n\
         effective[from, relation, to, evidence, confidence, provenance] := \
             *analysis_fact_shard_selection{view: $view, producer, owner, version}, \
             *analysis_fact_shard_observation{\
                 producer, owner, version, from, relation, to, evidence, confidence, provenance\
             }\n\
         ?[from, relation, to, evidence] := \
             incoming[from, relation, to, evidence, confidence, provenance], \
             not effective[from, relation, to, evidence, confidence, provenance] :limit 1",
        observation_rows,
        view,
    )? {
        return Ok(false);
    }
    let override_rows = overrides
        .iter()
        .map(|override_| {
            DataValue::List(vec![
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
    if incoming_relation_is_missing(
        transaction,
        "incoming[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows\n\
         effective[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_dependency_override{\
                 view: $view, revision, from, relation, unresolved_to, resolved_to, evidence\
             }, *analysis_revision_dependency_override_metadata{\
                 view: $view, revision, from, relation, unresolved_to, confidence, provenance\
             }\n\
         effective[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
             *analysis_baseline_dependency_override{\
                 view: $view, from, relation, unresolved_to, resolved_to, evidence, confidence, \
                 provenance\
             }\n\
         ?[from, relation, unresolved_to] := incoming[\
             from, relation, unresolved_to, resolved_to, evidence, confidence, provenance\
         ], not effective[\
             from, relation, unresolved_to, resolved_to, evidence, confidence, provenance\
         ] :limit 1",
        override_rows,
        view,
    )? {
        return Ok(false);
    }
    let diagnostic_rows = diagnostics
        .iter()
        .map(|(repository, diagnostic)| {
            DataValue::List(vec![
                repository.as_str().into(),
                diagnostic.code.as_str().into(),
                diagnostic.severity.as_str().into(),
                diagnostic.path.to_string_lossy().into_owned().into(),
                i64::from(diagnostic.line.unwrap_or_default()).into(),
                diagnostic.detail.as_deref().unwrap_or_default().into(),
            ])
        })
        .collect();
    Ok(!incoming_relation_is_missing(
        transaction,
        "incoming[repository, code, severity, path, line, detail] <- $rows\n\
         effective[repository, code, severity, path, line, detail] := \
             *analysis_revision{view: $view, revision}, *analysis_revision_diagnostic{\
                 view: $view, revision, repository, code, severity, path, line, detail\
             }\n\
         effective[repository, code, severity, path, line, detail] := \
             *analysis_baseline_diagnostic{\
                 view: $view, repository, code, severity, path, line, detail\
             }\n\
         ?[repository, code, severity, path, line] := \
             incoming[repository, code, severity, path, line, detail], \
             not effective[repository, code, severity, path, line, detail] :limit 1",
        diagnostic_rows,
        view,
    )?)
}

fn incoming_relation_is_missing(
    transaction: &MultiTransaction,
    script: &str,
    rows: Vec<DataValue>,
    view: &str,
) -> Result<bool, Box<dyn Error>> {
    Ok(!transaction
        .run_script(
            script,
            BTreeMap::from([
                ("view".into(), view.into()),
                ("rows".into(), DataValue::List(rows)),
            ]),
        )
        .map_err(|error| format!("failed to compare incoming effective contribution: {error}"))?
        .rows
        .is_empty())
}

fn relation_differs(
    transaction: &MultiTransaction,
    script: &str,
    rows: Vec<DataValue>,
    view: &str,
    owner: &str,
) -> Result<bool, Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("owner".into(), owner.into()),
        ("rows".into(), DataValue::List(rows)),
    ]);
    Ok(!transaction.run_script(script, params)?.rows.is_empty())
}

fn copy_revision(
    transaction: &MultiTransaction,
    view: &str,
    previous: i64,
    revision: i64,
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("previous".into(), previous.into()),
        ("revision".into(), revision.into()),
    ]);
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
             }, view = $view, revision = $revision \
         :put analysis_revision_enrichment_input {\
             view, revision, repository, analyzer => fingerprint\
         }",
        "?[view, revision, target, analyzer, context] := \
             *analysis_revision_context{\
                 view: $view, revision: $previous, target, analyzer, context\
             }, view = $view, revision = $revision \
         :put analysis_revision_context {view, revision, target, analyzer, context}",
        "?[view, revision, incomplete] := \
             *analysis_revision_metadata{view: $view, revision: $previous, incomplete}, \
             view = $view, revision = $revision \
         :put analysis_revision_metadata {view, revision => incomplete}",
        "?[view, revision, local_symbol, role, service, method, evidence, code, detail] := \
             *analysis_revision_grpc_diagnostic{\
                 view: $view, revision: $previous, local_symbol, role, service, method, evidence, \
                 code, detail\
             }, view = $view, revision = $revision \
         :put analysis_revision_grpc_diagnostic {\
             view, revision, local_symbol, role, service, method, evidence => code, detail\
         }",
    ] {
        transaction.run_script(script, params.clone())?;
    }

    for script in [
        "?[view, revision, id, kind, metadata] := *analysis_revision_entity{\
             view: $view, revision: $previous, id, kind, metadata\
         }, view = $view, revision = $revision \
         :put analysis_revision_entity {view, revision, id => kind, metadata}",
        "?[view, revision, from, relation, to, evidence, confidence, provenance] := \
             *analysis_revision_observation{\
                 view: $view, revision: $previous, from, relation, to, evidence, confidence, \
                 provenance\
             }, view = $view, revision = $revision \
         :put analysis_revision_observation {\
             view, revision, from, relation, to, evidence => confidence, provenance\
         }",
        "?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
             *analysis_revision_dependency_override{\
                 view: $view, revision: $previous, from, relation, unresolved_to, resolved_to, \
                 evidence\
             }, view = $view, revision = $revision \
         :put analysis_revision_dependency_override {\
             view, revision, from, relation, unresolved_to => resolved_to, evidence\
         }",
        "?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
             *analysis_revision_dependency_override_metadata{\
                 view: $view, revision: $previous, from, relation, unresolved_to, confidence, \
                 provenance\
             }, view = $view, revision = $revision \
         :put analysis_revision_dependency_override_metadata {\
             view, revision, from, relation, unresolved_to => confidence, provenance\
         }",
        "?[view, revision, repository, code, severity, path, line, detail] := \
             *analysis_revision_diagnostic{\
                 view: $view, revision: $previous, repository, code, severity, path, line, detail\
             }, view = $view, revision = $revision \
         :put analysis_revision_diagnostic {\
             view, revision, repository, code, severity, path, line => detail\
         }",
    ] {
        transaction.run_script(script, params.clone())?;
    }
    Ok(())
}

fn remove_owner_contribution_keys(
    transaction: &MultiTransaction,
    view: &str,
    owner: &str,
    revision: i64,
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("owner".into(), owner.into()),
        ("revision".into(), revision.into()),
    ]);
    for script in [
        "?[view, revision, id] := *enrichment_entity_contribution{\
             view: $view, owner: $owner, id\
         }, view = $view, revision = $revision \
         :rm analysis_revision_entity {view, revision, id}",
        "?[view, revision, from, relation, to, evidence] := \
             *enrichment_observation_contribution{\
                 view: $view, owner: $owner, from, relation, to, evidence\
             }, view = $view, revision = $revision \
         :rm analysis_revision_observation {view, revision, from, relation, to, evidence}",
        "?[view, revision, from, relation, unresolved_to] := \
             *enrichment_override_contribution{\
                 view: $view, owner: $owner, from, relation, unresolved_to\
             }, view = $view, revision = $revision \
         :rm analysis_revision_dependency_override {view, revision, from, relation, unresolved_to}",
        "?[view, revision, from, relation, unresolved_to] := \
             *enrichment_override_contribution{\
                 view: $view, owner: $owner, from, relation, unresolved_to\
             }, view = $view, revision = $revision \
         :rm analysis_revision_dependency_override_metadata {\
             view, revision, from, relation, unresolved_to\
         }",
        "?[view, revision, repository, code, severity, path, line] := \
             *enrichment_diagnostic_contribution{\
                 view: $view, owner: $owner, repository, code, severity, path, line\
             }, view = $view, revision = $revision \
         :rm analysis_revision_diagnostic {view, revision, repository, code, severity, path, line}",
    ] {
        transaction.run_script(script, params.clone())?;
    }
    Ok(())
}

fn replace_enrichment_contributions(
    transaction: &MultiTransaction,
    view: &str,
    owner: &str,
    entities: &[EntityFact],
    observations: &[Observation],
    overrides: &[DependencyOverride],
    diagnostics: &[(String, beholder_domain::AnalysisDiagnostic)],
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([("view".into(), view.into()), ("owner".into(), owner.into())]);
    for script in [
        "?[view, owner, id] := *enrichment_entity_contribution{\
             view: $view, owner, id\
         }, view = $view, owner = $owner \
         :rm enrichment_entity_contribution {view, owner, id}",
        "?[view, owner, from, relation, to, evidence] := \
             *enrichment_observation_contribution{\
                 view: $view, owner, from, relation, to, evidence\
             }, view = $view, owner = $owner \
             :rm enrichment_observation_contribution {\
                 view, owner, from, relation, to, evidence\
             }",
        "?[view, owner, from, relation, unresolved_to] := \
             *enrichment_override_contribution{\
                 view: $view, owner, from, relation, unresolved_to\
             }, view = $view, owner = $owner \
             :rm enrichment_override_contribution {\
                 view, owner, from, relation, unresolved_to\
             }",
        "?[view, owner, repository, code, severity, path, line] := \
             *enrichment_diagnostic_contribution{\
                 view: $view, owner, repository, code, severity, path, line\
             }, view = $view, owner = $owner \
             :rm enrichment_diagnostic_contribution {\
                 view, owner, repository, code, severity, path, line\
             }",
    ] {
        transaction.run_script(script, params.clone())?;
    }
    for entities in entities.chunks(FACT_BATCH_SIZE) {
        let rows = entities
            .iter()
            .map(|entity| {
                DataValue::List(vec![
                    view.into(),
                    owner.into(),
                    entity.id.as_str().into(),
                    entity_kind(entity.kind).into(),
                    entity_metadata(entity.metadata).into(),
                ])
            })
            .collect::<Vec<_>>();
        transaction.run_script(
            "?[view, owner, id, kind, metadata] <- $rows \
             :put enrichment_entity_contribution {view, owner, id => kind, metadata}",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    for observations in observations.chunks(FACT_BATCH_SIZE) {
        let rows = observations
            .iter()
            .map(|observation| {
                DataValue::List(vec![
                    view.into(),
                    owner.into(),
                    observation.from.as_str().into(),
                    observation.relation.as_str().into(),
                    observation.to.as_str().into(),
                    observation.evidence.as_str().into(),
                    observation.confidence.score().into(),
                    observation.provenance.as_str().into(),
                ])
            })
            .collect::<Vec<_>>();
        transaction.run_script(
            "?[view, owner, from, relation, to, evidence, confidence, provenance] <- $rows \
             :put enrichment_observation_contribution {\
                 view, owner, from, relation, to, evidence => confidence, provenance\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    for overrides in overrides.chunks(FACT_BATCH_SIZE) {
        let rows = overrides
            .iter()
            .map(|override_| {
                DataValue::List(vec![
                    view.into(),
                    owner.into(),
                    override_.from.as_str().into(),
                    override_.relation.as_str().into(),
                    override_.unresolved_to.as_str().into(),
                    override_.resolved_to.as_str().into(),
                    override_.evidence.as_str().into(),
                    override_.confidence.score().into(),
                    override_.provenance.as_str().into(),
                ])
            })
            .collect::<Vec<_>>();
        transaction.run_script(
            "?[view, owner, from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] <- $rows \
             :put enrichment_override_contribution {\
                 view, owner, from, relation, unresolved_to => resolved_to, evidence, confidence, \
                 provenance\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    for diagnostics in diagnostics.chunks(FACT_BATCH_SIZE) {
        let rows = diagnostics
            .iter()
            .map(|(repository, diagnostic)| {
                DataValue::List(vec![
                    view.into(),
                    owner.into(),
                    repository.as_str().into(),
                    diagnostic.code.as_str().into(),
                    diagnostic.severity.as_str().into(),
                    diagnostic.path.to_string_lossy().into_owned().into(),
                    i64::from(diagnostic.line.unwrap_or_default()).into(),
                    diagnostic.detail.as_deref().unwrap_or_default().into(),
                ])
            })
            .collect::<Vec<_>>();
        transaction.run_script(
            "?[view, owner, repository, code, severity, path, line, detail] <- $rows \
             :put enrichment_diagnostic_contribution {\
                 view, owner, repository, code, severity, path, line => detail\
             }",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    Ok(())
}

fn effective_revision_changed(
    transaction: &MultiTransaction,
    view: &str,
    previous: i64,
    revision: i64,
) -> Result<bool, Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("previous".into(), previous.into()),
        ("revision".into(), revision.into()),
    ]);
    for query in [
        "?[id] := *analysis_revision_entity{\
             view: $view, revision: $revision, id, kind, metadata\
         }, not *analysis_revision_entity{\
             view: $view, revision: $previous, id, kind, metadata\
         } :limit 1",
        "?[from, relation, to, evidence] := *analysis_revision_observation{\
             view: $view, revision: $revision, from, relation, to, evidence, confidence, provenance\
         }, not *analysis_revision_observation{\
             view: $view, revision: $previous, from, relation, to, evidence, confidence, provenance\
         } :limit 1",
        "?[repository, code, severity, path, line] := *analysis_revision_diagnostic{\
             view: $view, revision: $revision, repository, code, severity, path, line, detail\
         }, not *analysis_revision_diagnostic{\
             view: $view, revision: $previous, repository, code, severity, path, line, detail\
         } :limit 1",
        "?[from, relation, unresolved_to] := *analysis_revision_dependency_override{\
             view: $view, revision: $revision, from, relation, unresolved_to, resolved_to, evidence\
         }, not *analysis_revision_dependency_override{\
             view: $view, revision: $previous, from, relation, unresolved_to, resolved_to, evidence\
         } :limit 1",
        "?[from, relation, unresolved_to] := *analysis_revision_dependency_override_metadata{\
             view: $view, revision: $revision, from, relation, unresolved_to, confidence, provenance\
         }, not *analysis_revision_dependency_override_metadata{\
             view: $view, revision: $previous, from, relation, unresolved_to, confidence, provenance\
         } :limit 1",
    ] {
        if !transaction
            .run_script(query, params.clone())?
            .rows
            .is_empty()
        {
            return Ok(true);
        }
        let reverse = query
            .replace("revision: $revision", "revision: $swap")
            .replace("revision: $previous", "revision: $revision")
            .replace("revision: $swap", "revision: $previous");
        if !transaction
            .run_script(&reverse, params.clone())?
            .rows
            .is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_revision_candidate(
    transaction: &MultiTransaction,
    view: &str,
    revision: i64,
) -> Result<(), Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.into()),
        ("revision".into(), revision.into()),
    ]);
    for (relation, keys) in [
        ("analysis_revision_state", "view, revision, repository"),
        ("analysis_revision_input", "view, revision, repository"),
        (
            "analysis_revision_enrichment_input",
            "view, revision, repository, analyzer",
        ),
        (
            "analysis_revision_context",
            "view, revision, target, analyzer, context",
        ),
        ("analysis_revision_metadata", "view, revision"),
        (
            "analysis_revision_grpc_diagnostic",
            "view, revision, local_symbol, role, service, method, evidence",
        ),
        ("analysis_revision_entity", "view, revision, id"),
        (
            "analysis_revision_observation",
            "view, revision, from, relation, to, evidence",
        ),
        (
            "analysis_revision_diagnostic",
            "view, revision, repository, code, severity, path, line",
        ),
        (
            "analysis_revision_dependency_override",
            "view, revision, from, relation, unresolved_to",
        ),
        (
            "analysis_revision_dependency_override_metadata",
            "view, revision, from, relation, unresolved_to",
        ),
    ] {
        transaction.run_script(
            &format!(
                "?[{keys}] := *{relation}{{{keys}}}, view = $view, revision = $revision \
                 :rm {relation} {{{keys}}}"
            ),
            params.clone(),
        )?;
    }
    Ok(())
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
            "?[pending] := *garbage_collection_state{state}, pending = true\n\
             ?[pending] := *analysis_revision{view, revision: current}, \
                 *analysis_revision_metadata{view, revision}, revision < current, pending = true\n\
             :limit 1",
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
        collections::{BTreeMap, BTreeSet},
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

    fn current_revision(store: &SemanticStore, view: &str) -> i64 {
        store
            .db
            .run_script(
                "?[revision] := *analysis_revision{view: $view, revision}",
                BTreeMap::from([("view".into(), view.into())]),
                ScriptMutability::Immutable,
            )
            .unwrap()
            .rows[0][0]
            .get_int()
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
    fn selects_declared_baseline_facts_and_relationship_endpoints() {
        let store = SemanticStore::memory().unwrap();
        let view = WorkspaceView::new(
            "main",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "app".into(),
                },
                head: None,
                fingerprint: "state".into(),
            }],
        )
        .unwrap();
        let mut facts = facts(
            &view,
            vec![Observation::dependency(
                "repo://app/elixir/Producer.publish/1",
                DependencyRelation::Publishes,
                "kafka-topic://events",
                "lib/producer.ex:3",
            )],
        );
        facts.entities = vec![
            EntityFact::new(
                "repo://app/elixir/Producer.publish/1",
                EntityKind::Callable,
                None,
            )
            .unwrap(),
            EntityFact::new("kafka-topic://events", EntityKind::KafkaTopic, None).unwrap(),
            EntityFact::new(
                "proto-type://events.Envelope",
                EntityKind::ProtoType,
                Some(EntityMetadata::ProtoType {
                    kind: ProtoTypeKind::Message,
                }),
            )
            .unwrap(),
        ];
        store.publish(&view, &[facts], &[]).unwrap();

        let (entities, observations) = store
            .selected_baseline_semantics(
                "main",
                "app",
                &BTreeSet::from([EntityKind::ProtoType]),
                &BTreeSet::from([SemanticRelation::Dependency(DependencyRelation::Publishes)]),
            )
            .unwrap();

        assert_eq!(entities.len(), 3);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].relation.as_str(), "publishes");
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
                &initial.name,
                "example/a",
                &initial
                    .repository_enrichment_input_fingerprint(&initial.repository_states[0], "rust"),
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "1",
                },
                EnrichmentPayload {
                    entities: std::slice::from_ref(&entity_a),
                    ..EnrichmentPayload::default()
                },
            )
            .unwrap();
        store
            .publish_enrichment(
                &initial.name,
                "example/b",
                &initial
                    .repository_enrichment_input_fingerprint(&initial.repository_states[1], "rust"),
                EnrichmentOwner {
                    analyzer: "rust",
                    version: "1",
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
                    &started.name,
                    "example/a",
                    &target_input,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
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
                    &started.name,
                    "example/a",
                    &target_input,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "2",
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
                    &view.name,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
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
                    &view.name,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "2",
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
                    &stale.name,
                    "example/repo",
                    &stale.repository_enrichment_input_fingerprint(
                        &stale.repository_states[0],
                        "rust",
                    ),
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
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
                    &view.name,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "elixir",
                        version: "1",
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
                    &view.name,
                    "example/repo",
                    &input_fingerprint,
                    EnrichmentOwner {
                        analyzer: "elixir",
                        version: "2",
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

    #[test]
    fn colliding_enrichment_facts_never_replace_or_retract_baseline_facts() {
        let store = SemanticStore::memory().unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "baseline-collision",
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
            &["compiler"],
        );
        let entity =
            EntityFact::new("repo://example/repo/shared", EntityKind::Callable, None).unwrap();
        let observation = Observation::dependency(
            entity.id.as_str(),
            DependencyRelation::Calls,
            "call://shared",
            "src/lib.rs:1",
        );
        let diagnostic = AnalysisDiagnostic {
            code: "shared.warning".into(),
            severity: AnalysisDiagnosticSeverity::KnownLimitation,
            path: "src/lib.rs".into(),
            line: Some(7),
            detail: Some("baseline detail".into()),
        };
        let baseline_override = DependencyOverride {
            from: observation.from.clone(),
            relation: DependencyRelation::Calls,
            unresolved_to: observation.to.clone(),
            resolved_to: "repo://example/repo/baseline-target".into(),
            evidence: observation.evidence.clone(),
            confidence: Confidence::Exact,
            provenance: Provenance::Ast,
        };
        let mut baseline = facts(&view, vec![observation.clone()]);
        baseline.entities.push(entity.clone());
        baseline.diagnostics.push(diagnostic.clone());
        store
            .publish(&view, &[baseline], std::slice::from_ref(&baseline_override))
            .unwrap();

        let mut analyzer_observation = observation.clone();
        analyzer_observation.confidence = Confidence::Inferred;
        analyzer_observation.provenance = Provenance::Compiler;
        let analyzer_override = DependencyOverride {
            resolved_to: "repo://example/repo/analyzer-target".into(),
            confidence: Confidence::Inferred,
            provenance: Provenance::Compiler,
            ..baseline_override.clone()
        };
        let analyzer_diagnostic = AnalysisDiagnostic {
            detail: Some("analyzer detail".into()),
            ..diagnostic.clone()
        };
        let analyzer_entity =
            EntityFact::new(entity.id.as_str(), EntityKind::Namespace, None).unwrap();
        let input =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "compiler");
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer: "compiler",
                        version: "1",
                    },
                    EnrichmentPayload {
                        entities: &[analyzer_entity],
                        observations: &[analyzer_observation],
                        overrides: &[analyzer_override],
                        diagnostics: &[("example/repo".into(), analyzer_diagnostic)],
                    },
                )
                .unwrap()
        );
        assert_eq!(current_revision(&store, "baseline-collision"), 1);

        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer: "compiler",
                        version: "2",
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert_eq!(current_revision(&store, "baseline-collision"), 1);
        let rows = store
            .db
            .run_script(
                "?[kind, confidence, provenance, resolved_to, override_provenance, detail] := \
                     *analysis_revision{view: 'baseline-collision', revision}, \
                     *analysis_revision_state{view: 'baseline-collision', revision, state}, \
                     *state_entity{state, id: $id, kind}, \
                     *state_observation_metadata{\
                         state, from: $id, relation: 'calls', to: 'call://shared', confidence, \
                         provenance\
                     }, not *analysis_revision_entity{\
                         view: 'baseline-collision', revision, id: $id\
                     }, not *analysis_revision_observation{\
                         view: 'baseline-collision', revision, from: $id, relation: 'calls', \
                         to: 'call://shared', evidence: 'src/lib.rs:1'\
                     }, *analysis_revision_dependency_override{\
                         view: 'baseline-collision', revision, from: $id, relation: 'calls', \
                         unresolved_to: 'call://shared', resolved_to\
                     }, *analysis_revision_dependency_override_metadata{\
                         view: 'baseline-collision', revision, from: $id, relation: 'calls', \
                         unresolved_to: 'call://shared', provenance: override_provenance\
                     }, *analysis_revision_diagnostic{\
                         view: 'baseline-collision', revision, repository: 'example/repo', \
                         code: 'shared.warning', severity: 'known_limitation', path: 'src/lib.rs', \
                         line: 7, detail\
                     }",
                BTreeMap::from([("id".into(), entity.id.as_str().into())]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0][0].get_str(), Some(entity_kind(entity.kind)));
        assert_eq!(rows.rows[0][1].get_float(), Some(Confidence::Exact.score()));
        assert_eq!(rows.rows[0][2].get_str(), Some(Provenance::Ast.as_str()));
        assert_eq!(
            rows.rows[0][3].get_str(),
            Some("repo://example/repo/baseline-target")
        );
        assert_eq!(rows.rows[0][4].get_str(), Some(Provenance::Ast.as_str()));
        assert_eq!(rows.rows[0][5].get_str(), Some("baseline detail"));
    }

    #[test]
    fn identical_contributions_keep_independent_owners_without_revision_churn() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-multi-owner-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        let store = SemanticStore::persistent(&database, true).unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "multi-owner",
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
            &["compiler-a", "compiler-b"],
        );
        store
            .publish(&view, &[facts(&view, Vec::new())], &[])
            .unwrap();
        let entity = EntityFact::new(
            "repo://example/repo/generated/shared",
            EntityKind::Callable,
            None,
        )
        .unwrap();

        for analyzer in ["compiler-b", "compiler-a"] {
            let input =
                view.repository_enrichment_input_fingerprint(&view.repository_states[0], analyzer);
            assert!(
                store
                    .publish_enrichment(
                        &view.name,
                        "example/repo",
                        &input,
                        EnrichmentOwner {
                            analyzer,
                            version: "1",
                        },
                        EnrichmentPayload {
                            entities: std::slice::from_ref(&entity),
                            ..EnrichmentPayload::default()
                        },
                    )
                    .unwrap()
            );
        }
        assert_eq!(current_revision(&store, "multi-owner"), 2);
        let contributions = store
            .db
            .run_script(
                "?[count(owner)] := *enrichment_entity_contribution{\
                     view: 'multi-owner', owner, id: $id\
                 }",
                BTreeMap::from([("id".into(), entity.id.as_str().into())]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(contributions.rows[0][0].get_int(), Some(2));
        drop(store);

        let store = SemanticStore::persistent(&database, false).unwrap();
        assert_eq!(current_revision(&store, "multi-owner"), 2);
        let invalid_diagnostic = AnalysisDiagnostic {
            code: "compiler.partial".into(),
            severity: AnalysisDiagnosticSeverity::KnownLimitation,
            path: "src/lib.rs".into(),
            line: None,
            detail: None,
        };
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &view.repository_enrichment_input_fingerprint(
                        &view.repository_states[0],
                        "compiler-a",
                    ),
                    EnrichmentOwner {
                        analyzer: "compiler-a",
                        version: "invalid",
                    },
                    EnrichmentPayload {
                        diagnostics: &[("different/repository".into(), invalid_diagnostic)],
                        ..EnrichmentPayload::default()
                    },
                )
                .is_err()
        );
        assert_eq!(current_revision(&store, "multi-owner"), 2);

        let input_a =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "compiler-a");
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input_a,
                    EnrichmentOwner {
                        analyzer: "compiler-a",
                        version: "2",
                    },
                    EnrichmentPayload {
                        entities: std::slice::from_ref(&entity),
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap()
        );
        assert_eq!(current_revision(&store, "multi-owner"), 2);
        assert!(
            store
                .enrichment_matches("multi-owner", "example/repo", "compiler-a", "2")
                .unwrap()
        );
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input_a,
                    EnrichmentOwner {
                        analyzer: "compiler-a",
                        version: "3",
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert_eq!(current_revision(&store, "multi-owner"), 2);
        assert_eq!(
            store
                .context("multi-owner", entity.id.as_str())
                .unwrap()
                .root
                .id,
            entity.id.as_str()
        );

        let input_b =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "compiler-b");
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input_b,
                    EnrichmentOwner {
                        analyzer: "compiler-b",
                        version: "2",
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        assert_eq!(current_revision(&store, "multi-owner"), 3);
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn removed_enrichment_owners_are_retracted_and_pruned() {
        let store = SemanticStore::memory().unwrap();
        let states = ["repo-a", "repo-b"]
            .into_iter()
            .map(|repository| RepositoryState {
                repository: LogicalRepository {
                    identity: repository.into(),
                },
                head: None,
                fingerprint: format!("{repository}-source"),
            })
            .collect::<Vec<_>>();
        let view = with_enrichment_analyzers(
            WorkspaceView::new("owner-removal", "syntax", states.clone()).unwrap(),
            &["a", "b"],
        );
        let repository_facts = states
            .iter()
            .cloned()
            .map(|state| RepositoryFacts {
                state,
                analysis_identity: "analysis".into(),
                incomplete: false,
                diagnostics: Vec::new(),
                entities: Vec::new(),
                grpc_bindings: Vec::new(),
                observations: Vec::new(),
            })
            .collect::<Vec<_>>();
        store.publish(&view, &repository_facts, &[]).unwrap();

        let shared = EntityFact::new("repo://repo-a/shared", EntityKind::Callable, None).unwrap();
        let only_a = EntityFact::new("repo://repo-a/only-a", EntityKind::Callable, None).unwrap();
        for (repository, analyzer, entities) in [
            ("repo-a", "a", vec![shared.clone(), only_a.clone()]),
            ("repo-a", "b", vec![shared.clone()]),
            (
                "repo-b",
                "a",
                vec![EntityFact::new("repo://repo-b/only-a", EntityKind::Callable, None).unwrap()],
            ),
        ] {
            let state = view
                .repository_states
                .iter()
                .find(|state| state.repository.identity == repository)
                .unwrap();
            store
                .publish_enrichment(
                    &view.name,
                    repository,
                    &view.repository_enrichment_input_fingerprint(state, analyzer),
                    EnrichmentOwner {
                        analyzer,
                        version: "1",
                    },
                    EnrichmentPayload {
                        entities: &entities,
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap();
        }

        let only_b = with_enrichment_analyzers(
            WorkspaceView::new("owner-removal", "syntax", states.clone()).unwrap(),
            &["b"],
        );
        assert!(store.ensure_revision_inputs(&only_b).unwrap());
        assert_eq!(current_revision(&store, "owner-removal"), 4);
        let current_entity = |id: &str| {
            !store
                .db
                .run_script(
                    "?[id] := *analysis_revision{view: 'owner-removal', revision}, \
                         *analysis_revision_entity{view: 'owner-removal', revision, id: $id}, \
                         id = $id",
                    BTreeMap::from([("id".into(), id.into())]),
                    ScriptMutability::Immutable,
                )
                .unwrap()
                .rows
                .is_empty()
        };
        assert!(current_entity(shared.id.as_str()));
        assert!(!current_entity(only_a.id.as_str()));

        let removed_owners: Vec<DataValue> = states
            .iter()
            .map(|state| enrichment_owner_key("a", &state.repository.identity))
            .map(|owner| DataValue::List(vec![owner.into()]))
            .collect();
        let removed_state = store
            .db
            .run_script(
                "removed[owner] <- $owners\n\
                 present[owner] := removed[owner], *enrichment_output{\
                     view: 'owner-removal', owner\
                 }\n\
                 present[owner] := removed[owner], *enrichment_entity_contribution{\
                     view: 'owner-removal', owner\
                 }\n\
                 ?[owner] := present[owner]",
                BTreeMap::from([("owners".into(), DataValue::List(removed_owners))]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert!(removed_state.rows.is_empty());

        let repo_b_only = with_enrichment_analyzers(
            WorkspaceView::new("owner-removal", "syntax", vec![states[1].clone()]).unwrap(),
            &["b"],
        );
        store
            .publish(
                &repo_b_only,
                &[RepositoryFacts {
                    state: states[1].clone(),
                    analysis_identity: "analysis".into(),
                    incomplete: false,
                    diagnostics: Vec::new(),
                    entities: Vec::new(),
                    grpc_bindings: Vec::new(),
                    observations: Vec::new(),
                }],
                &[],
            )
            .unwrap();
        assert!(!current_entity(shared.id.as_str()));
        let repo_a_owner = enrichment_owner_key("b", "repo-a");
        assert!(
            store
                .db
                .run_script(
                    "present[owner] := *enrichment_output{\
                         view: 'owner-removal', owner\
                     }, owner = $owner\n\
                     present[owner] := *enrichment_entity_contribution{\
                         view: 'owner-removal', owner\
                     }, owner = $owner\n\
                     ?[owner] := present[owner]",
                    BTreeMap::from([("owner".into(), repo_a_owner.into())]),
                    ScriptMutability::Immutable,
                )
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn conflicting_overrides_use_confidence_then_owner_as_the_deterministic_policy() {
        let store = SemanticStore::memory().unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "override-policy",
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
            &["a", "b"],
        );
        let call = Observation::dependency(
            "repo://example/repo/caller",
            DependencyRelation::Calls,
            "call://target",
            "src/lib.rs:1",
        );
        store
            .publish(&view, &[facts(&view, vec![call.clone()])], &[])
            .unwrap();
        let override_for = |resolved_to: &str, confidence, provenance| DependencyOverride {
            from: call.from.clone(),
            relation: DependencyRelation::Calls,
            unresolved_to: call.to.clone(),
            resolved_to: resolved_to.into(),
            evidence: call.evidence.clone(),
            confidence,
            provenance,
        };
        for (analyzer, override_) in [
            (
                "b",
                override_for(
                    "repo://example/repo/target-b",
                    Confidence::Exact,
                    Provenance::Compiler,
                ),
            ),
            (
                "a",
                override_for(
                    "repo://example/repo/target-a",
                    Confidence::Exact,
                    Provenance::Ast,
                ),
            ),
        ] {
            let input =
                view.repository_enrichment_input_fingerprint(&view.repository_states[0], analyzer);
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer,
                        version: "1",
                    },
                    EnrichmentPayload {
                        overrides: std::slice::from_ref(&override_),
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap();
        }
        let selected = store
            .db
            .run_script(
                "?[resolved_to, provenance] := *analysis_revision{\
                     view: 'override-policy', revision\
                 }, *analysis_revision_dependency_override{\
                     view: 'override-policy', revision, from: $from, relation: 'calls', \
                     unresolved_to: 'call://target', resolved_to\
                 }, *analysis_revision_dependency_override_metadata{\
                     view: 'override-policy', revision, from: $from, relation: 'calls', \
                     unresolved_to: 'call://target', provenance\
                 }",
                BTreeMap::from([("from".into(), "repo://example/repo/caller".into())]),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert_eq!(
            selected.rows[0][0].get_str(),
            Some("repo://example/repo/target-a")
        );
        assert_eq!(selected.rows[0][1].get_str(), Some("ast"));

        let input = view.repository_enrichment_input_fingerprint(&view.repository_states[0], "a");
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer: "a",
                        version: "2",
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap()
        );
        let context = store
            .context("override-policy", "repo://example/repo/caller")
            .unwrap();
        assert!(
            context
                .edges
                .iter()
                .any(|edge| edge.to == "repo://example/repo/target-b")
        );
    }

    #[test]
    fn garbage_collection_sweeps_superseded_revisions_without_stale_states() {
        let store = SemanticStore::memory().unwrap();
        let view = with_enrichment_analyzers(
            WorkspaceView::new(
                "revision-gc",
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
        let input =
            view.repository_enrichment_input_fingerprint(&view.repository_states[0], "rust");
        let entity =
            EntityFact::new("repo://example/repo/generated", EntityKind::Callable, None).unwrap();
        assert!(
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer: "rust",
                        version: "1",
                    },
                    EnrichmentPayload {
                        entities: &[entity],
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap()
        );
        assert_eq!(store.garbage_collect().unwrap().repository_states_queued, 0);
        assert!(store.garbage_collection_pending().unwrap());
        store.sweep_garbage_collection(|_| true).unwrap();
        assert!(!store.garbage_collection_pending().unwrap());
        let old = store
            .db
            .run_script(
                "?[revision] := *analysis_revision_metadata{\
                     view: 'revision-gc', revision\
                 }, revision < 2",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .unwrap();
        assert!(old.rows.is_empty());
    }

    #[test]
    #[ignore = "manual multi-analyzer revision and storage growth benchmark"]
    fn multi_analyzer_enrichment_revision_growth_benchmark() {
        const ANALYZERS: usize = 250;
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-owner-bench-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("beholder.db");
        let wal = state_dir.join("beholder.db-wal");
        let analyzers = (0..ANALYZERS)
            .map(|index| format!("analyzer-{index:03}"))
            .collect::<Vec<_>>();
        let view = WorkspaceView::new(
            "owner-benchmark",
            "syntax",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repo".into(),
                },
                head: None,
                fingerprint: "source".into(),
            }],
        )
        .unwrap()
        .with_repository_contexts(
            analyzers
                .iter()
                .map(|analyzer| (analyzer.clone(), BTreeMap::new()))
                .collect(),
        )
        .unwrap();
        let store = SemanticStore::persistent(&database, true).unwrap();
        store
            .publish(&view, &[facts(&view, Vec::new())], &[])
            .unwrap();
        store.checkpoint().unwrap();
        let baseline_database_bytes = fs::metadata(&database).map_or(0, |metadata| metadata.len());
        let entity = EntityFact::new(
            "repo://example/repo/generated/shared",
            EntityKind::Callable,
            None,
        )
        .unwrap();
        let started = Instant::now();
        for analyzer in &analyzers {
            let input =
                view.repository_enrichment_input_fingerprint(&view.repository_states[0], analyzer);
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer,
                        version: "1",
                    },
                    EnrichmentPayload {
                        entities: std::slice::from_ref(&entity),
                        ..EnrichmentPayload::default()
                    },
                )
                .unwrap();
        }
        let publish_elapsed = started.elapsed();
        assert_eq!(current_revision(&store, "owner-benchmark"), 2);
        let contribution_database_bytes =
            fs::metadata(&database).map_or(0, |metadata| metadata.len());
        let contribution_wal_bytes = fs::metadata(&wal).map_or(0, |metadata| metadata.len());
        for analyzer in &analyzers {
            let input =
                view.repository_enrichment_input_fingerprint(&view.repository_states[0], analyzer);
            store
                .publish_enrichment(
                    &view.name,
                    "example/repo",
                    &input,
                    EnrichmentOwner {
                        analyzer,
                        version: "2",
                    },
                    EnrichmentPayload::default(),
                )
                .unwrap();
        }
        assert_eq!(current_revision(&store, "owner-benchmark"), 3);
        let cleanup_started = Instant::now();
        assert_eq!(store.garbage_collect().unwrap().repository_states_queued, 0);
        store.sweep_garbage_collection(|_| true).unwrap();
        let cleanup_elapsed = cleanup_started.elapsed();
        store.checkpoint().unwrap();
        let final_database_bytes = fs::metadata(&database).map_or(0, |metadata| metadata.len());
        let final_wal_bytes = fs::metadata(&wal).map_or(0, |metadata| metadata.len());
        eprintln!(
            "analyzers={ANALYZERS} revision=3 publish_ms={} cleanup_ms={} \
             baseline_database_bytes={baseline_database_bytes} \
             contribution_database_growth_bytes={} contribution_wal_bytes={contribution_wal_bytes} \
             final_database_growth_bytes={} final_wal_bytes={final_wal_bytes}",
            publish_elapsed.as_millis(),
            cleanup_elapsed.as_millis(),
            contribution_database_bytes.saturating_sub(baseline_database_bytes),
            final_database_bytes.saturating_sub(baseline_database_bytes),
        );
        assert_eq!(final_wal_bytes, 0);
        assert!(!store.garbage_collection_pending().unwrap());
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
