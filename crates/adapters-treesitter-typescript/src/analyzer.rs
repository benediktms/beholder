use crate::{
    FRONTEND_VERSION, GraphqlFactInput, GraphqlResolverInput, GraphqlResolverSource,
    RESOLVER_VERSION, SourceLanguage, TypescriptAnalysis, TypescriptRepository,
    collect_graphql_facts, collect_graphql_resolvers, diagnostics_from_analysis,
    entities_from_analysis, observations_from_analysis, resolve_repository_calls,
    resolve_workspace_calls, unresolved_call_diagnostics,
};
use crate::{
    analysis::analyze_with_plugins,
    plugin::{TypescriptLanguage, built_in_plugins},
};
use beholder_adapters_graphql::GraphqlSource;
use beholder_domain::SourceAnalysisError;
use beholder_indexing::{
    ActivePlugins, AnalysisCompleteness, AnalyzerContribution, AnalyzerError, AnalyzerMetadata,
    CacheStatistics, LanguageAnalyzer, RepositoryContribution, RepositoryFactsView,
    WorkspaceAnalyzer, WorkspaceSnapshot,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CacheKey([u8; 32]);

#[derive(Clone, Copy)]
enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

pub struct TypescriptAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, Arc<TypescriptAnalysis>>>,
    plugins: LanguageAnalyzer<TypescriptLanguage>,
}

