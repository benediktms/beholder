use crate::{InspectionResult, InspectionValue, query::ContextRow};
use beholder_domain as domain;
use beholder_dto::{
    CONTEXT_SCHEMA_V1, ContextResult, DEPENDENCIES_SCHEMA_V2, DependenciesResult, DependencyRef,
    ENTITY_SEARCH_SCHEMA_V2, EntityKind, EntityMetadata, EntityOrigin, EntityQuery, EntityRef,
    EntitySearchQuery, EntitySearchResult, EvidenceKind, EvidenceRef, GraphqlOperationKind,
    GraphqlTypeKind, IMPACT_SCHEMA_V2, ImpactRef, ImpactResult, PathQuery, ProtoTypeKind,
    QueryMetadata, RelationKind, RpcCardinality, SemanticEdge, SemanticPath, TRACE_SCHEMA_V2,
    TraceResult, TraversalMetadata, WORKSPACE_TOPOLOGY_SCHEMA_V1, WorkspaceTopology,
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

pub(super) fn workspace_topology(
    view: &str,
    result: InspectionResult,
    entities: InspectionResult,
) -> Result<WorkspaceTopology, Box<dyn Error>> {
    let facts = entity_kinds(entities)?;
    let mut graph = GraphBuilder::default();
    graph.hint_facts(facts.clone());
    for (id, (kind, _)) in &facts {
        graph.hint(id, *kind);
    }
    for row in result.rows {
        let from = text(&row, 0, "topology source")?;
        let to = text(&row, 1, "topology target")?;
        if !facts.contains_key(from) {
            return Err(format!("missing entity fact for topology endpoint {from}").into());
        }
        if !facts.contains_key(to) {
            return Err(format!("missing entity fact for topology endpoint {to}").into());
        }
        graph.add_edge(
            from,
            to,
            text(&row, 2, "topology relation")?,
            text(&row, 3, "topology evidence")?,
            float(&row, 4, "topology confidence")? as f32,
            text(&row, 5, "topology provenance")?,
        )?;
    }
    let output = graph.finish();
    if output
        .nodes
        .iter()
        .any(|node| node.kind == EntityKind::Unknown)
    {
        return Err("workspace topology contains an Unknown entity".into());
    }
    Ok(WorkspaceTopology {
        schema: WORKSPACE_TOPOLOGY_SCHEMA_V1.into(),
        metadata: QueryMetadata::completed(view, 0),
        nodes: output.nodes,
        edges: output.edges,
    })
}

pub(super) fn search_entities(
    view: &str,
    query: &str,
    limit: u32,
    entities: InspectionResult,
) -> Result<EntitySearchResult, Box<dyn Error>> {
    let mut matches = entity_kinds(entities)?
        .into_iter()
        .map(|(id, (kind, metadata))| entity_ref_with_origin(&id, kind, None, metadata))
        .filter(|entity| entity.id.starts_with(query) || entity.name.starts_with(query))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        search_rank(left, query)
            .cmp(&search_rank(right, query))
            .then_with(|| left.id.cmp(&right.id))
    });
    matches.truncate(limit as usize);
    Ok(EntitySearchResult {
        schema: ENTITY_SEARCH_SCHEMA_V2.into(),
        metadata: QueryMetadata::completed(view, 0),
        query: EntitySearchQuery {
            query: query.into(),
            limit,
        },
        matches,
    })
}

fn search_rank(entity: &EntityRef, query: &str) -> u8 {
    if entity.id == query {
        0
    } else if entity.name == query {
        1
    } else if entity.id.starts_with(query) || entity.name.starts_with(query) {
        2
    } else {
        3
    }
}

