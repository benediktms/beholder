use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONTEXT_SCHEMA_V1: &str = "beholder.context.v1";
pub const DEPENDENCIES_SCHEMA_V2: &str = "beholder.dependencies.v2";
pub const IMPACT_SCHEMA_V2: &str = "beholder.impact.v2";
pub const TRACE_SCHEMA_V2: &str = "beholder.trace.v2";
pub const WHY_SCHEMA_V2: &str = "beholder.why.v2";
pub const WORKSPACE_TOPOLOGY_SCHEMA_V1: &str = "beholder.workspace_topology.v1";
pub const ENTITY_SEARCH_SCHEMA_V2: &str = "beholder.entity_search.v2";
pub const DEFAULT_MAX_HOPS: u32 = 32;
pub const DEFAULT_ENTITY_SEARCH_LIMIT: u32 = 20;
pub const MAX_ENTITY_SEARCH_LIMIT: u32 = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GarbageCollection {
    pub repository_states_queued: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GarbageCollectionStatus {
    pub running: bool,
    pub repository_states_collectible: u64,
    pub repository_states_queued: u64,
    pub reclaimable_database_pages: u64,
    pub progress: Option<GarbageCollectionProgress>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GarbageCollectionEvent {
    Progress(GarbageCollectionProgress),
    Completed(GarbageCollection),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GarbageCollectionProgress {
    pub phase: GarbageCollectionPhase,
    pub step: Option<String>,
    pub rows: Option<u64>,
    pub completed_rows: Option<u64>,
    pub stale_states: Option<u32>,
    pub repositories: Option<u32>,
    pub completed_steps: u32,
    pub total_steps: u32,
}

impl GarbageCollectionProgress {
    pub fn phase(phase: GarbageCollectionPhase) -> Self {
        Self {
            phase,
            step: None,
            rows: None,
            completed_rows: None,
            stale_states: None,
            repositories: None,
            completed_steps: 0,
            total_steps: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GarbageCollectionPhase {
    ClaimingObsoleteStates,
    SweepingObsoleteStates,
    CheckpointingDatabase,
    ReclaimingDatabaseSpace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Freshness {
    pub stale: bool,
    pub indexing: bool,
    pub dirty_repositories: Vec<String>,
    #[serde(default)]
    pub enriching_repositories: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisCompleteness {
    #[default]
    Complete,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticSeverity {
    KnownLimitation,
    Warning,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AnalysisDiagnostic {
    pub code: String,
    pub severity: AnalysisDiagnosticSeverity,
    pub repository: String,
    pub path: PathBuf,
    pub line: Option<u32>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisMetadata {
    pub completeness: AnalysisCompleteness,
    #[serde(default)]
    pub diagnostic_counts: DiagnosticCounts,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticCounts {
    pub total: u64,
    pub known_limitations: u64,
    pub warnings: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryRevision {
    pub source_state: String,
    pub head: Option<String>,
    pub analysis_identity: String,
    pub analysis: AnalysisMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryStatus {
    pub identity: String,
    pub display_name: String,
    pub base: PathBuf,
    pub alternatives: Vec<PathBuf>,
    pub revision: Option<RepositoryRevision>,
    pub indexing: bool,
}

impl AnalysisMetadata {
    fn is_complete(&self) -> bool {
        self.completeness == AnalysisCompleteness::Complete
            && self.diagnostic_counts.total == 0
            && self.diagnostics.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueryMetadata {
    pub revision: u64,
    pub view: String,
    pub freshness: Freshness,
    #[serde(default, skip_serializing_if = "AnalysisMetadata::is_complete")]
    pub analysis: AnalysisMetadata,
}

fn serialize_metadata_with_counts<S: serde::Serializer>(
    metadata: &QueryMetadata,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let mut value = serializer.serialize_struct("QueryMetadata", 4)?;
    value.serialize_field("revision", &metadata.revision)?;
    value.serialize_field("view", &metadata.view)?;
    value.serialize_field("freshness", &metadata.freshness)?;
    value.serialize_field("analysis", &metadata.analysis)?;
    value.end()
}

impl QueryMetadata {
    pub fn completed(view: impl Into<String>, revision: u64) -> Self {
        Self {
            revision,
            view: view.into(),
            freshness: Freshness {
                stale: false,
                indexing: false,
                dirty_repositories: Vec::new(),
                enriching_repositories: Vec::new(),
            },
            analysis: AnalysisMetadata::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TraversalMetadata {
    pub max_hops: u32,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Callable,
    GraphqlArgument,
    GraphqlEnumValue,
    GraphqlField,
    GraphqlOperation,
    GraphqlType,
    KafkaTopic,
    Namespace,
    ProtoEnum,
    ProtoField,
    ProtoFile,
    ProtoMessage,
    ProtoService,
    Rpc,
    Service,
    UnityPrefab,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityOrigin {
    Source,
    Generated,
    ExternalDependency,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoTypeKind {
    Enum,
    Message,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcCardinality {
    BidirectionalStreaming,
    ClientStreaming,
    ServerStreaming,
    Unary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EntityMetadata {
    GraphqlOperation {
        operation_kind: GraphqlOperationKind,
    },
    GraphqlType {
        type_kind: GraphqlTypeKind,
    },
    ProtoMethod {
        cardinality: RpcCardinality,
    },
    ProtoType {
        type_kind: ProtoTypeKind,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlOperationKind {
    Mutation,
    Query,
    Subscription,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlTypeKind {
    Enum,
    Input,
    Interface,
    Object,
    Scalar,
    Union,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntityRef {
    pub id: String,
    pub kind: EntityKind,
    pub name: String,
    pub repository: Option<String>,
    pub origin: EntityOrigin,
    pub test: bool,
    pub metadata: Option<EntityMetadata>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    BindsContract,
    Calls,
    CallsGraphql,
    CallsRpc,
    ConsumedBy,
    Defines,
    FieldOf,
    Implements,
    ImplementedBy,
    Imports,
    Publishes,
    Requires,
    RequestType,
    ResolvedBy,
    Selects,
    ResponseType,
    Uses,
}

impl RelationKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::BindsContract => "binds_contract",
            Self::Calls => "calls",
            Self::CallsGraphql => "calls_graphql",
            Self::CallsRpc => "calls_rpc",
            Self::ConsumedBy => "consumed_by",
            Self::Defines => "defines",
            Self::FieldOf => "field_of",
            Self::Implements => "implements",
            Self::ImplementedBy => "implemented_by",
            Self::Imports => "imports",
            Self::Publishes => "publishes",
            Self::Requires => "requires",
            Self::RequestType => "request_type",
            Self::ResolvedBy => "resolved_by",
            Self::Selects => "selects",
            Self::ResponseType => "response_type",
            Self::Uses => "uses",
        }
    }
}

impl TryFrom<&str> for RelationKind {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "binds_contract" => Ok(Self::BindsContract),
            "calls" => Ok(Self::Calls),
            "calls_graphql" => Ok(Self::CallsGraphql),
            "calls_rpc" => Ok(Self::CallsRpc),
            "consumed_by" => Ok(Self::ConsumedBy),
            "defines" => Ok(Self::Defines),
            "field_of" => Ok(Self::FieldOf),
            "implements" => Ok(Self::Implements),
            "implemented_by" => Ok(Self::ImplementedBy),
            "imports" => Ok(Self::Imports),
            "publishes" => Ok(Self::Publishes),
            "requires" => Ok(Self::Requires),
            "request_type" => Ok(Self::RequestType),
            "resolved_by" => Ok(Self::ResolvedBy),
            "selects" => Ok(Self::Selects),
            "response_type" => Ok(Self::ResponseType),
            "uses" => Ok(Self::Uses),
            value => Err(format!("unsupported semantic relation: {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Ast,
    Compiler,
    Configuration,
    Descriptor,
    Generated,
    Inference,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EvidenceRef {
    #[serde(rename = "source")]
    pub source_kind: EvidenceKind,
    pub repository: Option<String>,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SemanticEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: RelationKind,
    pub confidence: f32,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceTopology {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticPath {
    pub nodes: Vec<String>,
    pub edges: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntityQuery {
    pub entity: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntitySearchQuery {
    pub query: String,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EntitySearchResult {
    pub schema: String,
    #[serde(flatten, serialize_with = "serialize_metadata_with_counts")]
    pub metadata: QueryMetadata,
    pub query: EntitySearchQuery,
    pub matches: Vec<EntityRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PathQuery {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextResult {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub query: EntityQuery,
    pub root: EntityRef,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyRef {
    pub entity: String,
    pub hops: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DependenciesResult {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub query: EntityQuery,
    pub traversal: TraversalMetadata,
    pub root: EntityRef,
    pub dependencies: Vec<DependencyRef>,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImpactRef {
    pub entity: String,
    pub hops: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ImpactResult {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub query: EntityQuery,
    pub traversal: TraversalMetadata,
    pub root: EntityRef,
    pub affected: Vec<ImpactRef>,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TraceResult {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub query: PathQuery,
    pub traversal: TraversalMetadata,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
    pub paths: Vec<SemanticPath>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WhyResult {
    pub schema: String,
    #[serde(flatten)]
    pub metadata: QueryMetadata,
    pub query: PathQuery,
    pub traversal: TraversalMetadata,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
    pub paths: Vec<SemanticPath>,
}

impl From<TraceResult> for WhyResult {
    fn from(value: TraceResult) -> Self {
        Self {
            schema: WHY_SCHEMA_V2.into(),
            metadata: value.metadata,
            query: value.query,
            traversal: value.traversal,
            nodes: value.nodes,
            edges: value.edges,
            paths: value.paths,
        }
    }
}

pub trait SemanticQueryResult {
    fn metadata_mut(&mut self) -> &mut QueryMetadata;
}

macro_rules! semantic_result {
    ($($result:ty),+ $(,)?) => {
        $(impl SemanticQueryResult for $result {
            fn metadata_mut(&mut self) -> &mut QueryMetadata {
                &mut self.metadata
            }
        })+
    };
}

semantic_result!(
    ContextResult,
    DependenciesResult,
    EntitySearchResult,
    ImpactResult,
    TraceResult,
    WhyResult,
    WorkspaceTopology,
);

#[derive(Clone, Debug, PartialEq)]
pub struct Revisioned<T> {
    pub result: T,
    pub analysis_revision: u64,
    pub analysis: AnalysisMetadata,
}

/// Limits apply to the new multi-path operation; legacy query contracts are unchanged.
pub const TRAVERSE_GRAPH_SCHEMA_V2: &str = "beholder.traverse_graph.v2";
pub const DEFAULT_TRAVERSAL_HOPS: u32 = 8;
pub const MAX_TRAVERSAL_HOPS: u32 = 32;
pub const DEFAULT_MAX_PATHS: u32 = 50;
pub const MAX_PATHS: u32 = 200;
pub const MAX_TRAVERSAL_ROWS: u32 = 10_000;
pub const MAX_TRAVERSAL_STEPS: u32 = 100_000;
pub const TRAVERSAL_ACQUISITION_TIMEOUT_MS: u32 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphDirection {
    Dependencies,
    Dependents,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraverseGraphQuery {
    pub start: String,
    pub direction: GraphDirection,
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_repositories: Vec<String>,
    pub max_hops: u32,
    pub max_paths: u32,
}

impl TraverseGraphQuery {
    pub fn normalize(&mut self) {
        self.target_repositories.sort();
        self.target_repositories.dedup();
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.start.trim().is_empty()
            || self
                .destination
                .as_ref()
                .is_some_and(|id| id.trim().is_empty())
        {
            return Err("start and destination must be non-empty canonical entity IDs");
        }
        if self.destination.is_some() && !self.target_repositories.is_empty() {
            return Err("destination and target_repositories are mutually exclusive");
        }
        if self
            .target_repositories
            .iter()
            .any(|repository| repository.trim().is_empty())
        {
            return Err("target repository identities must be non-empty");
        }
        if self.max_hops > MAX_TRAVERSAL_HOPS {
            return Err("max_hops must be between 0 and 32");
        }
        if self.max_paths == 0 || self.max_paths > MAX_PATHS {
            return Err("max_paths must be between 1 and 200");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathTermination {
    Destination,
    RepositoryBoundary,
    Leaf,
    Cycle,
    MaxHops,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReason {
    MaxHops,
    MaxPaths,
    AcquisitionLimit,
    WorkLimit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalPath {
    pub nodes: Vec<String>,
    pub edges: Vec<String>,
    pub termination: PathTermination,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphTraversalMetadata {
    pub max_hops: u32,
    pub max_paths: u32,
    pub max_rows: u32,
    pub max_steps: u32,
    pub acquisition_timeout_ms: u32,
    pub truncated: bool,
    pub truncation_reasons: Vec<TruncationReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraverseGraphResult {
    pub schema: String,
    #[serde(flatten, serialize_with = "serialize_metadata_with_counts")]
    pub metadata: QueryMetadata,
    pub query: TraverseGraphQuery,
    pub nodes: Vec<EntityRef>,
    pub edges: Vec<SemanticEdge>,
    pub paths: Vec<TraversalPath>,
    pub traversal: GraphTraversalMetadata,
}
semantic_result!(TraverseGraphResult);

#[cfg(test)]
mod tests {
    use super::*;

    fn traversal() -> TraverseGraphQuery {
        TraverseGraphQuery {
            start: "repo://example/root".into(),
            direction: GraphDirection::Dependencies,
            destination: None,
            target_repositories: Vec::new(),
            max_hops: DEFAULT_TRAVERSAL_HOPS,
            max_paths: DEFAULT_MAX_PATHS,
        }
    }

    #[test]
    fn traversal_normalizes_and_validates_repository_targets() {
        let mut query = traversal();
        query.target_repositories = vec!["repo-b".into(), "repo-a".into(), "repo-b".into()];
        query.normalize();
        assert_eq!(query.target_repositories, ["repo-a", "repo-b"]);
        assert_eq!(query.validate(), Ok(()));

        query.destination = Some("repo://example/destination".into());
        assert_eq!(
            query.validate(),
            Err("destination and target_repositories are mutually exclusive")
        );

        query.destination = None;
        query.target_repositories = vec!["".into()];
        assert_eq!(
            query.validate(),
            Err("target repository identities must be non-empty")
        );
    }

    #[test]
    fn completed_metadata_serializes_zero_diagnostic_counts() {
        let metadata = QueryMetadata::completed("main", 1);

        assert!(
            serde_json::to_value(&metadata)
                .unwrap()
                .get("analysis")
                .is_none()
        );
        let result = EntitySearchResult {
            schema: "test".into(),
            metadata,
            query: EntitySearchQuery {
                query: "example".into(),
                limit: 1,
            },
            matches: Vec::new(),
        };
        let value = serde_json::to_value(result).unwrap();

        assert_eq!(value["analysis"]["diagnostic_counts"]["total"], 0);
        let result = TraverseGraphResult {
            schema: "test".into(),
            metadata: QueryMetadata::completed("main", 1),
            query: traversal(),
            nodes: Vec::new(),
            edges: Vec::new(),
            paths: Vec::new(),
            traversal: GraphTraversalMetadata {
                max_hops: 1,
                max_paths: 1,
                max_rows: 1,
                max_steps: 1,
                acquisition_timeout_ms: 1,
                truncated: false,
                truncation_reasons: Vec::new(),
            },
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["analysis"]["diagnostic_counts"]["total"], 0);
    }
}
