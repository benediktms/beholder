use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONTEXT_SCHEMA_V1: &str = "beholder.context.v1";
pub const DEPENDENCIES_SCHEMA_V2: &str = "beholder.dependencies.v2";
pub const IMPACT_SCHEMA_V2: &str = "beholder.impact.v2";
pub const TRACE_SCHEMA_V2: &str = "beholder.trace.v2";
pub const WHY_SCHEMA_V2: &str = "beholder.why.v2";
pub const DEFAULT_MAX_HOPS: u32 = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GarbageCollection {
    pub repository_states_removed: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Freshness {
    pub stale: bool,
    pub indexing: bool,
    pub dirty_repositories: Vec<String>,
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
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

impl AnalysisMetadata {
    fn is_complete(&self) -> bool {
        self.completeness == AnalysisCompleteness::Complete && self.diagnostics.is_empty()
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

impl QueryMetadata {
    pub fn completed(view: impl Into<String>, revision: u64) -> Self {
        Self {
            revision,
            view: view.into(),
            freshness: Freshness {
                stale: false,
                indexing: false,
                dirty_repositories: Vec::new(),
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
    ImpactResult,
    TraceResult,
    WhyResult,
);

#[derive(Clone, Debug, PartialEq)]
pub struct Revisioned<T> {
    pub result: T,
    pub analysis_revision: u64,
    pub analysis: AnalysisMetadata,
}
