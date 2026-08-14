pub mod v1 {
    tonic::include_proto!("beholder.v1");
}

use beholder_domain::{
    LogicalRepository, ProtobufDescriptorSource, Workspace as DomainWorkspace,
    WorkspaceRepository as DomainRepository,
};
use beholder_dto as dto;
use std::path::PathBuf;

impl From<DomainWorkspace> for v1::Workspace {
    fn from(workspace: DomainWorkspace) -> Self {
        Self {
            name: workspace.name,
            repositories: workspace
                .repositories
                .into_iter()
                .map(|repository| v1::WorkspaceRepository {
                    identity: repository.repository.identity,
                    display_name: repository.display_name,
                    base: repository.base.to_string_lossy().into_owned(),
                    alternatives: repository
                        .alternatives
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect(),
                })
                .collect(),
            protobuf_descriptors: workspace
                .protobuf_descriptors
                .into_iter()
                .map(|descriptor| v1::ProtobufDescriptorSource {
                    repository: descriptor.repository.identity,
                    path: descriptor.path.to_string_lossy().into_owned(),
                })
                .collect(),
        }
    }
}

impl TryFrom<v1::Workspace> for DomainWorkspace {
    type Error = String;

    fn try_from(workspace: v1::Workspace) -> Result<Self, Self::Error> {
        Self::new(
            workspace.name,
            workspace
                .repositories
                .into_iter()
                .map(|repository| DomainRepository {
                    repository: LogicalRepository {
                        identity: repository.identity,
                    },
                    display_name: repository.display_name,
                    base: PathBuf::from(repository.base),
                    alternatives: repository
                        .alternatives
                        .into_iter()
                        .map(PathBuf::from)
                        .collect(),
                })
                .collect(),
        )?
        .with_protobuf_descriptors(
            workspace
                .protobuf_descriptors
                .into_iter()
                .map(|descriptor| ProtobufDescriptorSource {
                    repository: LogicalRepository {
                        identity: descriptor.repository,
                    },
                    path: PathBuf::from(descriptor.path),
                })
                .collect(),
        )
    }
}

impl From<dto::Freshness> for v1::Freshness {
    fn from(value: dto::Freshness) -> Self {
        Self {
            stale: value.stale,
            indexing: value.indexing,
            dirty_repositories: value.dirty_repositories,
        }
    }
}

impl From<v1::Freshness> for dto::Freshness {
    fn from(value: v1::Freshness) -> Self {
        Self {
            stale: value.stale,
            indexing: value.indexing,
            dirty_repositories: value.dirty_repositories,
        }
    }
}

impl From<dto::QueryMetadata> for v1::QueryMetadata {
    fn from(value: dto::QueryMetadata) -> Self {
        Self {
            revision: value.revision,
            view: value.view,
            freshness: Some(value.freshness.into()),
        }
    }
}

impl TryFrom<v1::QueryMetadata> for dto::QueryMetadata {
    type Error = &'static str;

    fn try_from(value: v1::QueryMetadata) -> Result<Self, Self::Error> {
        Ok(Self {
            revision: value.revision,
            view: value.view,
            freshness: value.freshness.ok_or("query freshness is missing")?.into(),
        })
    }
}

impl From<dto::EntityKind> for v1::EntityKind {
    fn from(value: dto::EntityKind) -> Self {
        match value {
            dto::EntityKind::Callable => Self::Callable,
            dto::EntityKind::GraphqlField => Self::GraphqlField,
            dto::EntityKind::KafkaTopic => Self::KafkaTopic,
            dto::EntityKind::Namespace => Self::Namespace,
            dto::EntityKind::ProtoEnum => Self::ProtoEnum,
            dto::EntityKind::ProtoField => Self::ProtoField,
            dto::EntityKind::ProtoFile => Self::ProtoFile,
            dto::EntityKind::ProtoMessage => Self::ProtoMessage,
            dto::EntityKind::ProtoService => Self::ProtoService,
            dto::EntityKind::Rpc => Self::Rpc,
            dto::EntityKind::Service => Self::Service,
            dto::EntityKind::Unknown => Self::Unknown,
        }
    }
}

