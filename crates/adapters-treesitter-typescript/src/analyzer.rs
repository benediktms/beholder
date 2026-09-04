use crate::{
    FRONTEND_VERSION, GraphqlFactInput, GraphqlResolverInput, GraphqlResolverSource,
    RESOLVER_VERSION, SourceLanguage, TypescriptAnalysis, TypescriptRepository,
    collect_graphql_facts, collect_graphql_resolvers, diagnostics_from_analysis,
    entities_from_analysis, resolve_repository_calls, resolve_workspace_calls,
    unresolved_call_diagnostics, unresolved_endpoint_entities,
};
use crate::{
    analysis::{analyze_with_plugins, semantics_from_analysis, source_stem},
    graphql::collect_grats_resolvers,
    plugin::{TypescriptLanguage, built_in_plugins},
};
use beholder_adapters_graphql::{GraphqlFacts, GraphqlSource};
use beholder_domain::{EntityFact, FactShard, Observation, SourceAnalysisError};
use beholder_indexing::{
    ActivePlugins, AnalysisCompleteness, AnalysisInputKind, AnalyzerContribution, AnalyzerError,
    AnalyzerMetadata, AnalyzerPlan, AnalyzerRepositoryPlan, CacheStatistics, LanguageAnalyzer,
    RepositoryContribution, RepositoryFactsView, WorkspaceAnalyzer, WorkspaceSnapshot,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CacheKey([u8; 32]);

#[derive(Clone)]
struct CachedAnalysis {
    analysis: Arc<TypescriptAnalysis>,
    semantic_shape: Arc<[u8]>,
}

#[derive(Clone, Copy)]
enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

type CachedSource = (Arc<TypescriptAnalysis>, Arc<[u8]>, CacheStatus);
type AnalyzedSource<'a> = (
    &'a Path,
    &'a str,
    Arc<TypescriptAnalysis>,
    Arc<[u8]>,
    CacheStatus,
);

pub struct TypescriptAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, CachedAnalysis>>,
    repository_cache: Mutex<BTreeMap<String, (CacheKey, RepositoryContribution)>>,
    plugins: LanguageAnalyzer<TypescriptLanguage>,
}

impl TypescriptAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("typescript"),
            cache: Mutex::new(BTreeMap::new()),
            repository_cache: Mutex::new(BTreeMap::new()),
            plugins: built_in_plugins().expect("built-in TypeScript plugins should compose"),
        }
    }

    fn analysis(
        &self,
        path: &Path,
        source: &str,
        language: SourceLanguage,
        active_plugins: &ActivePlugins,
        source_plugins: &str,
    ) -> Result<CachedSource, AnalyzerError> {
        let mut digest = Sha256::new();
        for part in [
            FRONTEND_VERSION.as_bytes(),
            path.as_os_str().as_encoded_bytes(),
            language.cache_version().as_bytes(),
            source.as_bytes(),
            source_plugins.as_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        let key = CacheKey(digest.finalize().into());
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((cached.analysis, cached.semantic_shape, CacheStatus::Memory));
        }
        let cache_path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&cache_path)
            && let Ok(analysis) = serde_json::from_slice::<TypescriptAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            let semantic_shape = semantic_analysis_shape(&analysis);
            self.cache
                .lock()
                .map_err(|_| "TypeScript frontend cache lock poisoned")?
                .insert(
                    key,
                    CachedAnalysis {
                        analysis: analysis.clone(),
                        semantic_shape: semantic_shape.clone(),
                    },
                );
            return Ok((analysis, semantic_shape, CacheStatus::Disk));
        }
        let analysis = Arc::new(analyze_with_plugins(
            source,
            language,
            path,
            &self.plugins,
            active_plugins,
        )?);
        let semantic_shape = semantic_analysis_shape(&analysis);
        if let Some(parent) = cache_path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
        {
            let _ = fs::write(cache_path, bytes);
        }
        self.cache
            .lock()
            .map_err(|_| "TypeScript frontend cache lock poisoned")?
            .insert(
                key,
                CachedAnalysis {
                    analysis: analysis.clone(),
                    semantic_shape: semantic_shape.clone(),
                },
            );
        Ok((analysis, semantic_shape, CacheStatus::Miss))
    }

    fn repository_cache_path(&self, key: &CacheKey) -> PathBuf {
        self.cache_dir
            .join("repository")
            .join(format!("{}.json", hex(key.0)))
    }

    fn cached_repository(
        &self,
        repository: &str,
        key: &CacheKey,
    ) -> Result<Option<RepositoryContribution>, AnalyzerError> {
        if let Some(contribution) = self
            .repository_cache
            .lock()
            .map_err(|_| "TypeScript repository cache lock poisoned")?
            .get(repository)
            .filter(|(cached_key, _)| cached_key == key)
            .map(|(_, contribution)| contribution.clone())
        {
            return Ok(Some(contribution));
        }
        let Ok(bytes) = fs::read(self.repository_cache_path(key)) else {
            return Ok(None);
        };
        let Ok(contribution) = serde_json::from_slice::<RepositoryContribution>(&bytes) else {
            return Ok(None);
        };
        if contribution.repository != repository {
            return Ok(None);
        }
        self.repository_cache
            .lock()
            .map_err(|_| "TypeScript repository cache lock poisoned")?
            .insert(repository.into(), (key.clone(), contribution.clone()));
        Ok(Some(contribution))
    }

    fn store_repository(
        &self,
        repository: &str,
        key: CacheKey,
        contribution: &RepositoryContribution,
    ) -> Result<(), AnalyzerError> {
        let path = self.repository_cache_path(&key);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(contribution)
        {
            let _ = fs::write(path, bytes);
        }
        self.repository_cache
            .lock()
            .map_err(|_| "TypeScript repository cache lock poisoned")?
            .insert(repository.into(), (key, contribution.clone()));
        Ok(())
    }
}

