use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityId, Evidence, GrpcBindingCandidate,
    Observation, RepositoryState,
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
};

pub type AnalyzerError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryInput {
    pub path: PathBuf,
    pub content: Arc<[u8]>,
    pub kind: InputKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum InputKind {
    #[default]
    Source,
    ProtobufDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositorySnapshot {
    pub base: PathBuf,
    pub state: RepositoryState,
    pub inputs: Vec<RepositoryInput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    pub name: String,
    pub repositories: Vec<RepositorySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerMetadata {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStatistics {
    pub memory_hits: usize,
    pub disk_hits: usize,
    pub misses: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnalysisCompleteness {
    #[default]
    Complete,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryContribution {
    pub repository: String,
    pub completeness: AnalysisCompleteness,
    pub entities: Vec<EntityFact>,
    pub grpc_bindings: Vec<GrpcBindingCandidate>,
    pub observations: Vec<Observation>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphqlResolverCandidate {
    pub repository: String,
    pub field: String,
    pub parent: Option<String>,
    pub resolver: EntityId,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerContribution {
    pub metadata: AnalyzerMetadata,
    pub active_repositories: Vec<String>,
    pub repositories: Vec<RepositoryContribution>,
    pub overrides: Vec<DependencyOverride>,
    pub graphql_resolvers: Vec<GraphqlResolverCandidate>,
    pub diagnostics: Vec<(String, AnalysisDiagnostic)>,
    pub cache: CacheStatistics,
}

pub trait WorkspaceAnalyzer: Send + Sync {
    fn metadata(&self) -> AnalyzerMetadata;
    fn accepts(&self, path: &Path) -> bool;
    fn is_active(&self, repository: &RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| self.accepts(&input.path))
    }
    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError>;
    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        Ok(())
    }
}

pub fn accepted_paths<'a>(
    analyzers: &[&dyn WorkspaceAnalyzer],
    paths: impl IntoIterator<Item = &'a Path>,
) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|path| analyzers.iter().any(|analyzer| analyzer.accepts(path)))
        .map(Path::to_path_buf)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};

    struct FakeAnalyzer;

    impl WorkspaceAnalyzer for FakeAnalyzer {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: "fake".into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            path.extension()
                .is_some_and(|extension| extension == "fake")
        }

        fn analyze(
            &self,
            snapshot: &WorkspaceSnapshot,
        ) -> Result<AnalyzerContribution, AnalyzerError> {
            Ok(AnalyzerContribution {
                metadata: self.metadata(),
                active_repositories: snapshot
                    .repositories
                    .iter()
                    .map(|repository| repository.state.repository.identity.clone())
                    .collect(),
                repositories: vec![RepositoryContribution {
                    repository: snapshot.repositories[0].state.repository.identity.clone(),
                    completeness: AnalysisCompleteness::Incomplete,
                    entities: Vec::new(),
                    grpc_bindings: Vec::new(),
                    observations: Vec::new(),
                    diagnostics: Vec::new(),
                }],
                overrides: Vec::new(),
                graphql_resolvers: Vec::new(),
                diagnostics: Vec::new(),
                cache: CacheStatistics {
                    misses: 1,
                    ..Default::default()
                },
            })
        }
    }

    #[test]
    fn fake_analyzer_selects_inputs_and_transports_canonical_contributions() {
        let analyzer = FakeAnalyzer;
        let paths = [Path::new("src/ignored.rs"), Path::new("src/input.fake")];
        assert_eq!(
            accepted_paths(&[&analyzer], paths),
            vec![PathBuf::from("src/input.fake")]
        );

        let contribution = analyzer
            .analyze(&WorkspaceSnapshot {
                name: "test".into(),
                repositories: vec![RepositorySnapshot {
                    base: PathBuf::from("repo"),
                    state: RepositoryState {
                        repository: LogicalRepository {
                            identity: "example/repo".into(),
                        },
                        head: None,
                        fingerprint: "state".into(),
                    },
                    inputs: vec![RepositoryInput {
                        path: PathBuf::from("src/input.fake"),
                        content: Arc::from(&b"input"[..]),
                        kind: InputKind::Source,
                    }],
                }],
            })
            .unwrap();

        assert_eq!(contribution.metadata.id, "fake");
        assert_eq!(contribution.active_repositories, ["example/repo"]);
        assert_eq!(
            contribution.repositories[0].completeness,
            AnalysisCompleteness::Incomplete
        );
        assert_eq!(contribution.cache.misses, 1);
    }
}