impl From<dto::EntityOrigin> for v1::EntityOrigin {
    fn from(value: dto::EntityOrigin) -> Self {
        match value {
            dto::EntityOrigin::Source => Self::Source,
            dto::EntityOrigin::Generated => Self::Generated,
            dto::EntityOrigin::ExternalDependency => Self::ExternalDependency,
        }
    }
}

fn entity_origin(value: i32) -> Result<dto::EntityOrigin, &'static str> {
    match v1::EntityOrigin::try_from(value).map_err(|_| "unknown entity origin")? {
        v1::EntityOrigin::Source => Ok(dto::EntityOrigin::Source),
        v1::EntityOrigin::Generated => Ok(dto::EntityOrigin::Generated),
        v1::EntityOrigin::ExternalDependency => Ok(dto::EntityOrigin::ExternalDependency),
    }
}

fn entity_kind(value: i32) -> Result<dto::EntityKind, &'static str> {
    Ok(
        match v1::EntityKind::try_from(value).map_err(|_| "unknown entity kind")? {
            v1::EntityKind::Callable => dto::EntityKind::Callable,
            v1::EntityKind::GraphqlField => dto::EntityKind::GraphqlField,
            v1::EntityKind::KafkaTopic => dto::EntityKind::KafkaTopic,
            v1::EntityKind::Namespace => dto::EntityKind::Namespace,
            v1::EntityKind::ProtoEnum => dto::EntityKind::ProtoEnum,
            v1::EntityKind::ProtoField => dto::EntityKind::ProtoField,
            v1::EntityKind::ProtoFile => dto::EntityKind::ProtoFile,
            v1::EntityKind::ProtoMessage => dto::EntityKind::ProtoMessage,
            v1::EntityKind::ProtoService => dto::EntityKind::ProtoService,
            v1::EntityKind::Rpc => dto::EntityKind::Rpc,
            v1::EntityKind::Service => dto::EntityKind::Service,
            v1::EntityKind::Unknown => dto::EntityKind::Unknown,
        },
    )
}

impl From<dto::EntityRef> for v1::Entity {
    fn from(value: dto::EntityRef) -> Self {
        Self {
            id: value.id,
            kind: v1::EntityKind::from(value.kind) as i32,
            name: value.name,
            repository: value.repository,
            origin: v1::EntityOrigin::from(value.origin) as i32,
            test: value.test,
        }
    }
}

impl TryFrom<v1::Entity> for dto::EntityRef {
    type Error = &'static str;

    fn try_from(value: v1::Entity) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            kind: entity_kind(value.kind)?,
            name: value.name,
            repository: value.repository,
            origin: entity_origin(value.origin)?,
            test: value.test,
        })
    }
}

impl From<dto::EvidenceKind> for v1::EvidenceKind {
    fn from(value: dto::EvidenceKind) -> Self {
        match value {
            dto::EvidenceKind::Ast => Self::Ast,
            dto::EvidenceKind::Configuration => Self::Configuration,
            dto::EvidenceKind::Descriptor => Self::Descriptor,
            dto::EvidenceKind::Generated => Self::Generated,
            dto::EvidenceKind::Inference => Self::Inference,
            dto::EvidenceKind::Unknown => Self::Unknown,
        }
    }
}

fn evidence_kind(value: i32) -> Result<dto::EvidenceKind, &'static str> {
    Ok(
        match v1::EvidenceKind::try_from(value).map_err(|_| "unknown evidence kind")? {
            v1::EvidenceKind::Ast => dto::EvidenceKind::Ast,
            v1::EvidenceKind::Configuration => dto::EvidenceKind::Configuration,
            v1::EvidenceKind::Descriptor => dto::EvidenceKind::Descriptor,
            v1::EvidenceKind::Generated => dto::EvidenceKind::Generated,
            v1::EvidenceKind::Inference => dto::EvidenceKind::Inference,
            v1::EvidenceKind::Unknown => dto::EvidenceKind::Unknown,
        },
    )
}