fn semantic_analysis_shape(analysis: &TypescriptAnalysis) -> Arc<[u8]> {
    serde_json::to_vec(&analysis.semantic_shape())
        .expect("TypeScript semantic shape should serialize")
        .into()
}

impl WorkspaceAnalyzer for TypescriptAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        let base = format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}");
        let plugins = self.plugins.identity();
        AnalyzerMetadata {
            id: "typescript".into(),
            version: format!("{}:{base}{}:{plugins}", base.len(), plugins.len()),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        crate::manifest::typescript_analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        crate::manifest::typescript_analysis_input_kind(path)
    }

    fn is_active(&self, repository: &beholder_indexing::RepositorySnapshot) -> bool {
        repository
            .inputs
            .iter()
            .any(|input| SourceLanguage::from_path(&input.path).is_some())
    }

    fn prepare(&self, snapshot: &WorkspaceSnapshot) -> AnalyzerPlan {
        let analyzer = AnalyzerMetadata {
            id: "typescript".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        };
        AnalyzerPlan::from_repositories(
            self.metadata(),
            snapshot.repositories.iter().filter_map(|repository| {
                let has_sources = repository
                    .inputs
                    .iter()
                    .any(|input| SourceLanguage::from_path(&input.path).is_some());
                self.plugins.prepare_repository(
                    analyzer.clone(),
                    repository,
                    has_sources,
                    has_sources,
                )
            }),
        )
    }

    fn repository_dependencies(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<beholder_domain::RepositoryDependencyCandidate>, AnalyzerError> {
        crate::manifest::typescript_repository_dependencies(snapshot)
    }

    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut typed_repositories = Vec::new();
        let mut cached_observations = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let inputs = repository
                .inputs
                .iter()
                .filter(|input| self.accepts(&input.path))
                .collect::<Vec<_>>();
            let sources = inputs
                .iter()
                .filter_map(|input| {
                    SourceLanguage::from_path(&input.path).map(|language| (*input, language))
                })
                .map(|(input, language)| {
                    text(input).map(|source| (input.path.as_path(), source, language))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if sources.is_empty() {
                continue;
            }
            active_repositories.push(repository.state.repository.identity.clone());
            let repository_plan = plan
                .repository(&repository.state.repository.identity)
                .ok_or("missing prepared TypeScript repository")?;
            let active_plugins = &repository_plan.active_plugins;
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
            let analyzed_results = sources
                .par_iter()
                .map(|(path, source, language)| {
                    let analysis = self
                        .analysis(
                            path,
                            source,
                            *language,
                            active_plugins,
                            &repository_plan.source_plugins,
                        )
                        .map_err(|error| SourceAnalysisError::from_source(path, error));
                    (*path, *source, analysis)
                })
                .collect::<Vec<_>>();
            let mut observations = Vec::new();
            let mut semantic_candidates = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            let mut analyzed = Vec::new();
            for (path, source, result) in analyzed_results {
                match result {
                    Ok((analysis, semantic_shape, status)) => {
                        analyzed.push((path, source, analysis, semantic_shape, status));
                    }
                    Err(error) if error.is_unsafe_recovery() => {
                        diagnostics.push(beholder_domain::AnalysisDiagnostic {
                            code: "typescript.parse_recovery".into(),
                            severity: beholder_domain::AnalysisDiagnosticSeverity::Warning,
                            path: path.into(),
                            line: None,
                            detail: Some(
                                "tree-sitter could not recover this source safely; the file was skipped"
                                    .into(),
                            ),
                        });
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            for (_, _, _, _, status) in &analyzed {
                match status {
                    CacheStatus::Memory => cache.memory_hits += 1,
                    CacheStatus::Disk => cache.disk_hits += 1,
                    CacheStatus::Miss => cache.misses += 1,
                }
            }
            let typed_repository = TypescriptRepository::new(
                repository.state.repository.identity.clone(),
                analyzed
                    .iter()
                    .map(|(path, _, analysis, _, _)| ((*path).to_path_buf(), analysis.clone()))
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
            let resolver_sources = analyzed
                .iter()
                .map(|(path, source, analysis, _, _)| GraphqlResolverSource {
                    path,
                    analysis,
                    source,
                })
                .collect::<Vec<_>>();
            let resolver_input = GraphqlResolverInput {
                repository: &repository.state.repository.identity,
                sources: &resolver_sources,
                manifests: &manifests,
            };
            let grats_resolvers = collect_grats_resolvers(&resolver_input);
            let semantic_key = repository_semantic_key(
                repository_plan,
                &analyzed,
                &manifests,
                &configs,
                &schemas,
                &diagnostics,
                &grats_resolvers,
            );
            if let Some(cached) = plan.cached_repository(&repository.state.repository.identity) {
                cached_observations.extend_from_slice(cached.observations);
                cached_observations.extend(
                    plan.cached_fact_shards(&repository.state.repository.identity)
                        .into_iter()
                        .flatten()
                        .flat_map(|shard| shard.observations.iter().cloned()),
                );
                typed_repositories.push(typed_repository);
                continue;
            }
            if let Some(cached) =
                self.cached_repository(&repository.state.repository.identity, &semantic_key)?
            {
                typed_repositories.push(typed_repository);
                repositories.push(cached);
                continue;
            }
            for (path, source, analysis, _, _) in &analyzed {
                let (source_observations, source_candidates) = semantics_from_analysis(
                    &repository.state.repository.identity,
                    analysis,
                    source,
                    path,
                );
                observations.extend(source_observations);
                semantic_candidates.extend(source_candidates);
                entities.extend(entities_from_analysis(
                    &repository.state.repository.identity,
                    analysis,
                    path,
                ));
                diagnostics.extend(diagnostics_from_analysis(analysis, path));
            }
            let source_refs = analyzed
                .iter()
                .map(|(path, _, analysis, _, _)| (*path, analysis.as_ref()))
                .collect::<Vec<_>>();
            let graphql_resolvers = collect_graphql_resolvers(resolver_input);
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
            entities.extend(unresolved_endpoint_entities(&observations));
            let fact_shards = build_fact_shards(
                &repository.state.repository.identity,
                &repository_plan.analysis.version,
                &analyzed,
                &entities,
                &observations,
            );
            typed_repositories.push(typed_repository);
            let completeness = if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.ends_with(".parse_recovery"))
            {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            };
            let contribution = RepositoryContribution {
                repository: repository.state.repository.identity.clone(),
                completeness,
                entities,
                grpc_bindings: enrichment.grpc_bindings,
                observations,
                semantic_candidates,
                diagnostics,
                replaced_diagnostic_codes: Default::default(),
                fact_shards,
            };
            self.store_repository(
                &repository.state.repository.identity,
                semantic_key,
                &contribution,
            )?;
            repositories.push(contribution);
        }

        let mut all_observations = cached_observations;
        all_observations.extend(
            repositories
                .iter()
                .flat_map(|repository| repository.observations.iter().cloned()),
        );
        let overrides = resolve_workspace_calls(&mut all_observations, &typed_repositories);
        for repository in &mut repositories {
            retain_unresolved_candidates(&mut repository.semantic_candidates, &all_observations);
        }
        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides,
            candidate_overrides: Vec::new(),
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
        self.repository_cache
            .lock()
            .map_err(|_| "TypeScript repository cache lock poisoned")?
            .clear();
        Ok(())
    }
}

fn repository_semantic_key(
    repository_plan: &AnalyzerRepositoryPlan,
    analyzed: &[AnalyzedSource<'_>],
    manifests: &[(&Path, &str)],
    configs: &[(&Path, &str)],
    schemas: &[(&Path, &str)],
    diagnostics: &[beholder_domain::AnalysisDiagnostic],
    grats_resolvers: &GraphqlFacts,
) -> CacheKey {
    let mut digest = Sha256::new();
    for part in [
        b"beholder-typescript-repository-v2".as_slice(),
        repository_plan.repository.as_bytes(),
        repository_plan.analysis.version.as_bytes(),
        repository_plan.source_plugins.as_bytes(),
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    for (path, _, _, semantic_shape, _) in analyzed {
        for part in [path.as_os_str().as_encoded_bytes(), semantic_shape.as_ref()] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
    }
    for (kind, inputs) in [
        (b"manifests".as_slice(), manifests),
        (b"configs".as_slice(), configs),
        (b"schemas".as_slice(), schemas),
    ] {
        digest.update((kind.len() as u64).to_le_bytes());
        digest.update(kind);
        for (path, source) in inputs {
            for part in [path.as_os_str().as_encoded_bytes(), source.as_bytes()] {
                digest.update((part.len() as u64).to_le_bytes());
                digest.update(part);
            }
        }
    }
    for diagnostic in diagnostics {
        for part in [
            diagnostic.code.as_bytes(),
            diagnostic.severity.as_str().as_bytes(),
            diagnostic.path.as_os_str().as_encoded_bytes(),
            diagnostic.detail.as_deref().unwrap_or_default().as_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        digest.update(diagnostic.line.unwrap_or_default().to_le_bytes());
    }
    hash_graphql_resolvers(&mut digest, grats_resolvers);
    CacheKey(digest.finalize().into())
}

fn hash_graphql_resolvers(digest: &mut Sha256, facts: &GraphqlFacts) {
    let mut semantic_facts = facts
        .entities
        .iter()
        .map(|entity| serde_json::to_vec(&("entity", &entity.id, entity.kind, entity.metadata)))
        .chain(facts.observations.iter().map(|observation| {
            serde_json::to_vec(&(
                "observation",
                &observation.from,
                observation.relation,
                &observation.to,
                observation.confidence,
                observation.provenance,
            ))
        }))
        .chain(facts.diagnostics.iter().map(|diagnostic| {
            serde_json::to_vec(&(
                "diagnostic",
                &diagnostic.code,
                diagnostic.severity,
                &diagnostic.path,
                &diagnostic.detail,
            ))
        }))
        .collect::<Result<Vec<_>, _>>()
        .expect("GraphQL resolver semantics should serialize");
    semantic_facts.sort();
    for fact in semantic_facts {
        digest.update((fact.len() as u64).to_le_bytes());
        digest.update(fact);
    }
}

fn retain_unresolved_candidates(
    candidates: &mut Vec<beholder_domain::SemanticCandidate>,
    observations: &[Observation],
) {
    let unresolved = observations
        .iter()
        .filter(|observation| {
            observation.relation
                == beholder_domain::SemanticRelation::Dependency(
                    beholder_domain::DependencyRelation::Calls,
                )
        })
        .map(|observation| {
            (
                observation.from.as_str(),
                observation.to.as_str(),
                observation.evidence.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    candidates.retain(|candidate| {
        unresolved.contains(&(
            candidate.from.as_str(),
            candidate.unresolved_to.as_str(),
            candidate.evidence.as_str(),
        ))
    });
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

fn build_fact_shards(
    repository: &str,
    analyzer_version: &str,
    analyzed: &[AnalyzedSource<'_>],
    entities: &[EntityFact],
    observations: &[Observation],
) -> Vec<FactShard> {
    analyzed
        .iter()
        .map(|(path, _, analysis, semantic_shape, _)| {
            let owner = format!(
                "repo://{repository}/{}/{}",
                analysis.language.id_segment(),
                source_stem(path)
            );
            let prefix = format!("{owner}/");
            let owned = |id: &str| id == owner || id.starts_with(&prefix);
            let entities = entities
                .iter()
                .filter(|entity| owned(entity.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let observations = observations
                .iter()
                .filter(|observation| owned(observation.from.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let mut digest = Sha256::new();
            for part in [
                analyzer_version.as_bytes(),
                owner.as_bytes(),
                semantic_shape.as_ref(),
            ] {
                digest.update((part.len() as u64).to_le_bytes());
                digest.update(part);
            }
            for entity in &entities {
                digest.update(entity.id.as_str().as_bytes());
                digest.update(format!("{:?}", entity.kind).as_bytes());
                digest.update(serde_json::to_vec(&entity.metadata).unwrap_or_default());
            }
            for observation in &observations {
                digest.update(observation.from.as_str().as_bytes());
                digest.update(observation.relation.as_str().as_bytes());
                digest.update(observation.to.as_str().as_bytes());
                digest.update(observation.confidence.score().to_le_bytes());
                digest.update(observation.provenance.as_str().as_bytes());
            }
            FactShard {
                repository: repository.into(),
                producer: "typescript".into(),
                owner: owner.into(),
                version: format!("{:x}", digest.finalize()),
                entities,
                observations,
            }
        })
        .collect()
}

fn text(input: &beholder_indexing::RepositoryInput) -> Result<&str, SourceAnalysisError> {
    std::str::from_utf8(&input.content)
        .map_err(|error| SourceAnalysisError::from_source(&input.path, Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};

    fn snapshot(source: &[u8], fingerprint: &str) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            name: "test".into(),
            repositories: vec![RepositorySnapshot {
                base: PathBuf::from("repo"),
                state: RepositoryState {
                    repository: LogicalRepository {
                        identity: "example/repo".into(),
                    },
                    head: None,
                    fingerprint: fingerprint.into(),
                },
                inputs: vec![RepositoryInput {
                    path: PathBuf::from("src/index.ts"),
                    content: Arc::from(source),
                    kind: InputKind::Source,
                }],
            }],
        }
    }

    #[test]
    fn semantic_shards_ignore_trivia_but_change_with_calls() {
        let cache_dir =
            std::env::temp_dir().join(format!("beholder-typescript-shards-{}", std::process::id()));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let version = |source: &[u8], fingerprint: &str| {
            analyzer
                .analyze(&snapshot(source, fingerprint))
                .unwrap()
                .repositories[0]
                .fact_shards[0]
                .version
                .clone()
        };

        let initial = version(b"export function run() { return first(); }", "initial");
        let initial_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();
        let formatted = version(
            b"// comment\n\nexport function run() {\n  return first();\n}\n",
            "formatted",
        );
        let formatted_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();
        let changed = version(b"export function run() { return second(); }", "changed");
        let changed_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();

        assert_eq!(initial, formatted);
        assert_eq!(initial_key, formatted_key);
        assert_ne!(initial, changed);
        assert_ne!(initial_key, changed_key);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn svelte_and_typescript_siblings_have_distinct_fact_shard_owners() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-svelte-siblings-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let mut snapshot = snapshot(b"export const load = () => {};", "siblings");
        snapshot.repositories[0].inputs[0].path = PathBuf::from("src/+layout.ts");
        snapshot.repositories[0].inputs.push(RepositoryInput {
            path: PathBuf::from("src/+layout.svelte"),
            content: Arc::from(&b"<script>export const prerender = true;</script>"[..]),
            kind: InputKind::Source,
        });

        let contribution = analyzer.analyze(&snapshot).unwrap();
        let owners = contribution.repositories[0]
            .fact_shards
            .iter()
            .map(|shard| shard.owner.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(owners.len(), 2);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn semantic_cache_changes_when_a_call_crosses_an_alias_assignment() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-alias-order-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let key = |source: &[u8], fingerprint: &str| {
            analyzer.analyze(&snapshot(source, fingerprint)).unwrap();
            analyzer.repository_cache.lock().unwrap()["example/repo"]
                .0
                .clone()
        };

        let before = key(
            b"export function run(context: Context) { let selected = first; context.get(selected).load(); selected = second; }",
            "before",
        );
        let after = key(
            b"export function run(context: Context) { let selected = first; selected = second; context.get(selected).load(); }",
            "after",
        );

        assert_ne!(before, after);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn semantic_cache_changes_when_scope_containment_changes() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-scope-containment-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let key = |source: &[u8], fingerprint: &str| {
            analyzer.analyze(&snapshot(source, fingerprint)).unwrap();
            analyzer.repository_cache.lock().unwrap()["example/repo"]
                .0
                .clone()
        };

        let nested = key(
            b"export function run(context: Context) { { const selected = first; { context.get(selected).load(); } } }",
            "nested",
        );
        let siblings = key(
            b"export function run(context: Context) { { const selected = first; } { context.get(selected).load(); } }",
            "siblings",
        );

        assert_ne!(nested, siblings);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn repository_semantics_are_reused_after_restart() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-repository-cache-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let initial = analyzer
            .analyze(&snapshot(
                b"export function run() { return first(); }",
                "initial",
            ))
            .unwrap();
        let key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();
        assert!(analyzer.repository_cache_path(&key).is_file());
        drop(analyzer);

        let restarted = TypescriptAnalyzer::new(cache_dir.clone());
        let formatted = restarted
            .analyze(&snapshot(
                b"// comment\nexport function run() {\n  return first();\n}",
                "formatted",
            ))
            .unwrap();

        assert_eq!(initial.repositories, formatted.repositories);
        assert_eq!(
            restarted.repository_cache.lock().unwrap()["example/repo"].0,
            key
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn repository_semantics_include_comment_derived_graphql_resolvers() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-graphql-cache-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let analyze = |source: &[u8], fingerprint: &str| {
            let mut snapshot = snapshot(source, fingerprint);
            snapshot.repositories[0].inputs.push(RepositoryInput {
                path: PathBuf::from("package.json"),
                content: Arc::from(&br#"{"dependencies":{"grats":"0.0.34"}}"#[..]),
                kind: InputKind::Source,
            });
            analyzer.analyze(&snapshot).unwrap()
        };

        let initial = analyze(
            b"/** @gqlQueryField first */\nexport function run() { return 1; }",
            "initial",
        );
        let initial_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();
        let formatted = analyze(
            b"/**\n * @gqlQueryField first\n */\nexport function run() {\n  return 1;\n}",
            "formatted",
        );
        let formatted_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();
        let changed = analyze(
            b"/** @gqlQueryField second */\nexport function run() { return 1; }",
            "changed",
        );
        let changed_key = analyzer.repository_cache.lock().unwrap()["example/repo"]
            .0
            .clone();

        assert_eq!(initial.repositories, formatted.repositories);
        assert_eq!(initial_key, formatted_key);
        assert_ne!(initial_key, changed_key);
        assert!(
            changed.repositories[0]
                .entities
                .iter()
                .any(|entity| { entity.id.as_str() == "graphql-field://Query/second" })
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn compiler_candidates_exclude_calls_resolved_by_the_syntax_frontend() {
        let cache_dir = std::env::temp_dir().join(format!(
            "beholder-typescript-candidates-{}",
            std::process::id()
        ));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let mut snapshot = snapshot(
            b"import { helper } from './helper'; export function run(api: Api) { helper(); api.send(); }",
            "state",
        );
        snapshot.repositories[0].inputs.push(RepositoryInput {
            path: PathBuf::from("src/helper.ts"),
            content: Arc::from(&b"export function helper() {}"[..]),
            kind: InputKind::Source,
        });

        let contribution = analyzer.analyze(&snapshot).unwrap();
        let candidates = &contribution.repositories[0].semantic_candidates;

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].unresolved_to.as_str(),
            "typescript-method://api/send"
        );
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn skips_unsafe_source_without_aborting_repository() {
        let cache_dir =
            std::env::temp_dir().join(format!("beholder-typescript-skip-{}", std::process::id()));
        let analyzer = TypescriptAnalyzer::new(cache_dir.clone());
        let snapshot = WorkspaceSnapshot {
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
                inputs: vec![
                    RepositoryInput {
                        path: PathBuf::from("src/valid.ts"),
                        content: Arc::from(&b"export function valid() {}"[..]),
                        kind: InputKind::Source,
                    },
                    RepositoryInput {
                        path: PathBuf::from("src/broken.ts"),
                        content: Arc::from(&b"const broken = {"[..]),
                        kind: InputKind::Source,
                    },
                ],
            }],
        };

        let contribution = analyzer.analyze(&snapshot).unwrap();

        assert_eq!(contribution.repositories.len(), 1);
        assert_eq!(
            contribution.repositories[0].completeness,
            AnalysisCompleteness::Incomplete
        );
        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == Path::new("src/broken.ts"))
        );
        assert!(!contribution.repositories[0].entities.is_empty());

        let mut moved = snapshot;
        moved.repositories[0].state.fingerprint = "moved".into();
        moved.repositories[0].inputs[1].path = PathBuf::from("src/moved-broken.ts");
        let contribution = analyzer.analyze(&moved).unwrap();

        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == Path::new("src/moved-broken.ts"))
        );
        let _ = fs::remove_dir_all(cache_dir);
    }
}
