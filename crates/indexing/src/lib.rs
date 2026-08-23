use beholder_domain::{
    AnalysisDiagnostic, DependencyOverride, EntityFact, EntityId, Evidence, GrpcBindingCandidate,
    Observation, RepositoryDependencyCandidate, RepositoryFacts, RepositoryState,
};
use rayon::ThreadPool;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs::{self, File},
    future::Future,
    hash::Hash,
    io::{BufReader, BufWriter, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type AnalyzerError = Box<dyn Error + Send + Sync>;
const CORE_RULE_PACK_VERSION: &str = "6";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryInput {
    pub path: PathBuf,
    pub content: Arc<[u8]>,
    pub kind: InputKind,
}

/// An immutable input declared by a compiler-backed analyzer.
///
/// The input kind describes why the compiler result depends on these bytes;
/// analyzer and compiler versions remain part of analyzer metadata rather than
/// being represented as synthetic files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisInput {
    pub path: PathBuf,
    pub content: Arc<[u8]>,
    pub kind: AnalysisInputKind,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AnalysisInputKind {
    Source,
    Configuration,
    Dependency,
    Toolchain,
    Environment,
}

impl AnalysisInput {
    pub fn from_repository(input: &RepositoryInput, kind: AnalysisInputKind) -> Self {
        Self {
            path: input.path.clone(),
            content: Arc::clone(&input.content),
            kind,
        }
    }
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
pub struct EnrichmentSnapshot {
    pub target_repository: String,
    pub workspace: WorkspaceSnapshot,
}

impl EnrichmentSnapshot {
    pub fn target(&self) -> Option<&RepositorySnapshot> {
        self.workspace
            .repositories
            .iter()
            .find(|repository| repository.state.repository.identity == self.target_repository)
    }

    pub fn contexts(&self) -> impl Iterator<Item = &RepositorySnapshot> {
        self.workspace
            .repositories
            .iter()
            .filter(|repository| repository.state.repository.identity != self.target_repository)
    }
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct CanonicalRepositoryAnalysis {
    #[serde(default)]
    incomplete: bool,
    entities: Vec<EntityFact>,
    #[serde(default)]
    grpc_bindings: Vec<GrpcBindingCandidate>,
    observations: Vec<Observation>,
    diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    analyzers: BTreeMap<String, CanonicalAnalyzerAnalysis>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct CanonicalAnalyzerAnalysis {
    entities: Vec<EntityFact>,
    observations: Vec<Observation>,
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
    pub repository_dependencies: Vec<RepositoryDependencyCandidate>,
    pub cache: CacheStatistics,
}

pub trait WorkspaceAnalyzer: Send + Sync {
    fn metadata(&self) -> AnalyzerMetadata;
    fn accepts(&self, path: &Path) -> bool;
    /// Declares the semantic role of an accepted input.
    ///
    /// Compiler-backed analyzers should override this for configuration,
    /// dependency, toolchain, and environment inputs. The declared role and
    /// immutable bytes can then be included in compiler cache identity.
    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        self.accepts(path).then_some(AnalysisInputKind::Source)
    }
    fn is_active(&self, repository: &RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| self.accepts(&input.path))
    }
    fn prepare(&self, snapshot: &WorkspaceSnapshot) -> AnalyzerPlan {
        AnalyzerPlan::without_plugins(self.metadata(), snapshot, |repository| {
            self.is_active(repository)
        })
    }
    /// Discovers analyzer-owned repository dependencies from immutable inputs.
    ///
    /// Language adapters retain ownership of manifest parsing and return only
    /// normalized scheduling evidence to the generic dependency graph.
    fn repository_dependencies(
        &self,
        _snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<RepositoryDependencyCandidate>, AnalyzerError> {
        Ok(Vec::new())
    }
    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError>;
    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError> {
        let plan = self.prepare(snapshot);
        self.analyze_prepared(snapshot, &plan)
    }
    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        Ok(())
    }
}

pub type EnrichmentFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AnalyzerContribution, AnalyzerError>> + Send + 'a>>;

pub trait WorkspaceEnricher: Send + Sync {
    fn metadata(&self) -> AnalyzerMetadata;
    fn accepts(&self, path: &Path) -> bool;
    /// Declares the semantic role of an accepted enrichment input.
    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        self.accepts(path).then_some(AnalysisInputKind::Source)
    }
    fn analysis_inputs(&self, repository: &RepositorySnapshot) -> Vec<AnalysisInput> {
        repository
            .inputs
            .iter()
            .filter_map(|input| {
                self.analysis_input_kind(&input.path)
                    .map(|kind| AnalysisInput::from_repository(input, kind))
            })
            .collect()
    }
    /// Synthetic toolchain and environment inputs which affect every target
    /// handled by this enricher.
    fn identity_inputs(&self) -> Vec<AnalysisInput> {
        Vec::new()
    }
    fn is_active(&self, repository: &RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| self.accepts(&input.path))
    }
    fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a>;
    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        Ok(())
    }
}

