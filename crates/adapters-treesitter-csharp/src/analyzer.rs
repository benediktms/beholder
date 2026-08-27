use crate::{
    CsharpAnalysis, CsharpProject, CsharpSource, FRONTEND_VERSION, RESOLVER_VERSION, UnityPrefab,
    analyze, diagnostics_from_analysis, entities_from_analysis, observations_from_analysis,
    parse_project, parse_unity_assemblies, parse_unity_meta, parse_unity_prefab, source_assemblies,
    unity_lifecycle, unity_prefab_dependencies,
};
use crate::{
    model::{CsharpRepository, CsharpRepositorySource},
    plugin::{CsharpLanguage, built_in_plugins},
    resolution::resolve_language_calls,
};
use beholder_domain::{AnalysisDiagnostic, AnalysisDiagnosticSeverity, SourceAnalysisError};
use beholder_indexing::{
    AnalysisCompleteness, AnalysisInputKind, AnalyzerContribution, AnalyzerError, AnalyzerMetadata,
    AnalyzerPlan, CacheStatistics, LanguageAnalyzer, RepositoryContribution, RepositoryFactsView,
    WorkspaceAnalyzer, WorkspaceSnapshot,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs,
    io::Cursor,
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

pub struct CsharpAnalyzer {
    cache_dir: PathBuf,
    cache: Mutex<BTreeMap<CacheKey, Arc<CsharpAnalysis>>>,
    plugins: LanguageAnalyzer<CsharpLanguage>,
}

impl CsharpAnalyzer {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("csharp").join(FRONTEND_VERSION),
            cache: Mutex::new(BTreeMap::new()),
            plugins: built_in_plugins().expect("built-in C# plugins should compose"),
        }
    }

    fn analysis(
        &self,
        path: &Path,
        source: &str,
        source_plugins: &str,
    ) -> Result<(Arc<CsharpAnalysis>, CacheStatus), AnalyzerError> {
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
            .map_err(|_| "C# frontend cache lock poisoned")?
            .get(&key)
            .cloned()
        {
            return Ok((analysis, CacheStatus::Memory));
        }
        let path = self.cache_dir.join(format!("{}.json", hex(key.0)));
        if let Ok(bytes) = fs::read(&path)
            && let Ok(analysis) = serde_json::from_slice::<CsharpAnalysis>(&bytes)
        {
            let analysis = Arc::new(analysis);
            self.cache
                .lock()
                .map_err(|_| "C# frontend cache lock poisoned")?
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
            .map_err(|_| "C# frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        Ok((analysis, CacheStatus::Miss))
    }
}

impl WorkspaceAnalyzer for CsharpAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        let base = format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}");
        let plugins = self.plugins.identity();
        AnalyzerMetadata {
            id: "csharp".into(),
            version: format!("{}:{base}{}:{plugins}", base.len(), plugins.len()),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        crate::manifest::csharp_analysis_input_kind(path).is_some()
    }

    fn analysis_input_kind(&self, path: &Path) -> Option<AnalysisInputKind> {
        crate::manifest::csharp_analysis_input_kind(path)
    }

    fn prepare(&self, snapshot: &WorkspaceSnapshot) -> AnalyzerPlan {
        let analyzer = AnalyzerMetadata {
            id: "csharp".into(),
            version: format!("{FRONTEND_VERSION}:{RESOLVER_VERSION}"),
        };
        AnalyzerPlan::from_repositories(
            self.metadata(),
            snapshot.repositories.iter().filter_map(|repository| {
                let has_sources = repository.inputs.iter().any(|input| {
                    input
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "cs")
                });
                self.plugins.prepare_repository(
                    analyzer.clone(),
                    repository,
                    self.is_active(repository),
                    has_sources,
                )
            }),
        )
    }

    fn repository_dependencies(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<beholder_domain::RepositoryDependencyCandidate>, AnalyzerError> {
        crate::manifest::csharp_repository_dependencies(snapshot)
    }

    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        let mut cache = CacheStatistics::default();

        for repository in &snapshot.repositories {
            let inputs = repository
                .inputs
                .iter()
                .filter(|input| self.accepts(&input.path))
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                continue;
            }
            active_repositories.push(repository.state.repository.identity.clone());
            let repository_plan = plan
                .repository(&repository.state.repository.identity)
                .ok_or("missing prepared C# repository")?;
            if plan
                .cached_repository(&repository.state.repository.identity)
                .is_some()
            {
                continue;
            }
            let project_inputs = inputs
                .iter()
                .filter(|input| {
                    input.path.extension().is_some_and(|extension| {
                        matches!(extension.to_str(), Some("csproj" | "asmdef"))
                    })
                })
                .map(|input| {
                    std::str::from_utf8(&input.content)
                        .map(|source| (input.path.clone(), source.to_owned()))
                        .map_err(|error| {
                            SourceAnalysisError::from_source(&input.path, Box::new(error))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut unity_prefabs = Vec::<UnityPrefab>::new();
            let mut unity_script_metas = Vec::new();
            let mut unity_prefab_metas = Vec::new();
            for input in &inputs {
                let name = input
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if name.ends_with(".prefab") {
                    unity_prefabs.push(parse_unity_prefab(
                        &input.path,
                        Cursor::new(input.content.as_ref()),
                    )?);
                } else if name.ends_with(".meta")
                    && let Some((guid, fingerprint)) =
                        parse_unity_meta(Cursor::new(input.content.as_ref()))?
                {
                    let asset_path = input.path.with_extension("");
                    if name.ends_with(".cs.meta") {
                        unity_script_metas.push((asset_path, guid, fingerprint));
                    } else {
                        unity_prefab_metas.push((asset_path, guid, fingerprint));
                    }
                }
            }
            let unity_assemblies = project_inputs
                .iter()
                .filter(|(path, _)| {
                    path.extension()
                        .is_some_and(|extension| extension == "asmdef")
                })
                .cloned()
                .collect::<Vec<_>>();
            let is_unity = !unity_assemblies.is_empty() || !unity_prefabs.is_empty();
            let projects = if is_unity {
                parse_unity_assemblies(&unity_assemblies)?
            } else {
                project_inputs
                    .iter()
                    .map(|(path, source)| parse_project(path, source))
                    .collect::<Result<Vec<CsharpProject>, _>>()?
            };
            let sources = inputs
                .iter()
                .filter(|input| {
                    input
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "cs")
                })
                .collect::<Vec<_>>();
            let active_plugins = &repository_plan.active_plugins;
            let analyzed = sources
                .par_iter()
                .map(|input| {
                    let (source, lossy) = decode_source(&input.content);
                    let (analysis, status) = self
                        .analysis(&input.path, &source, &repository_plan.source_plugins)
                        .map_err(|error| SourceAnalysisError::from_source(&input.path, error))?;
                    Ok::<_, SourceAnalysisError>((
                        input.path.clone(),
                        source.into_owned(),
                        analysis,
                        status,
                        lossy,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut observations = Vec::new();
            let mut entities = Vec::new();
            let mut diagnostics = Vec::new();
            let mut analyzed_assemblies = Vec::new();
            for (path, source, analysis, status, lossy) in &analyzed {
                match status {
                    CacheStatus::Memory => cache.memory_hits += 1,
                    CacheStatus::Disk => cache.disk_hits += 1,
                    CacheStatus::Miss => cache.misses += 1,
                }
                diagnostics.extend(diagnostics_from_analysis(analysis, path));
                if *lossy {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "csharp.lossy_encoding".into(),
                        severity: AnalysisDiagnosticSeverity::Warning,
                        path: path.clone(),
                        line: None,
                        detail: Some(
                            "source contained an unsupported text encoding and was decoded lossily"
                                .into(),
                        ),
                    });
                }
                for assembly in source_assemblies(&projects, path) {
                    observations.extend(observations_from_analysis(
                        &repository.state.repository.identity,
                        &assembly,
                        analysis,
                        source,
                        path,
                    ));
                    entities.extend(entities_from_analysis(
                        &repository.state.repository.identity,
                        &assembly,
                        analysis,
                        path,
                    ));
                    analyzed_assemblies.push((path.clone(), assembly, analysis.clone()));
                }
            }
            let source_refs = analyzed_assemblies
                .iter()
                .map(|(path, assembly, analysis)| CsharpSource {
                    path,
                    assembly,
                    analysis,
                })
                .collect::<Vec<_>>();
            if is_unity {
                let (unity_entities, unity_observations) = unity_lifecycle(
                    &repository.state.repository.identity,
                    &projects,
                    &source_refs,
                );
                entities.extend(unity_entities);
                observations.extend(unity_observations);
                let script_paths = unity_script_metas
                    .iter()
                    .map(|(path, guid, _)| (guid.clone(), path.clone()))
                    .collect();
                let prefab_paths = unity_prefab_metas
                    .iter()
                    .map(|(path, guid, _)| (guid.clone(), path.clone()))
                    .collect();
                let (prefab_entities, prefab_observations, prefab_diagnostics) =
                    unity_prefab_dependencies(
                        &repository.state.repository.identity,
                        &unity_prefabs,
                        &script_paths,
                        &prefab_paths,
                        &source_refs,
                    );
                entities.extend(prefab_entities);
                observations.extend(prefab_observations);
                diagnostics.extend(prefab_diagnostics);
            }
            observations.extend(resolve_language_calls(
                &repository.state.repository.identity,
                &projects,
                &source_refs,
            ));
            let typed_repository = CsharpRepository {
                repository: repository.state.repository.identity.clone(),
                projects: projects.clone(),
                sources: analyzed_assemblies
                    .iter()
                    .map(|(path, assembly, analysis)| CsharpRepositorySource {
                        path: path.clone(),
                        assembly: assembly.clone(),
                        analysis: analysis.as_ref().clone(),
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
                fact_shards: Vec::new(),
            });
        }

        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache,
        })
    }

    fn clear_cache(&self) -> Result<(), AnalyzerError> {
        self.cache
            .lock()
            .map_err(|_| "C# frontend cache lock poisoned")?
            .clear();
        Ok(())
    }
}

fn decode_source(bytes: &[u8]) -> (Cow<'_, str>, bool) {
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return match std::str::from_utf8(bytes) {
            Ok(source) => (Cow::Borrowed(source), false),
            Err(_) => (String::from_utf8_lossy(bytes), true),
        };
    }
    for (bom, little_endian) in [(&[0xff, 0xfe][..], true), (&[0xfe, 0xff][..], false)] {
        let Some(bytes) = bytes.strip_prefix(bom) else {
            continue;
        };
        let mut chunks = bytes.chunks_exact(2);
        let units = chunks
            .by_ref()
            .map(|bytes| {
                if little_endian {
                    u16::from_le_bytes([bytes[0], bytes[1]])
                } else {
                    u16::from_be_bytes([bytes[0], bytes[1]])
                }
            })
            .collect::<Vec<_>>();
        return match String::from_utf16(&units) {
            Ok(source) if chunks.remainder().is_empty() => (Cow::Owned(source), false),
            _ => (Cow::Owned(String::from_utf16_lossy(&units)), true),
        };
    }
    match std::str::from_utf8(bytes) {
        Ok(source) => (Cow::Borrowed(source), false),
        Err(_) => (String::from_utf8_lossy(bytes), true),
    }
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}
