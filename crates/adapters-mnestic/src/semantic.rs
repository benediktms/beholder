use crate::{InspectionResult, InspectionValue};
use beholder_dto::{
    CONTEXT_SCHEMA_V1, ContextResult, DEPENDENCIES_SCHEMA_V2, DependenciesResult, DependencyRef,
    EntityKind, EntityMetadata, EntityOrigin, EntityQuery, EntityRef, EvidenceKind, EvidenceRef,
    IMPACT_SCHEMA_V2, ImpactRef, ImpactResult, PathQuery, ProtoTypeKind, QueryMetadata,
    RelationKind, RpcCardinality, SemanticEdge, SemanticPath, TRACE_SCHEMA_V2, TraceResult,
    TraversalMetadata,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;

type EdgeKey = (String, String, RelationKind);
type EntityFactMap = BTreeMap<String, (EntityKind, Option<EntityMetadata>)>;
type Closure = (Vec<(String, u32)>, GraphOutput, bool);
#[derive(Clone, Copy)]
enum TraversalDirection {
    Outgoing,
    Incoming,
}

pub(super) fn context(
    view: &str,
    entity: &str,
    result: InspectionResult,
    entities: InspectionResult,
) -> Result<ContextResult, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
    graph.hint_facts(entity_kinds(entities)?);
    graph.hint(entity, infer_kind(entity));
    for row in result.rows {
        let direction = text(&row, 0, "context direction")?;
        let relation = text(&row, 1, "context relation")?;
        let related = text(&row, 2, "context related entity")?;
        let evidence = text(&row, 3, "context evidence")?;
        let confidence = float(&row, 4, "context confidence")? as f32;
        let provenance = text(&row, 5, "context provenance")?;
        let _ = match direction {
            "outgoing" => {
                graph.add_edge(entity, related, relation, evidence, confidence, provenance)?
            }
            "incoming" => {
                graph.add_edge(related, entity, relation, evidence, confidence, provenance)?
            }
            _ => return Err(format!("unknown context direction: {direction}").into()),
        };
    }
    let output = graph.finish();
    Ok(ContextResult {
        schema: CONTEXT_SCHEMA_V1.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntityQuery {
            entity: entity.into(),
        },
        root: output.entity(entity),
        nodes: output.nodes,
        edges: output.edges,
    })
}

pub(super) fn dependencies(
    view: &str,
    entity: &str,
    max_hops: u32,
    result: InspectionResult,
    entities: InspectionResult,
) -> Result<DependenciesResult, Box<dyn Error>> {
    let (entries, output, truncated) = closure(
        result,
        entities,
        entity,
        max_hops,
        TraversalDirection::Outgoing,
    )?;
    Ok(DependenciesResult {
        schema: DEPENDENCIES_SCHEMA_V2.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntityQuery {
            entity: entity.into(),
        },
        traversal: TraversalMetadata {
            max_hops,
            truncated,
        },
        root: output.entity(entity),
        dependencies: entries
            .into_iter()
            .map(|(entity, hops)| DependencyRef { entity, hops })
            .collect(),
        nodes: output.nodes,
        edges: output.edges,
    })
}

pub(super) fn impact(
    view: &str,
    entity: &str,
    max_hops: u32,
    result: InspectionResult,
    entities: InspectionResult,
) -> Result<ImpactResult, Box<dyn Error>> {
    let (entries, output, truncated) = closure(
        result,
        entities,
        entity,
        max_hops,
        TraversalDirection::Incoming,
    )?;
    Ok(ImpactResult {
        schema: IMPACT_SCHEMA_V2.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntityQuery {
            entity: entity.into(),
        },
        traversal: TraversalMetadata {
            max_hops,
            truncated,
        },
        root: output.entity(entity),
        affected: entries
            .into_iter()
            .map(|(entity, hops)| ImpactRef { entity, hops })
            .collect(),
        nodes: output.nodes,
        edges: output.edges,
    })
}

