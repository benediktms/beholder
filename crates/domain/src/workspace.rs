use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub repositories: Vec<WorkspaceRepository>,
    pub protobuf_descriptors: Vec<ProtobufDescriptorSource>,
}

impl Workspace {
    pub fn new(
        name: impl Into<String>,
        repositories: Vec<WorkspaceRepository>,
    ) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("workspace name must not be empty".into());
        }
        if repositories.is_empty() {
            return Err("workspace must contain a repository".into());
        }
        let mut merged = std::collections::BTreeMap::<String, WorkspaceRepository>::new();
        for repository in repositories {
            if repository.repository.identity.trim().is_empty() {
                return Err("repository identity must not be empty".into());
            }
            if repository.display_name.trim().is_empty() {
                return Err("repository display name must not be empty".into());
            }
            match merged.get_mut(&repository.repository.identity) {
                Some(existing) => {
                    existing.alternatives.push(repository.base);
                    existing.alternatives.extend(repository.alternatives);
                }
                None => {
                    merged.insert(repository.repository.identity.clone(), repository);
                }
            }
        }
        let repositories = merged
            .into_values()
            .map(|mut repository| {
                repository.alternatives.sort();
                repository.alternatives.dedup();
                repository
                    .alternatives
                    .retain(|path| path != &repository.base);
                repository
            })
            .collect();
        Ok(Self {
            name,
            repositories,
            protobuf_descriptors: Vec::new(),
        })
    }

    pub fn with_protobuf_descriptors(
        mut self,
        mut descriptors: Vec<ProtobufDescriptorSource>,
    ) -> Result<Self, String> {
        for descriptor in &descriptors {
            if !self
                .repositories
                .iter()
                .any(|repository| repository.repository == descriptor.repository)
            {
                return Err(format!(
                    "protobuf descriptor references unknown repository {}",
                    descriptor.repository.identity
                ));
            }
        }
        descriptors.sort_by(|left, right| {
            (&left.repository.identity, &left.path).cmp(&(&right.repository.identity, &right.path))
        });
        descriptors.dedup();
        self.protobuf_descriptors = descriptors;
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtobufDescriptorSource {
    pub repository: LogicalRepository,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRepository {
    pub repository: LogicalRepository,
    pub display_name: String,
    pub base: PathBuf,
    pub alternatives: Vec<PathBuf>,
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
    repository_analysis_identities: BTreeMap<String, String>,
}

impl WorkspaceView {
    pub fn new(
        name: impl Into<String>,
        analysis_identity: impl Into<String>,
        repository_states: Vec<RepositoryState>,
    ) -> Result<Self, String> {
        let analysis_identity = analysis_identity.into();
        let repository_analysis_identities = repository_states
            .iter()
            .map(|state| (state.repository.identity.clone(), analysis_identity.clone()))
            .collect();
        Self::new_scoped(
            name,
            analysis_identity,
            repository_states,
            repository_analysis_identities,
        )
    }

    pub fn new_scoped(
        name: impl Into<String>,
        analysis_identity: impl Into<String>,
        mut repository_states: Vec<RepositoryState>,
        repository_analysis_identities: BTreeMap<String, String>,
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
        if repository_analysis_identities.len() != repository_states.len()
            || repository_states.iter().any(|state| {
                repository_analysis_identities
                    .get(&state.repository.identity)
                    .is_none_or(String::is_empty)
            })
        {
            return Err(
                "workspace view repository analysis identities do not match its repositories"
                    .into(),
            );
        }
        Ok(Self {
            name,
            analysis_identity,
            repository_states,
            repository_analysis_identities,
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

    pub fn repository_input_fingerprint(&self, state: &RepositoryState) -> String {
        let analysis_identity = self
            .repository_analysis_identities
            .get(&state.repository.identity)
            .expect("workspace view repository state must have an analysis identity");
        format!(
            "{}:{}{}",
            analysis_identity.len(),
            analysis_identity,
            state.fingerprint
        )
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
    use crate::{DependencyRelation, SemanticRelation, StructuralRelation};

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
        let repository = |base: &str| WorkspaceRepository {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            display_name: "repo".into(),
            base: base.into(),
            alternatives: Vec::new(),
        };
        assert!(Workspace::new("", vec![repository("repo")]).is_err());
        assert!(Workspace::new("main", Vec::new()).is_err());
        let workspace =
            Workspace::new("main", vec![repository("base"), repository("alternative")]).unwrap();
        assert_eq!(workspace.repositories.len(), 1);
        assert_eq!(workspace.repositories[0].base, PathBuf::from("base"));
        assert_eq!(
            workspace.repositories[0].alternatives,
            vec![PathBuf::from("alternative")]
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

    #[test]
    fn repository_input_identity_ignores_unrelated_repository_analysis() {
        let state = |identity: &str| RepositoryState {
            repository: LogicalRepository {
                identity: identity.into(),
            },
            head: Some("head".into()),
            fingerprint: "source".into(),
        };
        let states = vec![state("example/a"), state("example/b")];
        let first = WorkspaceView::new_scoped(
            "main",
            "workspace-1",
            states.clone(),
            BTreeMap::from([
                ("example/a".into(), "a-1".into()),
                ("example/b".into(), "b-1".into()),
            ]),
        )
        .unwrap();
        let second = WorkspaceView::new_scoped(
            "main",
            "workspace-2",
            states.clone(),
            BTreeMap::from([
                ("example/a".into(), "a-1".into()),
                ("example/b".into(), "b-2".into()),
            ]),
        )
        .unwrap();

        assert_eq!(
            first.repository_input_fingerprint(&states[0]),
            second.repository_input_fingerprint(&states[0])
        );
        assert_ne!(
            first.repository_input_fingerprint(&states[1]),
            second.repository_input_fingerprint(&states[1])
        );
        assert_ne!(first.fingerprint(), second.fingerprint());
    }
}
