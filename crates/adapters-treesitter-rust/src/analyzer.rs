use crate::{
    FRONTEND_VERSION, RESOLVER_VERSION, RustAnalysis, analyze, diagnostics_from_analysis,
    entities_from_analysis, observations_from_analysis, resolve_repository_calls, tonic_bindings,
};
use beholder_domain::SourceAnalysisError;
use beholder_indexing::{
    AnalysisCompleteness, AnalyzerContribution, AnalyzerError, AnalyzerMetadata, CacheStatistics,
    RepositoryContribution, WorkspaceAnalyzer, WorkspaceSnapshot,
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

pub struct RustAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, Arc<RustAnalysis>>>,
}

impl RustAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("rust").join(FRONTEND_VERSION),
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    fn analysis(&self, source: &str) -> Result<(Arc<RustAnalysis>, CacheStatus), AnalyzerError> {
        let key = CacheKey(Sha256::digest(source.as_bytes()).into());
        if let Some(analysis) = self
            .cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&path)
            && let Ok(analysis) = serde_json::from_slice::<RustAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "Rust frontend cache lock poisoned")?
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
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for RustAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "rust".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        path.extension().is_some_and(|extension| extension == "rs")
    }

    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
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
                    Ok::<_, SourceAnalysisError>((*path, analysis, status))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut observations = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            for (path, analysis, status) in &analyzed {
                match status {
                    CacheStatus::Memory => cache.memory_hits += 1,
                    CacheStatus::Disk => cache.disk_hits += 1,
                    CacheStatus::Miss => cache.misses += 1,
                }
                observations.extend(observations_from_analysis(
                    &repository.state.repository.identity,
                    analysis,
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
                .map(|(path, analysis, _)| (*path, analysis.as_ref()))
                .collect::<Vec<_>>();
            let (grpc_bindings, grpc_diagnostics) =
                tonic_bindings(&repository.state.repository.identity, &source_refs);
            diagnostics.extend(grpc_diagnostics);
            resolve_repository_calls(&mut observations);
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

        let mut all_observations = repositories
            .iter()
            .flat_map(|repository| repository.observations.iter().cloned())
            .collect::<Vec<_>>();
        let overrides = resolve_repository_calls(&mut all_observations);
        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides,
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache,
        })
    }

    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        self.cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .clear();
        Ok(())
    }
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}