fn closure(
    result: InspectionResult,
    entities: InspectionResult,
    root: &str,
    max_hops: u32,
    direction: TraversalDirection,
) -> Result<Closure, Box<dyn Error>> {
    let mut output = graph(result, entities, &[root])?;
    let (distances, truncated) = distances(root, &output.edges, max_hops, direction);
    output.nodes.retain(|node| distances.contains_key(&node.id));
    output
        .edges
        .retain(|edge| distances.contains_key(&edge.from) && distances.contains_key(&edge.to));
    let mut entries = distances
        .into_iter()
        .filter(|(entity, _)| entity != root)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    Ok((entries, output, truncated))
}

fn graph(
    result: InspectionResult,
    entities: InspectionResult,
    roots: &[&str],
) -> Result<GraphOutput, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
    graph.hint_facts(entity_kinds(entities)?);
    for root in roots {
        graph.hint(root, infer_kind(root));
    }
    for row in result.rows {
        match text(&row, 0, "closure row kind")? {
            "edge" => {
                graph.add_edge(
                    text(&row, 3, "closure edge source")?,
                    text(&row, 4, "closure edge target")?,
                    text(&row, 5, "closure relation")?,
                    text(&row, 6, "closure evidence")?,
                    float(&row, 7, "closure confidence")? as f32,
                    text(&row, 8, "closure provenance")?,
                )?;
            }
            kind => return Err(format!("unknown closure row kind: {kind}").into()),
        }
    }
    Ok(graph.finish())
}

fn distances(
    root: &str,
    edges: &[SemanticEdge],
    max_hops: u32,
    direction: TraversalDirection,
) -> (BTreeMap<String, u32>, bool) {
    let mut adjacent = BTreeMap::<&str, Vec<&str>>::new();
    for edge in edges {
        let (from, to) = match direction {
            TraversalDirection::Outgoing => (edge.from.as_str(), edge.to.as_str()),
            TraversalDirection::Incoming
                if edge.kind == RelationKind::ImplementedBy && edge.from.starts_with("grpc://") =>
            {
                (edge.from.as_str(), edge.to.as_str())
            }
            TraversalDirection::Incoming => (edge.to.as_str(), edge.from.as_str()),
        };
        adjacent.entry(from).or_default().push(to);
    }

    let mut distances = BTreeMap::from([(root.to_owned(), 0)]);
    let mut queue = VecDeque::from([root.to_owned()]);
    let mut truncated = false;
    while let Some(from) = queue.pop_front() {
        let hops = distances[&from];
        if hops == max_hops {
            truncated |= adjacent
                .get(from.as_str())
                .is_some_and(|nodes| nodes.iter().any(|node| !distances.contains_key(*node)));
            continue;
        }
        for &to in adjacent.get(from.as_str()).into_iter().flatten() {
            if !distances.contains_key(to) {
                distances.insert(to.to_owned(), hops + 1);
                queue.push_back(to.to_owned());
            }
        }
    }
    (distances, truncated)
}

