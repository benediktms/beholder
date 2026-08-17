use super::schema::*;
use beholder_domain::{
    DependencyOverride, EntityFact, EntityKind, EntityMetadata, FactChanges, Observation,
    ProtoTypeKind, RepositoryFacts, RpcCardinality, WorkspaceView,
};
use mnestic_engine::{DataValue, DbInstance, MultiTransaction, ScriptMutability};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error};

const OBSERVATION_BATCH_SIZE: usize = 10_000;

pub(super) fn store_observations(
    transaction: &MultiTransaction,
    state: &str,
    observations: &[Observation],
) -> Result<(), Box<dyn Error>> {
    for observations in observations.chunks(OBSERVATION_BATCH_SIZE) {
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
        EntityKind::GraphqlField => "graphql_field",
        EntityKind::KafkaTopic => "kafka_topic",
        EntityKind::Namespace => "namespace",
        EntityKind::ProtoField => "proto_field",
        EntityKind::ProtoMethod => "proto_method",
        EntityKind::ProtoService => "proto_service",
        EntityKind::ProtoType => "proto_type",
        EntityKind::Service => "service",
    }
}

fn entity_metadata(metadata: Option<EntityMetadata>) -> &'static str {
    match metadata {
        None => "",
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
    if !entities.is_empty() {
        transaction.run_script(
            "?[state, id, kind, metadata] <- $rows\n\
             :put state_entity {state, id => kind, metadata}",
            BTreeMap::from([("rows".into(), DataValue::List(rows))]),
        )?;
    }
    Ok(())
}

pub(super) fn hash_string(hash: &mut Sha256, value: &str) {
    hash.update(value.len().to_le_bytes());
    hash.update(value.as_bytes());
}

type NormalizedObservations = BTreeMap<(String, String, String), (String, u64, String)>;
type NormalizedEntities = BTreeMap<String, (String, String)>;

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
    if !state.ends_with(fingerprint)
        && !state
            .strip_prefix(fingerprint)
            .is_some_and(|suffix| suffix.starts_with(':'))
    {
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
    Ok(
        (stored == normalized_observations(facts) && stored_entities == normalized_entities(facts))
            .then_some(state),
    )
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

pub(super) fn publish_observations(
    db: &DbInstance,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
    overrides: &[DependencyOverride],
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
    store_repository_states(&transaction, view, repositories, &analyzed_states)?;
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
    transaction.commit()?;
    Ok(changes)
}

pub(super) fn store_repository_states(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
    analyzed_states: &[String],
) -> Result<(), Box<dyn Error>> {
    for (facts, analyzed_state) in repositories.iter().zip(analyzed_states) {
        let state = &facts.state;
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            (
                "repository".into(),
                state.repository.identity.clone().into(),
            ),
            ("head".into(), state.head.clone().unwrap_or_default().into()),
            ("state".into(), analyzed_state.as_str().into()),
        ]);
        transaction.run_script(
            "?[fingerprint, repository, head] <- [[$state, $repository, $head]]\n\
             :put repository_state {fingerprint => repository, head}",
            params.clone(),
        )?;
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

pub(super) fn garbage_collect(db: &DbInstance) -> Result<u64, Box<dyn Error>> {
    let stale = db.run_script(
        "live_state[state] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}\n\
         ?[state] := *repository_state{fingerprint: state}, not live_state[state]",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?;

    db.run_script(
        "{ \
             live_state[state] := \
                 *analysis_revision{view, revision}, \
                 *analysis_revision_state{view, revision, state} \
             stale_state[state] := \
                 *repository_state{fingerprint: state}, not live_state[state] \
             ?[state, from, relation, to] := \
                 *state_observation{state, from, relation, to}, stale_state[state] \
                 :rm state_observation {state, from, relation, to} \
             } \
             { \
                 live_state[state] := \
                     *analysis_revision{view, revision}, \
                     *analysis_revision_state{view, revision, state} \
                 stale_state[state] := \
                     *repository_state{fingerprint: state}, not live_state[state] \
                 ?[state, id] := *state_entity{state, id}, stale_state[state] \
                 :rm state_entity {state, id} \
             } \
             { \
                 live_state[state] := \
                     *analysis_revision{view, revision}, \
                     *analysis_revision_state{view, revision, state} \
                 stale_state[state] := \
                     *repository_state{fingerprint: state}, not live_state[state] \
                 ?[state, from, relation, to] := \
                     *state_dependency_observation{state, from, relation, to}, stale_state[state] \
                 :rm state_dependency_observation {state, from, relation, to} \
             } \
             { \
                 live_state[state] := \
                     *analysis_revision{view, revision}, \
                     *analysis_revision_state{view, revision, state} \
                 stale_state[state] := \
                     *repository_state{fingerprint: state}, not live_state[state] \
                 ?[state, from, relation, to] := \
                     *state_observation_metadata{state, from, relation, to}, stale_state[state] \
                 :rm state_observation_metadata {state, from, relation, to} \
             } \
             { \
                 live_state[state] := \
                     *analysis_revision{view, revision}, \
                     *analysis_revision_state{view, revision, state} \
                 ?[fingerprint] := *repository_state{fingerprint}, not live_state[fingerprint] \
                 :rm repository_state {fingerprint} \
             }",
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;

    for (relation, keys) in [
        ("analysis_revision_state", "view, revision, repository"),
        (
            "analysis_revision_dependency_override",
            "view, revision, from, relation, unresolved_to",
        ),
        (
            "analysis_revision_dependency_override_metadata",
            "view, revision, from, relation, unresolved_to",
        ),
    ] {
        db.run_script(
            &format!(
                "?[{keys}] := \
                     *{relation}{{{keys}}}, \
                     *analysis_revision{{view, revision: current}}, \
                     revision != current \
                 :rm {relation} {{{keys}}}"
            ),
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }

    let relations = db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?;
    for legacy in ["observation", "dependency_observation"] {
        if relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some(legacy))
        {
            db.run_script(
                &format!("::remove {legacy}"),
                BTreeMap::new(),
                ScriptMutability::Mutable,
            )?;
        }
    }

    Ok(stale.rows.len().try_into()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SemanticStore;
    use beholder_domain::{
        Confidence, DependencyOverride, DependencyRelation, EntityFact, EntityKind, EntityMetadata,
        FactChanges, LogicalRepository, Observation, ProtoTypeKind, Provenance, RepositoryFacts,
        RepositoryState, StructuralRelation, WorkspaceView,
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
            entities: Vec::new(),
            observations,
        }
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
                        entities: Vec::new(),
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
                    entities: Vec::new(),
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
                        entities: Vec::new(),
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
                        entities: Vec::new(),
                        observations: vec![unresolved.clone()],
                    },
                    RepositoryFacts {
                        state: target,
                        analysis_identity: "analysis".into(),
                        entities: Vec::new(),
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
}
