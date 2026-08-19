use crate::{
    ElixirAnalysis, FRONTEND_VERSION, RESOLVER_VERSION, analyze, diagnostics_from_analysis,
    entities_from_analysis, generated_entities, generated_observations, graphql_resolver_bindings,
    grpc_bindings, observations_from_analysis, resolve_repository_calls, resolve_workspace_modules,
};
use beholder_domain::SourceAnalysisError;
use beholder_indexing::{
    AnalysisCompleteness, AnalyzerContribution, AnalyzerError, AnalyzerMetadata, CacheStatistics,
    GraphqlResolverCandidate, RepositoryContribution, WorkspaceAnalyzer, WorkspaceSnapshot,
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

pub struct ElixirAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, Arc<ElixirAnalysis>>>,
}

impl ElixirAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("elixir").join(FRONTEND_VERSION),
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    fn analysis(&self, source: &str) -> Result<(Arc<ElixirAnalysis>, CacheStatus), AnalyzerError> {
        let key = CacheKey(Sha256::digest(source.as_bytes()).into());
        if let Some(analysis) = self
            .cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&path)
            && let Ok(analysis) = serde_json::from_slice::<ElixirAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "Elixir frontend cache lock poisoned")?
                .insert(key, analysis.clone());
            return Ok((analysis, CacheStatus::Disk));
        }
        let analysis = Arc::new(analyze(source)?);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
        {
            let _ = fs::write(path, bytes);
        }
        self.cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for ElixirAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "elixir".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| matches!(extension.to_str(), Some("ex" | "exs")))
    }

    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut graphql_resolvers = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let sources = repository
                .inputs
                .iter()
                .filter(|input| self.accepts(&input.path))
                .map(|input| {
                    std::str::from_utf8(&input.content)
                        .map(|source| (input.path.as_path(), source))
                        .map_err(|error| {
                            SourceAnalysisError::from_source(&input.path, Box::new(error))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if sources.is_empty() {
                continue;
            }
            active_repositories.push(repository.state.repository.identity.clone());
            let analyzed = sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, status) = self
                        .analysis(source)
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
            let generated = generated_observations(
                &repository.state.repository.identity,
                &source_refs,
                &observations,
            );
            entities.extend(generated_entities(&generated));
            observations.extend(generated);
            resolve_repository_calls(&mut observations, &source_refs);
            let (grpc_bindings, grpc_diagnostics) =
                grpc_bindings(&repository.state.repository.identity, &source_refs);
            diagnostics.extend(grpc_diagnostics);
            graphql_resolvers.extend(
                graphql_resolver_bindings(&repository.state.repository.identity, &source_refs)
                    .into_iter()
                    .map(|binding| GraphqlResolverCandidate {
                        repository: repository.state.repository.identity.clone(),
                        field: binding.field,
                        parent: binding.parent,
                        resolver: binding.resolver.into(),
                        evidence: binding.evidence.into(),
                    }),
            );
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
                grpc_bindings,
                observations,
                diagnostics,
            });
        }

        let all_observations = repositories
            .iter()
            .flat_map(|repository| repository.observations.iter().cloned())
            .collect::<Vec<_>>();
        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides: resolve_workspace_modules(&all_observations),
            graphql_resolvers,
            diagnostics: Vec::new(),
            cache,
        })
    }

    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        self.cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .clear();
        Ok(())
    }
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}
