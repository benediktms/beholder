use beholder_dto::{
    ContextResult, DependenciesResult, EntityKind, EntityOrigin, EntityRef, EvidenceRef,
    ImpactResult, QueryMetadata, SemanticEdge, SemanticPath, TraceResult, TraversalMetadata,
    WhyResult,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode {
    Human,
    Json,
    JsonPretty,
    Raw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderOptions {
    pub mode: OutputMode,
    pub include_tests: bool,
}

impl From<OutputMode> for RenderOptions {
    fn from(mode: OutputMode) -> Self {
        Self {
            mode,
            include_tests: false,
        }
    }
}

pub fn context(
    result: &ContextResult,
    options: RenderOptions,
) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            None,
        )),
        OutputMode::Human => Ok(context_human(result, options.include_tests)),
    }
}

pub fn dependencies(
    result: &DependenciesResult,
    options: RenderOptions,
) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(dependencies_human(result, options.include_tests)),
    }
}

pub fn impact(result: &ImpactResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(impact_human(result, options.include_tests)),
    }
}

pub fn trace(result: &TraceResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &result.paths,
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(trace_human(result, options.include_tests)),
    }
}

pub fn why(result: &WhyResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &result.paths,
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(why_human(result, options.include_tests)),
    }
}

fn json(value: &impl Serialize, pretty: bool) -> Result<String, serde_json::Error> {
    if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
}

fn context_human(result: &ContextResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for edge in &result.edges {
        if edge.to == result.root.id && is_primary(&entities, &edge.from, include_tests) {
            incoming.push(format!(
                "  ← {} [{}]",
                context_name(&entities, &edge.from),
                incoming_relation(edge.kind.as_str())
            ));
        } else if edge.from == result.root.id && is_primary(&entities, &edge.to, include_tests) {
            outgoing.push(format!(
                "  → {} [{}]",
                context_name(&entities, &edge.to),
                edge.kind.as_str()
            ));
        }
    }
    let mut output = format!("{}\n", context_name(&entities, &result.root.id));
    if !incoming.is_empty() {
        output.push_str("\nincoming\n");
        output.push_str(&incoming.join("\n"));
        output.push('\n');
    }
    if !outgoing.is_empty() {
        output.push_str("\noutgoing\n");
        output.push_str(&outgoing.join("\n"));
        output.push('\n');
    }
    write_metadata(&mut output, &result.metadata);
    output
}

fn incoming_relation(relation: &str) -> &str {
    match relation {
        "calls" => "called by",
        "defines" => "defined by",
        "implements" => "implemented by",
        relation => relation,
    }
}

