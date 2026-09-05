use crate::v1;
use beholder_dto as dto;

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

impl From<dto::EntitySearchQuery> for v1::EntitySearchQuery {
    fn from(value: dto::EntitySearchQuery) -> Self {
        Self {
            query: value.query,
            limit: value.limit,
        }
    }
}

impl From<v1::EntitySearchQuery> for dto::EntitySearchQuery {
    fn from(value: v1::EntitySearchQuery) -> Self {
        Self {
            query: value.query,
            limit: value.limit,
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

impl From<dto::EntitySearchResult> for v1::SearchEntitiesResponse {
    fn from(value: dto::EntitySearchResult) -> Self {
        Self {
            schema: value.schema,
            metadata: Some(value.metadata.into()),
            query: Some(value.query.into()),
            matches: value.matches.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::SearchEntitiesResponse> for dto::EntitySearchResult {
    type Error = &'static str;

    fn try_from(value: v1::SearchEntitiesResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("entity search metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("entity search query is missing")?.into(),
            matches: entities(value.matches)?,
        })
    }
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

impl From<dto::GraphDirection> for v1::GraphDirection {
    fn from(value: dto::GraphDirection) -> Self {
        match value {
            dto::GraphDirection::Dependencies => Self::Dependencies,
            dto::GraphDirection::Dependents => Self::Dependents,
        }
    }
}
impl TryFrom<v1::GraphDirection> for dto::GraphDirection {
    type Error = &'static str;
    fn try_from(value: v1::GraphDirection) -> Result<Self, Self::Error> {
        match value {
            v1::GraphDirection::Dependencies => Ok(Self::Dependencies),
            v1::GraphDirection::Dependents => Ok(Self::Dependents),
            _ => Err("invalid GraphDirection"),
        }
    }
}

impl From<dto::PathTermination> for v1::PathTermination {
    fn from(value: dto::PathTermination) -> Self {
        match value {
            dto::PathTermination::Destination => Self::Destination,
            dto::PathTermination::Leaf => Self::Leaf,
            dto::PathTermination::Cycle => Self::Cycle,
            dto::PathTermination::MaxHops => Self::MaxHops,
        }
    }
}
impl TryFrom<v1::PathTermination> for dto::PathTermination {
    type Error = &'static str;
    fn try_from(value: v1::PathTermination) -> Result<Self, Self::Error> {
        match value {
            v1::PathTermination::Destination => Ok(Self::Destination),
            v1::PathTermination::Leaf => Ok(Self::Leaf),
            v1::PathTermination::Cycle => Ok(Self::Cycle),
            v1::PathTermination::MaxHops => Ok(Self::MaxHops),
            _ => Err("invalid PathTermination"),
        }
    }
}

impl From<dto::TruncationReason> for v1::TruncationReason {
    fn from(value: dto::TruncationReason) -> Self {
        match value {
            dto::TruncationReason::MaxHops => Self::MaxHops,
            dto::TruncationReason::MaxPaths => Self::MaxPaths,
            dto::TruncationReason::AcquisitionLimit => Self::AcquisitionLimit,
            dto::TruncationReason::WorkLimit => Self::WorkLimit,
        }
    }
}
impl TryFrom<v1::TruncationReason> for dto::TruncationReason {
    type Error = &'static str;
    fn try_from(value: v1::TruncationReason) -> Result<Self, Self::Error> {
        match value {
            v1::TruncationReason::MaxHops => Ok(Self::MaxHops),
            v1::TruncationReason::MaxPaths => Ok(Self::MaxPaths),
            v1::TruncationReason::AcquisitionLimit => Ok(Self::AcquisitionLimit),
            v1::TruncationReason::WorkLimit => Ok(Self::WorkLimit),
            _ => Err("invalid TruncationReason"),
        }
    }
}

impl TryFrom<v1::TraverseGraphRequest> for dto::TraverseGraphQuery {
    type Error = &'static str;
    fn try_from(value: v1::TraverseGraphRequest) -> Result<Self, Self::Error> {
        v1::TraverseGraphQuery {
            start: value.start,
            direction: value.direction,
            destination: value.destination,
            max_hops: value.max_hops.unwrap_or(dto::DEFAULT_TRAVERSAL_HOPS),
            max_paths: value.max_paths.unwrap_or(dto::DEFAULT_MAX_PATHS),
        }
        .try_into()
    }
}

impl From<dto::TraverseGraphQuery> for v1::TraverseGraphQuery {
    fn from(value: dto::TraverseGraphQuery) -> Self {
        Self {
            start: value.start,
            destination: value.destination,
            direction: v1::GraphDirection::from(value.direction) as i32,
            max_hops: value.max_hops,
            max_paths: value.max_paths,
        }
    }
}
impl TryFrom<v1::TraverseGraphQuery> for dto::TraverseGraphQuery {
    type Error = &'static str;
    fn try_from(value: v1::TraverseGraphQuery) -> Result<Self, Self::Error> {
        let query = Self {
            start: value.start,
            destination: value.destination,
            direction: v1::GraphDirection::try_from(value.direction)
                .map_err(|_| "invalid graph direction")?
                .try_into()?,
            max_hops: value.max_hops,
            max_paths: value.max_paths,
        };
        query.validate()?;
        Ok(query)
    }
}
impl From<dto::TraverseGraphResult> for v1::TraverseGraphResponse {
    fn from(value: dto::TraverseGraphResult) -> Self {
        let traversal = value.traversal;
        let paths = value
            .paths
            .into_iter()
            .map(|path| v1::TraversalPath {
                nodes: path.nodes,
                edges: path.edges,
                termination: v1::PathTermination::from(path.termination) as i32,
            })
            .collect();
        let mut response = common_into_proto!(value, TraverseGraphResponse);
        response.paths = paths;
        response.traversal = Some(v1::GraphTraversalMetadata {
            max_hops: traversal.max_hops,
            max_paths: traversal.max_paths,
            max_rows: traversal.max_rows,
            max_steps: traversal.max_steps,
            acquisition_timeout_ms: traversal.acquisition_timeout_ms,
            truncated: traversal.truncated,
            truncation_reasons: traversal
                .truncation_reasons
                .into_iter()
                .map(|reason| v1::TruncationReason::from(reason) as i32)
                .collect(),
        });
        response
    }
}
impl TryFrom<v1::TraverseGraphResponse> for dto::TraverseGraphResult {
    type Error = &'static str;
    fn try_from(value: v1::TraverseGraphResponse) -> Result<Self, Self::Error> {
        let traversal = value
            .traversal
            .ok_or("graph traversal metadata is missing")?;
        Ok(Self {
            schema: value.schema,
            metadata: value
                .metadata
                .ok_or("query metadata is missing")?
                .try_into()?,
            query: value.query.ok_or("graph query is missing")?.try_into()?,
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
            paths: value
                .paths
                .into_iter()
                .map(|path| {
                    Ok(dto::TraversalPath {
                        nodes: path.nodes,
                        edges: path.edges,
                        termination: v1::PathTermination::try_from(path.termination)
                            .map_err(|_| "invalid path termination")?
                            .try_into()?,
                    })
                })
                .collect::<Result<_, Self::Error>>()?,
            traversal: dto::GraphTraversalMetadata {
                max_hops: traversal.max_hops,
                max_paths: traversal.max_paths,
                max_rows: traversal.max_rows,
                max_steps: traversal.max_steps,
                acquisition_timeout_ms: traversal.acquisition_timeout_ms,
                truncated: traversal.truncated,
                truncation_reasons: traversal
                    .truncation_reasons
                    .into_iter()
                    .map(|reason| {
                        v1::TruncationReason::try_from(reason)
                            .map_err(|_| "invalid truncation reason")?
                            .try_into()
                    })
                    .collect::<Result<_, _>>()?,
            },
        })
    }
}

#[cfg(test)]
mod multipath_tests {
    use super::*;

    #[test]
    fn request_defaults_are_distinct_from_legacy_traversal_limits() {
        let mut request = v1::TraverseGraphRequest {
            start: "root".into(),
            direction: v1::GraphDirection::Dependencies as i32,
            ..Default::default()
        };
        let query = dto::TraverseGraphQuery::try_from(request.clone()).unwrap();
        assert_eq!((query.max_hops, query.max_paths), (8, 50));
        request.max_hops = Some(0);
        request.max_paths = Some(200);
        let query = dto::TraverseGraphQuery::try_from(request.clone()).unwrap();
        assert_eq!((query.max_hops, query.max_paths), (0, 200));
        request.max_hops = Some(33);
        assert!(dto::TraverseGraphQuery::try_from(request).is_err());
    }

    #[test]
    fn graph_query_round_trips_and_rejects_invalid_values() {
        let query = dto::TraverseGraphQuery {
            start: "repo://example/root".into(),
            direction: dto::GraphDirection::Dependents,
            destination: Some("repo://example/end".into()),
            max_hops: 32,
            max_paths: 200,
        };
        let result = dto::TraverseGraphResult {
            schema: dto::TRAVERSE_GRAPH_SCHEMA_V1.into(),
            metadata: dto::QueryMetadata::completed("main", 42),
            query: query.clone(),
            nodes: Vec::new(),
            edges: Vec::new(),
            paths: vec![dto::TraversalPath {
                nodes: vec![query.start.clone()],
                edges: Vec::new(),
                termination: dto::PathTermination::Cycle,
            }],
            traversal: dto::GraphTraversalMetadata {
                max_hops: 32,
                max_paths: 200,
                max_rows: dto::MAX_TRAVERSAL_ROWS,
                max_steps: dto::MAX_TRAVERSAL_STEPS,
                acquisition_timeout_ms: dto::TRAVERSAL_ACQUISITION_TIMEOUT_MS,
                truncated: true,
                truncation_reasons: vec![
                    dto::TruncationReason::MaxHops,
                    dto::TruncationReason::MaxPaths,
                    dto::TruncationReason::AcquisitionLimit,
                    dto::TruncationReason::WorkLimit,
                ],
            },
        };
        let wire = v1::TraverseGraphResponse::from(result.clone());
        assert_eq!(
            dto::TraverseGraphResult::try_from(wire.clone()).unwrap(),
            result
        );
        let mut invalid = wire.clone();
        invalid.paths[0].termination = 99;
        assert!(dto::TraverseGraphResult::try_from(invalid).is_err());
        let mut invalid = wire;
        invalid
            .traversal
            .as_mut()
            .unwrap()
            .truncation_reasons
            .push(0);
        assert!(dto::TraverseGraphResult::try_from(invalid).is_err());
        for direction in [0, 99] {
            let mut wire = v1::TraverseGraphQuery::from(query.clone());
            wire.direction = direction;
            assert!(dto::TraverseGraphQuery::try_from(wire).is_err());
        }
        for (hops, paths) in [(33, 50), (8, 0), (8, 201)] {
            let mut invalid = query.clone();
            invalid.max_hops = hops;
            invalid.max_paths = paths;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = query;
        invalid.destination = Some("  ".into());
        assert!(invalid.validate().is_err());
    }
}
