use beholder_dto::{
    EntityKind, EntityOrigin, EntityRef, EvidenceRef, QueryMetadata, SemanticEdge, SemanticPath,
    TraversalMetadata,
};
use std::{collections::BTreeMap, fmt::Write};

pub(super) fn raw(
    schema: &str,
    metadata: &QueryMetadata,
    nodes: &[EntityRef],
    edges: &[SemanticEdge],
    paths: &[SemanticPath],
    traversal: Option<&TraversalMetadata>,
) -> String {
    let mut output = format!(
        "{schema} · view {} · revision {} · stale={} · indexing={}",
        metadata.view, metadata.revision, metadata.freshness.stale, metadata.freshness.indexing
    );
    if metadata.analysis.completeness == beholder_dto::AnalysisCompleteness::Incomplete {
        output.push_str(" · incomplete=true");
    }
    if let Some(traversal) = traversal {
        let _ = write!(
            output,
            " · max_hops={} · truncated={}",
            traversal.max_hops, traversal.truncated
        );
    }
    output.push_str("\n\nnodes\n");
    for entity in nodes {
        let _ = writeln!(
            output,
            "  {} [{:?}] origin={:?} test={}",
            entity.id, entity.kind, entity.origin, entity.test
        );
    }
    output.push_str("\nedges\n");
    for edge in edges {
        let _ = writeln!(
            output,
            "  {}: {} -> {} [{}] confidence {:.2}",
            edge.id,
            edge.from,
            edge.to,
            edge.kind.as_str(),
            edge.confidence
        );
        for evidence in &edge.evidence {
            let _ = writeln!(output, "    {}", evidence_label(evidence));
        }
    }
    if !paths.is_empty() {
        output.push_str("\npaths\n");
        for (index, path) in paths.iter().enumerate() {
            let _ = writeln!(
                output,
                "  [{}] nodes={} edges={}",
                index + 1,
                path.nodes.join(" -> "),
                path.edges.join(",")
            );
        }
    }
    write_diagnostics(&mut output, metadata, "\ndiagnostics\n", "  ");
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Primary,
    Supporting,
    HiddenByDefault,
}

pub fn visibility(entity: &EntityRef, include_tests: bool) -> Visibility {
    if entity.test && !include_tests {
        Visibility::HiddenByDefault
    } else if entity.origin != EntityOrigin::Source {
        Visibility::Supporting
    } else if matches!(entity.kind, EntityKind::Namespace | EntityKind::ProtoFile) {
        Visibility::HiddenByDefault
    } else {
        Visibility::Primary
    }
}

pub(super) fn is_primary(
    entities: &BTreeMap<&str, &EntityRef>,
    id: &str,
    include_tests: bool,
) -> bool {
    entities
        .get(id)
        .is_none_or(|entity| visibility(entity, include_tests) == Visibility::Primary)
}

pub(super) struct ProjectedPath {
    pub(super) first: String,
    pub(super) steps: Vec<ProjectedStep>,
}

pub(super) struct ProjectedStep {
    pub(super) to: String,
    pub(super) kind: String,
    pub(super) confidence: f32,
    pub(super) evidence: Vec<EvidenceRef>,
}

pub(super) fn projected_paths(
    nodes: &[EntityRef],
    edges: &[SemanticEdge],
    paths: &[SemanticPath],
    include_tests: bool,
) -> Vec<ProjectedPath> {
    let entities = entities(nodes);
    let names = entity_names(nodes);
    let edges = edges
        .iter()
        .map(|edge| (edge.id.as_str(), edge))
        .collect::<BTreeMap<_, _>>();
    paths
        .iter()
        .filter_map(|path| {
            let first_id = path.nodes.first()?;
            let mut steps = Vec::new();
            let mut evidence = Vec::new();
            let mut confidence = 1.0_f32;
            for (index, edge_id) in path.edges.iter().enumerate() {
                let edge = *edges.get(edge_id.as_str())?;
                evidence.extend(edge.evidence.iter().cloned());
                confidence = confidence.min(edge.confidence);
                let target_id = path.nodes.get(index + 1)?;
                let target = entities.get(target_id.as_str())?;
                let endpoint = index + 1 == path.nodes.len() - 1;
                if endpoint || visibility(target, include_tests) == Visibility::Primary {
                    evidence.sort();
                    evidence.dedup();
                    steps.push(ProjectedStep {
                        to: label(&names, target),
                        kind: edge.kind.as_str().into(),
                        confidence,
                        evidence: std::mem::take(&mut evidence),
                    });
                    confidence = 1.0;
                }
            }
            Some(ProjectedPath {
                first: name(&names, first_id),
                steps,
            })
        })
        .collect()
}

pub(super) fn test_path(entity: &str, edges: &[SemanticEdge]) -> String {
    edges
        .iter()
        .filter(|edge| edge.from == entity)
        .flat_map(|edge| edge.evidence.iter())
        .find_map(|evidence| evidence.path.clone())
        .unwrap_or_else(|| "unknown source".into())
}

pub(super) fn entities(nodes: &[EntityRef]) -> BTreeMap<&str, &EntityRef> {
    nodes
        .iter()
        .map(|entity| (entity.id.as_str(), entity))
        .collect()
}

pub(super) fn entity_names(nodes: &[EntityRef]) -> BTreeMap<&str, String> {
    let mut groups: BTreeMap<String, Vec<&EntityRef>> = BTreeMap::new();
    for entity in nodes {
        groups.entry(entity_label(entity)).or_default().push(entity);
    }
    nodes
        .iter()
        .map(|entity| {
            let base = entity_label(entity);
            let peers = &groups[&base];
            let label = if peers.len() == 1 {
                base
            } else {
                shortest_unique_label(entity, peers)
            };
            (entity.id.as_str(), label)
        })
        .collect()
}

