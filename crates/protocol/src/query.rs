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
