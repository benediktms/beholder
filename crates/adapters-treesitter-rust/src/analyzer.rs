use crate::{
    FRONTEND_VERSION, RESOLVER_VERSION, RustAnalysis, diagnostics_from_analysis,
    entities_from_analysis, observations_from_analysis, resolve_repository_calls,
};
use crate::{
    analysis::analyze_with_plugins,
    model::RustRepository,
    plugin::{RustLanguage, built_in_plugins},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, SourceAnalysisError, UnsafeTreeRecovery,
};
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

pub struct RustAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, Arc<RustAnalysis>>>,
    plugins: LanguageAnalyzer<RustLanguage>,
}

impl RustAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("rust").join(FRONTEND_VERSION),
            cache: Mutex::new(BTreeMap::new()),
            plugins: built_in_plugins().expect("built-in Rust plugins should compose"),
        }
    }

    fn analysis(
        &self,
        path: &Path,
        source: &str,
        active_plugins: &ActivePlugins,
    ) -> Result<(Arc<RustAnalysis>, CacheStatus), AnalyzerError> {
        let mut digest = Sha256::new();
        digest.update(source.as_bytes());
        digest.update(active_plugins.identity().as_bytes());
        let key = CacheKey(digest.finalize().into());
        if let Some(analysis) = self
            .cache
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let cache_path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&cache_path)
            && let Ok(analysis) = serde_json::from_slice::<RustAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "Rust frontend cache lock poisoned")?
                .insert(key, analysis.clone());
            return Ok((analysis, CacheStatus::Disk));
        }
        let analysis = Arc::new(analyze_with_plugins(
            source,
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
            .map_err(|_| "Rust frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for RustAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "rust".into(),
            version: format!(
                "{FRONTEND_VERSION}:{RESOLVER_VERSION}:{}",
                self.plugins.identity()
            ),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        is_rust_source(path) || path.file_name().is_some_and(|name| name == "Cargo.toml")
    }

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| is_rust_source(&input.path))
    }

    fn analyze(&self, snapshot: &WorkspaceSnapshot) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let sources = repository
                .inputs
                .iter()
                .filter(|input| is_rust_source(&input.path))
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
            let active_plugins = self.plugins.activate(repository, true);
            enum SourceResult<'a> {
                Analyzed(&'a Path, Arc<RustAnalysis>, CacheStatus),
                Skipped(&'a Path, String),
            }
            let results = sources
                .par_iter()
                .map(
                    |(path, source)| match self.analysis(path, source, &active_plugins) {
                        Ok((analysis, status)) => {
                            Ok(SourceResult::Analyzed(path, analysis, status))
                        }
                        Err(error) if error.downcast_ref::<UnsafeTreeRecovery>().is_some() => {
                            Ok(SourceResult::Skipped(path, error.to_string()))
                        }
                        Err(error) => Err(SourceAnalysisError::from_source(path, error)),
                    },
                )
                .collect::<Result<Vec<_>, _>>()?;
            let mut observations = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            let mut analyzed = Vec::new();
            for result in results {
                match result {
                    SourceResult::Analyzed(path, analysis, status) => {
                        analyzed.push((path, analysis, status));
                    }
                    SourceResult::Skipped(path, detail) => diagnostics.push(AnalysisDiagnostic {
                        code: "rust.parse_recovery".into(),
                        severity: AnalysisDiagnosticSeverity::Warning,
                        path: path.to_path_buf(),
                        line: None,
                        detail: Some(detail),
                    }),
                }
            }
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
            let typed_repository = RustRepository {
                repository: repository.state.repository.identity.clone(),
                sources: analyzed
                    .iter()
                    .map(|(path, analysis, _)| ((*path).to_path_buf(), analysis.as_ref().clone()))
                    .collect(),
            };
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
                grpc_bindings: enrichment.grpc_bindings,
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

fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{EntityKind, LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};

    #[test]
    fn skips_unsafe_source_and_keeps_valid_siblings() {
        let cache = std::env::temp_dir().join(format!(
            "beholder-rust-analyzer-test-{}",
            std::process::id()
        ));
        let analyzer = RustAnalyzer::new(&cache);
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
                    inputs: [
                        ("src/valid.rs", "fn valid() {}"),
                        ("tests/ui/invalid.rs", "fn invalid("),
                    ]
                    .into_iter()
                    .map(|(path, source)| RepositoryInput {
                        path: path.into(),
                        content: Arc::from(source.as_bytes()),
                        kind: InputKind::Source,
                    })
                    .collect(),
                }],
            })
            .unwrap();

        let repository = &contribution.repositories[0];
        assert_eq!(repository.completeness, AnalysisCompleteness::Incomplete);
        assert!(repository.entities.iter().any(|entity| {
            entity.kind == EntityKind::Callable && entity.id.as_str().ends_with("/valid")
        }));
        assert!(repository.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "rust.parse_recovery"
                && diagnostic.path == Path::new("tests/ui/invalid.rs")
        }));
        fs::remove_dir_all(cache).unwrap();
    }
}
