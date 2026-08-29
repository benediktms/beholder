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
use beholder_domain::{FactShard, SourceAnalysisError};
use beholder_indexing::{
    ActivePlugins, AnalysisCompleteness, AnalysisInputKind, AnalyzerContribution, AnalyzerError,
    AnalyzerMetadata, AnalyzerPlan, CacheStatistics, GraphqlResolverCandidate, LanguageAnalyzer,
    RepositoryContribution, RepositoryFactsView, WorkspaceAnalyzer, WorkspaceSnapshot,
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
        source_plugins: &str,
    ) -> Result<(Arc<ElixirAnalysis>, CacheStatus), AnalyzerError> {
        let mut digest = Sha256::new();
        for part in [
            FRONTEND_VERSION.as_bytes(),
            path.as_os_str().as_encoded_bytes(),
            source.as_bytes(),
            source_plugins.as_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
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
        let base = format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}");
        let plugins = self.plugins.identity();
        AnalyzerMetadata {
            id: "elixir".into(),
            version: format!("{}:{base}{}:{plugins}", base.len(), plugins.len()),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        crate::manifest::elixir_analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        crate::manifest::elixir_analysis_input_kind(path)
    }

    fn repository_dependencies(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<beholder_domain::RepositoryDependencyCandidate>, AnalyzerError> {
        crate::manifest::mix_repository_dependencies(snapshot)
    }

    fn prepare(&self, snapshot: &WorkspaceSnapshot) -> AnalyzerPlan {
        let analyzer = AnalyzerMetadata {
            id: "elixir".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        };
        AnalyzerPlan::from_repositories(
            self.metadata(),
            snapshot.repositories.iter().filter_map(|repository| {
                let has_sources = repository
                    .inputs
                    .iter()
                    .any(|input| self.accepts(&input.path));
                self.plugins.prepare_repository(
                    analyzer.clone(),
                    repository,
                    has_sources,
                    has_sources,
                )
            }),
        )
    }

    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut graphql_resolvers = Vec::new();
        let mut cached_observations = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let sources = repository
                .inputs
                .iter()
                .filter(|input| {
                    self.analysis_input_kind(&input.path) == Some(AnalysisInputKind::Source)
                })
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
                .ok_or("missing prepared Elixir repository")?;
            let active_plugins = &repository_plan.active_plugins;
            let analyzed = sources
                .par_iter()
                .map(|(path, source)| {
                    let (analysis, status) = self
                        .analysis(
                            path,
                            source,
                            active_plugins,
                            &repository_plan.source_plugins,
                        )
                        .map_err(|error| SourceAnalysisError::from_source(path, error))?;
                    Ok::<_, SourceAnalysisError>((*path, *source, analysis, status))
                })
                .collect::<Result<Vec<_>, _>>()?;
            for (_, _, _, status) in &analyzed {
                match status {
                    CacheStatus::Memory => cache.memory_hits += 1,
                    CacheStatus::Disk => cache.disk_hits += 1,
                    CacheStatus::Miss => cache.misses += 1,
                }
            }
            let source_refs = analyzed
                .iter()
                .map(|(path, _, analysis, _)| (*path, analysis.as_ref()))
                .collect::<Vec<_>>();
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
            if let Some(cached) = plan.cached_repository(&repository.state.repository.identity) {
                cached_observations.extend_from_slice(cached.observations);
                cached_observations.extend(
                    plan.cached_fact_shards(&repository.state.repository.identity)
                        .into_iter()
                        .flatten()
                        .flat_map(|shard| shard.observations.iter().cloned()),
                );
                continue;
            }
            let mut observations = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            for (path, source, analysis, _) in &analyzed {
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
            let generated = generated_observations(
                &repository.state.repository.identity,
                &source_refs,
                &observations,
            );
            entities.extend(generated_entities(&generated));
            observations.extend(generated);
            let generated = crate::grpc::configured_delegate_observations(
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
                active_plugins,
            )?;
            entities.extend(enrichment.entities);
            observations.extend(enrichment.observations);
            diagnostics.extend(enrichment.diagnostics);
            let fact_shards = build_fact_shards(
                &repository.state.repository.identity,
                &analyzed,
                &entities,
                &observations,
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
                replaced_diagnostic_codes: Default::default(),
                fact_shards,
            });
        }

        let mut all_observations = cached_observations;
        all_observations.extend(
            repositories
                .iter()
                .flat_map(|repository| repository.observations.iter().cloned()),
        );
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

#[derive(Default)]
struct ShardFingerprint {
    interface: Vec<[u8; 32]>,
    body: Vec<[u8; 32]>,
}

fn build_fact_shards(
    repository: &str,
    analyzed: &[(&Path, &str, Arc<ElixirAnalysis>, CacheStatus)],
    entities: &[beholder_domain::EntityFact],
    observations: &[beholder_domain::Observation],
) -> Vec<FactShard> {
    let mut fingerprints = BTreeMap::<String, ShardFingerprint>::new();
    for (path, source, analysis, _) in analyzed {
        let source_owner = format!(
            "repo://{repository}/elixir-source/{}",
            path.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let source_fingerprint = fingerprints.entry(source_owner).or_default();
        source_fingerprint
            .interface
            .push(Sha256::digest([u8::from(!analysis.parse_error_lines.is_empty())]).into());
        if position_sensitive_source(source) {
            source_fingerprint
                .body
                .push(Sha256::digest(source.as_bytes()).into());
        }
        for module in &analysis.modules {
            let module_owner = format!("repo://{repository}/elixir/{}", module.name);
            fingerprints
                .entry(module_owner.clone())
                .or_default()
                .body
                .push(module.semantic_hash);
            for function in &module.functions {
                fingerprints
                    .entry(module_owner.clone())
                    .or_default()
                    .interface
                    .push(function.interface_hash);
                let fingerprint = fingerprints
                    .entry(format!(
                        "{module_owner}/{}/{}",
                        function.name, function.arity
                    ))
                    .or_default();
                fingerprint.interface.push(function.interface_hash);
                fingerprint.body.push(function.body_hash);
            }
            for callback in &module.callbacks {
                fingerprints
                    .entry(module_owner.clone())
                    .or_default()
                    .interface
                    .push(callback.interface_hash);
                let fingerprint = fingerprints
                    .entry(format!(
                        "{module_owner}/callback/{}/{}",
                        callback.name, callback.arity
                    ))
                    .or_default();
                fingerprint.interface.push(callback.interface_hash);
                fingerprint.body.push(callback.body_hash);
            }
        }
    }
    for entity in entities {
        fingerprints
            .entry(entity.id.as_str().to_owned())
            .or_default();
    }
    for observation in observations {
        fingerprints
            .entry(observation.from.as_str().to_owned())
            .or_default();
    }

    fingerprints
        .into_iter()
        .map(|(owner, mut fingerprint)| {
            fingerprint.interface.sort_unstable();
            let mut shard_entities = entities
                .iter()
                .filter(|entity| entity.id.as_str() == owner)
                .cloned()
                .collect::<Vec<_>>();
            shard_entities.sort_by(|left, right| {
                left.id
                    .as_str()
                    .cmp(right.id.as_str())
                    .then(left.kind.cmp(&right.kind))
            });
            let mut shard_observations = observations
                .iter()
                .filter(|observation| observation.from.as_str() == owner)
                .cloned()
                .collect::<Vec<_>>();
            shard_observations.sort_by(|left, right| {
                left.relation
                    .as_str()
                    .cmp(right.relation.as_str())
                    .then(left.to.as_str().cmp(right.to.as_str()))
                    .then(left.provenance.as_str().cmp(right.provenance.as_str()))
            });
            let mut digest = Sha256::new();
            for part in [
                FRONTEND_VERSION.as_bytes(),
                RESOLVER_VERSION.as_bytes(),
                owner.as_bytes(),
            ] {
                digest.update((part.len() as u64).to_le_bytes());
                digest.update(part);
            }
            for hash in fingerprint.interface.into_iter().chain(fingerprint.body) {
                digest.update(hash);
            }
            for entity in &shard_entities {
                digest.update(entity.id.as_str().as_bytes());
                digest.update(format!("{:?}", entity.kind).as_bytes());
                digest.update(serde_json::to_vec(&entity.metadata).unwrap_or_default());
            }
            for observation in &shard_observations {
                digest.update(observation.from.as_str().as_bytes());
                digest.update(observation.relation.as_str().as_bytes());
                digest.update(observation.to.as_str().as_bytes());
                digest.update(observation.confidence.score().to_le_bytes());
                digest.update(observation.provenance.as_str().as_bytes());
            }
            FactShard {
                repository: repository.to_owned(),
                producer: "elixir".into(),
                owner: owner.into(),
                version: format!("{:x}", digest.finalize()),
                entities: shard_entities,
                observations: shard_observations,
            }
        })
        .collect()
}

fn position_sensitive_source(source: &str) -> bool {
    ["__ENV__", "__CALLER__"]
        .into_iter()
        .any(|context| source.contains(context))
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};
    use std::collections::BTreeMap;

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

    fn shard_versions(analyzer: &ElixirAnalyzer, source: &str) -> BTreeMap<String, String> {
        analyzer
            .analyze(&snapshot(&[("lib/example.ex", source)]))
            .unwrap()
            .repositories
            .remove(0)
            .fact_shards
            .into_iter()
            .map(|shard| (shard.owner.to_string(), shard.version))
            .collect()
    }

    #[test]
    fn semantic_shards_ignore_comments_and_formatting() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-formatting-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run(value) do\n    value + 1\n  end\nend\n",
        );
        let formatted = shard_versions(
            &analyzer,
            "# heading\n\ndefmodule Example do\n  # implementation\n  def run(value) do\n      (value + 1)\n  end\nend\n",
        );

        assert_eq!(formatted, initial);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn body_changes_only_the_function_shard() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-body-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run(value), do: value + 1\nend\n",
        );
        let changed = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run(value), do: value + 2\nend\n",
        );
        let owner = "repo://example/repo/elixir/Example/run/1";

        assert_ne!(changed[owner], initial[owner]);
        assert!(initial.iter().all(|(candidate, version)| {
            candidate == owner || changed.get(candidate) == Some(version)
        }));
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn interface_changes_the_owning_module_shard() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-interface-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run(value), do: value\nend\n",
        );
        let changed = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run(value, context), do: {value, context}\nend\n",
        );
        let module = "repo://example/repo/elixir/Example";

        assert_ne!(changed[module], initial[module]);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn module_semantics_change_the_owning_module_shard() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-module-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  @callee Foo\n  def run, do: @callee.work()\nend\n",
        );
        let changed = shard_versions(
            &analyzer,
            "defmodule Example do\n  @callee Bar\n  def run, do: @callee.work()\nend\n",
        );
        let module = "repo://example/repo/elixir/Example";
        let function = "repo://example/repo/elixir/Example/run/0";

        assert_ne!(changed[module], initial[module]);
        assert_eq!(changed[function], initial[function]);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn quoted_definition_semantics_change_the_owning_module_shard() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-quoted-definition-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  defmacro __using__(_) do\n    quote do\n      def generated, do: [Foo]\n    end\n  end\nend\n",
        );
        let changed = shard_versions(
            &analyzer,
            "defmodule Example do\n  defmacro __using__(_) do\n    quote do\n      def generated, do: {Foo}\n    end\n  end\nend\n",
        );
        let module = "repo://example/repo/elixir/Example";

        assert_ne!(changed[module], initial[module]);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn body_hashes_preserve_semantic_collection_shapes() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-structure-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let list = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run, do: expand([Foo])\nend\n",
        );
        let tuple = shard_versions(
            &analyzer,
            "defmodule Example do\n  def run, do: expand({Foo})\nend\n",
        );
        let function = "repo://example/repo/elixir/Example/run/0";

        assert_ne!(tuple[function], list[function]);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn position_observing_sources_change_when_their_lines_move() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-semantic-position-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let initial = shard_versions(
            &analyzer,
            "defmodule Example do\n  def route, do: Router.route(__ENV__)\nend\n",
        );
        let shifted = shard_versions(
            &analyzer,
            "# shifted\ndefmodule Example do\n  def route, do: Router.route(__ENV__)\nend\n",
        );
        let source = "repo://example/repo/elixir-source/lib/example.ex";

        assert_ne!(shifted[source], initial[source]);
        let _ = fs::remove_dir_all(cache_dir);
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

        assert!(
            analyzer
                .metadata()
                .version
                .contains("18:elixir.grpc-elixir1:2")
        );
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
    fn resolves_calls_through_configured_grpc_delegates() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-elixir-configured-grpc-delegate-{}",
            std::process::id()
        ));
        let analyzer = ElixirAnalyzer::new(cache_dir.clone());
        let contribution = analyzer
            .analyze(&snapshot(&[
                (
                    "lib/packages.pb.ex",
                    r#"
                    defmodule Packages.Service do
                      use GRPC.Service, name: "packages.v1.PackagesService"
                      rpc :CreatePendingPackageInstances, Packages.Request, Packages.Response
                    end
                    defmodule Packages.Stub do
                      use GRPC.Stub, service: Packages.Service
                    end
                    "#,
                ),
                (
                    "lib/packages_client.ex",
                    r#"
                    # Autogenerated by the RPC client generator
                    defmodule Packages.ClientBehaviour do
                      @callback create_pending_package_instances(term()) :: term()
                    end
                    defmodule Packages.Client do
                      @behaviour Packages.ClientBehaviour
                      use RpcClient.Client, service: Packages.Service, stub: Packages.Stub
                      def create_pending_package_instances(request), do: call(request, :create_pending_package_instances, [])
                    end
                    "#,
                ),
                (
                    "lib/checkout.ex",
                    r#"
                    defmodule Checkout.Packages do
                      use ConfiguredDelegate,
                        behaviour: Packages.ClientBehaviour,
                        config_key: [:checkout, :packages_client]
                    end
                    defmodule Checkout.CloseOrder do
                      def create(request), do: Checkout.Packages.create_pending_package_instances(request)
                    end
                    "#,
                ),
            ]))
            .unwrap();
        let repository = &contribution.repositories[0];

        assert!(repository.observations.iter().any(|observation| {
            observation.from.as_str() == "repo://example/repo/elixir/Checkout.CloseOrder/create/1"
                && observation.to.as_str()
                    == "repo://example/repo/elixir/Checkout.Packages/create_pending_package_instances/1"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::Calls,
                    )
        }));
        assert!(repository.observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/repo/elixir/Checkout.Packages/create_pending_package_instances/1"
                && observation.to.as_str()
                    == "repo://example/repo/elixir/Packages.Client/create_pending_package_instances/1"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::Calls,
                    )
                && observation.confidence == beholder_domain::Confidence::Inferred
                && observation.provenance == beholder_domain::Provenance::Generated
        }));
        assert!(repository.grpc_bindings.iter().any(|binding| {
            binding.local_symbol.as_str()
                == "repo://example/repo/elixir/Packages.Client/create_pending_package_instances/1"
                && binding.service == "packages.v1.PackagesService"
                && binding.method == "CreatePendingPackageInstances"
        }));
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
