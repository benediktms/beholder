use crate::RepositoryState;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Evidence(String);

impl Evidence {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Ast,
    Descriptor,
    Generated,
    UniqueNameHeuristic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuralRelation {
    Defines,
    FieldOf,
    RequestType,
    ResponseType,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRelation {
    Calls,
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
            Self::Calls => "calls",
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    pub from: EntityId,
    pub relation: SemanticRelation,
    pub to: EntityId,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryFacts {
    pub state: RepositoryState,
    pub analysis_identity: String,
    pub observations: Vec<Observation>,
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
