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
    repository_contexts: BTreeMap<String, BTreeMap<String, Vec<String>>>,
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
            repository_contexts: BTreeMap::new(),
        })
    }

    pub fn with_repository_contexts(
        mut self,
        repository_contexts: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    ) -> Result<Self, String> {
        let repositories = self
            .repository_states
            .iter()
            .map(|state| state.repository.identity.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for (analyzer, targets) in &repository_contexts {
            if analyzer.trim().is_empty() {
                return Err("enrichment context analyzer must not be empty".into());
            }
            for (target, contexts) in targets {
                if !repositories.contains(target.as_str()) {
                    return Err(format!("unknown enrichment target repository {target}"));
                }
                if contexts
                    .iter()
                    .any(|context| context == target || !repositories.contains(context.as_str()))
                {
                    return Err(format!(
                        "invalid {analyzer} enrichment context repository for target {target}"
                    ));
                }
            }
        }
        self.repository_contexts = repository_contexts
            .into_iter()
            .map(|(analyzer, targets)| {
                let targets = targets
                    .into_iter()
                    .map(|(target, mut contexts)| {
                        contexts.sort();
                        contexts.dedup();
                        (target, contexts)
                    })
                    .collect();
                (analyzer, targets)
            })
            .collect();
        Ok(self)
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

    pub fn repository_enrichment_input_fingerprint(
        &self,
        state: &RepositoryState,
        analyzer: &str,
    ) -> String {
        let mut repositories = std::iter::once(state.repository.identity.as_str())
            .chain(
                self.repository_contexts(&state.repository.identity, analyzer)
                    .iter()
                    .map(String::as_str),
            );
        repositories
            .try_fold(String::new(), |mut fingerprint, repository| {
                let state = self
                    .repository_states
                    .iter()
                    .find(|state| state.repository.identity == repository)?;
                let analysis_identity = self.repository_analysis_identities.get(repository)?;
                fingerprint.push_str(&format!(
                    "{}:{repository}{}:{analysis_identity}{}:{}",
                    repository.len(),
                    analysis_identity.len(),
                    state.fingerprint.len(),
                    state.fingerprint
                ));
                Some(fingerprint)
            })
            .expect("workspace view input repositories must have states and analysis identities")
    }

    pub fn repository_contexts(&self, repository: &str, analyzer: &str) -> &[String] {
        self.repository_contexts
            .get(analyzer)
            .and_then(|targets| targets.get(repository))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn enrichment_analyzers(&self) -> impl Iterator<Item = &str> {
        self.repository_contexts.keys().map(String::as_str)
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

    #[test]
    fn repository_input_identity_includes_only_selected_contexts() {
        let state = |identity: &str, fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: identity.into(),
            },
            head: None,
            fingerprint: fingerprint.into(),
        };
        let view = |context_fingerprint: &str, unrelated_fingerprint: &str| {
            WorkspaceView::new(
                "main",
                "analysis",
                vec![
                    state("example/a", "a"),
                    state("example/b", context_fingerprint),
                    state("example/c", unrelated_fingerprint),
                ],
            )
            .unwrap()
            .with_repository_contexts(BTreeMap::from([(
                "typescript".into(),
                BTreeMap::from([("example/a".into(), vec!["example/b".into()])]),
            )]))
            .unwrap()
        };
        let original = view("b-1", "c-1");
        let changed_context = view("b-2", "c-1");
        let changed_unrelated = view("b-1", "c-2");

        assert_ne!(
            original.repository_enrichment_input_fingerprint(
                &original.repository_states[0],
                "typescript",
            ),
            changed_context
                .repository_enrichment_input_fingerprint(
                    &changed_context.repository_states[0],
                    "typescript",
                )
        );
        assert_eq!(
            original.repository_enrichment_input_fingerprint(
                &original.repository_states[0],
                "typescript",
            ),
            changed_unrelated
                .repository_enrichment_input_fingerprint(
                    &changed_unrelated.repository_states[0],
                    "typescript",
                )
        );
        assert_eq!(
            original.repository_enrichment_input_fingerprint(
                &original.repository_states[0],
                "rust",
            ),
            changed_context.repository_enrichment_input_fingerprint(
                &changed_context.repository_states[0],
                "rust",
            )
        );
    }
}