pub(super) fn trace(
    view: &str,
    from: &str,
    to: &str,
    max_hops: u32,
    result: InspectionResult,
    entities: InspectionResult,
) -> Result<TraceResult, Box<dyn Error>> {
    let mut output = graph(result, entities, &[from, to])?;
    let (path, truncated) = shortest_path(from, to, max_hops, &output.edges);
    let paths = path.into_iter().collect::<Vec<_>>();
    let path_nodes = paths
        .first()
        .map(|path| path.nodes.iter().cloned().collect())
        .unwrap_or_else(|| BTreeSet::from([from.into(), to.into()]));
    let path_edges = paths
        .first()
        .map(|path| path.edges.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    output.nodes.retain(|node| path_nodes.contains(&node.id));
    output.edges.retain(|edge| path_edges.contains(&edge.id));
    Ok(TraceResult {
        schema: TRACE_SCHEMA_V2.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: PathQuery {
            from: from.into(),
            to: to.into(),
        },
        traversal: TraversalMetadata {
            max_hops,
            truncated,
        },
        nodes: output.nodes,
        edges: output.edges,
        paths,
    })
}

fn shortest_path(
    from: &str,
    to: &str,
    max_hops: u32,
    edges: &[SemanticEdge],
) -> (Option<SemanticPath>, bool) {
    if from == to {
        return (
            Some(SemanticPath {
                nodes: vec![from.into()],
                edges: Vec::new(),
            }),
            false,
        );
    }
    let mut adjacent = BTreeMap::<&str, Vec<&SemanticEdge>>::new();
    for edge in edges {
        adjacent.entry(&edge.from).or_default().push(edge);
    }
    let mut parents = BTreeMap::<String, (String, String)>::new();
    let mut distances = BTreeMap::from([(from.to_owned(), 0)]);
    let mut queue = VecDeque::from([from.to_owned()]);
    let mut truncated = false;
    while let Some(node) = queue.pop_front() {
        let hops = distances[&node];
        if hops == max_hops {
            truncated |= adjacent
                .get(node.as_str())
                .is_some_and(|edges| edges.iter().any(|edge| !distances.contains_key(&edge.to)));
            continue;
        }
        for edge in adjacent.get(node.as_str()).into_iter().flatten() {
            if distances.contains_key(&edge.to) {
                continue;
            }
            distances.insert(edge.to.clone(), hops + 1);
            parents.insert(edge.to.clone(), (node.clone(), edge.id.clone()));
            if edge.to == to {
                let mut nodes = vec![to.to_owned()];
                let mut path_edges = Vec::new();
                let mut cursor = to;
                while cursor != from {
                    let (parent, edge) = &parents[cursor];
                    nodes.push(parent.clone());
                    path_edges.push(edge.clone());
                    cursor = parent;
                }
                nodes.reverse();
                path_edges.reverse();
                return (
                    Some(SemanticPath {
                        nodes,
                        edges: path_edges,
                    }),
                    false,
                );
            }
            queue.push_back(edge.to.clone());
        }
    }
    (None, truncated)
}

#[derive(Default)]
struct GraphBuilder {
    edges: BTreeMap<EdgeKey, EdgeData>,
    facts: EntityFactMap,
    kinds: BTreeMap<String, EntityKind>,
    metadata: BTreeMap<String, EntityMetadata>,
    origins: BTreeMap<String, EntityOrigin>,
}

struct EdgeData {
    confidence: f32,
    evidence: BTreeSet<EvidenceRef>,
}

impl GraphBuilder {
    fn hint_facts(&mut self, facts: EntityFactMap) {
        self.facts = facts;
    }

    fn hint(&mut self, id: &str, kind: EntityKind) {
        if let Some((kind, metadata)) = self.facts.get(id).copied() {
            self.kinds.insert(id.into(), kind);
            if let Some(metadata) = metadata {
                self.metadata.insert(id.into(), metadata);
            }
            return;
        }
        let current = self.kinds.entry(id.into()).or_insert(EntityKind::Unknown);
        if kind_priority(kind) > kind_priority(*current) {
            *current = kind;
        }
    }

    fn add_edge(
        &mut self,
        from: &str,
        to: &str,
        relation: &str,
        evidence: &str,
        confidence: f32,
        provenance: &str,
    ) -> Result<EdgeKey, Box<dyn Error>> {
        let relation = RelationKind::try_from(relation)?;
        let key = (from.into(), to.into(), relation);
        self.hint(from, relation_kind_hint(relation.as_str(), true, from));
        self.hint(to, relation_kind_hint(relation.as_str(), false, to));
        if provenance == "generated" {
            match relation {
                RelationKind::Defines => {
                    self.origins.insert(to.into(), EntityOrigin::Generated);
                }
                RelationKind::FieldOf => {
                    self.origins.insert(from.into(), EntityOrigin::Generated);
                }
                _ => {}
            }
        }
        let evidence = evidence_ref(from, to, evidence, provenance);
        self.edges
            .entry(key.clone())
            .and_modify(|edge| {
                edge.confidence = edge.confidence.max(confidence);
                edge.evidence.insert(evidence.clone());
            })
            .or_insert_with(|| EdgeData {
                confidence,
                evidence: BTreeSet::from([evidence]),
            });
        Ok(key)
    }

    fn finish(self) -> GraphOutput {
        let mut ids = self.kinds.keys().cloned().collect::<BTreeSet<_>>();
        for (from, to, _) in self.edges.keys() {
            ids.insert(from.clone());
            ids.insert(to.clone());
        }
        let nodes = ids
            .into_iter()
            .map(|id| {
                entity_ref_with_origin(
                    &id,
                    self.kinds
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| infer_kind(&id)),
                    self.origins.get(&id).copied(),
                    self.metadata.get(&id).copied(),
                )
            })
            .collect::<Vec<_>>();
        let edges = self
            .edges
            .into_iter()
            .enumerate()
            .map(|(index, (key, edge))| {
                let id = format!("e{}", index + 1);
                SemanticEdge {
                    id,
                    from: key.0,
                    to: key.1,
                    kind: key.2,
                    confidence: edge.confidence,
                    evidence: edge.evidence.into_iter().collect(),
                }
            })
            .collect();
        GraphOutput { nodes, edges }
    }
}