impl From<dto::EvidenceRef> for v1::Evidence {
    fn from(value: dto::EvidenceRef) -> Self {
        Self {
            source: v1::EvidenceKind::from(value.source_kind) as i32,
            repository: value.repository,
            path: value.path,
            line: value.line,
            detail: value.detail,
        }
    }
}

impl TryFrom<v1::Evidence> for dto::EvidenceRef {
    type Error = &'static str;

    fn try_from(value: v1::Evidence) -> Result<Self, Self::Error> {
        Ok(Self {
            source_kind: evidence_kind(value.source)?,
            repository: value.repository,
            path: value.path,
            line: value.line,
            detail: value.detail,
        })
    }
}

impl From<dto::RelationKind> for v1::RelationKind {
    fn from(value: dto::RelationKind) -> Self {
        match value {
            dto::RelationKind::Calls => Self::Calls,
            dto::RelationKind::CallsRpc => Self::CallsRpc,
            dto::RelationKind::ConsumedBy => Self::ConsumedBy,
            dto::RelationKind::Defines => Self::Defines,
            dto::RelationKind::FieldOf => Self::FieldOf,
            dto::RelationKind::ImplementedBy => Self::ImplementedBy,
            dto::RelationKind::Imports => Self::Imports,
            dto::RelationKind::Publishes => Self::Publishes,
            dto::RelationKind::Requires => Self::Requires,
            dto::RelationKind::RequestType => Self::RequestType,
            dto::RelationKind::ResolvedBy => Self::ResolvedBy,
            dto::RelationKind::Selects => Self::Selects,
            dto::RelationKind::ResponseType => Self::ResponseType,
            dto::RelationKind::Uses => Self::Uses,
        }
    }
}

fn relation_kind(value: i32) -> Result<dto::RelationKind, &'static str> {
    match v1::RelationKind::try_from(value).map_err(|_| "unknown relation kind")? {
        v1::RelationKind::Calls => Ok(dto::RelationKind::Calls),
        v1::RelationKind::CallsRpc => Ok(dto::RelationKind::CallsRpc),
        v1::RelationKind::ConsumedBy => Ok(dto::RelationKind::ConsumedBy),
        v1::RelationKind::Defines => Ok(dto::RelationKind::Defines),
        v1::RelationKind::FieldOf => Ok(dto::RelationKind::FieldOf),
        v1::RelationKind::ImplementedBy => Ok(dto::RelationKind::ImplementedBy),
        v1::RelationKind::Imports => Ok(dto::RelationKind::Imports),
        v1::RelationKind::Publishes => Ok(dto::RelationKind::Publishes),
        v1::RelationKind::Requires => Ok(dto::RelationKind::Requires),
        v1::RelationKind::RequestType => Ok(dto::RelationKind::RequestType),
        v1::RelationKind::ResolvedBy => Ok(dto::RelationKind::ResolvedBy),
        v1::RelationKind::Selects => Ok(dto::RelationKind::Selects),
        v1::RelationKind::ResponseType => Ok(dto::RelationKind::ResponseType),
        v1::RelationKind::Uses => Ok(dto::RelationKind::Uses),
        v1::RelationKind::Unknown => Err("relation kind is missing"),
    }
}

