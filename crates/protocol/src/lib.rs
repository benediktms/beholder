pub mod beholder {
    pub mod v1 {
        tonic::include_proto!("beholder.v1");
    }

    pub mod worker {
        pub mod v1 {
            tonic::include_proto!("beholder.worker.v1");
        }
    }
}

pub use beholder::v1;
pub use beholder::worker::v1 as worker_v1;

pub const ERROR_CODE_METADATA_KEY: &str = "beholder-error-code";

mod entity;
mod query;
mod worker;
mod workspace;

pub use worker::{
    WorkspaceSnapshotBuilder, analyze_events, analyze_requests, contribution_from_events,
    descriptor_from_wire, descriptor_to_wire, workspace_snapshot,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::relation_kind;
    use beholder_domain::{
        LogicalRepository, ProtobufDescriptorSource, Workspace as DomainWorkspace,
        WorkspaceRepository as DomainRepository,
    };
    use beholder_dto as dto;

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
    fn repository_status_round_trips_latest_revision() {
        let status = dto::RepositoryStatus {
            identity: "github.com/company/repo".into(),
            display_name: "repo".into(),
            base: "/code/repo".into(),
            alternatives: vec!["/code/repo-agent".into()],
            revision: Some(dto::RepositoryRevision {
                source_state: "source".into(),
                head: Some("head".into()),
                analysis_identity: "analysis".into(),
                analysis: dto::AnalysisMetadata {
                    completeness: dto::AnalysisCompleteness::Incomplete,
                    diagnostics: vec![dto::AnalysisDiagnostic {
                        code: "syntax.recovered".into(),
                        severity: dto::AnalysisDiagnosticSeverity::Warning,
                        repository: "github.com/company/repo".into(),
                        path: "src/lib.ts".into(),
                        line: Some(3),
                        detail: Some("recovered".into()),
                    }],
                },
            }),
            indexing: true,
        };

        let protocol = v1::RepositoryStatus::from(status.clone());

        assert_eq!(dto::RepositoryStatus::try_from(protocol).unwrap(), status);
    }

    #[test]
    fn typed_trace_round_trips_without_generic_rows() {
        let mut trace = dto::TraceResult {
            schema: dto::TRACE_SCHEMA_V2.into(),
            metadata: dto::QueryMetadata::completed("main", 3),
            query: dto::PathQuery {
                from: "a".into(),
                to: "b".into(),
            },
            traversal: dto::TraversalMetadata {
                max_hops: 7,
                truncated: true,
            },
            nodes: vec![
                dto::EntityRef {
                    id: "a".into(),
                    kind: dto::EntityKind::Callable,
                    name: "a".into(),
                    repository: None,
                    origin: dto::EntityOrigin::Source,
                    test: false,
                    metadata: Some(dto::EntityMetadata::ProtoMethod {
                        cardinality: dto::RpcCardinality::Unary,
                    }),
                },
                dto::EntityRef {
                    id: "graphql-type://OrderInput".into(),
                    kind: dto::EntityKind::GraphqlType,
                    name: "OrderInput".into(),
                    repository: None,
                    origin: dto::EntityOrigin::Source,
                    test: false,
                    metadata: Some(dto::EntityMetadata::GraphqlType {
                        type_kind: dto::GraphqlTypeKind::Input,
                    }),
                },
                dto::EntityRef {
                    id: "graphql-operation://CreateOrder".into(),
                    kind: dto::EntityKind::GraphqlOperation,
                    name: "CreateOrder".into(),
                    repository: None,
                    origin: dto::EntityOrigin::Source,
                    test: false,
                    metadata: Some(dto::EntityMetadata::GraphqlOperation {
                        operation_kind: dto::GraphqlOperationKind::Mutation,
                    }),
                },
                dto::EntityRef {
                    id: "graphql-argument://Mutation/createOrder/input".into(),
                    kind: dto::EntityKind::GraphqlArgument,
                    name: "input".into(),
                    repository: None,
                    origin: dto::EntityOrigin::Source,
                    test: false,
                    metadata: None,
                },
                dto::EntityRef {
                    id: "graphql-enum-value://OrderMode/PREVIEW".into(),
                    kind: dto::EntityKind::GraphqlEnumValue,
                    name: "PREVIEW".into(),
                    repository: None,
                    origin: dto::EntityOrigin::Source,
                    test: false,
                    metadata: None,
                },
            ],
            edges: vec![
                dto::SemanticEdge {
                    id: "e1".into(),
                    from: "a".into(),
                    to: "b".into(),
                    kind: dto::RelationKind::BindsContract,
                    confidence: 0.6,
                    evidence: vec![dto::EvidenceRef {
                        source_kind: dto::EvidenceKind::Inference,
                        repository: None,
                        path: Some("src/lib.rs".into()),
                        line: Some(1),
                        detail: Some("unique_name_heuristic".into()),
                    }],
                },
                dto::SemanticEdge {
                    id: "e2".into(),
                    from: "a".into(),
                    to: "graphql-operation://CreateOrder".into(),
                    kind: dto::RelationKind::CallsGraphql,
                    confidence: 1.0,
                    evidence: Vec::new(),
                },
            ],
            paths: Vec::new(),
        };
        trace.metadata.analysis = dto::AnalysisMetadata {
            completeness: dto::AnalysisCompleteness::Incomplete,
            diagnostics: vec![dto::AnalysisDiagnostic {
                code: "typescript.syntax_recovered".into(),
                severity: dto::AnalysisDiagnosticSeverity::Warning,
                repository: "repo".into(),
                path: "src/broken.ts".into(),
                line: Some(7),
                detail: Some("unexpected token".into()),
            }],
        };
        let response = v1::TraceResponse::from(trace.clone());
        assert_eq!(
            response.metadata.as_ref().unwrap().completeness,
            v1::AnalysisCompleteness::Incomplete as i32
        );
        assert_eq!(
            response.edges[0].kind,
            v1::RelationKind::BindsContract as i32
        );
        assert_eq!(
            response.edges[1].kind,
            v1::RelationKind::CallsGraphql as i32
        );
        assert_eq!(
            response.edges[0].evidence[0].source,
            v1::EvidenceKind::Inference as i32
        );
        assert_eq!(dto::TraceResult::try_from(response).unwrap(), trace);
        assert!(relation_kind(v1::RelationKind::Unspecified as i32).is_err());
        let protocol = include_str!("../../../proto/beholder/v1/daemon.proto");
        assert!(!protocol.contains("message QueryResult"));
        assert!(!protocol.contains("repeated string headers"));
    }
}