fn entity_kinds(result: InspectionResult) -> Result<EntityFactMap, Box<dyn Error>> {
    result
        .rows
        .iter()
        .map(|row| {
            let id = text(row, 0, "entity id")?.to_owned();
            let entity = match (
                text(row, 1, "entity kind")?,
                text(row, 2, "entity metadata")?,
            ) {
                ("callable", "") => (EntityKind::Callable, None),
                ("graphql_field", "") => (EntityKind::GraphqlField, None),
                ("grpc_operation", "") => (EntityKind::Rpc, None),
                ("kafka_topic", "") => (EntityKind::KafkaTopic, None),
                ("namespace", "") => (EntityKind::Namespace, None),
                ("proto_field", "") => (EntityKind::ProtoField, None),
                ("proto_method", "rpc_cardinality:bidirectional_streaming") => (
                    EntityKind::Rpc,
                    Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::BidirectionalStreaming,
                    }),
                ),
                ("proto_method", "rpc_cardinality:client_streaming") => (
                    EntityKind::Rpc,
                    Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::ClientStreaming,
                    }),
                ),
                ("proto_method", "rpc_cardinality:server_streaming") => (
                    EntityKind::Rpc,
                    Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::ServerStreaming,
                    }),
                ),
                ("proto_method", "rpc_cardinality:unary") => (
                    EntityKind::Rpc,
                    Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::Unary,
                    }),
                ),
                ("proto_service", "") => (EntityKind::ProtoService, None),
                ("proto_type", "proto_type:enum") => (
                    EntityKind::ProtoEnum,
                    Some(EntityMetadata::ProtoType {
                        type_kind: ProtoTypeKind::Enum,
                    }),
                ),
                ("proto_type", "proto_type:message") => (
                    EntityKind::ProtoMessage,
                    Some(EntityMetadata::ProtoType {
                        type_kind: ProtoTypeKind::Message,
                    }),
                ),
                ("service", "") => (EntityKind::Service, None),
                ("unity_prefab", "") => (EntityKind::UnityPrefab, None),
                (kind, metadata) => {
                    return Err(
                        format!("invalid persisted entity fact {kind} with {metadata}").into(),
                    );
                }
            };
            Ok((id, entity))
        })
        .collect()
}

fn kind_priority(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Unknown => 0,
        EntityKind::Namespace => 1,
        EntityKind::Callable => 2,
        EntityKind::GraphqlField
        | EntityKind::KafkaTopic
        | EntityKind::Rpc
        | EntityKind::Service
        | EntityKind::ProtoEnum
        | EntityKind::ProtoField
        | EntityKind::ProtoFile
        | EntityKind::ProtoMessage
        | EntityKind::ProtoService
        | EntityKind::UnityPrefab => 3,
    }
}

struct GraphOutput {
    nodes: Vec<EntityRef>,
    edges: Vec<SemanticEdge>,
}

impl GraphOutput {
    fn entity(&self, id: &str) -> EntityRef {
        self.nodes
            .iter()
            .find(|entity| entity.id == id)
            .cloned()
            .unwrap_or_else(|| entity_ref(id, infer_kind(id)))
    }
}

fn entity_ref(id: &str, kind: EntityKind) -> EntityRef {
    entity_ref_with_origin(id, kind, None, None)
}