pub trait AnalyzerLanguage: 'static {
    type Analysis;
    type Syntax;
    type Repository;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginMetadata {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginActivation {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivePlugin {
    pub metadata: PluginMetadata,
    pub activation: PluginActivation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActivePlugins {
    plugins: BTreeMap<String, ActivePlugin>,
}

impl ActivePlugins {
    pub fn identity(&self) -> String {
        encode_identity(
            self.plugins
                .values()
                .map(|plugin| (&plugin.metadata.id, &plugin.metadata.version)),
        )
    }

    pub fn plugins(&self) -> impl Iterator<Item = &ActivePlugin> {
        self.plugins.values()
    }

    fn contains(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerRepositoryPlan {
    pub repository: String,
    pub analysis: AnalyzerMetadata,
    pub source_plugins: String,
    pub active_plugins: ActivePlugins,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerPlan {
    pub analyzer: AnalyzerMetadata,
    repositories: BTreeMap<String, AnalyzerRepositoryPlan>,
    cached_repositories: BTreeMap<String, Arc<CanonicalAnalyzerAnalysis>>,
}

impl AnalyzerPlan {
    pub fn without_plugins(
        analyzer: AnalyzerMetadata,
        snapshot: &WorkspaceSnapshot,
        is_active: impl Fn(&RepositorySnapshot) -> bool,
    ) -> Self {
        let repositories = snapshot
            .repositories
            .iter()
            .filter(|repository| is_active(repository))
            .map(|repository| {
                let identity = repository.state.repository.identity.clone();
                (
                    identity.clone(),
                    AnalyzerRepositoryPlan {
                        repository: identity,
                        analysis: analyzer.clone(),
                        source_plugins: String::new(),
                        active_plugins: ActivePlugins::default(),
                    },
                )
            })
            .collect();
        Self {
            analyzer,
            repositories,
            cached_repositories: BTreeMap::new(),
        }
    }

    pub fn from_repositories(
        analyzer: AnalyzerMetadata,
        repositories: impl IntoIterator<Item = AnalyzerRepositoryPlan>,
    ) -> Self {
        Self {
            analyzer,
            repositories: repositories
                .into_iter()
                .map(|repository| (repository.repository.clone(), repository))
                .collect(),
            cached_repositories: BTreeMap::new(),
        }
    }

    pub fn repository(&self, identity: &str) -> Option<&AnalyzerRepositoryPlan> {
        self.repositories.get(identity)
    }

    pub fn repositories(&self) -> impl Iterator<Item = &AnalyzerRepositoryPlan> {
        self.repositories.values()
    }

    pub fn cached_repository(&self, identity: &str) -> Option<RepositoryFactsView<'_>> {
        self.cached_repositories
            .get(identity)
            .map(|analysis| RepositoryFactsView {
                entities: &analysis.entities,
                observations: &analysis.observations,
            })
    }

    fn cache_repository(&mut self, identity: String, analysis: Arc<CanonicalAnalyzerAnalysis>) {
        self.cached_repositories.insert(identity, analysis);
    }
}

pub struct WorkspaceAnalysisPlan {
    analyzers: Vec<AnalyzerPlan>,
    analysis_identity: String,
    repository_identities: BTreeMap<String, String>,
    repository_fingerprints: BTreeMap<String, String>,
    cached_repositories: BTreeMap<String, (Arc<CanonicalRepositoryAnalysis>, CacheStatus)>,
}

impl WorkspaceAnalysisPlan {
    pub fn analysis_identity(&self) -> &str {
        &self.analysis_identity
    }

    pub fn repository_enrichment_identities(&self) -> BTreeMap<String, String> {
        self.repository_identities
            .iter()
            .map(|(repository, identity)| {
                (
                    repository.clone(),
                    format!(
                        "{}:{identity}:core-rules:{CORE_RULE_PACK_VERSION}",
                        identity.len()
                    ),
                )
            })
            .collect()
    }

    fn analyzer(&self, index: usize) -> &AnalyzerPlan {
        &self.analyzers[index]
    }

    fn repository_identity(&self, repository: &str) -> Option<&str> {
        self.repository_identities
            .get(repository)
            .map(String::as_str)
    }

    fn validate(&self, snapshot: &WorkspaceSnapshot) -> Result<(), AnalyzerError> {
        let snapshots = snapshot
            .repositories
            .iter()
            .map(|repository| {
                (
                    repository.state.repository.identity.clone(),
                    repository.state.fingerprint.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if snapshots.len() != snapshot.repositories.len() {
            return Err("workspace snapshot contains duplicate repository identities".into());
        }
        if snapshots != self.repository_fingerprints {
            return Err("prepared analysis plan does not match workspace repositories".into());
        }
        if self.analysis_identity != workspace_analysis_identity(&self.repository_identities) {
            return Err("prepared workspace analysis identity is inconsistent".into());
        }
        Ok(())
    }
}

pub struct SourceRecognitionInput<'a, L: AnalyzerLanguage> {
    pub path: &'a Path,
    pub text: &'a str,
    pub syntax: &'a L::Syntax,
}

pub trait SourceRecognizer<L: AnalyzerLanguage>: Send + Sync {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, L>,
        analysis: &mut L::Analysis,
    ) -> Result<(), AnalyzerError>;
}

pub struct RepositoryFactsView<'a> {
    pub entities: &'a [EntityFact],
    pub observations: &'a [Observation],
}

#[derive(Default)]
pub struct RepositoryEnrichment {
    pub entities: Vec<EntityFact>,
    pub grpc_bindings: Vec<GrpcBindingCandidate>,
    pub observations: Vec<Observation>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

pub trait RepositoryEnricher<L: AnalyzerLanguage>: Send + Sync {
    fn enrich(
        &self,
        repository: &L::Repository,
        base: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError>;
}

pub trait Plugin<L: AnalyzerLanguage>: Send + Sync + 'static {
    fn metadata(&self) -> PluginMetadata;
    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation>;
    fn install(&self, builder: &mut LanguageAnalyzerBuilder<L>);
}

struct InstalledPlugin<L: AnalyzerLanguage> {
    metadata: PluginMetadata,
    plugin: Box<dyn Plugin<L>>,
}

struct InstalledSourceRecognizer<L: AnalyzerLanguage> {
    plugin: PluginMetadata,
    recognizer: Box<dyn SourceRecognizer<L>>,
}

struct InstalledRepositoryEnricher<L: AnalyzerLanguage> {
    plugin: PluginMetadata,
    enricher: Box<dyn RepositoryEnricher<L>>,
}

pub struct LanguageAnalyzerBuilder<L: AnalyzerLanguage> {
    plugins: Vec<InstalledPlugin<L>>,
    source_recognizers: Vec<InstalledSourceRecognizer<L>>,
    repository_enrichers: Vec<InstalledRepositoryEnricher<L>>,
    installing: Option<PluginMetadata>,
}

impl<L: AnalyzerLanguage> Default for LanguageAnalyzerBuilder<L> {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            source_recognizers: Vec::new(),
            repository_enrichers: Vec::new(),
            installing: None,
        }
    }
}

impl<L: AnalyzerLanguage> LanguageAnalyzerBuilder<L> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a plugin for this builder's language.
    ///
    /// ```compile_fail
    /// use beholder_indexing::{AnalyzerLanguage, LanguageAnalyzerBuilder, Plugin, PluginMetadata};
    ///
    /// struct Rust;
    /// impl AnalyzerLanguage for Rust {
    ///     type Analysis = ();
    ///     type Syntax = ();
    ///     type Repository = ();
    /// }
    /// struct TypeScript;
    /// impl AnalyzerLanguage for TypeScript {
    ///     type Analysis = ();
    ///     type Syntax = ();
    ///     type Repository = ();
    /// }
    /// struct TypeScriptPlugin;
    /// impl Plugin<TypeScript> for TypeScriptPlugin {
    ///     fn metadata(&self) -> PluginMetadata {
    ///         PluginMetadata { id: "typescript.example".into(), version: "1".into() }
    ///     }
    ///     fn activate(&self, _: &beholder_indexing::RepositorySnapshot) -> Option<beholder_indexing::PluginActivation> { None }
    ///     fn install(&self, _: &mut LanguageAnalyzerBuilder<TypeScript>) {}
    /// }
    ///
    /// let _ = LanguageAnalyzerBuilder::<Rust>::new().add_plugin(TypeScriptPlugin);
    /// ```
    pub fn add_plugin<P: Plugin<L>>(mut self, plugin: P) -> Self {
        let metadata = plugin.metadata();
        self.installing = Some(metadata.clone());
        plugin.install(&mut self);
        self.installing = None;
        self.plugins.push(InstalledPlugin {
            metadata,
            plugin: Box::new(plugin),
        });
        self
    }

    pub fn install_source_recognizer(&mut self, recognizer: impl SourceRecognizer<L> + 'static) {
        self.source_recognizers.push(InstalledSourceRecognizer {
            plugin: self
                .installing
                .clone()
                .expect("source recognizers must be installed by a plugin"),
            recognizer: Box::new(recognizer),
        });
    }

    pub fn install_repository_enricher(&mut self, enricher: impl RepositoryEnricher<L> + 'static) {
        self.repository_enrichers.push(InstalledRepositoryEnricher {
            plugin: self
                .installing
                .clone()
                .expect("repository enrichers must be installed by a plugin"),
            enricher: Box::new(enricher),
        });
    }

    pub fn build(mut self) -> Result<LanguageAnalyzer<L>, AnalyzerError> {
        self.plugins
            .sort_by(|left, right| left.metadata.id.cmp(&right.metadata.id));
        if let Some(duplicate) = self
            .plugins
            .windows(2)
            .find(|pair| pair[0].metadata.id == pair[1].metadata.id)
        {
            return Err(format!("duplicate plugin identity {}", duplicate[0].metadata.id).into());
        }
        self.source_recognizers
            .sort_by(|left, right| left.plugin.id.cmp(&right.plugin.id));
        self.repository_enrichers
            .sort_by(|left, right| left.plugin.id.cmp(&right.plugin.id));
        Ok(LanguageAnalyzer {
            plugins: self.plugins,
            source_recognizers: self.source_recognizers,
            repository_enrichers: self.repository_enrichers,
            language: PhantomData,
        })
    }
}

pub struct LanguageAnalyzer<L: AnalyzerLanguage> {
    plugins: Vec<InstalledPlugin<L>>,
    source_recognizers: Vec<InstalledSourceRecognizer<L>>,
    repository_enrichers: Vec<InstalledRepositoryEnricher<L>>,
    language: PhantomData<fn() -> L>,
}

impl<L: AnalyzerLanguage> LanguageAnalyzer<L> {
    pub fn identity(&self) -> String {
        encode_identity(
            self.plugins
                .iter()
                .map(|plugin| (&plugin.metadata.id, &plugin.metadata.version)),
        )
    }

    pub fn activate(
        &self,
        repository: &RepositorySnapshot,
        has_language_inputs: bool,
    ) -> ActivePlugins {
        if !has_language_inputs {
            return ActivePlugins::default();
        }
        ActivePlugins {
            plugins: self
                .plugins
                .iter()
                .filter_map(|installed| {
                    installed.plugin.activate(repository).map(|activation| {
                        (
                            installed.metadata.id.clone(),
                            ActivePlugin {
                                metadata: installed.metadata.clone(),
                                activation,
                            },
                        )
                    })
                })
                .collect(),
        }
    }

    pub fn activate_direct(&self, path: &Path) -> ActivePlugins {
        ActivePlugins {
            plugins: self
                .plugins
                .iter()
                .map(|installed| {
                    (
                        installed.metadata.id.clone(),
                        ActivePlugin {
                            metadata: installed.metadata.clone(),
                            activation: PluginActivation {
                                path: path.to_path_buf(),
                                reason: "direct source analysis".into(),
                            },
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn source_identity(&self, active: &ActivePlugins) -> String {
        let plugins = self
            .source_recognizers
            .iter()
            .filter(|installed| active.contains(&installed.plugin.id))
            .map(|installed| (&installed.plugin.id, &installed.plugin.version));
        encode_identity(plugins)
    }

    pub fn prepare_repository(
        &self,
        analyzer: AnalyzerMetadata,
        repository: &RepositorySnapshot,
        analyzer_active: bool,
        has_language_inputs: bool,
    ) -> Option<AnalyzerRepositoryPlan> {
        if !analyzer_active {
            return None;
        }
        let active_plugins = self.activate(repository, has_language_inputs);
        let source_plugins = self.source_identity(&active_plugins);
        let plugin_identity = active_plugins.identity();
        let analysis = AnalyzerMetadata {
            id: analyzer.id,
            version: if plugin_identity.is_empty() {
                analyzer.version
            } else {
                format!(
                    "{}:{}{}:{}",
                    analyzer.version.len(),
                    analyzer.version,
                    plugin_identity.len(),
                    plugin_identity
                )
            },
        };
        Some(AnalyzerRepositoryPlan {
            repository: repository.state.repository.identity.clone(),
            analysis,
            source_plugins,
            active_plugins,
        })
    }

    pub fn recognize(
        &self,
        input: SourceRecognitionInput<'_, L>,
        analysis: &mut L::Analysis,
        active: &ActivePlugins,
    ) -> Result<(), AnalyzerError> {
        for installed in &self.source_recognizers {
            if !active.contains(&installed.plugin.id) {
                continue;
            }
            installed.recognizer.recognize(
                SourceRecognitionInput {
                    path: input.path,
                    text: input.text,
                    syntax: input.syntax,
                },
                analysis,
            )?;
        }
        Ok(())
    }

    pub fn enrich(
        &self,
        repository: &L::Repository,
        base: RepositoryFactsView<'_>,
        active: &ActivePlugins,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let mut merged = RepositoryEnrichment::default();
        for installed in &self.repository_enrichers {
            if !active.contains(&installed.plugin.id) {
                continue;
            }
            let contribution = installed.enricher.enrich(
                repository,
                RepositoryFactsView {
                    entities: base.entities,
                    observations: base.observations,
                },
            )?;
            extend_unique(&mut merged.entities, contribution.entities);
            extend_unique(&mut merged.grpc_bindings, contribution.grpc_bindings);
            extend_unique(&mut merged.observations, contribution.observations);
            extend_unique(&mut merged.diagnostics, contribution.diagnostics);
        }
        Ok(merged)
    }
}

pub struct IndexerBuilder {
    analyzers: Vec<Box<dyn WorkspaceAnalyzer>>,
    enrichers: Vec<Box<dyn WorkspaceEnricher>>,
    cache_dir: PathBuf,
    workers: usize,
}

impl IndexerBuilder {
    pub fn new(cache_dir: PathBuf, workers: usize) -> Self {
        Self {
            analyzers: Vec::new(),
            enrichers: Vec::new(),
            cache_dir,
            workers,
        }
    }

    pub fn add_analyzer(mut self, analyzer: impl WorkspaceAnalyzer + 'static) -> Self {
        self.analyzers.push(Box::new(analyzer));
        self
    }

    pub fn add_enricher(mut self, analyzer: impl WorkspaceEnricher + 'static) -> Self {
        self.enrichers.push(Box::new(analyzer));
        self
    }

    pub fn build(mut self) -> Result<Indexer, AnalyzerError> {
        self.analyzers
            .sort_by_key(|analyzer| analyzer.metadata().id);
        self.enrichers
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
        if let Some(duplicate) = self
            .enrichers
            .windows(2)
            .find(|pair| pair[0].metadata().id == pair[1].metadata().id)
        {
            return Err(
                format!("duplicate enricher identity {}", duplicate[0].metadata().id).into(),
            );
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.workers)
            .thread_name(|index| format!("beholder-index-{index}"))
            .build()?;
        Ok(Indexer {
            analyzers: self.analyzers,
            enrichers: self.enrichers,
            cache_dir: self.cache_dir,
            pool,
            repository_cache: Mutex::new(BTreeMap::new()),
        })
    }
}

pub struct Indexer {
    analyzers: Vec<Box<dyn WorkspaceAnalyzer>>,
    enrichers: Vec<Box<dyn WorkspaceEnricher>>,
    cache_dir: PathBuf,
    pool: ThreadPool,
    repository_cache: Mutex<BTreeMap<(String, String), Arc<CanonicalRepositoryAnalysis>>>,
}

impl Indexer {
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn accepts(&self, path: &Path) -> bool {
        self.analyzers.iter().any(|analyzer| analyzer.accepts(path))
            || self.enrichers.iter().any(|enricher| enricher.accepts(path))
    }

    pub fn prepare(&self, snapshot: &WorkspaceSnapshot) -> WorkspaceAnalysisPlan {
        let mut analyzers = self
            .analyzers
            .iter()
            .map(|analyzer| analyzer.prepare(snapshot))
            .collect::<Vec<_>>();
        let repository_identities = snapshot
            .repositories
            .iter()
            .map(|repository| {
                let repository_id = repository.state.repository.identity.clone();
                let metadata = analyzers
                    .iter()
                    .filter_map(|analyzer| {
                        analyzer
                            .repository(&repository_id)
                            .map(|repository| repository.analysis.clone())
                    })
                    .collect::<Vec<_>>();
                (repository_id, repository_analysis_identity(&metadata))
            })
            .collect::<BTreeMap<_, _>>();
        let analysis_identity = workspace_analysis_identity(&repository_identities);
        let repository_fingerprints = snapshot
            .repositories
            .iter()
            .map(|repository| {
                (
                    repository.state.repository.identity.clone(),
                    repository.state.fingerprint.clone(),
                )
            })
            .collect();
        let cached_repositories = snapshot
            .repositories
            .iter()
            .filter_map(|repository| {
                let identity = &repository.state.repository.identity;
                let analysis_identity = repository_identities.get(identity)?;
                self.lookup_repository(&repository.state.fingerprint, analysis_identity)
                    .map(|cached| (identity.clone(), cached))
            })
            .collect::<BTreeMap<_, _>>();
        for analyzer in &mut analyzers {
            let identities = analyzer.repositories.keys().cloned().collect::<Vec<_>>();
            for identity in identities {
                if let Some((analysis, _)) = cached_repositories.get(&identity) {
                    let contribution = analysis
                        .analyzers
                        .get(&analyzer.analyzer.id)
                        .cloned()
                        .unwrap_or_default();
                    analyzer.cache_repository(identity, Arc::new(contribution));
                }
            }
        }
        WorkspaceAnalysisPlan {
            analyzers,
            analysis_identity,
            repository_identities,
            repository_fingerprints,
            cached_repositories,
        }
    }

    pub fn analysis_identity(&self, snapshot: &WorkspaceSnapshot) -> String {
        self.prepare(snapshot).analysis_identity
    }

    pub fn catalog_identity(&self) -> String {
        format!(
            "{}:core-rules:{CORE_RULE_PACK_VERSION}",
            repository_analysis_identity(
                &self
                    .analyzers
                    .iter()
                    .map(|analyzer| analyzer.metadata())
                    .collect::<Vec<_>>(),
            )
        )
    }

    pub fn enricher_is_active(&self, id: &str, repository: &RepositorySnapshot) -> bool {
        self.enrichers
            .iter()
            .find(|enricher| enricher.metadata().id == id)
            .is_some_and(|enricher| enricher.is_active(repository))
    }

    pub fn enrichment_catalog(&self) -> Vec<AnalyzerMetadata> {
        self.enrichers
            .iter()
            .map(|analyzer| analyzer.metadata())
            .collect()
    }

    pub fn enrichment_input_identities(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> BTreeMap<String, BTreeMap<String, String>> {
        self.enrichers
            .iter()
            .map(|enricher| {
                let metadata = enricher.metadata();
                let shared = enricher.identity_inputs();
                let repositories = snapshot
                    .repositories
                    .iter()
                    .map(|repository| {
                        let mut inputs = enricher.analysis_inputs(repository);
                        inputs.extend(shared.iter().cloned());
                        (
                            repository.state.repository.identity.clone(),
                            analysis_inputs_identity(inputs),
                        )
                    })
                    .collect();
                (metadata.id, repositories)
            })
            .collect()
    }

    pub async fn enrich(
        &self,
        snapshot: EnrichmentSnapshot,
        id: &str,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let analyzer = self
            .enrichers
            .iter()
            .find(|analyzer| analyzer.metadata().id == id)
            .ok_or_else(|| format!("unknown enricher identity {id}"))?;
        analyzer.enrich(snapshot).await
    }

    pub fn analyze(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<WorkspaceAnalysis, AnalyzerError> {
        let plan = self.prepare(snapshot);
        self.analyze_prepared(snapshot, &plan)
    }

    pub fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &WorkspaceAnalysisPlan,
    ) -> Result<WorkspaceAnalysis, AnalyzerError> {
        plan.validate(snapshot)?;
        if self.analyzers.len() != plan.analyzers.len() {
            return Err("prepared analysis plan does not match analyzer catalog".into());
        }
        let mut merged = snapshot
            .repositories
            .iter()
            .map(|repository| {
                let identity = repository.state.repository.identity.clone();
                (
                    identity.clone(),
                    plan.cached_repositories
                        .get(&identity)
                        .map(|(analysis, _)| analysis.as_ref().clone())
                        .unwrap_or_default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut overrides = Vec::new();
        let mut graphql_resolvers = Vec::new();
        let mut diagnostics = Vec::new();
        let mut repository_dependencies = Vec::new();
        let mut cache = CacheStatistics::default();
        for (index, analyzer) in self.analyzers.iter().enumerate() {
            let analyzer_plan = plan.analyzer(index);
            if analyzer_plan.analyzer != analyzer.metadata() {
                return Err(format!(
                    "prepared analysis plan does not match analyzer {}",
                    analyzer.metadata().id
                )
                .into());
            }
            repository_dependencies.extend(analyzer.repository_dependencies(snapshot)?);
            let contribution = self
                .pool
                .install(|| analyzer.analyze_prepared(snapshot, analyzer_plan))?;
            cache.memory_hits += contribution.cache.memory_hits;
            cache.disk_hits += contribution.cache.disk_hits;
            cache.misses += contribution.cache.misses;
            overrides.extend(contribution.overrides);
            graphql_resolvers.extend(contribution.graphql_resolvers);
            diagnostics.extend(contribution.diagnostics);
            let analyzer_id = contribution.metadata.id.clone();
            for repository in contribution.repositories {
                let analysis = merged.get_mut(&repository.repository).ok_or_else(|| {
                    format!(
                        "analyzer returned unknown repository {}",
                        repository.repository
                    )
                })?;
                analysis.incomplete |= repository.completeness == AnalysisCompleteness::Incomplete;
                let analyzer = analysis.analyzers.entry(analyzer_id.clone()).or_default();
                extend_unique(&mut analyzer.entities, repository.entities.clone());
                extend_unique(&mut analyzer.observations, repository.observations.clone());
                extend_unique(&mut analysis.entities, repository.entities);
                extend_unique(&mut analysis.grpc_bindings, repository.grpc_bindings);
                extend_unique(&mut analysis.observations, repository.observations);
                extend_unique(&mut analysis.diagnostics, repository.diagnostics);
            }
        }
        let mut repositories = Vec::new();
        for repository in &snapshot.repositories {
            let identity = &repository.state.repository.identity;
            let analysis = merged
                .remove(identity)
                .ok_or_else(|| format!("missing analysis for repository {identity}"))?;
            let analysis_identity = plan
                .repository_identity(identity)
                .ok_or_else(|| format!("missing analysis identity for repository {identity}"))?
                .to_owned();
            let (analysis, status) =
                if let Some((analysis, status)) = plan.cached_repositories.get(identity) {
                    (Arc::clone(analysis), *status)
                } else {
                    (
                        self.store_repository(
                            &repository.state.fingerprint,
                            &analysis_identity,
                            analysis,
                        ),
                        CacheStatus::Miss,
                    )
                };
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
        bind_graphql_resolvers(&mut repositories, graphql_resolvers);
        repository_dependencies.sort();
        repository_dependencies.dedup();
        Ok(WorkspaceAnalysis {
            analysis_identity: plan.analysis_identity.clone(),
            repositories,
            overrides,
            diagnostics,
            repository_dependencies,
            cache,
        })
    }

    pub fn clear_cache(&self) -> Result<(), AnalyzerError> {
        for analyzer in &self.analyzers {
            analyzer.clear_cache()?;
        }
        for enricher in &self.enrichers {
            enricher.clear_cache()?;
        }
        if !self.cache_dir.as_os_str().is_empty() && self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        self.repository_cache.lock().unwrap().clear();
        Ok(())
    }

    fn lookup_repository(
        &self,
        fingerprint: &str,
        analysis_identity: &str,
    ) -> Option<(Arc<CanonicalRepositoryAnalysis>, CacheStatus)> {
        let key = (fingerprint.to_owned(), analysis_identity.to_owned());
        if let Some(analysis) = self.repository_cache.lock().unwrap().get(&key) {
            return Some((Arc::clone(analysis), CacheStatus::Memory));
        }
        let path = self.repository_cache_path(fingerprint, analysis_identity);
        let file = File::open(path).ok()?;
        let analysis = serde_json::from_reader(BufReader::new(file)).ok()?;
        let analysis = Arc::new(analysis);
        self.repository_cache
            .lock()
            .unwrap()
            .insert(key, Arc::clone(&analysis));
        Some((analysis, CacheStatus::Disk))
    }

    fn store_repository(
        &self,
        fingerprint: &str,
        analysis_identity: &str,
        analysis: CanonicalRepositoryAnalysis,
    ) -> Arc<CanonicalRepositoryAnalysis> {
        let key = (fingerprint.to_owned(), analysis_identity.to_owned());
        let path = self.repository_cache_path(fingerprint, analysis_identity);
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
        analysis
    }

    fn repository_cache_path(&self, fingerprint: &str, analysis_identity: &str) -> PathBuf {
        let encoded_identity = analysis_identity
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.cache_dir
            .join("repository")
            .join("semantic")
            .join(encoded_identity)
            .join(format!("{fingerprint}.json"))
    }
}

fn analysis_inputs_identity(inputs: impl IntoIterator<Item = AnalysisInput>) -> String {
    let mut inputs = inputs.into_iter().collect::<Vec<_>>();
    inputs.sort_by(|left, right| {
        (&left.kind, &left.path, left.content.as_ref()).cmp(&(
            &right.kind,
            &right.path,
            right.content.as_ref(),
        ))
    });
    inputs.dedup();
    let mut digest = Sha256::new();
    for input in inputs {
        digest.update([analysis_input_kind_tag(input.kind)]);
        framed_digest(&mut digest, input.path.as_os_str().as_encoded_bytes());
        framed_digest(&mut digest, input.content.as_ref());
    }
    format!("{:x}", digest.finalize())
}

fn analysis_input_kind_tag(kind: AnalysisInputKind) -> u8 {
    match kind {
        AnalysisInputKind::Source => 0,
        AnalysisInputKind::Configuration => 1,
        AnalysisInputKind::Dependency => 2,
        AnalysisInputKind::Toolchain => 3,
        AnalysisInputKind::Environment => 4,
    }
}

fn framed_digest(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn repository_analysis_identity(metadata: &[AnalyzerMetadata]) -> String {
    if metadata.is_empty() {
        "none".into()
    } else {
        encode_identity(
            metadata
                .iter()
                .map(|metadata| (&metadata.id, &metadata.version)),
        )
    }
}

fn encode_identity<'a>(pairs: impl IntoIterator<Item = (&'a String, &'a String)>) -> String {
    let mut pairs = pairs.into_iter().collect::<Vec<_>>();
    pairs.sort();
    pairs.dedup();
    pairs
        .into_iter()
        .map(|(id, version)| format!("{}:{id}{}:{version}", id.len(), version.len()))
        .collect()
}

fn workspace_analysis_identity(repository_identities: &BTreeMap<String, String>) -> String {
    workspace_analysis_identity_with_rules(repository_identities, CORE_RULE_PACK_VERSION)
}

fn workspace_analysis_identity_with_rules(
    repository_identities: &BTreeMap<String, String>,
    rule_pack_version: &str,
) -> String {
    let repositories = repository_identities
        .iter()
        .map(|(repository, identity)| {
            format!(
                "{}:{repository}:{}:{identity}",
                repository.len(),
                identity.len()
            )
        })
        .collect::<Vec<_>>()
        .join(":");
    format!("repositories:{repositories}:core-rules:{rule_pack_version}")
}

fn extend_unique<T: Clone + Eq + Hash>(target: &mut Vec<T>, source: Vec<T>) {
    let mut seen = target.iter().cloned().collect::<HashSet<_>>();
    for value in source {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
}

fn bind_graphql_resolvers(
    repositories: &mut [AnalyzedRepository],
    bindings: Vec<GraphqlResolverCandidate>,
) {
    for binding in bindings {
        let Some(repository) = repositories
            .iter_mut()
            .find(|repository| repository.facts.state.repository.identity == binding.repository)
        else {
            continue;
        };
        let fields = repository
            .facts
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
            let observation = Observation::dependency(
                field,
                beholder_domain::DependencyRelation::ResolvedBy,
                binding.resolver,
                binding.evidence,
            );
            if !repository.facts.observations.contains(&observation) {
                repository.facts.observations.push(observation);
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeAnalyzer {
        id: &'static str,
    }

    struct ResolverAnalyzer {
        resolver: &'static str,
    }

    struct ScopedEnricher {
        environment: &'static [u8],
    }

    struct FakeLanguage;

    impl AnalyzerLanguage for FakeLanguage {
        type Analysis = Vec<String>;
        type Syntax = ();
        type Repository = ();
    }

    #[derive(Clone, Copy)]
    struct FakePlugin {
        id: &'static str,
    }

    #[derive(Clone)]
    struct SourceOnlyPlugin {
        activations: Arc<AtomicUsize>,
        version: &'static str,
        active: bool,
    }

    #[derive(Clone, Copy)]
    struct RepositoryOnlyPlugin {
        version: &'static str,
        active: bool,
    }

    impl Plugin<FakeLanguage> for FakePlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: self.id.into(),
                version: "1".into(),
            }
        }

        fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
            (self.id != "inactive").then(|| PluginActivation {
                path: repository.inputs[0].path.clone(),
                reason: "fake input".into(),
            })
        }

        fn install(&self, builder: &mut LanguageAnalyzerBuilder<FakeLanguage>) {
            builder.install_source_recognizer(*self);
            builder.install_repository_enricher(*self);
        }
    }

    impl SourceRecognizer<FakeLanguage> for FakePlugin {
        fn recognize(
            &self,
            _: SourceRecognitionInput<'_, FakeLanguage>,
            analysis: &mut Vec<String>,
        ) -> Result<(), AnalyzerError> {
            analysis.push(self.id.into());
            Ok(())
        }
    }

    impl RepositoryEnricher<FakeLanguage> for FakePlugin {
        fn enrich(
            &self,
            _: &(),
            _: RepositoryFactsView<'_>,
        ) -> Result<RepositoryEnrichment, AnalyzerError> {
            Ok(RepositoryEnrichment {
                entities: vec![EntityFact {
                    id: format!("rust-function://{}", self.id).into(),
                    kind: EntityKind::Callable,
                    metadata: None,
                }],
                ..Default::default()
            })
        }
    }

    impl Plugin<FakeLanguage> for SourceOnlyPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: "source-only".into(),
                version: self.version.into(),
            }
        }

        fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
            self.activations.fetch_add(1, Ordering::SeqCst);
            self.active.then(|| PluginActivation {
                path: repository.inputs[0].path.clone(),
                reason: "source evidence".into(),
            })
        }

        fn install(&self, builder: &mut LanguageAnalyzerBuilder<FakeLanguage>) {
            builder.install_source_recognizer(self.clone());
        }
    }

    impl SourceRecognizer<FakeLanguage> for SourceOnlyPlugin {
        fn recognize(
            &self,
            _: SourceRecognitionInput<'_, FakeLanguage>,
            _: &mut Vec<String>,
        ) -> Result<(), AnalyzerError> {
            Ok(())
        }
    }

    impl Plugin<FakeLanguage> for RepositoryOnlyPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: "repository-only".into(),
                version: self.version.into(),
            }
        }

        fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
            self.active.then(|| PluginActivation {
                path: repository.inputs[0].path.clone(),
                reason: "repository evidence".into(),
            })
        }

        fn install(&self, builder: &mut LanguageAnalyzerBuilder<FakeLanguage>) {
            builder.install_repository_enricher(*self);
        }
    }

    impl RepositoryEnricher<FakeLanguage> for RepositoryOnlyPlugin {
        fn enrich(
            &self,
            _: &(),
            _: RepositoryFactsView<'_>,
        ) -> Result<RepositoryEnrichment, AnalyzerError> {
            Ok(RepositoryEnrichment::default())
        }
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

        fn analyze_prepared(
            &self,
            snapshot: &WorkspaceSnapshot,
            plan: &AnalyzerPlan,
        ) -> Result<AnalyzerContribution, AnalyzerError> {
            let repositories = snapshot
                .repositories
                .iter()
                .filter(|repository| {
                    plan.cached_repository(&repository.state.repository.identity)
                        .is_none()
                })
                .map(|repository| RepositoryContribution {
                    repository: repository.state.repository.identity.clone(),
                    completeness: AnalysisCompleteness::Complete,
                    entities: vec![EntityFact {
                        id: format!("rust-function://{}", self.id).into(),
                        kind: EntityKind::Callable,
                        metadata: None,
                    }],
                    grpc_bindings: Vec::new(),
                    observations: Vec::new(),
                    diagnostics: Vec::new(),
                })
                .collect::<Vec<_>>();
            Ok(AnalyzerContribution {
                metadata: WorkspaceAnalyzer::metadata(self),
                active_repositories: snapshot
                    .repositories
                    .iter()
                    .map(|repository| repository.state.repository.identity.clone())
                    .collect(),
                cache: CacheStatistics {
                    misses: repositories.len(),
                    ..Default::default()
                },
                repositories,
                overrides: Vec::new(),
                graphql_resolvers: Vec::new(),
                diagnostics: Vec::new(),
            })
        }
    }

    impl WorkspaceAnalyzer for ResolverAnalyzer {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: "resolver".into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            path.extension()
                .is_some_and(|extension| extension == "fake")
        }

        fn analyze_prepared(
            &self,
            snapshot: &WorkspaceSnapshot,
            plan: &AnalyzerPlan,
        ) -> Result<AnalyzerContribution, AnalyzerError> {
            let identity = snapshot.repositories[0].state.repository.identity.clone();
            let repositories = plan
                .cached_repository(&identity)
                .is_none()
                .then(|| RepositoryContribution {
                    repository: identity.clone(),
                    completeness: AnalysisCompleteness::Complete,
                    entities: vec![EntityFact {
                        id: "graphql-field://Query/item".into(),
                        kind: EntityKind::GraphqlField,
                        metadata: None,
                    }],
                    grpc_bindings: Vec::new(),
                    observations: Vec::new(),
                    diagnostics: Vec::new(),
                })
                .into_iter()
                .collect();
            Ok(AnalyzerContribution {
                metadata: self.metadata(),
                active_repositories: vec![identity.clone()],
                repositories,
                overrides: Vec::new(),
                graphql_resolvers: vec![GraphqlResolverCandidate {
                    repository: identity,
                    field: "item".into(),
                    parent: Some("Query".into()),
                    resolver: self.resolver.into(),
                    evidence: "resolver evidence".into(),
                }],
                diagnostics: Vec::new(),
                cache: CacheStatistics::default(),
            })
        }
    }

    impl WorkspaceEnricher for FakeAnalyzer {
        fn metadata(&self) -> AnalyzerMetadata {
            WorkspaceAnalyzer::metadata(self)
        }

        fn accepts(&self, path: &Path) -> bool {
            WorkspaceAnalyzer::accepts(self, path)
        }

        fn enrich<'a>(&'a self, snapshot: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
            Box::pin(async move { self.analyze(&snapshot.workspace) })
        }
    }

    impl WorkspaceEnricher for ScopedEnricher {
        fn metadata(&self) -> AnalyzerMetadata {
            AnalyzerMetadata {
                id: "scoped".into(),
                version: "1".into(),
            }
        }

        fn accepts(&self, path: &Path) -> bool {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("fake" | "config")
            )
        }

        fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("fake") => Some(AnalysisInputKind::Source),
                Some("config") => Some(AnalysisInputKind::Configuration),
                _ => None,
            }
        }

        fn identity_inputs(&self) -> Vec<AnalysisInput> {
            vec![AnalysisInput {
                path: "$environment/SCOPED".into(),
                content: Arc::from(self.environment),
                kind: AnalysisInputKind::Environment,
            }]
        }

        fn enrich<'a>(&'a self, _: EnrichmentSnapshot) -> EnrichmentFuture<'a> {
            Box::pin(async { Err("not exercised".into()) })
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
    fn enrichment_input_identity_uses_only_declared_semantic_inputs() {
        let indexer = IndexerBuilder::new(cache_dir("scoped-enrichment-inputs"), 1)
            .add_enricher(ScopedEnricher {
                environment: b"one",
            })
            .build()
            .unwrap();
        let mut original = snapshot();
        original.repositories[0].inputs.extend([
            RepositoryInput {
                path: "compiler.config".into(),
                content: Arc::from(&b"configuration-one"[..]),
                kind: InputKind::Source,
            },
            RepositoryInput {
                path: "README.md".into(),
                content: Arc::from(&b"unrelated-one"[..]),
                kind: InputKind::Source,
            },
        ]);
        let original_identity = indexer.enrichment_input_identities(&original);
        let mut unrelated = original.clone();
        unrelated.repositories[0].inputs[2].content = Arc::from(&b"unrelated-two"[..]);
        unrelated.repositories[0].state.fingerprint = "repository-changed".into();
        let mut configured = original.clone();
        configured.repositories[0].inputs[1].content = Arc::from(&b"configuration-two"[..]);

        assert_eq!(
            original_identity,
            indexer.enrichment_input_identities(&unrelated)
        );
        assert_ne!(
            original_identity,
            indexer.enrichment_input_identities(&configured)
        );

        let changed_environment = IndexerBuilder::new(cache_dir("changed-environment"), 1)
            .add_enricher(ScopedEnricher {
                environment: b"two",
            })
            .build()
            .unwrap();
        assert_ne!(
            original_identity,
            changed_environment.enrichment_input_identities(&original)
        );
    }

    #[test]
    fn rust_feature_target_and_compiler_flags_change_semantic_identity() {
        let identity = |features: &'static [u8], target: &'static [u8], flags: &'static [u8]| {
            analysis_inputs_identity([
                AnalysisInput {
                    path: "$environment/BEHOLDER_RUST_FEATURES".into(),
                    content: Arc::from(features),
                    kind: AnalysisInputKind::Environment,
                },
                AnalysisInput {
                    path: "$environment/CARGO_BUILD_TARGET".into(),
                    content: Arc::from(target),
                    kind: AnalysisInputKind::Environment,
                },
                AnalysisInput {
                    path: "$environment/RUSTFLAGS".into(),
                    content: Arc::from(flags),
                    kind: AnalysisInputKind::Environment,
                },
            ])
        };
        let baseline = identity(b"default", b"x86_64-unknown-linux-gnu", b"");

        assert_ne!(baseline, identity(b"api", b"x86_64-unknown-linux-gnu", b""));
        assert_ne!(
            baseline,
            identity(b"default", b"wasm32-unknown-unknown", b"")
        );
        assert_ne!(
            baseline,
            identity(b"default", b"x86_64-unknown-linux-gnu", b"--cfg loom")
        );
    }

    #[test]
    fn elixir_mix_environment_and_compiler_options_change_semantic_identity() {
        let identity = |mix_env: &'static [u8], compiler_options: &'static [u8]| {
            analysis_inputs_identity([
                AnalysisInput {
                    path: "$environment/BEHOLDER_ELIXIR_MIX_ENV".into(),
                    content: Arc::from(mix_env),
                    kind: AnalysisInputKind::Environment,
                },
                AnalysisInput {
                    path: "$environment/ERL_COMPILER_OPTIONS".into(),
                    content: Arc::from(compiler_options),
                    kind: AnalysisInputKind::Environment,
                },
            ])
        };
        let baseline = identity(b"dev", b"");

        assert_ne!(baseline, identity(b"test", b""));
        assert_ne!(baseline, identity(b"dev", b"[debug_info]"));
    }

    #[test]
    fn persisted_repository_analysis_is_reused_after_restart() {
        let cache_dir = cache_dir("restart");
        let first = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .unwrap();
        assert_eq!(
            first.analyze(&snapshot()).unwrap().repositories[0].cache,
            CacheStatus::Miss
        );
        drop(first);

        let restarted = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .unwrap();
        assert_eq!(
            restarted.analyze(&snapshot()).unwrap().repositories[0].cache,
            CacheStatus::Disk
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn prepared_plan_rejects_a_changed_repository_snapshot() {
        let cache_dir = cache_dir("immutable-plan");
        let indexer = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .unwrap();
        let original = snapshot();
        let plan = indexer.prepare(&original);
        let mut changed = original.clone();
        changed.repositories[0].state.fingerprint = "changed".into();

        let error = indexer.analyze_prepared(&changed, &plan).err().unwrap();

        assert_eq!(
            error.to_string(),
            "prepared analysis plan does not match workspace repositories"
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn prepared_plan_rejects_duplicate_repository_identities() {
        let cache_dir = cache_dir("duplicate-repositories");
        let indexer = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "fake" })
            .build()
            .unwrap();
        let mut duplicated = snapshot();
        duplicated
            .repositories
            .push(duplicated.repositories[0].clone());

        let error = indexer.analyze(&duplicated).err().unwrap();

        assert_eq!(
            error.to_string(),
            "workspace snapshot contains duplicate repository identities"
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn cached_analyzer_views_do_not_include_other_analyzers_facts() {
        let cache_dir = cache_dir("analyzer-cache-views");
        let indexer = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "first" })
            .add_analyzer(FakeAnalyzer { id: "second" })
            .build()
            .unwrap();
        let snapshot = snapshot();
        indexer.analyze(&snapshot).unwrap();

        let plan = indexer.prepare(&snapshot);
        for analyzer in &plan.analyzers {
            let cached = analyzer.cached_repository("example/repo").unwrap();
            assert_eq!(cached.entities.len(), 1);
            assert_eq!(
                cached.entities[0].id.as_str(),
                format!("rust-function://{}", analyzer.analyzer.id)
            );
        }
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn workspace_rules_apply_after_repository_cache_reuse() {
        let cache_dir = cache_dir("post-cache-workspace-rules");
        let first = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(ResolverAnalyzer {
                resolver: "elixir-function://First.resolve/3",
            })
            .build()
            .unwrap()
            .analyze(&snapshot())
            .unwrap();
        assert_eq!(first.repositories[0].cache, CacheStatus::Miss);

        let second = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(ResolverAnalyzer {
                resolver: "elixir-function://Second.resolve/3",
            })
            .build()
            .unwrap()
            .analyze(&snapshot())
            .unwrap();

        assert_eq!(second.repositories[0].cache, CacheStatus::Disk);
        assert_eq!(second.repositories[0].facts.observations.len(), 1);
        assert_eq!(
            second.repositories[0].facts.observations[0].to.as_str(),
            "elixir-function://Second.resolve/3"
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[tokio::test]
    async fn enrichment_is_separate_from_baseline_analysis() {
        let cache_dir = cache_dir("enrichment");
        let indexer = IndexerBuilder::new(cache_dir.clone(), 1)
            .add_analyzer(FakeAnalyzer { id: "syntax" })
            .add_enricher(FakeAnalyzer { id: "semantic" })
            .build()
            .unwrap();

        let baseline = indexer.analyze(&snapshot()).unwrap();
        let enrichment = indexer
            .enrich(
                EnrichmentSnapshot {
                    target_repository: "example/repo".into(),
                    workspace: snapshot(),
                },
                "semantic",
            )
            .await
            .unwrap();

        assert!(baseline.analysis_identity.contains("6:syntax1:1"));
        assert!(!baseline.analysis_identity.contains("8:semantic1:1"));
        assert_eq!(
            baseline.repositories[0].facts.entities[0].id.as_str(),
            "rust-function://syntax"
        );
        assert_eq!(enrichment.metadata.id, "semantic");
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

    #[test]
    fn extend_unique_preserves_first_seen_order() {
        let mut target = vec![1, 2];

        extend_unique(&mut target, vec![2, 3, 3, 1, 4]);

        assert_eq!(target, vec![1, 2, 3, 4]);
    }

    #[test]
    fn typed_plugins_install_both_supported_capabilities() {
        let analyzer = LanguageAnalyzerBuilder::<FakeLanguage>::new()
            .add_plugin(FakePlugin { id: "fake" })
            .build()
            .unwrap();
        let active = analyzer.activate(&snapshot().repositories[0], true);
        let mut analysis = Vec::new();

        analyzer
            .recognize(
                SourceRecognitionInput {
                    path: Path::new("input.fake"),
                    text: "input",
                    syntax: &(),
                },
                &mut analysis,
                &active,
            )
            .unwrap();
        let enrichment = analyzer
            .enrich(
                &(),
                RepositoryFactsView {
                    entities: &[],
                    observations: &[],
                },
                &active,
            )
            .unwrap();

        assert_eq!(analysis, ["fake"]);
        assert_eq!(enrichment.entities[0].id.as_str(), "rust-function://fake");
    }

    #[test]
    fn plugin_registration_order_does_not_change_output() {
        let analyze = |plugins: [FakePlugin; 2]| {
            let analyzer = plugins
                .into_iter()
                .fold(LanguageAnalyzerBuilder::new(), |builder, plugin| {
                    builder.add_plugin(plugin)
                })
                .build()
                .unwrap();
            let active = analyzer.activate(&snapshot().repositories[0], true);
            analyzer
                .enrich(
                    &(),
                    RepositoryFactsView {
                        entities: &[],
                        observations: &[],
                    },
                    &active,
                )
                .unwrap()
                .entities
        };

        assert_eq!(
            analyze([FakePlugin { id: "b" }, FakePlugin { id: "a" }]),
            analyze([FakePlugin { id: "a" }, FakePlugin { id: "b" }])
        );
    }

    #[test]
    fn duplicate_plugin_identities_fail_construction() {
        let error = LanguageAnalyzerBuilder::<FakeLanguage>::new()
            .add_plugin(FakePlugin { id: "fake" })
            .add_plugin(FakePlugin { id: "fake" })
            .build()
            .err()
            .unwrap();

        assert_eq!(error.to_string(), "duplicate plugin identity fake");
    }

    #[test]
    fn repository_plugin_activation_excludes_inactive_output_and_identity() {
        let analyzer = LanguageAnalyzerBuilder::<FakeLanguage>::new()
            .add_plugin(FakePlugin { id: "active" })
            .add_plugin(FakePlugin { id: "inactive" })
            .build()
            .unwrap();
        let active = analyzer.activate(&snapshot().repositories[0], true);
        let mut analysis = Vec::new();

        analyzer
            .recognize(
                SourceRecognitionInput {
                    path: Path::new("input.fake"),
                    text: "input",
                    syntax: &(),
                },
                &mut analysis,
                &active,
            )
            .unwrap();

        assert_eq!(active.identity(), "6:active1:1");
        assert_eq!(analysis, ["active"]);
        assert_eq!(
            active.plugins().next().unwrap().activation,
            PluginActivation {
                path: PathBuf::from("src/input.fake"),
                reason: "fake input".into(),
            }
        );
    }

    #[test]
    fn repository_plugin_activation_skips_evaluation_without_language_inputs() {
        struct PanicPlugin;

        impl Plugin<FakeLanguage> for PanicPlugin {
            fn metadata(&self) -> PluginMetadata {
                PluginMetadata {
                    id: "panic".into(),
                    version: "1".into(),
                }
            }

            fn activate(&self, _: &RepositorySnapshot) -> Option<PluginActivation> {
                panic!("plugin activation should have been skipped")
            }

            fn install(&self, _: &mut LanguageAnalyzerBuilder<FakeLanguage>) {}
        }

        let analyzer = LanguageAnalyzerBuilder::<FakeLanguage>::new()
            .add_plugin(PanicPlugin)
            .build()
            .unwrap();

        assert_eq!(
            analyzer
                .activate(&snapshot().repositories[0], false)
                .identity(),
            ""
        );
    }

    #[test]
    fn prepared_repository_scopes_plugin_identities_by_capability() {
        let activations = Arc::new(AtomicUsize::new(0));
        let analyzer = LanguageAnalyzerBuilder::<FakeLanguage>::new()
            .add_plugin(SourceOnlyPlugin {
                activations: Arc::clone(&activations),
                version: "2",
                active: true,
            })
            .add_plugin(RepositoryOnlyPlugin {
                version: "3",
                active: true,
            })
            .build()
            .unwrap();
        let repository = &snapshot().repositories[0];

        let prepared = analyzer
            .prepare_repository(
                AnalyzerMetadata {
                    id: "fake".into(),
                    version: "frontend:resolver".into(),
                },
                repository,
                true,
                true,
            )
            .unwrap();

        assert_eq!(activations.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.source_plugins, "11:source-only1:2");
        assert_eq!(
            prepared.analysis.version,
            "17:frontend:resolver38:15:repository-only1:311:source-only1:2"
        );
        assert_eq!(prepared.active_plugins.plugins().count(), 2);
    }

    #[test]
    fn plugin_version_changes_invalidate_only_their_capability_boundaries() {
        let prepare = |source_version, source_active, repository_version, repository_active| {
            let analyzer = LanguageAnalyzerBuilder::<FakeLanguage>::new()
                .add_plugin(SourceOnlyPlugin {
                    activations: Arc::new(AtomicUsize::new(0)),
                    version: source_version,
                    active: source_active,
                })
                .add_plugin(RepositoryOnlyPlugin {
                    version: repository_version,
                    active: repository_active,
                })
                .build()
                .unwrap();
            analyzer
                .prepare_repository(
                    AnalyzerMetadata {
                        id: "fake".into(),
                        version: "frontend:resolver".into(),
                    },
                    &snapshot().repositories[0],
                    true,
                    true,
                )
                .unwrap()
        };

        let baseline = prepare("1", true, "1", true);
        let source_changed = prepare("2", true, "1", true);
        let repository_changed = prepare("1", true, "2", true);
        let inactive_changed = prepare("1", true, "2", false);
        let inactive_changed_again = prepare("1", true, "3", false);

        assert_ne!(baseline.source_plugins, source_changed.source_plugins);
        assert_ne!(baseline.analysis, source_changed.analysis);
        assert_eq!(baseline.source_plugins, repository_changed.source_plugins);
        assert_ne!(baseline.analysis, repository_changed.analysis);
        assert_eq!(
            inactive_changed.source_plugins,
            inactive_changed_again.source_plugins
        );
        assert_eq!(inactive_changed.analysis, inactive_changed_again.analysis);
    }

    #[test]
    fn workspace_identity_is_repository_scoped_and_rule_versioned() {
        let repositories = BTreeMap::from([
            ("b/repo".into(), "rust:1:plugin:2".into()),
            ("a/repo".into(), "csharp:1".into()),
        ]);

        let identity = workspace_analysis_identity(&repositories);

        assert!(identity.starts_with("repositories:6:a/repo:8:csharp:1"));
        assert!(identity.contains("6:b/repo:15:rust:1:plugin:2"));
        assert!(identity.ends_with(":core-rules:6"));
        assert!(!repository_analysis_identity(&[]).contains("core-rules"));
        assert_ne!(
            identity,
            workspace_analysis_identity_with_rules(&repositories, "5")
        );
        assert_ne!(
            encode_identity([(&"a".into(), &"b:c".into())]),
            encode_identity([(&"a:b".into(), &"c".into())])
        );
    }
}
