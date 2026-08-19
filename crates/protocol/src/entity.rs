use crate::v1;
use beholder_dto as dto;

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

impl From<dto::TraversalMetadata> for v1::TraversalMetadata {
    fn from(value: dto::TraversalMetadata) -> Self {
        Self {
            max_hops: value.max_hops,
            truncated: value.truncated,
        }
    }
}

impl From<v1::TraversalMetadata> for dto::TraversalMetadata {
    fn from(value: v1::TraversalMetadata) -> Self {
        Self {
            max_hops: value.max_hops,
            truncated: value.truncated,
        }
    }
}

impl From<dto::EntityKind> for v1::EntityKind {
    fn from(value: dto::EntityKind) -> Self {
        match value {
            dto::EntityKind::Callable => Self::Callable,
            dto::EntityKind::GraphqlArgument => Self::GraphqlArgument,
            dto::EntityKind::GraphqlEnumValue => Self::GraphqlEnumValue,
            dto::EntityKind::GraphqlField => Self::GraphqlField,
            dto::EntityKind::GraphqlOperation => Self::GraphqlOperation,
            dto::EntityKind::GraphqlType => Self::GraphqlType,
            dto::EntityKind::KafkaTopic => Self::KafkaTopic,
            dto::EntityKind::Namespace => Self::Namespace,
            dto::EntityKind::ProtoEnum => Self::ProtoEnum,
            dto::EntityKind::ProtoField => Self::ProtoField,
            dto::EntityKind::ProtoFile => Self::ProtoFile,
            dto::EntityKind::ProtoMessage => Self::ProtoMessage,
            dto::EntityKind::ProtoService => Self::ProtoService,
            dto::EntityKind::Rpc => Self::Rpc,
            dto::EntityKind::Service => Self::Service,
            dto::EntityKind::UnityPrefab => Self::UnityPrefab,
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
            v1::EntityKind::GraphqlArgument => dto::EntityKind::GraphqlArgument,
            v1::EntityKind::GraphqlEnumValue => dto::EntityKind::GraphqlEnumValue,
            v1::EntityKind::GraphqlField => dto::EntityKind::GraphqlField,
            v1::EntityKind::GraphqlOperation => dto::EntityKind::GraphqlOperation,
            v1::EntityKind::GraphqlType => dto::EntityKind::GraphqlType,
            v1::EntityKind::KafkaTopic => dto::EntityKind::KafkaTopic,
            v1::EntityKind::Namespace => dto::EntityKind::Namespace,
            v1::EntityKind::ProtoEnum => dto::EntityKind::ProtoEnum,
            v1::EntityKind::ProtoField => dto::EntityKind::ProtoField,
            v1::EntityKind::ProtoFile => dto::EntityKind::ProtoFile,
            v1::EntityKind::ProtoMessage => dto::EntityKind::ProtoMessage,
            v1::EntityKind::ProtoService => dto::EntityKind::ProtoService,
            v1::EntityKind::Rpc => dto::EntityKind::Rpc,
            v1::EntityKind::Service => dto::EntityKind::Service,
            v1::EntityKind::UnityPrefab => dto::EntityKind::UnityPrefab,
            v1::EntityKind::Unknown => dto::EntityKind::Unknown,
        },
    )
}

fn entity_metadata(value: v1::EntityMetadata) -> Result<dto::EntityMetadata, &'static str> {
    match value.metadata.ok_or("entity metadata is missing")? {
        v1::entity_metadata::Metadata::GraphqlOperationKind(value) => {
            Ok(dto::EntityMetadata::GraphqlOperation {
                operation_kind: match v1::GraphqlOperationKind::try_from(value)
                    .map_err(|_| "unknown GraphQL operation kind")?
                {
                    v1::GraphqlOperationKind::Mutation => dto::GraphqlOperationKind::Mutation,
                    v1::GraphqlOperationKind::Query => dto::GraphqlOperationKind::Query,
                    v1::GraphqlOperationKind::Subscription => {
                        dto::GraphqlOperationKind::Subscription
                    }
                    v1::GraphqlOperationKind::Unknown => {
                        return Err("GraphQL operation kind is missing");
                    }
                },
            })
        }
        v1::entity_metadata::Metadata::GraphqlTypeKind(value) => {
            Ok(dto::EntityMetadata::GraphqlType {
                type_kind: match v1::GraphqlTypeKind::try_from(value)
                    .map_err(|_| "unknown GraphQL type kind")?
                {
                    v1::GraphqlTypeKind::Enum => dto::GraphqlTypeKind::Enum,
                    v1::GraphqlTypeKind::Input => dto::GraphqlTypeKind::Input,
                    v1::GraphqlTypeKind::Interface => dto::GraphqlTypeKind::Interface,
                    v1::GraphqlTypeKind::Object => dto::GraphqlTypeKind::Object,
                    v1::GraphqlTypeKind::Scalar => dto::GraphqlTypeKind::Scalar,
                    v1::GraphqlTypeKind::Union => dto::GraphqlTypeKind::Union,
                    v1::GraphqlTypeKind::Unknown => return Err("GraphQL type kind is missing"),
                },
            })
        }
        v1::entity_metadata::Metadata::ProtoTypeKind(value) => Ok(dto::EntityMetadata::ProtoType {
            type_kind: match v1::ProtoTypeKind::try_from(value)
                .map_err(|_| "unknown Protobuf type kind")?
            {
                v1::ProtoTypeKind::Enum => dto::ProtoTypeKind::Enum,
                v1::ProtoTypeKind::Message => dto::ProtoTypeKind::Message,
                v1::ProtoTypeKind::Unknown => return Err("Protobuf type kind is missing"),
            },
        }),
        v1::entity_metadata::Metadata::RpcCardinality(value) => {
            Ok(dto::EntityMetadata::ProtoMethod {
                cardinality: match v1::RpcCardinality::try_from(value)
                    .map_err(|_| "unknown RPC cardinality")?
                {
                    v1::RpcCardinality::BidirectionalStreaming => {
                        dto::RpcCardinality::BidirectionalStreaming
                    }
                    v1::RpcCardinality::ClientStreaming => dto::RpcCardinality::ClientStreaming,
                    v1::RpcCardinality::ServerStreaming => dto::RpcCardinality::ServerStreaming,
                    v1::RpcCardinality::Unary => dto::RpcCardinality::Unary,
                    v1::RpcCardinality::Unknown => return Err("RPC cardinality is missing"),
                },
            })
        }
    }
}