fn entity_ref_with_origin(
    id: &str,
    kind: EntityKind,
    origin: Option<EntityOrigin>,
    metadata: Option<EntityMetadata>,
) -> EntityRef {
    EntityRef {
        id: id.into(),
        kind,
        name: entity_name(id),
        repository: repository(id),
        origin: origin.unwrap_or_else(|| {
            if id.starts_with("rust-call://")
                || id.starts_with("rust-method://")
                || id.starts_with("elixir-call://")
                || id.starts_with("elixir-module://")
                || id.starts_with("erlang-module://")
                || id.starts_with("unity://")
            {
                EntityOrigin::ExternalDependency
            } else {
                EntityOrigin::Source
            }
        }),
        test: is_test_entity(id),
        metadata,
    }
}

fn is_test_entity(id: &str) -> bool {
    let test_segment = id.split('/').any(|part| {
        let lower = part.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "test" | "tests" | "spec" | "specs" | "bench" | "benches"
        ) || [".test", ".spec", "_test", "_spec"]
            .iter()
            .any(|marker| lower.ends_with(marker))
            || [".test.", ".spec.", "_test.", "_spec."]
                .iter()
                .any(|marker| lower.contains(marker))
            || part.ends_with("Test")
    });
    let name = id.rsplit('/').next().unwrap_or(id);
    test_segment
        || ["test_", "spec_", "bench_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || ["_test", "_spec"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn infer_kind(id: &str) -> EntityKind {
    if id.starts_with("grpc://")
        || id.starts_with("proto-method://")
        || id.starts_with("rpc/")
        || id.starts_with("rpc://")
    {
        EntityKind::Rpc
    } else if id.starts_with("graphql-field://") {
        EntityKind::GraphqlField
    } else if id.starts_with("kafka-topic://") {
        EntityKind::KafkaTopic
    } else if id.contains("/unity-prefab/") {
        EntityKind::UnityPrefab
    } else if id.starts_with("rust-call://")
        || id.starts_with("rust-method://")
        || id.starts_with("elixir-call://")
    {
        EntityKind::Callable
    } else if id.starts_with("elixir-module://") || id.starts_with("erlang-module://") {
        EntityKind::Namespace
    } else if id.starts_with("proto-field://") {
        EntityKind::ProtoField
    } else if id.starts_with("proto-service://") {
        EntityKind::ProtoService
    } else if id.contains("/elixir-source/") {
        EntityKind::Namespace
    } else if let Some(symbol) = id.rsplit_once("/elixir/").map(|(_, symbol)| symbol) {
        if symbol
            .rsplit_once('/')
            .is_some_and(|(_, arity)| arity.parse::<usize>().is_ok())
        {
            EntityKind::Callable
        } else {
            EntityKind::Namespace
        }
    } else {
        EntityKind::Unknown
    }
}

fn relation_kind_hint(relation: &str, source: bool, id: &str) -> EntityKind {
    let inferred = infer_kind(id);
    if inferred != EntityKind::Unknown {
        return inferred;
    }
    match (relation, source) {
        ("defines", true) => EntityKind::Namespace,
        ("defines", false) | ("calls", _) | ("implemented_by", false) => EntityKind::Callable,
        ("calls_rpc", false) | ("implemented_by", true) => EntityKind::Rpc,
        ("selects", false) | ("resolved_by", true) => EntityKind::GraphqlField,
        _ => EntityKind::Unknown,
    }
}

fn entity_name(id: &str) -> String {
    if let Some(module) = id.strip_prefix("elixir-module://") {
        return module.into();
    }
    if let Some(symbol) = id.strip_prefix("elixir-call://")
        && let Some((function, arity)) = symbol.rsplit_once('/')
        && arity.parse::<usize>().is_ok()
    {
        return if let Some((module, function)) = function.rsplit_once('/') {
            format!("{module}.{function}/{arity}")
        } else {
            format!("{function}/{arity}")
        };
    }
    if let Some((service, method)) = id
        .strip_prefix("proto-method://")
        .or_else(|| id.strip_prefix("grpc://"))
        .and_then(|id| id.split_once('/'))
    {
        return format!(
            "{}.{}",
            service.rsplit('.').next().unwrap_or(service),
            method
        );
    }
    if let Some(symbol) = id.rsplit_once("/elixir/").map(|(_, symbol)| symbol)
        && let Some((function, arity)) = symbol.rsplit_once('/')
        && arity.parse::<usize>().is_ok()
    {
        return format!(
            "{}/{}",
            function.rsplit('/').next().unwrap_or(function),
            arity
        );
    }
    id.rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(id)
        .to_owned()
}