fn dependencies_human(result: &DependenciesResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let mut output = format!("{}\n", result.root.name);
    let dependencies = result
        .dependencies
        .iter()
        .filter(|dependency| is_primary(&entities, &dependency.entity, include_tests))
        .collect::<Vec<_>>();
    for dependency in &dependencies {
        let _ = writeln!(
            output,
            "  → {} ({} {})",
            name(&entities, &dependency.entity),
            dependency.hops,
            plural(dependency.hops, "hop", "hops")
        );
    }
    let _ = writeln!(output, "\n{} dependencies", dependencies.len());
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

fn impact_human(result: &ImpactResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let mut groups: BTreeMap<String, Vec<&EntityRef>> = BTreeMap::new();
    let mut tests: BTreeMap<String, Vec<&EntityRef>> = BTreeMap::new();
    let mut hidden_tests = 0;
    for affected in &result.affected {
        let entity = entities.get(affected.entity.as_str());
        if entity.is_some_and(|entity| entity.test) {
            if include_tests {
                let path = test_path(&affected.entity, &result.edges);
                if let Some(entity) = entity {
                    tests.entry(path).or_default().push(entity);
                }
            } else {
                hidden_tests += 1;
            }
            continue;
        }
        if entity.is_some_and(|entity| visibility(entity, include_tests) != Visibility::Primary) {
            continue;
        }
        let group = if affected.hops == 1 {
            "direct".into()
        } else {
            entity.map_or_else(|| "other".into(), |entity| kind_label(entity.kind).into())
        };
        if let Some(entity) = entity {
            groups.entry(group).or_default().push(entity);
        }
    }
    let affected_count = groups.values().map(Vec::len).sum::<usize>();
    let test_count = tests.values().map(Vec::len).sum::<usize>();
    let mut output = format!("{}\n", result.root.name);
    for (group, entities) in groups {
        let mut names = display_names(&entities);
        names.sort_unstable();
        let _ = writeln!(output, "\n{group}");
        for name in names {
            let _ = writeln!(output, "  - {name}");
        }
    }
    if !tests.is_empty() {
        output.push_str("\ntests\n");
        for (path, entities) in tests {
            let mut names = display_names(&entities);
            names.sort_unstable();
            let _ = writeln!(output, "  {path}");
            for name in names {
                let _ = writeln!(output, "    - {name}");
            }
        }
    }
    let repositories = result
        .nodes
        .iter()
        .filter_map(|entity| entity.repository.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let _ = writeln!(
        output,
        "\n{} affected symbols · {} repositories",
        affected_count + test_count,
        repositories
    );
    if hidden_tests > 0 {
        let _ = writeln!(
            output,
            "{hidden_tests} tests hidden · use --include-tests to show them"
        );
    }
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

fn display_names(entities: &[&EntityRef]) -> Vec<String> {
    let mut by_name: BTreeMap<&str, Vec<&EntityRef>> = BTreeMap::new();
    for entity in entities {
        by_name.entry(&entity.name).or_default().push(entity);
    }
    entities
        .iter()
        .map(|entity| {
            let peers = &by_name[entity.name.as_str()];
            if peers.len() == 1 {
                entity.name.clone()
            } else {
                shortest_unique_suffix(entity, peers)
            }
        })
        .collect()
}

fn shortest_unique_suffix(entity: &EntityRef, peers: &[&EntityRef]) -> String {
    let segments = entity.id.split('/').collect::<Vec<_>>();
    for width in 2..=segments.len() {
        let candidate = suffix(&segments, width);
        if peers.iter().all(|peer| {
            peer.id == entity.id
                || suffix(&peer.id.split('/').collect::<Vec<_>>(), width) != candidate
        }) {
            return candidate;
        }
    }
    entity.id.clone()
}

fn suffix(segments: &[&str], width: usize) -> String {
    segments[segments.len().saturating_sub(width)..].join("::")
}

fn trace_human(result: &TraceResult, include_tests: bool) -> String {
    let projected = projected_paths(&result.nodes, &result.edges, &result.paths, include_tests);
    if projected.is_empty() {
        let mut output = format!("No path from {} to {}", result.query.from, result.query.to);
        write_traversal(&mut output, &result.traversal);
        write_metadata(&mut output, &result.metadata);
        return output;
    }
    let mut output = String::new();
    if projected.len() == 1 {
        let path = &projected[0];
        output.push_str(&path.first);
        output.push('\n');
        for step in &path.steps {
            let _ = writeln!(output, "  → {} [{}]", step.to, step.kind);
        }
    } else {
        for (index, path) in projected.iter().enumerate() {
            let _ = write!(output, "[{}] {}", index + 1, path.first);
            for step in &path.steps {
                let _ = write!(output, " >{}> {}", step.kind, step.to);
            }
            output.push('\n');
        }
    }
    let hops = projected
        .iter()
        .map(|path| path.steps.len())
        .min()
        .unwrap_or(0);
    let repositories = result
        .nodes
        .iter()
        .filter_map(|entity| entity.repository.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let confidence = projected
        .iter()
        .flat_map(|path| path.steps.iter().map(|step| step.confidence))
        .fold(1.0_f32, f32::min);
    let _ = write!(
        output,
        "\n{hops} {} · {repositories} repositories · confidence {confidence:.2}",
        plural(hops as u32, "hop", "hops")
    );
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

fn why_human(result: &WhyResult, include_tests: bool) -> String {
    let projected = projected_paths(&result.nodes, &result.edges, &result.paths, include_tests);
    if projected.is_empty() {
        let mut output = format!("No path from {} to {}", result.query.from, result.query.to);
        write_traversal(&mut output, &result.traversal);
        write_metadata(&mut output, &result.metadata);
        return output;
    }
    let mut output = String::new();
    for (path_index, path) in projected.iter().enumerate() {
        if projected.len() > 1 {
            let _ = writeln!(output, "[{}]", path_index + 1);
        }
        let _ = writeln!(output, "{}", path.first);
        for step in &path.steps {
            let _ = writeln!(output, "  → {} [{}]", step.to, step.kind);
            for evidence in &step.evidence {
                let _ = writeln!(output, "     {}", evidence_label(evidence));
            }
        }
        if path_index + 1 < projected.len() {
            output.push('\n');
        }
    }
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

fn raw(
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

fn is_primary(entities: &BTreeMap<&str, &EntityRef>, id: &str, include_tests: bool) -> bool {
    entities
        .get(id)
        .is_none_or(|entity| visibility(entity, include_tests) == Visibility::Primary)
}

struct ProjectedPath {
    first: String,
    steps: Vec<ProjectedStep>,
}

struct ProjectedStep {
    to: String,
    kind: String,
    confidence: f32,
    evidence: Vec<EvidenceRef>,
}

fn projected_paths(
    nodes: &[EntityRef],
    edges: &[SemanticEdge],
    paths: &[SemanticPath],
    include_tests: bool,
) -> Vec<ProjectedPath> {
    let entities = entities(nodes);
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
                        to: target.name.clone(),
                        kind: edge.kind.as_str().into(),
                        confidence,
                        evidence: std::mem::take(&mut evidence),
                    });
                    confidence = 1.0;
                }
            }
            Some(ProjectedPath {
                first: name(&entities, first_id),
                steps,
            })
        })
        .collect()
}

fn test_path(entity: &str, edges: &[SemanticEdge]) -> String {
    edges
        .iter()
        .filter(|edge| edge.from == entity)
        .flat_map(|edge| edge.evidence.iter())
        .find_map(|evidence| evidence.path.clone())
        .unwrap_or_else(|| "unknown source".into())
}

fn entities(nodes: &[EntityRef]) -> BTreeMap<&str, &EntityRef> {
    nodes
        .iter()
        .map(|entity| (entity.id.as_str(), entity))
        .collect()
}

fn name(entities: &BTreeMap<&str, &EntityRef>, id: &str) -> String {
    entities
        .get(id)
        .map(|entity| entity.name.clone())
        .unwrap_or_else(|| id.into())
}

fn context_name(entities: &BTreeMap<&str, &EntityRef>, id: &str) -> String {
    entities.get(id).map_or_else(
        || id.into(),
        |entity| {
            let repository = entity.repository.as_deref().unwrap_or("cross-repository");
            match containing_scope(entity) {
                Some(scope) => format!("{repository} · {scope} · {}", entity.name),
                None => format!("{repository} · {}", entity.name),
            }
        },
    )
}

fn containing_scope(entity: &EntityRef) -> Option<&str> {
    let repository = entity.repository.as_deref()?;
    let symbol = entity
        .id
        .strip_prefix(&format!("repo://{repository}/"))?
        .split_once('/')?
        .1;
    let scope = symbol.strip_suffix(&format!("/{}", entity.name))?;
    scope
        .rsplit('/')
        .find(|segment| !matches!(*segment, "callback" | "field"))
}

fn evidence_label(evidence: &EvidenceRef) -> String {
    match (&evidence.path, evidence.line, &evidence.detail) {
        (Some(path), Some(line), _) => format!("{path}:{line}"),
        (Some(path), None, _) => path.clone(),
        (_, _, Some(detail)) => detail.clone(),
        _ => format!("{:?}", evidence.source_kind),
    }
}

fn kind_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Callable => "callables",
        EntityKind::GraphqlField => "GraphQL",
        EntityKind::KafkaTopic => "Kafka",
        EntityKind::Namespace => "namespaces",
        EntityKind::ProtoEnum => "Protobuf enums",
        EntityKind::ProtoField => "Protobuf fields",
        EntityKind::ProtoFile => "Protobuf files",
        EntityKind::ProtoMessage => "Protobuf messages",
        EntityKind::ProtoService => "Protobuf services",
        EntityKind::Rpc => "RPCs",
        EntityKind::Service => "services",
        EntityKind::Unknown => "other",
    }
}