fn protocol_entity_metadata(value: dto::EntityMetadata) -> v1::EntityMetadata {
    let metadata = match value {
        dto::EntityMetadata::GraphqlOperation { operation_kind } => {
            v1::entity_metadata::Metadata::GraphqlOperationKind(match operation_kind {
                dto::GraphqlOperationKind::Mutation => v1::GraphqlOperationKind::Mutation as i32,
                dto::GraphqlOperationKind::Query => v1::GraphqlOperationKind::Query as i32,
                dto::GraphqlOperationKind::Subscription => {
                    v1::GraphqlOperationKind::Subscription as i32
                }
            })
        }
        dto::EntityMetadata::GraphqlType { type_kind } => {
            v1::entity_metadata::Metadata::GraphqlTypeKind(match type_kind {
                dto::GraphqlTypeKind::Enum => v1::GraphqlTypeKind::Enum as i32,
                dto::GraphqlTypeKind::Input => v1::GraphqlTypeKind::Input as i32,
                dto::GraphqlTypeKind::Interface => v1::GraphqlTypeKind::Interface as i32,
                dto::GraphqlTypeKind::Object => v1::GraphqlTypeKind::Object as i32,
                dto::GraphqlTypeKind::Scalar => v1::GraphqlTypeKind::Scalar as i32,
                dto::GraphqlTypeKind::Union => v1::GraphqlTypeKind::Union as i32,
            })
        }
        dto::EntityMetadata::ProtoType { type_kind } => {
            v1::entity_metadata::Metadata::ProtoTypeKind(match type_kind {
                dto::ProtoTypeKind::Enum => v1::ProtoTypeKind::Enum as i32,
                dto::ProtoTypeKind::Message => v1::ProtoTypeKind::Message as i32,
            })
        }
        dto::EntityMetadata::ProtoMethod { cardinality } => {
            v1::entity_metadata::Metadata::RpcCardinality(match cardinality {
                dto::RpcCardinality::BidirectionalStreaming => {
                    v1::RpcCardinality::BidirectionalStreaming as i32
                }
                dto::RpcCardinality::ClientStreaming => v1::RpcCardinality::ClientStreaming as i32,
                dto::RpcCardinality::ServerStreaming => v1::RpcCardinality::ServerStreaming as i32,
                dto::RpcCardinality::Unary => v1::RpcCardinality::Unary as i32,
            })
        }
    };
    v1::EntityMetadata {
        metadata: Some(metadata),
    }
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
            metadata: value.metadata.map(protocol_entity_metadata),
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
            metadata: value.metadata.map(entity_metadata).transpose()?,
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
            dto::RelationKind::BindsContract => Self::BindsContract,
            dto::RelationKind::Calls => Self::Calls,
            dto::RelationKind::CallsGraphql => Self::CallsGraphql,
            dto::RelationKind::CallsRpc => Self::CallsRpc,
            dto::RelationKind::ConsumedBy => Self::ConsumedBy,
            dto::RelationKind::Defines => Self::Defines,
            dto::RelationKind::FieldOf => Self::FieldOf,
            dto::RelationKind::Implements => Self::Implements,
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

pub(super) fn relation_kind(value: i32) -> Result<dto::RelationKind, &'static str> {
    match v1::RelationKind::try_from(value).map_err(|_| "unknown relation kind")? {
        v1::RelationKind::BindsContract => Ok(dto::RelationKind::BindsContract),
        v1::RelationKind::Calls => Ok(dto::RelationKind::Calls),
        v1::RelationKind::CallsGraphql => Ok(dto::RelationKind::CallsGraphql),
        v1::RelationKind::CallsRpc => Ok(dto::RelationKind::CallsRpc),
        v1::RelationKind::ConsumedBy => Ok(dto::RelationKind::ConsumedBy),
        v1::RelationKind::Defines => Ok(dto::RelationKind::Defines),
        v1::RelationKind::FieldOf => Ok(dto::RelationKind::FieldOf),
        v1::RelationKind::Implements => Ok(dto::RelationKind::Implements),
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