pub(super) fn context(
    view: &str,
    entity: &str,
    result: Vec<ContextRow>,
    entities: InspectionResult,
) -> Result<ContextResult, Box<dyn Error>> {
    let mut graph = GraphBuilder::default();
    graph.hint_facts(entity_kinds(entities)?);
    graph.hint(entity, infer_kind(entity));
    for row in result {
        let _ = match row.direction.as_str() {
            "outgoing" => graph.add_edge(
                entity,
                &row.related,
                &row.relation,
                &row.evidence,
                row.confidence as f32,
                &row.provenance,
            )?,
            "incoming" => graph.add_edge(
                &row.related,
                entity,
                &row.relation,
                &row.evidence,
                row.confidence as f32,
                &row.provenance,
            )?,
            direction => return Err(format!("unknown context direction: {direction}").into()),
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

fn traversal_endpoints(edge: &SemanticEdge, direction: TraversalDirection) -> (&str, &str) {
    match direction {
        TraversalDirection::Outgoing => (&edge.from, &edge.to),
        TraversalDirection::Incoming
            if edge.kind == RelationKind::ImplementedBy && edge.from.starts_with("grpc://") =>
        {
            (&edge.from, &edge.to)
        }
        TraversalDirection::Incoming => (&edge.to, &edge.from),
    }
}

fn distances(
    root: &str,
    edges: &[SemanticEdge],
    max_hops: u32,
    direction: TraversalDirection,
) -> (BTreeMap<String, u32>, bool) {
    let mut adjacent = BTreeMap::<&str, Vec<&str>>::new();
    for edge in edges {
        let (from, to) = traversal_endpoints(edge, direction);
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

pub(super) fn traverse_graph(
    view: &str,
    query: beholder_dto::TraverseGraphQuery,
    result: InspectionResult,
    entities: InspectionResult,
    incomplete: BTreeSet<String>,
    repository_boundaries: BTreeSet<String>,
    target_reachability: BTreeMap<String, BTreeSet<String>>,
) -> Result<beholder_dto::TraverseGraphResult, Box<dyn Error>> {
    use beholder_dto::*;
    let mut output = graph(result, entities, &[&query.start])?;
    let mut adjacent = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    let direction = match query.direction {
        GraphDirection::Dependencies => TraversalDirection::Outgoing,
        GraphDirection::Dependents => TraversalDirection::Incoming,
    };
    for edge in &output.edges {
        let (from, to) = traversal_endpoints(edge, direction);
        adjacent.entry(from).or_default().push((to, &edge.id));
    }
    for neighbours in adjacent.values_mut() {
        neighbours.sort();
    }
    let mut search = PathSearch {
        query: &query,
        adjacent,
        incomplete: &incomplete,
        repository_boundaries: &repository_boundaries,
        target_reachability: &target_reachability,
        paths: Vec::new(),
        reasons: BTreeSet::new(),
        steps: 0,
    };
    if !incomplete.is_empty() {
        search.reasons.insert(TruncationReason::AcquisitionLimit);
    }
    search.visit(&mut vec![query.start.clone()], &mut Vec::new());
    let PathSearch { paths, reasons, .. } = search;
    let nodes: BTreeSet<_> = paths
        .iter()
        .flat_map(|path| path.nodes.iter())
        .chain(std::iter::once(&query.start))
        .collect();
    output.nodes.retain(|node| nodes.contains(&node.id));
    output
        .edges
        .retain(|edge| nodes.contains(&edge.from) && nodes.contains(&edge.to));
    Ok(TraverseGraphResult {
        schema: TRAVERSE_GRAPH_SCHEMA_V2.into(),
        metadata: QueryMetadata::completed(view, 0),
        traversal: GraphTraversalMetadata {
            max_hops: query.max_hops,
            max_paths: query.max_paths,
            max_rows: MAX_TRAVERSAL_ROWS,
            max_steps: MAX_TRAVERSAL_STEPS,
            acquisition_timeout_ms: TRAVERSAL_ACQUISITION_TIMEOUT_MS,
            truncated: !reasons.is_empty(),
            truncation_reasons: reasons.into_iter().collect(),
        },
        query,
        nodes: output.nodes,
        edges: output.edges,
        paths,
    })
}

struct PathSearch<'a> {
    query: &'a beholder_dto::TraverseGraphQuery,
    adjacent: BTreeMap<&'a str, Vec<(&'a str, &'a str)>>,
    incomplete: &'a BTreeSet<String>,
    repository_boundaries: &'a BTreeSet<String>,
    target_reachability: &'a BTreeMap<String, BTreeSet<String>>,
    paths: Vec<beholder_dto::TraversalPath>,
    reasons: BTreeSet<beholder_dto::TruncationReason>,
    steps: u32,
}

impl PathSearch<'_> {
    fn repository_state(&self, nodes: &[String]) -> (BTreeSet<String>, Option<String>) {
        let targets = self
            .query
            .target_repositories
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut visited = BTreeSet::new();
        let mut completion = None;
        for node in nodes {
            if let Some(repository) = target_repository(node, &targets) {
                visited.insert(repository.to_owned());
                if completion.is_none() && visited == targets {
                    completion = Some(repository.to_owned());
                }
            }
        }
        (visited, completion)
    }

    fn can_follow(&self, nodes: &[String], next: &str) -> bool {
        if self.query.target_repositories.is_empty() {
            return true;
        }
        let targets = self
            .query
            .target_repositories
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let (mut visited, completion) = self.repository_state(nodes);
        if let Some(completion) = completion {
            return !next.starts_with("repo://")
                || target_repository(next, &targets) == Some(completion.as_str());
        }
        if let Some(owner) = target_repository(next, &targets) {
            visited.insert(owner.to_owned());
        }
        let missing = targets.difference(&visited).collect::<BTreeSet<_>>();
        missing.is_empty()
            || self
                .target_reachability
                .get(next)
                .is_some_and(|reachable| missing.iter().all(|target| reachable.contains(*target)))
    }

    fn emit(
        &mut self,
        nodes: &[String],
        edges: &[String],
        termination: beholder_dto::PathTermination,
    ) {
        if self.paths.len() == self.query.max_paths as usize {
            self.reasons
                .insert(beholder_dto::TruncationReason::MaxPaths);
        } else {
            self.paths.push(beholder_dto::TraversalPath {
                nodes: nodes.to_vec(),
                edges: edges.to_vec(),
                termination,
            });
        }
    }

    fn visit(&mut self, nodes: &mut Vec<String>, edges: &mut Vec<String>) {
        use beholder_dto::{MAX_TRAVERSAL_STEPS, PathTermination::*, TruncationReason};
        if self.reasons.contains(&TruncationReason::MaxPaths)
            || self.reasons.contains(&TruncationReason::WorkLimit)
        {
            return;
        }
        if self.steps == MAX_TRAVERSAL_STEPS {
            self.reasons.insert(TruncationReason::WorkLimit);
            return;
        }
        self.steps += 1;
        let node = nodes.last().unwrap().clone();
        if self.query.destination.as_ref() == Some(&node) {
            self.emit(nodes, edges, Destination);
            return;
        }
        let (_, completion) = self.repository_state(nodes);
        if completion.is_some() && !node.starts_with("repo://") {
            if !self.incomplete.contains(&node) {
                self.emit(nodes, edges, RepositoryBoundary);
            }
            return;
        }
        let emitted_boundary = completion.is_some()
            && self.repository_boundaries.contains(&node)
            && !self.incomplete.contains(&node);
        if emitted_boundary {
            self.emit(nodes, edges, RepositoryBoundary);
            if self.reasons.contains(&TruncationReason::MaxPaths) {
                return;
            }
        }
        let neighbours = self
            .adjacent
            .get(node.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|(next, _)| self.can_follow(nodes, next))
            .collect::<Vec<_>>();
        if neighbours.len() > (MAX_TRAVERSAL_STEPS - self.steps) as usize {
            self.reasons.insert(TruncationReason::WorkLimit);
            return;
        }
        self.steps += neighbours.len() as u32;
        let has_extension = neighbours
            .iter()
            .any(|(to, _)| !nodes.iter().any(|id| id == to));
        if !has_extension {
            if self.query.destination.is_none()
                && !self.incomplete.contains(&node)
                && (self.query.target_repositories.is_empty() || completion.is_some())
                && !emitted_boundary
            {
                self.emit(
                    nodes,
                    edges,
                    if neighbours.is_empty() { Leaf } else { Cycle },
                );
            }
            return;
        }
        if edges.len() == self.query.max_hops as usize {
            self.reasons.insert(TruncationReason::MaxHops);
            if self.query.destination.is_none()
                && (self.query.target_repositories.is_empty() || completion.is_some())
            {
                self.emit(nodes, edges, MaxHops);
            }
            return;
        }
        for (to, edge) in neighbours {
            if self.steps == MAX_TRAVERSAL_STEPS {
                self.reasons.insert(TruncationReason::WorkLimit);
                break;
            }
            self.steps += 1;
            if nodes.iter().any(|id| id == to) {
                continue;
            }
            nodes.push(to.to_owned());
            edges.push(edge.to_owned());
            self.visit(nodes, edges);
            nodes.pop();
            edges.pop();
            if self.reasons.contains(&TruncationReason::MaxPaths)
                || self.reasons.contains(&TruncationReason::WorkLimit)
            {
                break;
            }
        }
    }
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
                ("graphql_argument", "") => (EntityKind::GraphqlArgument, None),
                ("graphql_enum_value", "") => (EntityKind::GraphqlEnumValue, None),
                ("graphql_field", "") => (EntityKind::GraphqlField, None),
                ("graphql_operation", "") => (EntityKind::GraphqlOperation, None),
                ("graphql_operation", "graphql_operation:mutation") => (
                    EntityKind::GraphqlOperation,
                    Some(EntityMetadata::GraphqlOperation {
                        operation_kind: GraphqlOperationKind::Mutation,
                    }),
                ),
                ("graphql_operation", "graphql_operation:query") => (
                    EntityKind::GraphqlOperation,
                    Some(EntityMetadata::GraphqlOperation {
                        operation_kind: GraphqlOperationKind::Query,
                    }),
                ),
                ("graphql_operation", "graphql_operation:subscription") => (
                    EntityKind::GraphqlOperation,
                    Some(EntityMetadata::GraphqlOperation {
                        operation_kind: GraphqlOperationKind::Subscription,
                    }),
                ),
                ("graphql_type", "graphql_type:enum") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Enum,
                    }),
                ),
                ("graphql_type", "graphql_type:input") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Input,
                    }),
                ),
                ("graphql_type", "graphql_type:interface") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Interface,
                    }),
                ),
                ("graphql_type", "graphql_type:object") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Object,
                    }),
                ),
                ("graphql_type", "graphql_type:scalar") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Scalar,
                    }),
                ),
                ("graphql_type", "graphql_type:union") => (
                    EntityKind::GraphqlType,
                    Some(EntityMetadata::GraphqlType {
                        type_kind: GraphqlTypeKind::Union,
                    }),
                ),
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
        EntityKind::GraphqlArgument
        | EntityKind::GraphqlEnumValue
        | EntityKind::GraphqlField
        | EntityKind::GraphqlOperation
        | EntityKind::GraphqlType
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
    } else if id.starts_with("graphql-argument://") {
        EntityKind::GraphqlArgument
    } else if id.starts_with("graphql-enum-value://") {
        EntityKind::GraphqlEnumValue
    } else if id.starts_with("graphql-operation://") {
        EntityKind::GraphqlOperation
    } else if id.starts_with("graphql-type://") {
        EntityKind::GraphqlType
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
        ("calls_graphql", true) => EntityKind::Callable,
        ("calls_graphql", false) => EntityKind::GraphqlOperation,
        ("calls_rpc", false) | ("implemented_by", true) => EntityKind::Rpc,
        ("selects", false) | ("resolved_by", true) => EntityKind::GraphqlField,
        _ => EntityKind::Unknown,
    }
}