fn repository(id: &str) -> Option<String> {
    id.strip_prefix("repo://").and_then(|rest| {
        rest.rsplit_once("/elixir-source/")
            .or_else(|| rest.rsplit_once("/typescript-source/"))
            .or_else(|| rest.rsplit_once("/javascript-source/"))
            .or_else(|| rest.rsplit_once("/csharp-source/"))
            .or_else(|| rest.rsplit_once("/unity-prefab/"))
            .or_else(|| rest.rsplit_once("/rust/"))
            .or_else(|| rest.rsplit_once("/elixir/"))
            .or_else(|| rest.rsplit_once("/typescript/"))
            .or_else(|| rest.rsplit_once("/javascript/"))
            .or_else(|| rest.rsplit_once("/csharp/"))
            .map(|(repository, _)| repository.into())
    })
}

fn evidence_ref(from: &str, to: &str, evidence: &str, provenance: &str) -> EvidenceRef {
    let (mut path, line) = evidence
        .rsplit_once(':')
        .and_then(|(path, line)| {
            line.parse()
                .ok()
                .map(|line| (Some(path.into()), Some(line)))
        })
        .unwrap_or((None, None));
    if provenance == "descriptor" && path.is_none() {
        path = Some(evidence.into());
    }
    let has_path = path.is_some();
    EvidenceRef {
        source_kind: match provenance {
            "ast" => EvidenceKind::Ast,
            "unique_name_heuristic" => EvidenceKind::Inference,
            "descriptor" => EvidenceKind::Descriptor,
            "generated" => EvidenceKind::Generated,
            _ => EvidenceKind::Unknown,
        },
        repository: repository(from).or_else(|| repository(to)),
        path,
        line,
        detail: match provenance {
            "unique_name_heuristic" => Some(provenance.into()),
            _ => (!has_path).then(|| evidence.into()),
        },
    }
}

fn text<'a>(
    row: &'a [InspectionValue],
    index: usize,
    name: &str,
) -> Result<&'a str, Box<dyn Error>> {
    row.get(index)
        .and_then(InspectionValue::as_str)
        .ok_or_else(|| format!("{name} must be text").into())
}

