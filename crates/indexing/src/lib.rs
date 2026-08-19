use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityId, Evidence, GrpcBindingCandidate,
    Observation, RepositoryFacts, RepositoryState,
};
use rayon::ThreadPool;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type AnalyzerError = Box<dyn Error + Send + Sync>;
const CORE_RULE_PACK_VERSION: &str = "5";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CanonicalRepositoryAnalysis {
    #[serde(default)]
    incomplete: bool,
    entities: Vec<EntityFact>,
    #[serde(default)]
    grpc_bindings: Vec<GrpcBindingCandidate>,
    observations: Vec<Observation>,
    diagnostics: Vec<AnalysisDiagnostic>,
}

pub struct AnalyzedRepository {
    pub facts: RepositoryFacts,
    pub cache: CacheStatus,
}

pub struct WorkspaceAnalysis {
    pub analysis_identity: String,
    pub repositories: Vec<AnalyzedRepository>,
    pub overrides: Vec<DependencyOverride>,
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

pub struct IndexerBuilder {
    analyzers: Vec<Box<dyn WorkspaceAnalyzer>>,
    cache_dir: PathBuf,
    workers: usize,
}

impl IndexerBuilder {
    pub fn new(cache_dir: PathBuf, workers: usize) -> Self {
        Self {
            analyzers: Vec::new(),
            cache_dir,
            workers,
        }
    }

    pub fn add_analyzer(mut self, analyzer: impl WorkspaceAnalyzer + 'static) -> Self {
        self.analyzers.push(Box::new(analyzer));
        self
    }

    pub fn build(mut self) -> Result<Indexer, AnalyzerError> {
        self.analyzers
            .sort_by_key(|analyzer| analyzer.metadata().id);
        if let Some(duplicate) = self
            .analyzers
            .windows(2)
            .find(|pair| pair[0].metadata().id == pair[1].metadata().id)
        {
            return Err(
                format!("duplicate analyzer identity {}", duplicate[0].metadata().id).into(),
            );
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.workers)
            .thread_name(|index| format!("beholder-index-{index}"))
            .build()?;
        Ok(Indexer {
            analyzers: self.analyzers,
            cache_dir: self.cache_dir,
            pool,
            repository_cache: Mutex::new(BTreeMap::new()),
        })
    }
}

pub struct Indexer {
    analyzers: Vec<Box<dyn WorkspaceAnalyzer>>,
    cache_dir: PathBuf,
    pool: ThreadPool,
    repository_cache: Mutex<BTreeMap<(String, String), Arc<CanonicalRepositoryAnalysis>>>,
}

impl Indexer {
    pub fn accepts(&self, path: &Path) -> bool {
        self.analyzers.iter().any(|analyzer| analyzer.accepts(path))
    }

    pub fn analysis_identity(&self, snapshot: &WorkspaceSnapshot) -> String {
        let active = self
            .analyzers
            .iter()
            .filter(|analyzer| {
                snapshot
                    .repositories
                    .iter()
                    .any(|repository| analyzer.is_active(repository))
            })
            .map(|analyzer| analyzer.metadata())
            .collect::<Vec<_>>();
        analysis_identity(&active)
    }

