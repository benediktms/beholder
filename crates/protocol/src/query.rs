use crate::v1;
use beholder_dto as dto;

impl From<dto::GraphCommunityKind> for v1::GraphCommunityKind {
    fn from(value: dto::GraphCommunityKind) -> Self {
        match value {
            dto::GraphCommunityKind::Repository => Self::Repository,
            dto::GraphCommunityKind::External => Self::External,
        }
    }
}

fn graph_community_kind(value: i32) -> Result<dto::GraphCommunityKind, &'static str> {
    match v1::GraphCommunityKind::try_from(value).map_err(|_| "unknown graph community kind")? {
        v1::GraphCommunityKind::Repository => Ok(dto::GraphCommunityKind::Repository),
        v1::GraphCommunityKind::External => Ok(dto::GraphCommunityKind::External),
        v1::GraphCommunityKind::Unspecified => Err("graph community kind is missing"),
    }
}

impl From<dto::GraphCommunity> for v1::GraphCommunity {
    fn from(value: dto::GraphCommunity) -> Self {
        Self {
            id: value.id,
            kind: v1::GraphCommunityKind::from(value.kind) as i32,
            name: value.name,
            repository: value.repository,
            entity_count: value.entity_count,
        }
    }
}

impl TryFrom<v1::GraphCommunity> for dto::GraphCommunity {
    type Error = &'static str;

    fn try_from(value: v1::GraphCommunity) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            kind: graph_community_kind(value.kind)?,
            name: value.name,
            repository: value.repository,
            entity_count: value.entity_count,
        })
    }
}

impl From<dto::GraphCommunityEdge> for v1::GraphCommunityEdge {
    fn from(value: dto::GraphCommunityEdge) -> Self {
        Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: v1::RelationKind::from(value.kind) as i32,
            count: value.count,
        }
    }
}

impl TryFrom<v1::GraphCommunityEdge> for dto::GraphCommunityEdge {
    type Error = &'static str;

    fn try_from(value: v1::GraphCommunityEdge) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: crate::entity::relation_kind(value.kind)?,
            count: value.count,
        })
    }
}

impl From<dto::GraphNeighborhoodFocus> for v1::GraphNeighborhoodFocus {
    fn from(value: dto::GraphNeighborhoodFocus) -> Self {
        use v1::graph_neighborhood_focus::Focus;
        Self {
            focus: Some(match value {
                dto::GraphNeighborhoodFocus::Repository(repository) => Focus::Repository(repository),
                dto::GraphNeighborhoodFocus::Entity(entity) => Focus::Entity(entity),
                dto::GraphNeighborhoodFocus::External => Focus::External(true),
            }),
        }
    }
}

impl TryFrom<v1::GraphNeighborhoodFocus> for dto::GraphNeighborhoodFocus {
    type Error = &'static str;

    fn try_from(value: v1::GraphNeighborhoodFocus) -> Result<Self, Self::Error> {
        use v1::graph_neighborhood_focus::Focus;
        match value.focus.ok_or("graph neighborhood focus is missing")? {
            Focus::Repository(repository) if !repository.trim().is_empty() => {
                Ok(Self::Repository(repository))
            }
            Focus::Entity(entity) if !entity.trim().is_empty() => Ok(Self::Entity(entity)),
            Focus::External(true) => Ok(Self::External),
            Focus::Repository(_) => Err("graph neighborhood repository is empty"),
            Focus::Entity(_) => Err("graph neighborhood entity is empty"),
            Focus::External(false) => Err("graph neighborhood external focus is false"),
        }
    }
}

impl From<dto::EntityQuery> for v1::EntityQuery {
    fn from(value: dto::EntityQuery) -> Self {
        Self {
            entity: value.entity,
        }
    }
}

impl From<v1::EntityQuery> for dto::EntityQuery {
    fn from(value: v1::EntityQuery) -> Self {
        Self {
            entity: value.entity,
        }
    }
}

impl From<dto::PathQuery> for v1::SemanticPathQuery {
    fn from(value: dto::PathQuery) -> Self {
        Self {
            from: value.from,
            to: value.to,
        }
    }
}

impl From<v1::SemanticPathQuery> for dto::PathQuery {
    fn from(value: v1::SemanticPathQuery) -> Self {
        Self {
            from: value.from,
            to: value.to,
        }
    }
}