fn float(row: &[InspectionValue], index: usize, name: &str) -> Result<f64, Box<dyn Error>> {
    match row.get(index) {
        Some(InspectionValue::Float(value)) => Ok(*value),
        Some(InspectionValue::Integer(value)) => Ok(*value as f64),
        _ => Err(format!("{name} must be numeric").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphBuilder, entity_ref, infer_kind, is_test_entity};
    use beholder_dto::{EntityKind, EntityOrigin};
    use std::collections::BTreeMap;

    #[test]
    fn typed_entity_facts_override_relation_hints() {
        let mut graph = GraphBuilder::default();
        graph.hint_facts(BTreeMap::from([
            (
                "repo://example/rust/lib".into(),
                (EntityKind::Namespace, None),
            ),
            (
                "repo://example/rust/unrelated".into(),
                (EntityKind::Callable, None),
            ),
        ]));
        graph.hint("repo://example/rust/lib", EntityKind::Callable);

        let graph = graph.finish();
        assert_eq!(
            graph.entity("repo://example/rust/lib").kind,
            EntityKind::Namespace
        );
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn treats_unity_callbacks_as_external_dependencies() {
        assert_eq!(
            entity_ref(
                "unity://UnityEngine.MonoBehaviour/Update()",
                EntityKind::Callable
            )
            .origin,
            EntityOrigin::ExternalDependency
        );
    }

    #[test]
    fn keeps_strongest_confidence_for_duplicate_edges() {
        let mut graph = GraphBuilder::default();
        graph
            .add_edge("a", "b", "calls", "a.rs:1", 0.6, "unique_name_heuristic")
            .unwrap();
        graph
            .add_edge("a", "b", "calls", "a.rs:2", 1.0, "ast")
            .unwrap();

        let graph = graph.finish();
        assert_eq!(graph.edges[0].confidence, 1.0);
        assert_eq!(graph.edges[0].evidence.len(), 2);
    }

    #[test]
    fn attributes_evidence_to_a_repository_owned_target() {
        let mut graph = GraphBuilder::default();
        graph
            .add_edge(
                "grpc://example.Service/Call",
                "repo://example/server/elixir/Example.Server/call/2",
                "implemented_by",
                "lib/server.ex:4",
                1.0,
                "generated",
            )
            .unwrap();

        let graph = graph.finish();
        assert_eq!(
            graph.edges[0].evidence[0].repository.as_deref(),
            Some("example/server")
        );
    }

    #[test]
    fn maps_elixir_modules_and_functions() {
        let module_id = "repo://github.com/example/elixir/elixir/Example.Items";
        let module = entity_ref(module_id, infer_kind(module_id));
        assert_eq!(module.kind, EntityKind::Namespace);
        assert_eq!(module.name, "Example.Items");
        assert_eq!(
            module.repository.as_deref(),
            Some("github.com/example/elixir")
        );

        let function_id = format!("{module_id}/activate/1");
        let function = entity_ref(&function_id, infer_kind(&function_id));
        assert_eq!(function.kind, EntityKind::Callable);
        assert_eq!(function.name, "activate/1");
        assert_eq!(
            function.repository.as_deref(),
            Some("github.com/example/elixir")
        );

        let source = entity_ref(
            "repo://github.com/example/elixir/elixir-source/lib/elixir/lib/example.ex",
            EntityKind::Namespace,
        );
        assert_eq!(
            source.repository.as_deref(),
            Some("github.com/example/elixir")
        );
    }

    #[test]
    fn maps_typescript_repositories() {
        let entity = entity_ref(
            "repo://github.com/example/app/typescript/src/client/run",
            EntityKind::Callable,
        );
        assert_eq!(entity.repository.as_deref(), Some("github.com/example/app"));
    }

    #[test]
    fn maps_generated_definitions_and_fields_to_generated_entities() {
        let mut graph = GraphBuilder::default();
        graph
            .add_edge(
                "repo://example/elixir-source/example.pb.ex",
                "repo://example/elixir/Example.Message",
                "defines",
                "example.pb.ex:1",
                1.0,
                "generated",
            )
            .unwrap();
        graph
            .add_edge(
                "repo://example/elixir/Example.Message/field/id",
                "repo://example/elixir/Example.Message",
                "field_of",
                "example.pb.ex:2",
                1.0,
                "generated",
            )
            .unwrap();

        let graph = graph.finish();
        for id in [
            "repo://example/elixir/Example.Message",
            "repo://example/elixir/Example.Message/field/id",
        ] {
            assert_eq!(
                graph
                    .nodes
                    .iter()
                    .find(|node| node.id == id)
                    .unwrap()
                    .origin,
                EntityOrigin::Generated
            );
        }
    }

    #[test]
    fn recognises_rust_and_javascript_test_segments() {
        assert!(is_test_entity("repo://app/rust/src/tests/checkout"));
        assert!(is_test_entity(
            "repo://app/typescript/src/checkout.spec/test"
        ));
        assert!(is_test_entity("repo://app/javascript/specs/checkout"));
        assert!(is_test_entity("repo://app/rust/src/config/test_load"));
        assert!(is_test_entity("repo://app/typescript/src/checkout_spec"));
        assert!(is_test_entity(
            "repo://app/elixir/test/checkout_test.exs/can_pay"
        ));
        assert!(is_test_entity(
            "repo://app/elixir/MyApp.CheckoutTest/can_pay/1"
        ));
        assert!(is_test_entity(
            "repo://app/typescript/src/checkout.test.ts/canPay"
        ));
        assert!(is_test_entity(
            "repo://app/go/checkout_test.go/TestCheckout"
        ));
        assert!(!is_test_entity("repo://app/rust/src/checkout"));
    }
}
