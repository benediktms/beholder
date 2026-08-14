use crate::{InspectionResult, InspectionValue};
use beholder_dto::{
    CONTEXT_SCHEMA_V1, ContextResult, DEPENDENCIES_SCHEMA_V1, DependenciesResult, DependencyRef,
    EntityKind, EntityOrigin, EntityQuery, EntityRef, EvidenceKind, EvidenceRef, IMPACT_SCHEMA_V1,
    ImpactRef, ImpactResult, PathQuery, QueryMetadata, RelationKind, SemanticEdge, SemanticPath,
    TRACE_SCHEMA_V1, TraceResult,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

type EdgeKey = (String, String, RelationKind);
type Closure = (Vec<(String, u32)>, GraphOutput);

pub(super) fn context(
    view: &str,
    entity: &str,
    result: InspectionResult,
) -> Result<ContextResult, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
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
    result: InspectionResult,
) -> Result<DependenciesResult, Box<dyn Error>> {
    let (entries, output) = closure(result, entity)?;
    Ok(DependenciesResult {
        schema: DEPENDENCIES_SCHEMA_V1.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntityQuery {
            entity: entity.into(),
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
    result: InspectionResult,
) -> Result<ImpactResult, Box<dyn Error>> {
    let (entries, output) = closure(result, entity)?;
    Ok(ImpactResult {
        schema: IMPACT_SCHEMA_V1.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntityQuery {
            entity: entity.into(),
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

fn closure(result: InspectionResult, root: &str) -> Result<Closure, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
    graph.hint(root, infer_kind(root));
    let mut entries: Vec<(String, u32)> = Vec::new();
    for row in result.rows {
        match text(&row, 0, "closure row kind")? {
            "entity" => entries.push((
                text(&row, 1, "closure entity")?.into(),
                integer(&row, 2, "closure hops")?.try_into()?,
            )),
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
    entries.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    Ok((entries, graph.finish()))
}

pub(super) fn trace(
    view: &str,
    from: &str,
    to: &str,
    result: InspectionResult,
) -> Result<TraceResult, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
    graph.hint(from, infer_kind(from));
    graph.hint(to, infer_kind(to));
    let mut paths = Vec::new();
    for row in result.rows {
        let nodes = list(&row, 0, "trace nodes")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or("trace node must be text")
                    .map(str::to_owned)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let steps = list(&row, 1, "trace steps")?;
        if nodes.len() != steps.len() + 1 {
            return Err("trace path node and edge counts do not align".into());
        }
        let mut keys = Vec::with_capacity(steps.len());
        for step in steps {
            let InspectionValue::List(values) = step else {
                return Err("trace step must be a list".into());
            };
            let index: usize = values
                .first()
                .and_then(InspectionValue::as_i64)
                .ok_or("trace step index must be an integer")?
                .try_into()?;
            let relation = values
                .get(1)
                .and_then(InspectionValue::as_str)
                .ok_or("trace step relation must be text")?;
            let evidence = values
                .get(2)
                .and_then(InspectionValue::as_str)
                .ok_or("trace step evidence must be text")?;
            let confidence = values
                .get(3)
                .and_then(|value| match value {
                    InspectionValue::Float(value) => Some(*value as f32),
                    InspectionValue::Integer(value) => Some(*value as f32),
                    _ => None,
                })
                .ok_or("trace step confidence must be numeric")?;
            let provenance = values
                .get(4)
                .and_then(InspectionValue::as_str)
                .ok_or("trace step provenance must be text")?;
            let source = nodes
                .get(index)
                .ok_or("trace step source is out of bounds")?;
            let target = nodes
                .get(index + 1)
                .ok_or("trace step target is out of bounds")?;
            keys.push(graph.add_edge(source, target, relation, evidence, confidence, provenance)?);
        }
        paths.push((nodes, keys));
    }
    let output = graph.finish();
    let paths = paths
        .into_iter()
        .map(|(nodes, keys)| SemanticPath {
            nodes,
            edges: keys
                .into_iter()
                .map(|key| output.edge_ids[&key].clone())
                .collect(),
        })
        .collect();
    Ok(TraceResult {
        schema: TRACE_SCHEMA_V1.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: PathQuery {
            from: from.into(),
            to: to.into(),
        },
        nodes: output.nodes,
        edges: output.edges,
        paths,
    })
}

#[derive(Default)]
struct GraphBuilder {
    edges: BTreeMap<EdgeKey, EdgeData>,
    kinds: BTreeMap<String, EntityKind>,
}

struct EdgeData {
    confidence: f32,
    evidence: BTreeSet<EvidenceRef>,
}

impl GraphBuilder {
    fn hint(&mut self, id: &str, kind: EntityKind) {
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
        let evidence = evidence_ref(from, evidence, provenance);
        self.edges
            .entry(key.clone())
            .and_modify(|edge| {
                edge.confidence = edge.confidence.min(confidence);
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
                entity_ref(
                    &id,
                    self.kinds
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| infer_kind(&id)),
                )
            })
            .collect::<Vec<_>>();
        let mut edge_ids = BTreeMap::new();
        let edges = self
            .edges
            .into_iter()
            .enumerate()
            .map(|(index, (key, edge))| {
                let id = format!("e{}", index + 1);
                edge_ids.insert(key.clone(), id.clone());
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
        GraphOutput {
            nodes,
            edges,
            edge_ids,
        }
    }
}

fn kind_priority(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Unknown => 0,
        EntityKind::Namespace => 1,
        EntityKind::Callable => 2,
        EntityKind::GraphqlField
        | EntityKind::KafkaTopic
        | EntityKind::Rpc
        | EntityKind::Service => 3,
    }
}

struct GraphOutput {
    nodes: Vec<EntityRef>,
    edges: Vec<SemanticEdge>,
    edge_ids: BTreeMap<EdgeKey, String>,
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
    EntityRef {
        id: id.into(),
        kind,
        name: entity_name(id),
        repository: repository(id),
        origin: if id.starts_with("rust-call://") || id.starts_with("rust-method://") {
            EntityOrigin::ExternalDependency
        } else {
            EntityOrigin::Source
        },
        test: is_test_entity(id),
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
    if id.starts_with("grpc://") || id.starts_with("rpc/") || id.starts_with("rpc://") {
        EntityKind::Rpc
    } else if id.starts_with("graphql-field://") {
        EntityKind::GraphqlField
    } else if id.starts_with("kafka-topic://") {
        EntityKind::KafkaTopic
    } else if id.starts_with("rust-call://") || id.starts_with("rust-method://") {
        EntityKind::Callable
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
    id.rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(id)
        .to_owned()
}

fn repository(id: &str) -> Option<String> {
    id.strip_prefix("repo://").and_then(|rest| {
        rest.split_once("/rust/")
            .map(|(repository, _)| repository.into())
    })
}

fn evidence_ref(from: &str, evidence: &str, provenance: &str) -> EvidenceRef {
    let (path, line) = evidence
        .rsplit_once(':')
        .and_then(|(path, line)| {
            line.parse()
                .ok()
                .map(|line| (Some(path.into()), Some(line)))
        })
        .unwrap_or((None, None));
    let has_path = path.is_some();
    EvidenceRef {
        source_kind: match provenance {
            "ast" => EvidenceKind::Ast,
            "unique_name_heuristic" => EvidenceKind::Inference,
            _ => EvidenceKind::Unknown,
        },
        repository: repository(from),
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

fn integer(row: &[InspectionValue], index: usize, name: &str) -> Result<i64, Box<dyn Error>> {
    row.get(index)
        .and_then(InspectionValue::as_i64)
        .ok_or_else(|| format!("{name} must be an integer").into())
}

fn float(row: &[InspectionValue], index: usize, name: &str) -> Result<f64, Box<dyn Error>> {
    match row.get(index) {
        Some(InspectionValue::Float(value)) => Ok(*value),
        Some(InspectionValue::Integer(value)) => Ok(*value as f64),
        _ => Err(format!("{name} must be numeric").into()),
    }
}

fn list<'a>(
    row: &'a [InspectionValue],
    index: usize,
    name: &str,
) -> Result<&'a [InspectionValue], Box<dyn Error>> {
    match row.get(index) {
        Some(InspectionValue::List(values)) => Ok(values),
        _ => Err(format!("{name} must be a list").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::is_test_entity;

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
