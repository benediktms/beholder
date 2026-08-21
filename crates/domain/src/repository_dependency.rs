use crate::{DependencyOverride, RepositoryFacts};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RepositoryDependencyKind {
    SemanticObservation,
    ResolutionOverride,
    ProjectReference,
    WorkspaceMember,
    PathDependency,
    CompilerDiscovered,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RepositoryDependencyEvidence {
    pub kind: RepositoryDependencyKind,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryDependency {
    pub from: String,
    pub to: String,
    pub kinds: BTreeSet<RepositoryDependencyKind>,
    pub evidence: BTreeSet<RepositoryDependencyEvidence>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepositoryDependencyGraph {
    repositories: BTreeSet<String>,
    outgoing: BTreeMap<String, BTreeSet<String>>,
    incoming: BTreeMap<String, BTreeSet<String>>,
    dependencies: BTreeMap<String, BTreeMap<String, RepositoryDependency>>,
}

impl RepositoryDependencyGraph {
    pub fn new<I, R>(repositories: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = R>,
        R: Into<String>,
    {
        let repositories = repositories
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<_>>();
        if repositories
            .iter()
            .any(|repository| repository.trim().is_empty())
        {
            return Err("repository dependency graph identities must not be empty".into());
        }
        Ok(Self {
            repositories,
            ..Self::default()
        })
    }

    pub fn from_baseline(
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
    ) -> Result<Self, String> {
        let mut graph = Self::new(
            repositories
                .iter()
                .map(|repository| repository.state.repository.identity.clone()),
        )?;
        let mut owners = BTreeMap::new();
        for repository in repositories {
            for entity in &repository.entities {
                owners.insert(
                    entity.id.as_str(),
                    repository.state.repository.identity.as_str(),
                );
            }
        }
        let repository_prefixes = graph
            .repositories
            .iter()
            .map(|repository| (format!("repo://{repository}/"), repository.clone()))
            .collect::<Vec<_>>();
        let owner = |entity: &str| {
            owners.get(entity).copied().or_else(|| {
                repository_prefixes
                    .iter()
                    .filter(|(prefix, _)| entity.starts_with(prefix))
                    .map(|(_, repository)| repository.as_str())
                    .max_by_key(|repository| repository.len())
            })
        };
        let observations = repositories.iter().flat_map(|repository| {
            repository
                .observations
                .iter()
                .filter(|observation| observation.relation.dependency().is_some())
        });
        for observation in observations {
            if let (Some(from), Some(to)) =
                (owner(observation.from.as_str()), owner(observation.to.as_str()))
            {
                graph
                    .add_dependency(
                        from,
                        to,
                        RepositoryDependencyKind::SemanticObservation,
                        observation.evidence.as_str(),
                    )?;
            }
        }
        for override_ in overrides {
            if let (Some(from), Some(to)) = (
                owner(override_.from.as_str()),
                owner(override_.resolved_to.as_str()),
            ) {
                graph
                    .add_dependency(
                        from,
                        to,
                        RepositoryDependencyKind::ResolutionOverride,
                        override_.evidence.as_str(),
                    )?;
            }
        }
        Ok(graph)
    }

    pub fn add_dependency(
        &mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        kind: RepositoryDependencyKind,
        evidence: impl Into<String>,
    ) -> Result<(), String> {
        let from = from.into();
        let to = to.into();
        if !self.repositories.contains(&from) {
            return Err(format!("unknown dependency source repository {from}"));
        }
        if !self.repositories.contains(&to) {
            return Err(format!("unknown dependency target repository {to}"));
        }
        if from == to {
            return Ok(());
        }
        self.outgoing
            .entry(from.clone())
            .or_default()
            .insert(to.clone());
        self.incoming
            .entry(to.clone())
            .or_default()
            .insert(from.clone());
        let dependency = self
            .dependencies
            .entry(from.clone())
            .or_default()
            .entry(to.clone())
            .or_insert_with(|| RepositoryDependency {
                from,
                to,
                kinds: BTreeSet::new(),
                evidence: BTreeSet::new(),
            });
        dependency.kinds.insert(kind);
        dependency.evidence.insert(RepositoryDependencyEvidence {
            kind,
            detail: evidence.into(),
        });
        Ok(())
    }

    pub fn direct_context(&self, target: &str) -> impl Iterator<Item = &str> {
        self.outgoing
            .get(target)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    pub fn repositories(&self) -> impl Iterator<Item = &str> {
        self.repositories.iter().map(String::as_str)
    }

    pub fn contains_dependency(&self, from: &str, to: &str) -> bool {
        self.outgoing
            .get(from)
            .is_some_and(|dependencies| dependencies.contains(to))
    }

    pub fn affected_targets(&self, context: &str) -> impl Iterator<Item = &str> {
        self.incoming
            .get(context)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    pub fn dependencies_from(
        &self,
        target: &str,
    ) -> impl Iterator<Item = &RepositoryDependency> {
        self.dependencies
            .get(target)
            .into_iter()
            .flat_map(|dependencies| dependencies.values())
    }

    pub fn context_map(&self) -> BTreeMap<String, Vec<String>> {
        self.outgoing
            .iter()
            .map(|(target, contexts)| (target.clone(), contexts.iter().cloned().collect()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DependencyRelation, LogicalRepository, Observation, RepositoryState};

    fn repository(identity: &str, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: identity.into(),
                },
                head: None,
                fingerprint: identity.into(),
            },
            analysis_identity: "baseline".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations,
        }
    }

    #[test]
    fn baseline_graph_selects_direct_context_and_reverse_dependents() {
        let repositories = vec![
            repository(
                "example/a",
                vec![Observation::dependency(
                    "repo://example/a/typescript/caller",
                    DependencyRelation::Calls,
                    "repo://example/b/typescript/callee",
                    "src/index.ts:1",
                )],
            ),
            repository("example/b", Vec::new()),
            repository("example/c", Vec::new()),
        ];

        let graph = RepositoryDependencyGraph::from_baseline(&repositories, &[]).unwrap();

        assert_eq!(graph.direct_context("example/a").collect::<Vec<_>>(), ["example/b"]);
        assert_eq!(graph.affected_targets("example/b").collect::<Vec<_>>(), ["example/a"]);
        assert!(graph.direct_context("example/c").next().is_none());
    }

    #[test]
    fn dependency_evidence_collapses_into_one_scheduling_edge() {
        let mut graph = RepositoryDependencyGraph::new(["example/a", "example/b"]).unwrap();
        graph
            .add_dependency(
                "example/a",
                "example/b",
                RepositoryDependencyKind::SemanticObservation,
                "src/first.ts:1",
            )
            .unwrap();
        graph
            .add_dependency(
                "example/a",
                "example/b",
                RepositoryDependencyKind::ProjectReference,
                "tsconfig.json",
            )
            .unwrap();

        let dependency = graph.dependencies_from("example/a").next().unwrap();
        assert_eq!(graph.direct_context("example/a").count(), 1);
        assert!(graph.contains_dependency("example/a", "example/b"));
        assert_eq!(dependency.kinds.len(), 2);
        assert_eq!(dependency.evidence.len(), 2);
    }
}