pub(super) fn name(names: &BTreeMap<&str, String>, id: &str) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.into())
}

pub(super) fn label(names: &BTreeMap<&str, String>, entity: &EntityRef) -> String {
    names
        .get(entity.id.as_str())
        .cloned()
        .unwrap_or_else(|| entity_label(entity))
}

pub(super) fn entity_label(entity: &EntityRef) -> String {
    let repository = entity.repository.as_deref().unwrap_or("cross-repository");
    match containing_scope(entity) {
        Some(scope) => format!("{repository} · {scope} · {}", entity.name),
        None => format!("{repository} · {}", entity.name),
    }
}

pub(super) fn containing_scope(entity: &EntityRef) -> Option<&str> {
    symbol_scope(entity)?
        .rsplit('/')
        .find(|segment| !matches!(*segment, "callback" | "field"))
}

pub(super) fn shortest_unique_label(entity: &EntityRef, peers: &[&EntityRef]) -> String {
    let repository = entity.repository.as_deref().unwrap_or("cross-repository");
    let scope = scope_segments(entity);
    for width in 1..=scope.len() {
        let candidate = scope[scope.len() - width..].join("/");
        if peers.iter().all(|peer| {
            peer.id == entity.id || {
                let peer_scope = scope_segments(peer);
                peer_scope.len() < width
                    || peer_scope[peer_scope.len() - width..].join("/") != candidate
            }
        }) {
            return format!("{repository} · {candidate} · {}", entity.name);
        }
    }
    format!("{repository} · {}", entity.id)
}

pub(super) fn scope_segments(entity: &EntityRef) -> Vec<&str> {
    symbol_scope(entity)
        .into_iter()
        .flat_map(|scope| scope.split('/'))
        .filter(|segment| !matches!(*segment, "callback" | "field"))
        .collect()
}

pub(super) fn symbol_scope(entity: &EntityRef) -> Option<&str> {
    let repository = entity.repository.as_deref()?;
    let symbol = entity
        .id
        .strip_prefix(&format!("repo://{repository}/"))?
        .split_once('/')?
        .1;
    symbol.strip_suffix(&format!("/{}", entity.name))
}

pub(super) fn evidence_label(evidence: &EvidenceRef) -> String {
    match (&evidence.path, evidence.line, &evidence.detail) {
        (Some(path), Some(line), _) => format!("{path}:{line}"),
        (Some(path), None, _) => path.clone(),
        (_, _, Some(detail)) => detail.clone(),
        _ => format!("{:?}", evidence.source_kind),
    }
}

pub(super) fn kind_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Callable => "callables",
        EntityKind::GraphqlArgument => "GraphQL arguments",
        EntityKind::GraphqlEnumValue => "GraphQL enum values",
        EntityKind::GraphqlField => "GraphQL",
        EntityKind::GraphqlOperation => "GraphQL operations",
        EntityKind::GraphqlType => "GraphQL types",
        EntityKind::KafkaTopic => "Kafka",
        EntityKind::Namespace => "namespaces",
        EntityKind::ProtoEnum => "Protobuf enums",
        EntityKind::ProtoField => "Protobuf fields",
        EntityKind::ProtoFile => "Protobuf files",
        EntityKind::ProtoMessage => "Protobuf messages",
        EntityKind::ProtoService => "Protobuf services",
        EntityKind::Rpc => "RPCs",
        EntityKind::Service => "services",
        EntityKind::UnityPrefab => "Unity prefabs",
        EntityKind::Unknown => "other",
    }
}

pub(super) fn plural<'a>(count: u32, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

pub(super) fn write_metadata(
    output: &mut String,
    metadata: &QueryMetadata,
    include_diagnostics: bool,
) {
    let _ = write!(
        output,
        "\nview {} · revision {} · stale={} · indexing={}",
        metadata.view, metadata.revision, metadata.freshness.stale, metadata.freshness.indexing
    );
    if metadata.analysis.completeness == beholder_dto::AnalysisCompleteness::Incomplete {
        let _ = write!(
            output,
            "\nanalysis incomplete · {} diagnostics",
            metadata.analysis.diagnostics.len()
        );
        if include_diagnostics {
            write_diagnostics(output, metadata, "\n", "  ");
        }
    }
}

fn write_diagnostics(output: &mut String, metadata: &QueryMetadata, heading: &str, indent: &str) {
    if metadata.analysis.diagnostics.is_empty() {
        return;
    }
    output.push_str(heading);
    let mut diagnostics = metadata.analysis.diagnostics.iter().collect::<Vec<_>>();
    diagnostics.sort();
    for diagnostic in diagnostics {
        let severity = match diagnostic.severity {
            beholder_dto::AnalysisDiagnosticSeverity::KnownLimitation => "known_limitation",
            beholder_dto::AnalysisDiagnosticSeverity::Warning => "warning",
        };
        let _ = write!(
            output,
            "{indent}{severity} {} {}",
            diagnostic.repository,
            diagnostic.path.display()
        );
        if let Some(line) = diagnostic.line {
            let _ = write!(output, ":{line}");
        }
        let _ = write!(output, " {}", diagnostic.code);
        if let Some(detail) = &diagnostic.detail {
            let _ = write!(output, " {detail}");
        }
        output.push('\n');
    }
}

pub(super) fn write_traversal(output: &mut String, traversal: &TraversalMetadata) {
    if traversal.truncated {
        let _ = write!(
            output,
            "\ntraversal incomplete · depth limit {} reached",
            traversal.max_hops
        );
    } else {
        let _ = write!(
            output,
            "\ntraversal complete · max depth {}",
            traversal.max_hops
        );
    }
}
