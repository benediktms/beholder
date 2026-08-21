use crate::{
    ElixirAnalysis, ElixirRepository, FRONTEND_VERSION, RESOLVER_VERSION,
    diagnostics_from_analysis, entities_from_analysis, generated_entities, generated_observations,
    graphql_resolver_bindings, observations_from_analysis, resolve_repository_calls,
    resolve_workspace_modules,
};
use crate::{
    analysis::analyze_with_plugins,
    plugin::{ElixirLanguage, built_in_plugins},
};
use beholder_domain::SourceAnalysisError;
use beholder_indexing::{
    ActivePlugins, AnalysisCompleteness, AnalyzerContribution, AnalyzerError, AnalyzerMetadata,
    CacheStatistics, GraphqlResolverCandidate, LanguageAnalyzer, RepositoryContribution,
    RepositoryFactsView, WorkspaceAnalyzer, WorkspaceSnapshot,
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
    plugins: LanguageAnalyzer<ElixirLanguage>,
}

impl ElixirAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("elixir").join(FRONTEND_VERSION),
            cache: Mutex::new(BTreeMap::new()),
            plugins: built_in_plugins().expect("built-in Elixir plugins should compose"),
        }
    }

    fn analysis(
        &self,
        path: &Path,
        source: &str,
        active_plugins: &ActivePlugins,
    ) -> Result<(Arc<ElixirAnalysis>, CacheStatus), AnalyzerError> {
        let mut digest = Sha256::new();
        digest.update(source.as_bytes());
        digest.update(active_plugins.identity().as_bytes());
        let key = CacheKey(digest.finalize().into());
        if let Some(analysis) = self
            .cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let cache_path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&cache_path)
            && let Ok(analysis) = serde_json::from_slice::<ElixirAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "Elixir frontend cache lock poisoned")?
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
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for ElixirAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "elixir".into(),
            version: format!(
                "{FRONTEND_VERSION}:{RESOLVER_VERSION}:{}",
                self.plugins.identity()
            ),
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
            let active_plugins = self.plugins.activate(repository, true);
            let analyzed = sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, status) = self
                        .analysis(path, source, &active_plugins)
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
            let typed_repository = ElixirRepository {
                repository: repository.state.repository.identity.clone(),
                sources: analyzed
                    .iter()
                    .map(|(path, _, analysis, _)| {
                        ((*path).to_path_buf(), analysis.as_ref().clone())
                    })
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
                grpc_bindings: enrichment.grpc_bindings,
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

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};

    fn snapshot(inputs: &[(&str, &str)]) -> WorkspaceSnapshot {
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
                inputs: inputs
                    .iter()
                    .map(|(path, content)| RepositoryInput {
                        path: PathBuf::from(path),
                        content: Arc::from(content.as_bytes()),
                        kind: InputKind::Source,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn grpc_plugin_enriches_an_active_repository() {
        let cache_dir =
            std::env::temp_dir().join(format!("beholder-elixir-plugin-{}", std::process::id()));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let contribution = analyzer
            .analyze(&snapshot(&[
                (
                    "lib/pricing.pb.ex",
                    r#"
                    defmodule Pricing.Service do
                      use GRPC.Service, name: "pricing.v1.Pricing"
                      rpc :GetQuote, Pricing.Request, Pricing.Response
                    end
                    "#,
                ),
                (
                    "lib/server.ex",
                    r#"
                    defmodule Pricing.Server do
                      use GRPC.Server, service: Pricing.Service
                      def get_quote(request, stream), do: {request, stream}
                    end
                    "#,
                ),
            ]))
            .unwrap();

        assert!(analyzer.metadata().version.contains("elixir.grpc-elixir:1"));
        assert_eq!(contribution.repositories.len(), 1);
        assert!(
            contribution.repositories[0]
                .grpc_bindings
                .iter()
                .any(|binding| binding.local_symbol.as_str().ends_with("/get_quote/2"))
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn grpc_plugin_does_not_enrich_an_unrelated_repository() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-plugin-inactive-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let contribution = analyzer
            .analyze(&snapshot(&[(
                "lib/example.ex",
                "defmodule Example do\n  def run, do: :ok\nend",
            )]))
            .unwrap();

        assert!(contribution.repositories[0].grpc_bindings.is_empty());
        let _ = fs::remove_dir_all(cache_dir);
    }
}