fn plural<'a>(count: u32, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

fn write_metadata(output: &mut String, metadata: &QueryMetadata) {
    let _ = write!(
        output,
        "\nview {} · revision {} · stale={} · indexing={}",
        metadata.view, metadata.revision, metadata.freshness.stale, metadata.freshness.indexing
    );
}

fn write_traversal(output: &mut String, traversal: &TraversalMetadata) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_dto::{
        CONTEXT_SCHEMA_V1, ContextResult, DEPENDENCIES_SCHEMA_V2, DependenciesResult, EntityQuery,
        EvidenceKind, Freshness, IMPACT_SCHEMA_V2, ImpactRef, ImpactResult, PathQuery,
        QueryMetadata, RelationKind, TRACE_SCHEMA_V2,
    };

    fn traversal() -> TraversalMetadata {
        TraversalMetadata {
            max_hops: 32,
            truncated: false,
        }
    }

    fn trace_result() -> TraceResult {
        TraceResult {
            schema: TRACE_SCHEMA_V2.into(),
            metadata: QueryMetadata {
                revision: 42,
                view: "main".into(),
                freshness: Freshness {
                    stale: false,
                    indexing: false,
                    dirty_repositories: Vec::new(),
                },
            },
            query: PathQuery {
                from: "a".into(),
                to: "rpc".into(),
            },
            traversal: traversal(),
            nodes: vec![
                entity("a", "CheckoutPage", EntityKind::Callable, false),
                entity("generated", "PricingClient", EntityKind::Callable, true),
                entity("rpc", "Pricing.GetPrice", EntityKind::Rpc, false),
            ],
            edges: vec![
                edge("e1", "a", "generated", "calls", "src/checkout.rs", 12),
                edge("e2", "generated", "rpc", "calls_rpc", "generated.rs", 8),
            ],
            paths: vec![SemanticPath {
                nodes: vec!["a".into(), "generated".into(), "rpc".into()],
                edges: vec!["e1".into(), "e2".into()],
            }],
        }
    }

    fn entity(id: &str, name: &str, kind: EntityKind, generated: bool) -> EntityRef {
        EntityRef {
            id: id.into(),
            kind,
            name: name.into(),
            repository: Some("repo".into()),
            origin: if generated {
                EntityOrigin::Generated
            } else {
                EntityOrigin::Source
            },
            test: false,
            metadata: None,
        }
    }

    fn edge(id: &str, from: &str, to: &str, kind: &str, path: &str, line: u32) -> SemanticEdge {
        SemanticEdge {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            kind: RelationKind::try_from(kind).unwrap(),
            confidence: 1.0,
            evidence: vec![EvidenceRef {
                source_kind: EvidenceKind::Ast,
                repository: Some("repo".into()),
                path: Some(path.into()),
                line: Some(line),
                detail: None,
            }],
        }
    }

    #[test]
    fn trace_json_is_versioned_and_compact_output_is_deterministic() {
        let result = trace_result();
        let json = trace(&result, OutputMode::Json.into()).unwrap();
        assert!(json.starts_with(r#"{"schema":"beholder.trace.v2","revision":42,"view":"main""#));
        assert!(json.contains(r#""traversal":{"max_hops":32,"truncated":false}"#));
        assert_eq!(
            trace(&result, OutputMode::Human.into()).unwrap(),
            "CheckoutPage\n  → Pricing.GetPrice [calls_rpc]\n\n1 hop · 1 repositories · confidence 1.00\ntraversal complete · max depth 32\nview main · revision 42 · stale=false · indexing=false"
        );
    }

    #[test]
    fn incomplete_traversal_is_explicit() {
        let mut result = trace_result();
        result.paths.clear();
        result.traversal.max_hops = 1;
        result.traversal.truncated = true;
        let output = trace(&result, OutputMode::Human.into()).unwrap();
        assert!(output.contains("traversal incomplete · depth limit 1 reached"));
    }

    #[test]
    fn raw_retains_hidden_supporting_nodes_and_why_retains_evidence() {
        let result = trace_result();
        let raw = trace(&result, OutputMode::Raw.into()).unwrap();
        assert!(raw.contains("generated [Callable] origin=Generated"));
        let why_result = WhyResult::from(result);
        let human = why(&why_result, OutputMode::Human.into()).unwrap();
        assert!(human.contains("src/checkout.rs:12"));
        assert!(human.contains("generated.rs:8"));
        for mode in [OutputMode::Json, OutputMode::JsonPretty, OutputMode::Raw] {
            let output = why(&why_result, mode.into()).unwrap();
            assert!(output.contains("42"));
            assert!(output.contains("main"));
        }
    }

    #[test]
    fn freshness_and_revision_survive_every_renderer() {
        let trace_result = trace_result();
        let root = trace_result.nodes[0].clone();
        let context_result = ContextResult {
            schema: CONTEXT_SCHEMA_V1.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            root: root.clone(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let dependencies_result = DependenciesResult {
            schema: DEPENDENCIES_SCHEMA_V2.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root: root.clone(),
            dependencies: Vec::new(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let impact_result = ImpactResult {
            schema: IMPACT_SCHEMA_V2.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root,
            affected: Vec::new(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let why_result = WhyResult::from(trace_result.clone());
        for mode in [
            OutputMode::Human,
            OutputMode::Json,
            OutputMode::JsonPretty,
            OutputMode::Raw,
        ] {
            for output in [
                context(&context_result, mode.into()).unwrap(),
                dependencies(&dependencies_result, mode.into()).unwrap(),
                impact(&impact_result, mode.into()).unwrap(),
                trace(&trace_result, mode.into()).unwrap(),
                why(&why_result, mode.into()).unwrap(),
            ] {
                assert!(output.contains("42"));
                assert!(output.contains("main"));
            }
        }
    }

    #[test]
    fn context_labels_incoming_calls_from_the_root_perspective() {
        let trace = trace_result();
        let result = ContextResult {
            schema: CONTEXT_SCHEMA_V1.into(),
            metadata: trace.metadata,
            query: EntityQuery {
                entity: "generated".into(),
            },
            root: trace.nodes[1].clone(),
            nodes: trace.nodes,
            edges: trace.edges,
        };

        assert!(
            context(&result, OutputMode::Human.into())
                .unwrap()
                .contains("← repo · CheckoutPage [called by]")
        );
    }

    #[test]
    fn context_names_include_repository_and_containing_scope() {
        let elixir = entity(
            "repo://repo/elixir/Example.Client/run/1",
            "run/1",
            EntityKind::Callable,
            false,
        );
        let rust = entity(
            "repo://repo/rust/src/client/impl/Client/run",
            "run",
            EntityKind::Callable,
            false,
        );
        let nodes = [elixir, rust];
        let entities = entities(&nodes);

        assert_eq!(
            context_name(&entities, "repo://repo/elixir/Example.Client/run/1"),
            "repo · Example.Client · run/1"
        );
        assert_eq!(
            context_name(&entities, "repo://repo/rust/src/client/impl/Client/run"),
            "repo · Client · run"
        );
    }

    #[test]
    fn impact_hides_tests_by_default_and_groups_included_tests_by_file() {
        let mut test = entity(
            "repo/rust/src/tests/test_checkout",
            "test_checkout",
            EntityKind::Callable,
            false,
        );
        test.test = true;
        let root = entity("root", "checkout", EntityKind::Callable, false);
        let result = ImpactResult {
            schema: IMPACT_SCHEMA_V2.into(),
            metadata: QueryMetadata::completed("main", 42),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root: root.clone(),
            affected: vec![ImpactRef {
                entity: test.id.clone(),
                hops: 1,
            }],
            nodes: vec![root, test.clone()],
            edges: vec![edge(
                "e1",
                &test.id,
                "root",
                "calls",
                "src/checkout/tests.rs",
                9,
            )],
        };

        let compact = impact(&result, OutputMode::Human.into()).unwrap();
        assert!(!compact.contains("test_checkout"));
        assert!(compact.contains("1 tests hidden"));

        let included = impact(
            &result,
            RenderOptions {
                mode: OutputMode::Human,
                include_tests: true,
            },
        )
        .unwrap();
        assert!(included.contains("src/checkout/tests.rs\n    - test_checkout"));
    }

    #[test]
    fn duplicate_callable_names_use_the_shortest_unique_suffix() {
        let first = entity(
            "repo://app/rust/crates/cli/src/commands/analyses/run/impl/AnalysisCtx/new",
            "new",
            EntityKind::Callable,
            false,
        );
        let second = entity(
            "repo://app/rust/crates/cli/src/commands/artifacts/run/impl/ArtifactCtx/new",
            "new",
            EntityKind::Callable,
            false,
        );

        assert_eq!(
            display_names(&[&first, &second]),
            ["AnalysisCtx::new", "ArtifactCtx::new"]
        );
    }
}