    pub fn analyze(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<WorkspaceAnalysis, AnalyzerError> {
        let mut merged = snapshot
            .repositories
            .iter()
            .map(|repository| {
                (
                    repository.state.repository.identity.clone(),
                    CanonicalRepositoryAnalysis::default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut overrides = Vec::new();
        let mut graphql_resolvers = Vec::new();
        let mut diagnostics = Vec::new();
        let mut cache = CacheStatistics::default();
        for analyzer in &self.analyzers {
            let contribution = self.pool.install(|| analyzer.analyze(snapshot))?;
            cache.memory_hits += contribution.cache.memory_hits;
            cache.disk_hits += contribution.cache.disk_hits;
            cache.misses += contribution.cache.misses;
            overrides.extend(contribution.overrides);
            graphql_resolvers.extend(contribution.graphql_resolvers);
            diagnostics.extend(contribution.diagnostics);
            for repository in contribution.repositories {
                let analysis = merged.get_mut(&repository.repository).ok_or_else(|| {
                    format!(
                        "analyzer returned unknown repository {}",
                        repository.repository
                    )
                })?;
                analysis.incomplete |= repository.completeness == AnalysisCompleteness::Incomplete;
                extend_unique(&mut analysis.entities, repository.entities);
                extend_unique(&mut analysis.grpc_bindings, repository.grpc_bindings);
                extend_unique(&mut analysis.observations, repository.observations);
                extend_unique(&mut analysis.diagnostics, repository.diagnostics);
            }
        }
        bind_graphql_resolvers(&mut merged, graphql_resolvers);

        let mut repositories = Vec::new();
        for repository in &snapshot.repositories {
            let identity = &repository.state.repository.identity;
            let analysis = merged
                .remove(identity)
                .ok_or_else(|| format!("missing analysis for repository {identity}"))?;
            let metadata = self
                .analyzers
                .iter()
                .filter(|analyzer| analyzer.is_active(repository))
                .map(|analyzer| analyzer.metadata())
                .collect::<Vec<_>>();
            let analysis_identity = analysis_identity(&metadata);
            let (analysis, status) = self.cached_repository(
                &repository.state.fingerprint,
                &analysis_identity,
                analysis,
            )?;
            diagnostics.extend(
                analysis
                    .diagnostics
                    .iter()
                    .cloned()
                    .map(|diagnostic| (identity.clone(), diagnostic)),
            );
            repositories.push(AnalyzedRepository {
                facts: RepositoryFacts {
                    state: repository.state.clone(),
                    analysis_identity,
                    incomplete: analysis.incomplete,
                    diagnostics: analysis.diagnostics.clone(),
                    entities: analysis.entities.clone(),
                    grpc_bindings: analysis.grpc_bindings.clone(),
                    observations: analysis.observations.clone(),
                },
                cache: status,
            });
        }
        Ok(WorkspaceAnalysis {
            analysis_identity: self.analysis_identity(snapshot),
            repositories,
            overrides,
            diagnostics,
            cache,
        })
    }

    pub fn clear_cache(&self) -> Result<(), AnalyzerError> {
        for analyzer in &self.analyzers {
            analyzer.clear_cache()?;
        }
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        self.repository_cache.lock().unwrap().clear();
        Ok(())
    }

    fn cached_repository(
        &self,
        fingerprint: &str,
        analysis_identity: &str,
        analysis: CanonicalRepositoryAnalysis,
    ) -> Result<(Arc<CanonicalRepositoryAnalysis>, CacheStatus), AnalyzerError> {
        let key = (fingerprint.to_owned(), analysis_identity.to_owned());
        if let Some(analysis) = self.repository_cache.lock().unwrap().get(&key) {
            return Ok((Arc::clone(analysis), CacheStatus::Memory));
        }
        let encoded_identity = analysis_identity
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = self
            .cache_dir
            .join("repository")
            .join("semantic")
            .join(encoded_identity)
            .join(format!("{fingerprint}.json"));
        if let Ok(file) = File::open(&path)
            && let Ok(analysis) = serde_json::from_reader(BufReader::new(file))
        {
            let analysis = Arc::new(analysis);
            self.repository_cache
                .lock()
                .unwrap()
                .insert(key, Arc::clone(&analysis));
            return Ok((analysis, CacheStatus::Disk));
        }
        let analysis = Arc::new(analysis);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(file) = File::create(path)
        {
            let mut writer = BufWriter::new(file);
            let _ = serde_json::to_writer(&mut writer, analysis.as_ref());
            let _ = writer.flush();
        }
        self.repository_cache
            .lock()
            .unwrap()
            .insert(key, Arc::clone(&analysis));
        Ok((analysis, CacheStatus::Miss))
    }
}

fn analysis_identity(metadata: &[AnalyzerMetadata]) -> String {
    let analyzers = metadata
        .iter()
        .map(|metadata| format!("{}:{}", metadata.id, metadata.version))
        .collect::<Vec<_>>();
    format!(
        "{}:core-rules:{CORE_RULE_PACK_VERSION}",
        if analyzers.is_empty() {
            "none".into()
        } else {
            analyzers.join(":")
        }
    )
}

fn extend_unique<T: PartialEq>(target: &mut Vec<T>, source: Vec<T>) {
    for value in source {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

fn bind_graphql_resolvers(
    repositories: &mut BTreeMap<String, CanonicalRepositoryAnalysis>,
    bindings: Vec<GraphqlResolverCandidate>,
) {
    for binding in bindings {
        let Some(repository) = repositories.get_mut(&binding.repository) else {
            continue;
        };
        let fields = repository
            .entities
            .iter()
            .filter(|entity| entity.kind == beholder_domain::EntityKind::GraphqlField)
            .filter_map(|entity| {
                let path = entity.id.as_str().strip_prefix("graphql-field://")?;
                let (parent, field) = path.split_once('/')?;
                Some(((parent, field), entity.id.as_str()))
            })
            .collect::<BTreeMap<_, _>>();
        let field = binding
            .parent
            .as_deref()
            .and_then(|parent| fields.get(&(parent, binding.field.as_str())).copied())
            .or_else(|| {
                let mut matches = fields
                    .iter()
                    .filter(|((_, name), _)| *name == binding.field)
                    .map(|(_, id)| *id);
                let field = matches.next()?;
                matches.next().is_none().then_some(field)
            });
        if let Some(field) = field {
            repository.observations.push(Observation::dependency(
                field,
                beholder_domain::DependencyRelation::ResolvedBy,
                binding.resolver,
                binding.evidence,
            ));
        }
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
    use beholder_domain::{EntityKind, LogicalRepository, RepositoryState};

    struct FakeAnalyzer {
        id: &'static str,
    }

    impl WorkspaceAnalyzer for FakeAnalyzer {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: self.id.into(),
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
                    completeness: AnalysisCompleteness::Complete,
                    entities: vec![EntityFact {
                        id: format!("rust-function://{}", self.id).into(),
                        kind: EntityKind::Callable,
                        metadata: None,
                    }],
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

    fn snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
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
        }
    }

    fn cache_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("beholder-indexer-{}-{name}", std::process::id()))
    }

    #[test]
    fn builder_installs_analyzer_for_workspace_analysis() {
        let cache_dir = cache_dir("installed");
        let indexer = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .unwrap();

        let analysis = indexer.analyze(&snapshot()).unwrap();

        assert!(indexer.accepts(Path::new("src/input.fake")));
        assert_eq!(analysis.repositories[0].facts.entities.len(), 1);
        assert_eq!(analysis.cache.misses, 1);
        assert_eq!(
            indexer.analyze(&snapshot()).unwrap().repositories[0].cache,
            CacheStatus::Memory
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn builder_rejects_duplicate_analyzer_identities() {
        let error = IndexerBuilder::new(cache_dir("duplicate"), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .err()
            .unwrap();

        assert_eq!(error.to_string(), "duplicate analyzer identity fake");
    }

    #[test]
    fn analyzer_registration_order_does_not_change_output() {
        let first_dir = cache_dir("order-first");
        let second_dir = cache_dir("order-second");
        let first = IndexerBuilder::new(first_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "b" })
            .add_analyzer(FakeAnalyzer { id: "a" })
            .build()
            .unwrap()
            .analyze(&snapshot())
            .unwrap();
        let second = IndexerBuilder::new(second_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "a" })
            .add_analyzer(FakeAnalyzer { id: "b" })
            .build()
            .unwrap()
            .analyze(&snapshot())
            .unwrap();

        assert_eq!(first.analysis_identity, second.analysis_identity);
        assert_eq!(first.repositories[0].facts, second.repositories[0].facts);
        let _ = fs::remove_dir_all(first_dir);
        let _ = fs::remove_dir_all(second_dir);
    }
}