macro_rules! common_into_proto {
    ($value:ident, $response:ident) => {
        v1::$response {
            schema: $value.schema,
            metadata: Some($value.metadata.into()),
            query: Some($value.query.into()),
            nodes: $value.nodes.into_iter().map(Into::into).collect(),
            edges: $value.edges.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    };
}

fn entities(values: Vec<v1::Entity>) -> Result<Vec<dto::EntityRef>, &'static str> {
    values.into_iter().map(TryInto::try_into).collect()
}

fn edges(values: Vec<v1::Edge>) -> Result<Vec<dto::SemanticEdge>, &'static str> {
    values.into_iter().map(TryInto::try_into).collect()
}

impl From<dto::WorkspaceTopology> for v1::GetWorkspaceTopologyResponse {
    fn from(value: dto::WorkspaceTopology) -> Self {
        Self {
            schema: value.schema,
            metadata: Some(value.metadata.into()),
            nodes: value.nodes.into_iter().map(Into::into).collect(),
            edges: value.edges.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::GetWorkspaceTopologyResponse> for dto::WorkspaceTopology {
    type Error = &'static str;

    fn try_from(value: v1::GetWorkspaceTopologyResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("topology metadata is missing")?
                .try_into()?,
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
        })
    }
}

impl From<dto::WorkspaceGraphOverview> for v1::GetWorkspaceGraphOverviewResponse {
    fn from(value: dto::WorkspaceGraphOverview) -> Self {
        Self {
            schema: value.schema,
            metadata: Some(value.metadata.into()),
            communities: value.communities.into_iter().map(Into::into).collect(),
            edges: value.edges.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::GetWorkspaceGraphOverviewResponse> for dto::WorkspaceGraphOverview {
    type Error = &'static str;

    fn try_from(value: v1::GetWorkspaceGraphOverviewResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("graph overview metadata is missing")?
                .try_into()?,
            communities: value
                .communities
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            edges: value
                .edges
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

pub fn workspace_graph_neighborhood_from_batches(
    batches: Vec<v1::StreamWorkspaceGraphNeighborhoodResponse>,
) -> Result<dto::WorkspaceGraphNeighborhood, &'static str> {
    let first = batches.first().ok_or("graph neighborhood stream returned no batches")?;
    if !batches.last().is_some_and(|batch| batch.complete) {
        return Err("graph neighborhood stream ended before completion");
    }
    if batches
        .iter()
        .enumerate()
        .any(|(index, batch)| batch.batch_index != index as u32)
    {
        return Err("graph neighborhood stream has non-contiguous batch indexes");
    }
    let schema = first.schema.clone();
    let metadata = first
        .metadata
        .clone()
        .ok_or("graph neighborhood metadata is missing")?
        .try_into()?;
    let focus = first
        .focus
        .clone()
        .ok_or("graph neighborhood focus is missing")?
        .try_into()?;
    let max_edges = first.max_edges;
    let truncated = first.truncated;
    if batches.iter().any(|batch| {
        batch.schema != schema
            || batch.metadata != first.metadata
            || batch.focus != first.focus
            || batch.max_edges != max_edges
            || batch.truncated != truncated
    }) {
        return Err("graph neighborhood stream metadata changed between batches");
    }
    Ok(dto::WorkspaceGraphNeighborhood {
        schema,
        metadata,
        focus,
        neighborhood: dto::GraphNeighborhoodMetadata {
            max_edges,
            truncated,
        },
        nodes: batches
            .iter()
            .flat_map(|batch| batch.nodes.iter().cloned())
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()?,
        edges: batches
            .into_iter()
            .flat_map(|batch| batch.edges)
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()?,
    })
}

pub fn workspace_graph_neighborhood_batch_from_proto(
    batch: v1::StreamWorkspaceGraphNeighborhoodResponse,
) -> Result<dto::WorkspaceGraphNeighborhoodBatch, &'static str> {
    Ok(dto::WorkspaceGraphNeighborhoodBatch {
        schema: batch.schema,
        metadata: batch
            .metadata
            .ok_or("graph neighborhood metadata is missing")?
            .try_into()?,
        focus: batch
            .focus
            .ok_or("graph neighborhood focus is missing")?
            .try_into()?,
        neighborhood: dto::GraphNeighborhoodMetadata {
            max_edges: batch.max_edges,
            truncated: batch.truncated,
        },
        nodes: entities(batch.nodes)?,
        edges: edges(batch.edges)?,
        batch_index: batch.batch_index,
        complete: batch.complete,
    })
}

impl From<dto::ContextResult> for v1::ContextResponse {
    fn from(value: dto::ContextResult) -> Self {
        let root = value.root.clone();
        let mut response = common_into_proto!(value, ContextResponse);
        response.root = Some(root.into());
        response
    }
}

impl TryFrom<v1::ContextResponse> for dto::ContextResult {
    type Error = &'static str;

    fn try_from(value: v1::ContextResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("context metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("context query is missing")?.into(),
            root: value.root.ok_or("context root is missing")?.try_into()?,
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
        })
    }
}

impl From<dto::DependenciesResult> for v1::DependenciesResponse {
    fn from(value: dto::DependenciesResult) -> Self {
        let root = value.root.clone();
        let traversal = value.traversal.clone();
        let dependencies = value
            .dependencies
            .iter()
            .map(|item| v1::Dependency {
                entity: item.entity.clone(),
                hops: item.hops,
            })
            .collect();
        let mut response = common_into_proto!(value, DependenciesResponse);
        response.root = Some(root.into());
        response.dependencies = dependencies;
        response.traversal = Some(traversal.into());
        response
    }
}

impl TryFrom<v1::DependenciesResponse> for dto::DependenciesResult {
    type Error = &'static str;

    fn try_from(value: v1::DependenciesResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("dependencies metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("dependencies query is missing")?.into(),
            traversal: value
                .traversal
                .ok_or("dependencies traversal metadata is missing")?
                .into(),
            root: value
                .root
                .ok_or("dependencies root is missing")?
                .try_into()?,
            dependencies: value
                .dependencies
                .into_iter()
                .map(|item| dto::DependencyRef {
                    entity: item.entity,
                    hops: item.hops,
                })
                .collect(),
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
        })
    }
}

impl From<dto::ImpactResult> for v1::ImpactResponse {
    fn from(value: dto::ImpactResult) -> Self {
        let root = value.root.clone();
        let traversal = value.traversal.clone();
        let affected = value
            .affected
            .iter()
            .map(|item| v1::Impact {
                entity: item.entity.clone(),
                hops: item.hops,
            })
            .collect();
        let mut response = common_into_proto!(value, ImpactResponse);
        response.root = Some(root.into());
        response.affected = affected;
        response.traversal = Some(traversal.into());
        response
    }
}

impl TryFrom<v1::ImpactResponse> for dto::ImpactResult {
    type Error = &'static str;

