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
    UniqueNameHeuristic,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ast => "ast",
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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRelation {
    Calls,
    CallsRpc,
    ConsumedBy,
    ImplementedBy,
    Publishes,
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
            Self::ImplementedBy => "implemented_by",
            Self::Publishes => "publishes",
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FactChanges {
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub repositories: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(name: impl Into<String>, mut repositories: Vec<PathBuf>) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("workspace name must not be empty".into());
        }
        if repositories.is_empty() {
            return Err("workspace must contain a repository".into());
        }
        repositories.sort();
        repositories.dedup();
        Ok(Self { name, repositories })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalRepository {
    pub identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryState {
    pub repository: LogicalRepository,
    pub head: Option<String>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceView {
    pub name: String,
    pub analysis_identity: String,
    pub repository_states: Vec<RepositoryState>,
}

impl WorkspaceView {
    pub fn new(
        name: impl Into<String>,
        analysis_identity: impl Into<String>,
        mut repository_states: Vec<RepositoryState>,
    ) -> Result<Self, String> {
        let name = name.into();
        let analysis_identity = analysis_identity.into();
        if name.is_empty() {
            return Err("workspace view name must not be empty".into());
        }
        if analysis_identity.is_empty() {
            return Err("workspace view analysis identity must not be empty".into());
        }
        if repository_states.is_empty() {
            return Err("workspace view must contain a repository state".into());
        }
        repository_states
            .sort_by(|left, right| left.repository.identity.cmp(&right.repository.identity));
        if let Some(duplicate) = repository_states
            .windows(2)
            .find(|states| states[0].repository == states[1].repository)
        {
            return Err(format!(
                "workspace view contains duplicate repository {}",
                duplicate[0].repository.identity
            ));
        }
        Ok(Self {
            name,
            analysis_identity,
            repository_states,
        })
    }

    pub fn fingerprint(&self) -> String {
        std::iter::once(format!(
            "{}:{}",
            self.analysis_identity.len(),
            self.analysis_identity
        ))
        .chain(
            self.repository_states
                .iter()
                .map(|state| format!("{}:{}", state.fingerprint.len(), state.fingerprint)),
        )
        .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitClone {
    pub common_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkingTree {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitTopology {
    pub repository: LogicalRepository,
    pub clone: GitClone,
    pub working_trees: Vec<WorkingTree>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dependency_relations_are_traversable() {
        assert_eq!(
            SemanticRelation::Structural(StructuralRelation::Defines).dependency(),
            None
        );
        assert_eq!(
            SemanticRelation::Dependency(DependencyRelation::Calls).dependency(),
            Some(DependencyRelation::Calls)
        );
    }

    #[test]
    fn workspace_smoke() {
        assert!(Workspace::new("", vec!["repo".into()]).is_err());
        assert!(Workspace::new("main", Vec::new()).is_err());
        assert_eq!(
            Workspace::new("main", vec!["repo".into(), "repo".into()])
                .unwrap()
                .repositories,
            vec![PathBuf::from("repo")]
        );

        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "state".into(),
        };
        assert!(WorkspaceView::new("main", "", vec![state.clone()]).is_err());
        assert_ne!(
            WorkspaceView::new("main", "analysis-1", vec![state.clone()])
                .unwrap()
                .fingerprint(),
            WorkspaceView::new("main", "analysis-2", vec![state])
                .unwrap()
                .fingerprint()
        );
    }
}
