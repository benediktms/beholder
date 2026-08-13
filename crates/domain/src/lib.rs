use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    pub from: String,
    pub relation: String,
    pub to: String,
    pub evidence: String,
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