pub(super) fn entity_name(id: &str) -> String {
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

pub(super) fn repository(id: &str) -> Option<String> {
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

pub(super) fn target_repository<'a>(id: &str, targets: &'a BTreeSet<String>) -> Option<&'a str> {
    let rest = id.strip_prefix("repo://")?;
    targets.iter().find_map(|target| {
        rest.strip_prefix(target.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
            .then_some(target.as_str())
    })
}

fn evidence_ref(from: &str, to: &str, evidence: &str, provenance: &str) -> EvidenceRef {
    let decoded = domain::Evidence::from(evidence).decode();
    let descriptor_path = provenance == "descriptor" && decoded.path.is_none();
    let path = decoded
        .path
        .or_else(|| descriptor_path.then(|| evidence.into()));
    let has_path = path.is_some();
    EvidenceRef {
        source_kind: match provenance {
            "ast" => EvidenceKind::Ast,
            "compiler" => EvidenceKind::Compiler,
            "unique_name_heuristic" => EvidenceKind::Inference,
            "descriptor" => EvidenceKind::Descriptor,
            "generated" => EvidenceKind::Generated,
            _ => EvidenceKind::Unknown,
        },
        repository: repository(from).or_else(|| repository(to)),
        path,
        line: decoded.line,
        range: decoded.range,
        detail: match provenance {
            "unique_name_heuristic" => Some(provenance.into()),
            _ if descriptor_path => None,
            _ => decoded
                .detail
                .or_else(|| (!has_path).then(|| evidence.into())),
        },
        contexts: decoded.contexts,
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
    use super::{
        GraphBuilder, PathSearch, context, dependencies, entity_ref, impact, infer_kind,
        is_test_entity, search_entities, trace, traverse_graph, workspace_topology,
    };
    use crate::{InspectionResult, InspectionValue};
    use beholder_domain::{
        ConditionArmKind, ConditionConstruct, Evidence, EvidenceContext, EvidencePayload,
        SourcePosition, SourceRange,
    };
    use beholder_dto::{
        EntityKind, EntityOrigin, GraphDirection, PathTermination, TraverseGraphQuery,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn every_evidence_query_preserves_same_line_occurrences() {
        let from = "repo://example/repo/rust/caller";
        let to = "repo://example/repo/rust/target";
        let evidence = [4, 14].map(|character| {
            Evidence::structured(EvidencePayload {
                path: Some("src/lib.rs".into()),
                line: Some(2),
                detail: None,
                range: Some(SourceRange {
                    start: SourcePosition { line: 1, character },
                    end: SourcePosition {
                        line: 1,
                        character: character + 4,
                    },
                }),
                contexts: vec![EvidenceContext::ConditionArm {
                    construct: ConditionConstruct::If,
                    arm: ConditionArmKind::Then,
                    condition: None,
                    arm_range: SourceRange {
                        start: SourcePosition {
                            line: 1,
                            character: 0,
                        },
                        end: SourcePosition {
                            line: 2,
                            character: 0,
                        },
                    },
                }],
            })
            .unwrap()
            .as_str()
            .to_owned()
        });
        let entities = || InspectionResult {
            headers: Vec::new(),
            rows: [from, to]
                .map(|id| {
                    vec![
                        InspectionValue::String(id.into()),
                        InspectionValue::String("callable".into()),
                        InspectionValue::String(String::new()),
                    ]
                })
                .into(),
            next: None,
        };
        let graph_rows = || InspectionResult {
            headers: Vec::new(),
            rows: evidence
                .iter()
                .map(|evidence| {
                    vec![
                        InspectionValue::String("edge".into()),
                        InspectionValue::String(String::new()),
                        InspectionValue::String(String::new()),
                        InspectionValue::String(from.into()),
                        InspectionValue::String(to.into()),
                        InspectionValue::String("calls".into()),
                        InspectionValue::String(evidence.clone()),
                        InspectionValue::Float(1.0),
                        InspectionValue::String("ast".into()),
                    ]
                })
                .collect(),
            next: None,
        };
        let context_result = context(
            "main",
            from,
            evidence
                .iter()
                .map(|evidence| crate::query::ContextRow {
                    direction: "outgoing".into(),
                    relation: "calls".into(),
                    related: to.into(),
                    evidence: evidence.clone(),
                    confidence: 1.0,
                    provenance: "ast".into(),
                })
                .collect(),
            entities(),
        )
        .unwrap();
        let expected = &context_result.edges[0].evidence;
        assert_eq!(expected.len(), 2);
        assert_ne!(expected[0].range, expected[1].range);

        let dependencies = dependencies("main", from, 8, graph_rows(), entities()).unwrap();
        let impact = impact("main", to, 8, graph_rows(), entities()).unwrap();
        let trace = trace("main", from, to, 8, graph_rows(), entities()).unwrap();
        let why = beholder_dto::WhyResult::from(trace.clone());
        let traversal = traverse_graph(
            "main",
            TraverseGraphQuery {
                start: from.into(),
                direction: GraphDirection::Dependencies,
                destination: Some(to.into()),
                target_repositories: Vec::new(),
                max_hops: 8,
                max_paths: 50,
            },
            graph_rows(),
            entities(),
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap();
        let topology = workspace_topology(
            "main",
            InspectionResult {
                headers: Vec::new(),
                rows: evidence
                    .iter()
                    .map(|evidence| {
                        vec![
                            InspectionValue::String(from.into()),
                            InspectionValue::String(to.into()),
                            InspectionValue::String("calls".into()),
                            InspectionValue::String(evidence.clone()),
                            InspectionValue::Float(1.0),
                            InspectionValue::String("ast".into()),
                        ]
                    })
                    .collect(),
                next: None,
            },
            entities(),
        )
        .unwrap();

        for evidence in [
            &dependencies.edges[0].evidence,
            &impact.edges[0].evidence,
            &trace.edges[0].evidence,
            &why.edges[0].evidence,
            &traversal.edges[0].evidence,
            &topology.edges[0].evidence,
        ] {
            assert_eq!(evidence, expected);
        }
    }

    #[test]
    fn repository_filter_recognises_arbitrary_target_segments() {
        let start = "repo://example/a/semantic/generated";
        let query = TraverseGraphQuery {
            start: start.into(),
            direction: GraphDirection::Dependencies,
            destination: None,
            target_repositories: vec!["example/a".into()],
            max_hops: 8,
            max_paths: 50,
        };
        let incomplete = BTreeSet::new();
        let boundaries = BTreeSet::new();
        let reachability = BTreeMap::new();
        let mut search = PathSearch {
            query: &query,
            adjacent: BTreeMap::new(),
            incomplete: &incomplete,
            repository_boundaries: &boundaries,
            target_reachability: &reachability,
            paths: Vec::new(),
            reasons: BTreeSet::new(),
            steps: 0,
        };

        assert!(search.can_follow(&[start.into()], "repo://example/a/custom/entity"));
        assert!(!search.can_follow(&[start.into()], "repo://example/b/custom/entity"));
        search.visit(&mut vec![start.into()], &mut Vec::new());

        assert_eq!(search.paths.len(), 1);
        assert_eq!(search.paths[0].nodes, [start]);
        assert_eq!(search.paths[0].termination, PathTermination::Leaf);
    }

    #[test]
    fn entity_search_ranks_canonical_id_then_name_then_prefix() {
        let entities = InspectionResult {
            headers: vec!["id".into(), "kind".into(), "metadata".into()],
            rows: [
                "Run",
                "repo://example/rust/lib/Run",
                "RunLater",
                "repo://example/rust/lib/RunLater",
            ]
            .into_iter()
            .map(|id| {
                vec![
                    InspectionValue::String(id.into()),
                    InspectionValue::String("callable".into()),
                    InspectionValue::String(String::new()),
                ]
            })
            .collect(),
            next: None,
        };

        let result = search_entities("main", "Run", 4, entities).unwrap();

        assert_eq!(
            result
                .matches
                .iter()
                .map(|entity| entity.id.as_str())
                .collect::<Vec<_>>(),
            [
                "Run",
                "repo://example/rust/lib/Run",
                "RunLater",
                "repo://example/rust/lib/RunLater",
            ]
        );
    }

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
