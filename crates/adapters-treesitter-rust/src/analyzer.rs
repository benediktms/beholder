use crate::{
    FRONTEND_VERSION, RESOLVER_VERSION, RustAnalysis, diagnostics_from_analysis,
    entities_from_analysis, observations_from_analysis, resolve_repository_calls,
};
use crate::{
    incremental::{CacheStatus, IncrementalRust},
    model::RustRepository,
    plugin::{RustLanguage, built_in_plugins},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, SourceAnalysisError, UnsafeTreeRecovery,
};
use beholder_indexing::{
    AnalysisCompleteness, AnalysisInputKind, AnalyzerContribution, AnalyzerError, AnalyzerMetadata,
    AnalyzerPlan, CacheStatistics, LanguageAnalyzer, RepositoryContribution, RepositoryFactsView,
    WorkspaceAnalyzer, WorkspaceSnapshot,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct RustAnalyzer {
    cache_dir: PathBuf,
    incremental: Mutex<IncrementalRust>,
    plugins: LanguageAnalyzer<RustLanguage>,
}

impl RustAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        let cache_dir = cache_dir.join("rust").join(FRONTEND_VERSION);
        Self {
            incremental: Mutex::new(IncrementalRust::new(cache_dir.clone())),
            cache_dir,
            plugins: built_in_plugins().expect("built-in Rust plugins should compose"),
        }
    }
}

impl WorkspaceAnalyzer for RustAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        let base = format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}");
        let plugins = self.plugins.identity();
        AnalyzerMetadata {
            id: "rust".into(),
            version: format!("{}:{base}{}:{plugins}", base.len(), plugins.len()),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        crate::manifest::rust_analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        crate::manifest::rust_analysis_input_kind(path)
    }

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| is_rust_source(&input.path))
    }

    fn prepare(&self, snapshot: &WorkspaceSnapshot) -> AnalyzerPlan {
        let analyzer = AnalyzerMetadata {
            id: "rust".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        };
        AnalyzerPlan::from_repositories(
            self.metadata(),
            snapshot.repositories.iter().filter_map(|repository| {
                let active = self.is_active(repository);
                self.plugins
                    .prepare_repository(analyzer.clone(), repository, active, active)
            }),
        )
    }

    fn repository_dependencies(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<beholder_domain::RepositoryDependencyCandidate>, AnalyzerError> {
        crate::manifest::cargo_repository_dependencies(snapshot)
    }

    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut cached_observations = Vec::new();
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
            let repository_plan = plan
                .repository(&repository.state.repository.identity)
                .ok_or("missing prepared Rust repository")?;
            if let Some(cached) = plan.cached_repository(&repository.state.repository.identity) {
                cached_observations.extend_from_slice(cached.observations);
                continue;
            }
            let active_plugins = &repository_plan.active_plugins;
            enum SourceResult {
                Analyzed(PathBuf, Arc<RustAnalysis>, CacheStatus),
                Skipped(PathBuf, String),
            }
            let results = self
                .incremental
                .lock()
                .map_err(|_| "Rust frontend cache lock poisoned")?
                .analyze_many(
                    &repository.state.repository.identity,
                    &sources,
                    active_plugins,
                    &repository_plan.source_plugins,
                )
                .into_iter()
                .map(|(path, result)| match result {
                    Ok(result) => Ok(SourceResult::Analyzed(path, result.analysis, result.status)),
                    Err(error) if error.downcast_ref::<UnsafeTreeRecovery>().is_some() => {
                        Ok(SourceResult::Skipped(path, error.to_string()))
                    }
                    Err(error) => Err(SourceAnalysisError::from_source(&path, error)),
                })
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
                        path,
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
                    .map(|(path, analysis, _)| (path.clone(), analysis.as_ref().clone()))
                    .collect(),
            };
            let enrichment = self.plugins.enrich(
                &typed_repository,
                RepositoryFactsView {
                    entities: &entities,
                    observations: &observations,
                },
                active_plugins,
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

        let mut all_observations = cached_observations;
        all_observations.extend(
            repositories
                .iter()
                .flat_map(|repository| repository.observations.iter().cloned()),
        );
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
        *self
            .incremental
            .lock()
            .map_err(|_| "Rust frontend cache lock poisoned")? =
            IncrementalRust::new(self.cache_dir.clone());
        Ok(())
    }
}

fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{EntityKind, LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};
    use std::fs;

    #[test]
    fn skips_unsafe_source_and_keeps_valid_siblings() {
        let cache = std::env::temp_dir().join(format!(
            "beholder-rust-analyzer-test-{}",
            std::process::id()
        ));
        let analyzer = RustAnalyzer::new(cache.clone());
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