    fn try_from(value: v1::ImpactResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("impact metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("impact query is missing")?.into(),
            traversal: value
                .traversal
                .ok_or("impact traversal metadata is missing")?
                .into(),
            root: value.root.ok_or("impact root is missing")?.try_into()?,
            affected: value
                .affected
                .into_iter()
                .map(|item| dto::ImpactRef {
                    entity: item.entity,
                    hops: item.hops,
                })
                .collect(),
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
        })
    }
}

impl From<dto::TraceResult> for v1::TraceResponse {
    fn from(value: dto::TraceResult) -> Self {
        let traversal = value.traversal.clone();
        let paths = value.paths.iter().cloned().map(Into::into).collect();
        let mut response = common_into_proto!(value, TraceResponse);
        response.paths = paths;
        response.traversal = Some(traversal.into());
        response
    }
}

impl TryFrom<v1::TraceResponse> for dto::TraceResult {
    type Error = &'static str;

    fn try_from(value: v1::TraceResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("trace metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("trace query is missing")?.into(),
            traversal: value
                .traversal
                .ok_or("trace traversal metadata is missing")?
                .into(),
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
            paths: value.paths.into_iter().map(Into::into).collect(),
        })
    }
}

impl From<dto::WhyResult> for v1::WhyResponse {
    fn from(value: dto::WhyResult) -> Self {
        let traversal = value.traversal.clone();
        let paths = value.paths.iter().cloned().map(Into::into).collect();
        let mut response = common_into_proto!(value, WhyResponse);
        response.paths = paths;
        response.traversal = Some(traversal.into());
        response
    }
}

impl TryFrom<v1::WhyResponse> for dto::WhyResult {
    type Error = &'static str;

    fn try_from(value: v1::WhyResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("why metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("why query is missing")?.into(),
            traversal: value
                .traversal
                .ok_or("why traversal metadata is missing")?
                .into(),
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
            paths: value.paths.into_iter().map(Into::into).collect(),
        })
    }
}