impl TypescriptAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("typescript"),
            cache: Mutex::new(BTreeMap::new()),
            plugins: built_in_plugins().expect("built-in TypeScript plugins should compose"),
        }
    }

    fn analysis(
        &self,
        path: &Path,
        source: &str,
        language: SourceLanguage,
        active_plugins: &ActivePlugins,
    ) -> Result<(Arc<TypescriptAnalysis>, CacheStatus), AnalyzerError> {
        let mut digest = Sha256::new();
        digest.update(source.as_bytes());
        digest.update(path.to_string_lossy().as_bytes());
        digest.update(language.cache_version().as_bytes());
        digest.update(active_plugins.identity().as_bytes());
        let key = CacheKey(digest.finalize().into());
        if let Some(analysis) = self
            .cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let cache_path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&cache_path)
            && let Ok(analysis) = serde_json::from_slice::<TypescriptAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "TypeScript frontend cache lock poisoned")?
                .insert(key, analysis.clone());
            return Ok((analysis, CacheStatus::Disk));
        }
        let analysis = Arc::new(analyze_with_plugins(
            source,
            language,
            path,
            &self.plugins,
            active_plugins,
        )?);
        if let Some(parent) = cache_path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
        {
            let _ = fs::write(cache_path, bytes);
        }
        self.cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for TypescriptAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "typescript".into(),
            version: format!(
                "{FRONTEND_VERSION}:{RESOLVER_VERSION}:{}",
                self.plugins.identity()
            ),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        SourceLanguage::from_path(path).is_some()
            || path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "graphql" | "gql"))
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == "package.json"
                        || ((name.starts_with("tsconfig.") || name.starts_with("jsconfig."))
                            && name.ends_with(".json"))
                })
    }

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        repository.inputs.iter().any(|input| {
            SourceLanguage::from_path(&input.path).is_some()
                || input
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name == "package.json"
                            || ((name.starts_with("tsconfig.") || name.starts_with("jsconfig."))
                                && name.ends_with(".json"))
                    })
        })
    }

    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut typed_repositories = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let inputs = repository
                .inputs
                .iter()
                .filter(|input| self.accepts(&input.path))
                .collect::<Vec<_>>();
            if !self.is_active(repository) {
                continue;
            }
            active_repositories.push(repository.state.repository.identity.clone());
            let sources = inputs
                .iter()
                .filter_map(|input| {
                    SourceLanguage::from_path(&input.path).map(|language| (*input, language))
                })
                .map(|(input, language)| {
                    text(input).map(|source| (input.path.as_path(), source, language))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let active_plugins = self.plugins.activate(repository, !sources.is_empty());
            let manifests = inputs
                .iter()
                .filter(|input| {
                    input.path.file_name().and_then(|name| name.to_str()) == Some("package.json")
                })
                .map(|input| text(input).map(|source| (input.path.as_path(), source)))
                .collect::<Result<Vec<_>, _>>()?;
            let configs = inputs
                .iter()
                .filter(|input| {
                    input
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            (name.starts_with("tsconfig.") || name.starts_with("jsconfig."))
                                && name.ends_with(".json")
                        })
                })
                .map(|input| text(input).map(|source| (input.path.as_path(), source)))
                .collect::<Result<Vec<_>, _>>()?;
            let schemas = inputs
                .iter()
                .filter(|input| {
                    input
                        .path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| matches!(extension, "graphql" | "gql"))
                })
                .map(|input| text(input).map(|source| (input.path.as_path(), source)))
                .collect::<Result<Vec<_>, _>>()?;
            let analyzed = sources
                .par_iter()
                .map(|(path, source, language)| {
                    let (analysis, status) = self
                        .analysis(path, source, *language, &active_plugins)
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    Ok::<_, SourceAnalysisError>((*path, *source, analysis, status))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut observations = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            for (path, source, analysis, status) in &analyzed {
                match status {
                    CacheStatus::Memory => cache.memory_hits += 1,
                    CacheStatus::Disk => cache.disk_hits += 1,
                    CacheStatus::Miss => cache.misses += 1,
                }
                observations.extend(observations_from_analysis(
                    &repository.state.repository.identity,
                    analysis,
                    source,
                    path,
                ));
                entities.extend(entities_from_analysis(
                    &repository.state.repository.identity,
                    analysis,
                    path,
                ));
                diagnostics.extend(diagnostics_from_analysis(analysis, path));
            }
            let source_refs = analyzed
                .iter()
                .map(|(path, _, analysis, _)| (*path, analysis.as_ref()))
                .collect::<Vec<_>>();
            let resolver_sources = analyzed
                .iter()
                .map(|(path, source, analysis, _)| GraphqlResolverSource {
                    path,
                    analysis,
                    source,
                })
                .collect::<Vec<_>>();
            let graphql_resolvers = collect_graphql_resolvers(GraphqlResolverInput {
                repository: &repository.state.repository.identity,
                sources: &resolver_sources,
                manifests: &manifests,
            });
            observations.extend(graphql_resolvers.observations);
            entities.extend(graphql_resolvers.entities);
            diagnostics.extend(graphql_resolvers.diagnostics);
            resolve_repository_calls(
                &repository.state.repository.identity,
                &mut observations,
                &source_refs,
                &manifests,
                &configs,
            );
            let typed_repository = TypescriptRepository::new(
                repository.state.repository.identity.clone(),
                analyzed
                    .iter()
                    .map(|(path, _, analysis, _)| {
                        ((*path).to_path_buf(), analysis.as_ref().clone())
                    })
                    .collect(),
                manifests
                    .iter()
                    .map(|(path, source)| ((*path).to_path_buf(), (*source).to_owned()))
                    .collect(),
                configs
                    .iter()
                    .map(|(path, source)| ((*path).to_path_buf(), (*source).to_owned()))
                    .collect(),
            );
            let enrichment = self.plugins.enrich(
                &typed_repository,
                RepositoryFactsView {
                    entities: &entities,
                    observations: &observations,
                },
                &active_plugins,
            )?;
            entities.extend(enrichment.entities);
            observations.extend(enrichment.observations);
            diagnostics.extend(enrichment.diagnostics);
            let graphql_sources = schemas
                .iter()
                .map(|(path, source)| GraphqlSource {
                    path,
                    source,
                    owner: None,
                })
                .collect::<Vec<_>>();
            let graphql = collect_graphql_facts(GraphqlFactInput {
                repository: &repository.state.repository.identity,
                sources: &resolver_sources,
                schemas: &graphql_sources,
            });
            observations.extend(graphql.observations);
            entities.extend(graphql.entities);
            diagnostics.extend(graphql.diagnostics);
            typed_repositories.push(typed_repository);
            let completeness = if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.ends_with(".parse_recovery"))
            {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            };
            repositories.push(RepositoryContribution {
                repository: repository.state.repository.identity.clone(),
                completeness,
                entities,
                grpc_bindings: enrichment.grpc_bindings,
                observations,
                diagnostics,
            });
        }

        let mut all_observations = repositories
            .iter()
            .flat_map(|repository| repository.observations.iter().cloned())
            .collect::<Vec<_>>();
        let overrides = resolve_workspace_calls(&mut all_observations, &typed_repositories);
        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides,
            graphql_resolvers: Vec::new(),
            diagnostics: unresolved_call_diagnostics(&all_observations),
            cache,
        })
    }

    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        self.cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .clear();
        Ok(())
    }
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

fn text(input: &beholder_indexing::RepositoryInput) -> Result<&str, SourceAnalysisError> {
    std::str::from_utf8(&input.content)
        .map_err(|error| SourceAnalysisError::from_source(&input.path, Box::new(error)))
}