impl From<dto::SemanticEdge> for v1::Edge {
    fn from(value: dto::SemanticEdge) -> Self {
        Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: v1::RelationKind::from(value.kind) as i32,
            confidence: value.confidence,
            evidence: value.evidence.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::Edge> for dto::SemanticEdge {
    type Error = &'static str;

    fn try_from(value: v1::Edge) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: relation_kind(value.kind)?,
            confidence: value.confidence,
            evidence: value
                .evidence
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl From<dto::SemanticPath> for v1::SemanticPath {
    fn from(value: dto::SemanticPath) -> Self {
        Self {
            nodes: value.nodes,
            edges: value.edges,
        }
    }
}

impl From<v1::SemanticPath> for dto::SemanticPath {
    fn from(value: v1::SemanticPath) -> Self {
        Self {
            nodes: value.nodes,
            edges: value.edges,
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
        let paths = value.paths.iter().cloned().map(Into::into).collect();
        let mut response = common_into_proto!(value, TraceResponse);
        response.paths = paths;
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
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
            paths: value.paths.into_iter().map(Into::into).collect(),
        })
    }
}

impl From<dto::WhyResult> for v1::WhyResponse {
    fn from(value: dto::WhyResult) -> Self {
        let paths = value.paths.iter().cloned().map(Into::into).collect();
        let mut response = common_into_proto!(value, WhyResponse);
        response.paths = paths;
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
            nodes: entities(value.nodes)?,
            edges: edges(value.edges)?,
            paths: value.paths.into_iter().map(Into::into).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_round_trips_logical_repository_selection() {
        let workspace = DomainWorkspace::new(
            "main",
            vec![DomainRepository {
                repository: LogicalRepository {
                    identity: "github.com/company/repo".into(),
                },
                display_name: "repo".into(),
                base: "/code/repo".into(),
                alternatives: vec!["/code/repo-agent".into()],
            }],
        )
        .unwrap()
        .with_protobuf_descriptors(vec![ProtobufDescriptorSource {
            repository: LogicalRepository {
                identity: "github.com/company/repo".into(),
            },
            path: "/code/repo/contracts.bin".into(),
        }])
        .unwrap();

        let protocol = v1::Workspace::from(workspace.clone());
        assert_eq!(protocol.repositories[0].identity, "github.com/company/repo");
        assert_eq!(protocol.repositories[0].base, "/code/repo");
        assert_eq!(protocol.repositories[0].alternatives, ["/code/repo-agent"]);
        assert_eq!(
            protocol.protobuf_descriptors[0].path,
            "/code/repo/contracts.bin"
        );
        assert_eq!(DomainWorkspace::try_from(protocol).unwrap(), workspace);
    }

    #[test]
    fn typed_trace_round_trips_without_generic_rows() {
        let trace = dto::TraceResult {
            schema: dto::TRACE_SCHEMA_V1.into(),
            metadata: dto::QueryMetadata::completed("main", 3),
            query: dto::PathQuery {
                from: "a".into(),
                to: "b".into(),
            },
            nodes: vec![dto::EntityRef {
                id: "a".into(),
                kind: dto::EntityKind::Callable,
                name: "a".into(),
                repository: None,
                origin: dto::EntityOrigin::Source,
                test: false,
            }],
            edges: vec![dto::SemanticEdge {
                id: "e1".into(),
                from: "a".into(),
                to: "b".into(),
                kind: dto::RelationKind::Requires,
                confidence: 0.6,
                evidence: vec![dto::EvidenceRef {
                    source_kind: dto::EvidenceKind::Inference,
                    repository: None,
                    path: Some("src/lib.rs".into()),
                    line: Some(1),
                    detail: Some("unique_name_heuristic".into()),
                }],
            }],
            paths: Vec::new(),
        };
        let response = v1::TraceResponse::from(trace.clone());
        assert_eq!(response.edges[0].kind, v1::RelationKind::Requires as i32);
        assert_eq!(
            response.edges[0].evidence[0].source,
            v1::EvidenceKind::Inference as i32
        );
        assert_eq!(dto::TraceResult::try_from(response).unwrap(), trace);
        assert!(relation_kind(v1::RelationKind::Unknown as i32).is_err());
        let protocol = include_str!("../../../proto/beholder/v1/daemon.proto");
        assert!(!protocol.contains("message QueryResult"));
        assert!(!protocol.contains("repeated string headers"));
    }
}
