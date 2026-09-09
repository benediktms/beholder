use crate::RepositoryState;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

const STRUCTURED_EVIDENCE_PREFIX: &str = "beholder:evidence:v1:";

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EntityId(String);

impl EntityId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for EntityId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for EntityId {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Evidence(String);

impl Evidence {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn structured(payload: EvidencePayload) -> Result<Self, String> {
        payload.validate()?;
        serde_json::to_string(&payload)
            .map(|payload| Self(format!("{STRUCTURED_EVIDENCE_PREFIX}{payload}")))
            .map_err(|error| error.to_string())
    }

    pub fn decode(&self) -> EvidencePayload {
        self.0
            .strip_prefix(STRUCTURED_EVIDENCE_PREFIX)
            .and_then(|payload| serde_json::from_str::<EvidencePayload>(payload).ok())
            .filter(|payload| payload.validate().is_ok())
            .unwrap_or_else(|| EvidencePayload::legacy(&self.0))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct EvidencePayload {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub detail: Option<String>,
    pub range: Option<SourceRange>,
    #[serde(default)]
    pub contexts: Vec<EvidenceContext>,
}

impl EvidencePayload {
    fn legacy(evidence: &str) -> Self {
        let (location, detail) = evidence
            .split_once(" · ")
            .map_or((evidence, None), |(location, detail)| {
                (location, Some(detail.to_owned()))
            });
        let (path, line) = location
            .rsplit_once(':')
            .and_then(|(path, line)| {
                line.parse()
                    .ok()
                    .map(|line| (Some(path.into()), Some(line)))
            })
            .unwrap_or_else(|| (Some(location.into()), None));
        Self {
            path,
            line,
            detail,
            range: None,
            contexts: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.range.as_ref().is_some_and(|range| !range.is_valid())
            || self.contexts.iter().any(|context| !context.is_valid())
        {
            return Err("evidence contains an invalid source range".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Inferred,
}

impl Confidence {
    pub fn score(self) -> f64 {
        match self {
            Self::Exact => 1.0,
            Self::Inferred => 0.6,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Ast,
    Compiler,
    Descriptor,
    Generated,
    UniqueNameHeuristic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticSeverity {
    KnownLimitation,
    Warning,
}

impl AnalysisDiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KnownLimitation => "known_limitation",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct AnalysisDiagnostic {
    pub code: String,
    pub severity: AnalysisDiagnosticSeverity,
    pub path: PathBuf,
    pub line: Option<u32>,
    pub detail: Option<String>,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ast => "ast",
            Self::Compiler => "compiler",
            Self::Descriptor => "descriptor",
            Self::Generated => "generated",
            Self::UniqueNameHeuristic => "unique_name_heuristic",
        }
    }
}

impl From<String> for Evidence {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Evidence {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;

    fn position(line: u32, character: u32) -> SourcePosition {
        SourcePosition { line, character }
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> SourceRange {
        SourceRange {
            start: position(start.0, start.1),
            end: position(end.0, end.1),
        }
    }

    fn excerpt(text: &str, start: (u32, u32), end: (u32, u32)) -> SourceExcerpt {
        SourceExcerpt {
            text: text.into(),
            range: range(start, end),
        }
    }

    #[test]
    fn structured_evidence_is_deterministic_and_preserves_context_order() {
        let payload = EvidencePayload {
            path: Some("src/lib.rs".into()),
            line: Some(3),
            detail: None,
            range: Some(range((2, 5), (2, 12))),
            contexts: vec![
                EvidenceContext::CallableClause {
                    role: CallableClauseRole::Enclosing,
                    signature: excerpt("fn run(🚀: &str)", (1, 0), (1, 17)),
                    guard: None,
                    definition_range: range((1, 0), (4, 1)),
                },
                EvidenceContext::ConditionArm {
                    construct: ConditionConstruct::If,
                    arm: ConditionArmKind::Then,
                    condition: Some(excerpt("🚀.len() > 0", (2, 8), (2, 20))),
                    arm_range: range((2, 22), (3, 5)),
                },
            ],
        };

        let first = Evidence::structured(payload.clone()).unwrap();
        let second = Evidence::structured(payload.clone()).unwrap();

        assert_eq!(first, second);
        assert!(
            first
                .as_str()
                .starts_with("beholder:evidence:v1:{\"path\":\"src/lib.rs\",\"line\":3")
        );
        assert_eq!(first.decode(), payload);
    }

    #[test]
    fn decodes_legacy_location_and_plugin_detail_forms() {
        assert_eq!(
            Evidence::from("src/lib.rs:7").decode(),
            EvidencePayload {
                path: Some("src/lib.rs".into()),
                line: Some(7),
                detail: None,
                range: None,
                contexts: Vec::new(),
            }
        );
        assert_eq!(
            Evidence::from("src/lib.rs:7 · compiler selected target").decode(),
            EvidencePayload {
                path: Some("src/lib.rs".into()),
                line: Some(7),
                detail: Some("compiler selected target".into()),
                range: None,
                contexts: Vec::new(),
            }
        );
        assert_eq!(
            Evidence::from("descriptor.proto").decode().path.as_deref(),
            Some("descriptor.proto")
        );
        assert_eq!(
            Evidence::from("descriptor.proto · generated service").decode(),
            EvidencePayload {
                path: Some("descriptor.proto".into()),
                line: None,
                detail: Some("generated service".into()),
                range: None,
                contexts: Vec::new(),
            }
        );
    }

    #[test]
    fn malformed_or_invalid_reserved_payload_falls_back_to_legacy_evidence() {
        for evidence in [
            "beholder:evidence:v1:not-json",
            "beholder:evidence:v1:{\"path\":null,\"line\":null,\"detail\":null,\"range\":{\"start\":{\"line\":2,\"character\":0},\"end\":{\"line\":1,\"character\":0}},\"contexts\":[]}",
        ] {
            let decoded = Evidence::from(evidence).decode();
            assert_eq!(decoded.path.as_deref(), Some(evidence));
            assert_eq!(decoded.detail, None);
            assert!(decoded.contexts.is_empty());
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuralRelation {
    Defines,
    FieldOf,
    RequestType,
    ResponseType,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRelation {
    BindsContract,
    Calls,
    CallsGraphql,
    CallsRpc,
    ConsumedBy,
    Implements,
    ImplementedBy,
    Imports,
    Publishes,
    Requires,
    ResolvedBy,
    Selects,
    Uses,
}

impl DependencyRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BindsContract => "binds_contract",
            Self::Calls => "calls",
            Self::CallsGraphql => "calls_graphql",
            Self::CallsRpc => "calls_rpc",
            Self::ConsumedBy => "consumed_by",
            Self::Implements => "implements",
            Self::ImplementedBy => "implemented_by",
            Self::Imports => "imports",
            Self::Publishes => "publishes",
            Self::Requires => "requires",
            Self::ResolvedBy => "resolved_by",
            Self::Selects => "selects",
            Self::Uses => "uses",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SemanticRelation {
    Structural(StructuralRelation),
    Dependency(DependencyRelation),
}

impl SemanticRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structural(StructuralRelation::Defines) => "defines",
            Self::Structural(StructuralRelation::FieldOf) => "field_of",
            Self::Structural(StructuralRelation::RequestType) => "request_type",
            Self::Structural(StructuralRelation::ResponseType) => "response_type",
            Self::Dependency(relation) => relation.as_str(),
        }
    }

    pub fn dependency(self) -> Option<DependencyRelation> {
        match self {
            Self::Dependency(relation) => Some(relation),
            Self::Structural(_) => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Observation {
    pub from: EntityId,
    pub relation: SemanticRelation,
    pub to: EntityId,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourcePosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceRange {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

impl SourceRange {
    pub fn is_valid(&self) -> bool {
        self.start <= self.end
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceExcerpt {
    pub text: String,
    pub range: SourceRange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionConstruct {
    If,
    Cond,
    Ternary,
    TemplateIf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionArmKind {
    Then,
    ElseIf,
    Else,
    Clause,
    Consequence,
    Alternative,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternConstruct {
    Match,
    Case,
    SwitchStatement,
    SwitchExpression,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallableClauseRole {
    Declaration,
    Enclosing,
    SelectedTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceContext {
    ConditionArm {
        construct: ConditionConstruct,
        arm: ConditionArmKind,
        condition: Option<SourceExcerpt>,
        arm_range: SourceRange,
    },
    PatternArm {
        construct: PatternConstruct,
        selector: Option<SourceExcerpt>,
        pattern: Option<SourceExcerpt>,
        guard: Option<SourceExcerpt>,
        is_default: bool,
        arm_range: SourceRange,
    },
    CallableClause {
        role: CallableClauseRole,
        signature: SourceExcerpt,
        guard: Option<SourceExcerpt>,
        definition_range: SourceRange,
    },
}

impl EvidenceContext {
    fn is_valid(&self) -> bool {
        fn excerpt_is_valid(excerpt: &Option<SourceExcerpt>) -> bool {
            excerpt
                .as_ref()
                .is_none_or(|excerpt| excerpt.range.is_valid())
        }

        match self {
            Self::ConditionArm {
                condition,
                arm_range,
                ..
            } => excerpt_is_valid(condition) && arm_range.is_valid(),
            Self::PatternArm {
                selector,
                pattern,
                guard,
                arm_range,
                ..
            } => {
                excerpt_is_valid(selector)
                    && excerpt_is_valid(pattern)
                    && excerpt_is_valid(guard)
                    && arm_range.is_valid()
            }
            Self::CallableClause {
                signature,
                guard,
                definition_range,
                ..
            } => {
                signature.range.is_valid() && excerpt_is_valid(guard) && definition_range.is_valid()
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SourceSpan {
    pub path: PathBuf,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SemanticCandidate {
    pub id: String,
    pub repository: String,
    pub from: EntityId,
    pub relation: DependencyRelation,
    pub unresolved_to: EntityId,
    pub span: SourceSpan,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateOverride {
    pub candidate_id: String,
    pub resolved_to: EntityId,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryFacts {
    pub state: RepositoryState,
    pub analysis_identity: String,
    pub incomplete: bool,
    pub diagnostics: Vec<AnalysisDiagnostic>,
    pub entities: Vec<EntityFact>,
    pub grpc_bindings: Vec<GrpcBindingCandidate>,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactShard {
    pub repository: String,
    pub producer: String,
    pub owner: EntityId,
    pub version: String,
    pub entities: Vec<EntityFact>,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct GrpcBindingCandidate {
    pub local_symbol: EntityId,
    pub role: GrpcBindingRole,
    pub service: String,
    pub method: String,
    pub cardinality: RpcCardinality,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrpcBindingRole {
    Client,
    Server,
}

impl GrpcBindingRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct EntityFact {
    pub id: EntityId,
    pub kind: EntityKind,
    pub metadata: Option<EntityMetadata>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum EntityKind {
    Callable,
    GraphqlArgument,
    GraphqlEnumValue,
    GraphqlField,
    GraphqlOperation,
    GraphqlType,
    GrpcOperation,
    KafkaTopic,
    Namespace,
    ProtoField,
    ProtoMethod,
    ProtoService,
    ProtoType,
    Service,
    UnityPrefab,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum EntityMetadata {
    GraphqlOperation { kind: GraphqlOperationKind },
    GraphqlType { kind: GraphqlTypeKind },
    ProtoMethod { cardinality: RpcCardinality },
    ProtoType { kind: ProtoTypeKind },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum GraphqlOperationKind {
    Mutation,
    Query,
    Subscription,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum GraphqlTypeKind {
    Enum,
    Input,
    Interface,
    Object,
    Scalar,
    Union,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ProtoTypeKind {
    Enum,
    Message,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum RpcCardinality {
    BidirectionalStreaming,
    ClientStreaming,
    ServerStreaming,
    Unary,
}

impl EntityFact {
    pub fn new(
        id: impl Into<EntityId>,
        kind: EntityKind,
        metadata: Option<EntityMetadata>,
    ) -> Result<Self, &'static str> {
        match (kind, metadata) {
            (EntityKind::GraphqlOperation, Some(EntityMetadata::GraphqlOperation { .. }))
            | (EntityKind::GraphqlType, Some(EntityMetadata::GraphqlType { .. }))
            | (EntityKind::ProtoMethod, Some(EntityMetadata::ProtoMethod { .. }))
            | (EntityKind::ProtoType, Some(EntityMetadata::ProtoType { .. }))
            | (
                EntityKind::Callable
                | EntityKind::GraphqlArgument
                | EntityKind::GraphqlEnumValue
                | EntityKind::GraphqlField
                | EntityKind::GrpcOperation
                | EntityKind::KafkaTopic
                | EntityKind::Namespace
                | EntityKind::ProtoField
                | EntityKind::ProtoService
                | EntityKind::Service
                | EntityKind::UnityPrefab,
                None,
            ) => Ok(Self {
                id: id.into(),
                kind,
                metadata,
            }),
            _ => Err("entity metadata does not match entity kind"),
        }
    }
}

/// Validates the entity contract shared by semantic producers and publishers.
///
/// A published graph is only useful when entity identity is authoritative: every
/// relationship endpoint must have one fact, and multiple producers describing
/// the same entity must agree completely. Keeping this validation in the domain
/// crate makes workers, the baseline publisher, and enrichment publication use
/// the same rules.
pub fn validate_entity_complete_semantics<'a>(
    entities: impl IntoIterator<Item = &'a EntityFact>,
    observations: impl IntoIterator<Item = &'a Observation>,
    overrides: impl IntoIterator<Item = &'a DependencyOverride>,
) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut selected = BTreeMap::<&str, (&EntityKind, &Option<EntityMetadata>)>::new();
    for entity in entities {
        match selected.get(entity.id.as_str()) {
            Some((kind, metadata)) if **kind != entity.kind || **metadata != entity.metadata => {
                return Err(format!("conflicting entity facts for {}", entity.id));
            }
            Some(_) => {}
            None => {
                selected.insert(entity.id.as_str(), (&entity.kind, &entity.metadata));
            }
        }
    }

    let mut required = BTreeSet::new();
    for observation in observations {
        required.insert(observation.from.as_str());
        required.insert(observation.to.as_str());
    }
    for dependency_override in overrides {
        required.insert(dependency_override.from.as_str());
        required.insert(dependency_override.resolved_to.as_str());
    }
    if let Some(missing) = required.into_iter().find(|id| !selected.contains_key(id)) {
        return Err(format!(
            "missing entity fact for relationship endpoint {missing}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod entity_fact_tests {
    use super::*;

    #[test]
    fn accepts_resolved_overrides_without_facts_for_lookup_keys() {
        let caller =
            EntityFact::new("repo://example/rust/caller", EntityKind::Callable, None).unwrap();
        let target =
            EntityFact::new("repo://example/rust/target", EntityKind::Callable, None).unwrap();
        let observation = Observation::dependency(
            caller.id.clone(),
            DependencyRelation::Calls,
            target.id.clone(),
            "src/lib.rs:1",
        );
        let override_ = DependencyOverride {
            from: caller.id.clone(),
            relation: DependencyRelation::Calls,
            unresolved_to: "rust-call://target".into(),
            resolved_to: target.id.clone(),
            evidence: observation.evidence.clone(),
            confidence: Confidence::Inferred,
            provenance: Provenance::UniqueNameHeuristic,
        };

        assert!(
            validate_entity_complete_semantics([&caller, &target], [&observation], [&override_],)
                .is_ok()
        );
    }

    #[test]
    fn rejects_incompatible_metadata() {
        assert!(
            EntityFact::new(
                "proto-type://example.v1.Quote",
                EntityKind::ProtoType,
                Some(EntityMetadata::ProtoMethod {
                    cardinality: RpcCardinality::Unary,
                }),
            )
            .is_err()
        );
        assert!(
            EntityFact::new(
                "proto-type://example.v1.Quote",
                EntityKind::ProtoType,
                Some(EntityMetadata::ProtoType {
                    kind: ProtoTypeKind::Message,
                }),
            )
            .is_ok()
        );
    }

    #[test]
    fn diagnostic_severity_has_a_stable_wire_name() {
        assert_eq!(
            AnalysisDiagnosticSeverity::KnownLimitation.as_str(),
            "known_limitation"
        );
        assert_eq!(AnalysisDiagnosticSeverity::Warning.as_str(), "warning");
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyOverride {
    pub from: EntityId,
    pub relation: DependencyRelation,
    pub unresolved_to: EntityId,
    pub resolved_to: EntityId,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub provenance: Provenance,
}

impl Observation {
    pub fn structural(
        from: impl Into<EntityId>,
        relation: StructuralRelation,
        to: impl Into<EntityId>,
        evidence: impl Into<Evidence>,
    ) -> Self {
        Self {
            from: from.into(),
            relation: SemanticRelation::Structural(relation),
            to: to.into(),
            evidence: evidence.into(),
            confidence: Confidence::Exact,
            provenance: Provenance::Ast,
        }
    }

    pub fn dependency(
        from: impl Into<EntityId>,
        relation: DependencyRelation,
        to: impl Into<EntityId>,
        evidence: impl Into<Evidence>,
    ) -> Self {
        Self {
            from: from.into(),
            relation: SemanticRelation::Dependency(relation),
            to: to.into(),
            evidence: evidence.into(),
            confidence: Confidence::Exact,
            provenance: Provenance::Ast,
        }
    }

    pub fn descriptor(
        from: impl Into<EntityId>,
        relation: StructuralRelation,
        to: impl Into<EntityId>,
        descriptor: impl Into<Evidence>,
    ) -> Self {
        Self {
            from: from.into(),
            relation: SemanticRelation::Structural(relation),
            to: to.into(),
            evidence: descriptor.into(),
            confidence: Confidence::Exact,
            provenance: Provenance::Descriptor,
        }
    }

    pub fn generated(
        from: impl Into<EntityId>,
        relation: StructuralRelation,
        to: impl Into<EntityId>,
        evidence: impl Into<Evidence>,
    ) -> Self {
        Self {
            from: from.into(),
            relation: SemanticRelation::Structural(relation),
            to: to.into(),
            evidence: evidence.into(),
            confidence: Confidence::Exact,
            provenance: Provenance::Generated,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FactChanges {
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}
