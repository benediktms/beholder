use beholder_adapters_treesitter_rust::{
    RustAnalyzer, analyze, source_entity_id, validate_immutable_rust_inputs,
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, Provenance, SemanticRelation, UnsafeTreeRecovery,
};
use beholder_indexing::{AnalyzerContribution, WorkspaceAnalyzer, WorkspaceSnapshot};
use beholder_protocol::{
    WorkspaceSnapshotBuilder, analyze_events,
    worker_v1::{
        AnalysisPhase, AnalysisProgress, AnalyzeEvent, AnalyzeRequest, analyze_event,
        analyzer_worker_server::{AnalyzerWorker, AnalyzerWorkerServer},
    },
};
use ra_ap_ide::{
    AnalysisHost, FileId, FilePosition, GotoDefinitionConfig, RaFixtureConfig, TextSize,
};
use ra_ap_ide_db::ChangeWithProcMacros;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, CargoFeatures};
use ra_ap_vfs::VfsPath;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::{Request, Response, Status, Streaming};
use tracing::Instrument;

const ANALYZER_VERSION: &str = "7:7:rust.tonic:1:rust-analyzer-0.0.348:worker-7";
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
static MATERIALIZATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct RustWorker {
    analyzer: Arc<RustAnalyzer>,
    analysis_pool: Arc<rayon::ThreadPool>,
}

#[tonic::async_trait]
impl AnalyzerWorker for RustWorker {
    type AnalyzeStream = ReceiverStream<Result<AnalyzeEvent, Status>>;

    async fn analyze(
        &self,
        request: Request<Streaming<AnalyzeRequest>>,
    ) -> Result<Response<Self::AnalyzeStream>, Status> {
        let span = tracing::info_span!(
            "worker.analyze",
            workspace = tracing::field::Empty,
            rpc.system = "grpc",
            rpc.service = "beholder.worker.v1.AnalyzerWorker",
            rpc.method = "Analyze"
        );
        beholder_observability::set_parent_from_metadata(&span, request.metadata());
        let mut stream = request.into_inner();
        let analyzer = self.analyzer.clone();
        let analysis_pool = self.analysis_pool.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        tokio::spawn(
            async move {
                if send_progress(&sender, AnalysisPhase::ReceivingSnapshot).await {
                    return;
                }
                let mut snapshot = WorkspaceSnapshotBuilder::default();
                loop {
                    match stream.message().await {
                        Ok(Some(request)) => {
                            if let Err(error) = snapshot.push(request) {
                                let _ = sender.send(Err(Status::invalid_argument(error))).await;
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    }
                }
                let snapshot = match snapshot.finish() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let _ = sender.send(Err(Status::invalid_argument(error))).await;
                        return;
                    }
                };
                tracing::Span::current().record("workspace", &snapshot.workspace.name);
                if send_progress(&sender, AnalysisPhase::Analyzing).await {
                    return;
                }
                let analysis_span = tracing::info_span!(
                    "worker.rust.semantic_analysis",
                    workspace = snapshot.workspace.name,
                    target_repository = snapshot.target_repository,
                    repositories = snapshot.workspace.repositories.len()
                );
                let result = tokio::task::spawn_blocking(
                    move || -> Result<Vec<AnalyzeEvent>, Box<dyn Error + Send + Sync>> {
                        analysis_span.in_scope(|| {
                            analysis_pool.install(|| {
                                let mut contribution = analyzer.analyze(&snapshot.workspace)?;
                                enrich_semantics(
                                    &snapshot.workspace,
                                    &snapshot.target_repository,
                                    &mut contribution,
                                );
                                retain_semantic_enrichment(
                                    &mut contribution,
                                    &snapshot.target_repository,
                                );
                                contribution.metadata.version = ANALYZER_VERSION.into();
                                analyze_events(contribution).map_err(Into::into)
                            })
                        })
                    },
                )
                .await;
                let events = match result {
                    Ok(Ok(events)) => events,
                    Ok(Err(error)) => {
                        let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                        return;
                    }
                    Err(error) => {
                        let _ = sender
                            .send(Err(Status::internal(format!(
                                "Rust worker task failed: {error}"
                            ))))
                            .await;
                        return;
                    }
                };
                for event in events {
                    if sender.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
            .instrument(span),
        );
        Ok(Response::new(ReceiverStream::new(receiver)))
    }
}

async fn send_progress(
    sender: &tokio::sync::mpsc::Sender<Result<AnalyzeEvent, Status>>,
    phase: AnalysisPhase,
) -> bool {
    sender
        .send(Ok(AnalyzeEvent {
            event: Some(analyze_event::Event::Progress(AnalysisProgress {
                phase: phase as i32,
                detail: None,
            })),
        }))
        .await
        .is_err()
}

struct MaterializedWorkspace {
    root: PathBuf,
    repositories: BTreeMap<String, PathBuf>,
}

impl MaterializedWorkspace {
    fn new(snapshot: &WorkspaceSnapshot) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let bases = snapshot
            .repositories
            .iter()
            .map(|repository| absolute_lexical(&repository.base))
            .collect::<Vec<_>>();
        let mut common = bases
            .first()
            .cloned()
            .ok_or("Rust enrichment snapshot contains no repositories")?;
        while !bases.iter().all(|base| base.starts_with(&common)) {
            if !common.pop() {
                return Err("Rust enrichment repositories have no common filesystem root".into());
            }
        }
        let root = std::env::temp_dir().join(format!(
            "beholder-rust-snapshot-{}-{}",
            std::process::id(),
            MATERIALIZATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        let result = (|| {
            let mut repositories = BTreeMap::new();
            for (repository, base) in snapshot.repositories.iter().zip(bases) {
                let relative = base.strip_prefix(&common)?;
                let materialized_base = root.join(relative);
                if repositories
                    .values()
                    .any(|existing| existing == &materialized_base)
                {
                    return Err(
                        "Rust enrichment snapshot maps repositories to the same directory".into(),
                    );
                }
                if repositories
                    .insert(
                        repository.state.repository.identity.clone(),
                        materialized_base.clone(),
                    )
                    .is_some()
                {
                    return Err("Rust enrichment snapshot contains duplicate repositories".into());
                }
                for input in &repository.inputs {
                    if input.path.is_absolute()
                        || input
                            .path
                            .components()
                            .any(|component| matches!(component, std::path::Component::ParentDir))
                    {
                        return Err(format!(
                            "Rust enrichment input escapes its repository: {}",
                            input.path.display()
                        )
                        .into());
                    }
                    let destination = materialized_base.join(&input.path);
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(destination, input.content.as_ref())?;
                }
            }
            Ok::<_, Box<dyn Error + Send + Sync>>(repositories)
        })();
        match result {
            Ok(repositories) => Ok(Self { root, repositories }),
            Err(error) => {
                let _ = fs::remove_dir_all(&root);
                Err(error)
            }
        }
    }

    fn repository(&self, identity: &str) -> Option<&Path> {
        self.repositories.get(identity).map(PathBuf::as_path)
    }

    fn verify(&self, snapshot: &WorkspaceSnapshot) -> Result<(), Box<dyn Error + Send + Sync>> {
        for repository in &snapshot.repositories {
            let base = self
                .repository(&repository.state.repository.identity)
                .ok_or("materialized Rust repository is missing")?;
            for input in &repository.inputs {
                if fs::read(base.join(&input.path))?.as_slice() != input.content.as_ref() {
                    return Err(format!(
                        "{} changed during immutable Rust analysis",
                        input.path.display()
                    )
                    .into());
                }
            }
        }
        Ok(())
    }
}

impl Drop for MaterializedWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub async fn serve(socket: &Path, cache_dir: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket)?;
    let analysis_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .thread_name(|index| format!("beholder-rust-{index}"))
            .stack_size(16 * 1024 * 1024)
            .build()?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    }
    tonic::transport::Server::builder()
        .add_service(
            AnalyzerWorkerServer::new(RustWorker {
                analyzer: Arc::new(RustAnalyzer::new(cache_dir)),
                analysis_pool,
            })
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES),
        )
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

fn enrich_semantics(
    snapshot: &WorkspaceSnapshot,
    target_repository: &str,
    contribution: &mut AnalyzerContribution,
) {
    let Some(repository) = snapshot
        .repositories
        .iter()
        .find(|repository| repository.state.repository.identity == target_repository)
    else {
        return;
    };
    let materialized = match MaterializedWorkspace::new(snapshot) {
        Ok(materialized) => materialized,
        Err(error) => {
            contribution.diagnostics.push((
                target_repository.into(),
                AnalysisDiagnostic {
                    code: "rust.semantic_resolution_unavailable".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path: PathBuf::from("Cargo.toml"),
                    line: None,
                    detail: Some(error.to_string()),
                },
            ));
            return;
        }
    };
    let Some(materialized_target) = materialized.repository(target_repository) else {
        return;
    };
    let cargo_roots = cargo_roots(repository, materialized_target);
    if cargo_roots.is_empty() {
        return;
    }
    let mut enriched = contribution.clone();
    let result = (|| {
        validate_immutable_rust_inputs(snapshot)?;
        for cargo_root in cargo_roots {
            enrich_repository(
                snapshot,
                &materialized,
                repository,
                &cargo_root,
                &mut enriched,
            )?;
        }
        materialized.verify(snapshot)?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    })();
    if let Err(error) = result {
        contribution.diagnostics.push((
            target_repository.into(),
            AnalysisDiagnostic {
                code: "rust.semantic_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: PathBuf::from("Cargo.toml"),
                line: None,
                detail: Some(error.to_string()),
            },
        ));
    } else {
        *contribution = enriched;
    }
}

fn cargo_roots(
    repository: &beholder_indexing::RepositorySnapshot,
    materialized_base: &Path,
) -> Vec<PathBuf> {
    let manifest_dirs = repository
        .inputs
        .iter()
        .filter(|input| {
            input
                .path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        })
        .filter_map(|input| input.path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    manifest_dirs
        .iter()
        .filter(|directory| {
            !directory
                .ancestors()
                .skip(1)
                .any(|ancestor| manifest_dirs.contains(ancestor))
        })
        .map(|directory| materialized_base.join(directory))
        .collect()
}

fn retain_semantic_enrichment(contribution: &mut AnalyzerContribution, target_repository: &str) {
    let target_prefix = format!("repo://{target_repository}/");
    contribution.overrides.retain(|override_| {
        override_.provenance == Provenance::Compiler
            && override_.from.as_str().starts_with(&target_prefix)
    });
    contribution
        .active_repositories
        .retain(|repository| repository == target_repository);
    contribution
        .repositories
        .retain(|repository| repository.repository == target_repository);
    contribution
        .diagnostics
        .retain(|(repository, _)| repository == target_repository);
    for repository in &mut contribution.repositories {
        repository.entities.clear();
        repository.grpc_bindings.clear();
        repository.observations.clear();
        repository.diagnostics.retain(|diagnostic| {
            diagnostic.code == "rust.receiver_method_resolution_unavailable"
                || diagnostic.code == "rust.compiler_target_unrepresented"
        });
    }
}

fn enrich_repository(
    snapshot: &WorkspaceSnapshot,
    materialized: &MaterializedWorkspace,
    repository: &beholder_indexing::RepositorySnapshot,
    cargo_root: &Path,
    contribution: &mut AnalyzerContribution,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut cargo = CargoConfig {
        features: cargo_features(
            std::env::var("BEHOLDER_RUST_FEATURES").ok().as_deref(),
            environment_enabled("BEHOLDER_RUST_ALL_FEATURES"),
            environment_enabled("BEHOLDER_RUST_NO_DEFAULT_FEATURES"),
        ),
        target: std::env::var("CARGO_BUILD_TARGET")
            .ok()
            .filter(|target| !target.trim().is_empty()),
        set_test: true,
        no_deps: snapshot.repositories.len() == 1,
        ..CargoConfig::default()
    };
    let cargo_home = materialized.root.join(".cargo-home");
    fs::create_dir_all(&cargo_home)?;
    cargo.extra_env.insert(
        "CARGO_HOME".into(),
        Some(cargo_home.to_string_lossy().into_owned()),
    );
    let load = LoadCargoConfig {
        load_out_dirs_from_check: false,
        with_proc_macro_server: ProcMacroServerChoice::None,
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 1,
    };
    let (mut database, vfs, _) = load_workspace_at(cargo_root, &cargo, &load, &|_| {})?;
    let mut change = ChangeWithProcMacros::default();
    let mut snapshot_files = Vec::new();
    for snapshot_repository in &snapshot.repositories {
        let Some(materialized_base) =
            materialized.repository(&snapshot_repository.state.repository.identity)
        else {
            continue;
        };
        for input in snapshot_repository.inputs.iter().filter(|input| {
            input
                .path
                .extension()
                .is_some_and(|extension| extension == "rs")
        }) {
            let source = std::str::from_utf8(&input.content)?.to_owned();
            let absolute = materialized_base.join(&input.path);
            let path = VfsPath::new_real_path(absolute.to_string_lossy().into_owned());
            let Some((file_id, _)) = vfs.file_id(&path) else {
                continue;
            };
            change.change_file(file_id, Some(source.clone()));
            snapshot_files.push((snapshot_repository, &input.path, file_id, source));
        }
    }
    database.apply_change(change);
    let analysis = AnalysisHost::with_database(database).analysis();
    let mut files = BTreeMap::<PathBuf, (FileId, String)>::new();
    let mut definitions = BTreeMap::new();
    let mut call_sites = Vec::new();
    for (snapshot_repository, path, file_id, source) in snapshot_files {
        let syntax = match analyze(&source) {
            Ok(syntax) => syntax,
            Err(error) if error.downcast_ref::<UnsafeTreeRecovery>().is_some() => continue,
            Err(error) => return Err(error),
        };
        let source_id = source_entity_id(&snapshot_repository.state.repository.identity, path);
        for function in syntax.functions() {
            let function_id = format!("{source_id}/{}", function.qualified_name());
            definitions.insert(
                (file_id, text_size(function.name_offset())?),
                function_id.clone(),
            );
            if snapshot_repository.state.repository == repository.state.repository {
                call_sites.extend(function.calls().map(|call| CallSite {
                    file_id,
                    from: function_id.clone(),
                    unresolved: if call.receiver_method() {
                        format!("rust-method://{}", call.name())
                    } else {
                        format!("rust-call://{}", call.name())
                    },
                    evidence: format!("{}:{}", path.display(), line(&source, call.offset())),
                    offset: call.offset(),
                }));
            }
        }
        if snapshot_repository.state.repository == repository.state.repository {
            files.insert(path.clone(), (file_id, source));
        }
    }
    let config = GotoDefinitionConfig {
        ra_fixture: RaFixtureConfig {
            disable_ra_fixture: true,
            ..RaFixtureConfig::default()
        },
    };
    let repository_contribution = contribution
        .repositories
        .iter_mut()
        .find(|facts| facts.repository == repository.state.repository.identity)
        .ok_or("Rust contribution omitted an active repository")?;
    let candidates = repository_contribution
        .observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && (observation.to.as_str().starts_with("rust-call://")
                    || observation.to.as_str().starts_with("rust-method://")
                    || observation.provenance == Provenance::UniqueNameHeuristic)
        })
        .map(|observation| {
            (
                observation.from.to_string(),
                observation.evidence.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    call_sites.retain(|call| candidates.contains(&(call.from.clone(), call.evidence.clone())));
    let local_files = files
        .values()
        .map(|(file_id, _)| *file_id)
        .collect::<BTreeSet<_>>();
    let mut compiler_diagnostics = Vec::new();
    for call in call_sites {
        let Some(targets) = analysis.goto_definition(
            FilePosition {
                file_id: call.file_id,
                offset: text_size(call.offset)?,
            },
            &config,
        )?
        else {
            continue;
        };
        let mut local = targets.info.iter().filter_map(|target| {
            definitions
                .get(&(target.file_id, target.focus_or_full_range().start()))
                .or_else(|| {
                    definitions.iter().find_map(|((file_id, offset), entity)| {
                        (*file_id == target.file_id && target.full_range.contains(*offset))
                            .then_some(entity)
                    })
                })
                .cloned()
        });
        let Some(target) = local.next() else {
            if targets
                .info
                .iter()
                .any(|target| local_files.contains(&target.file_id))
            {
                let (path, line) = evidence_location(&call.evidence);
                compiler_diagnostics.push(AnalysisDiagnostic {
                    code: "rust.compiler_target_unrepresented".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path,
                    line,
                    detail: Some(format!(
                        "compiler resolved {} to a local definition absent from syntax facts",
                        call.unresolved
                    )),
                });
            }
            continue;
        };
        if local.next().is_some() {
            continue;
        }
        let Some(observation) =
            repository_contribution
                .observations
                .iter_mut()
                .find(|observation| {
                    observation.from.as_str() == call.from
                        && observation.relation
                            == SemanticRelation::Dependency(DependencyRelation::Calls)
                        && observation.evidence.as_str() == call.evidence
                        && (observation.to.as_str() == call.unresolved
                            || observation.provenance == Provenance::UniqueNameHeuristic)
                })
        else {
            continue;
        };
        if observation.to.as_str() == target {
            contribution.overrides.retain(|override_| {
                override_.from != observation.from
                    || override_.relation != DependencyRelation::Calls
                    || override_.unresolved_to.as_str() != call.unresolved
            });
            contribution.overrides.push(DependencyOverride {
                from: observation.from.clone(),
                relation: DependencyRelation::Calls,
                unresolved_to: call.unresolved.clone().into(),
                resolved_to: target.clone().into(),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Exact,
                provenance: Provenance::Compiler,
            });
            observation.confidence = Confidence::Exact;
            observation.provenance = Provenance::Compiler;
            continue;
        }
        contribution.overrides.retain(|override_| {
            override_.from != observation.from
                || override_.relation != DependencyRelation::Calls
                || override_.evidence != observation.evidence
        });
        contribution.overrides.push(DependencyOverride {
            from: observation.from.clone(),
            relation: DependencyRelation::Calls,
            unresolved_to: call.unresolved.into(),
            resolved_to: target.clone().into(),
            evidence: observation.evidence.clone(),
            confidence: Confidence::Exact,
            provenance: Provenance::Compiler,
        });
        observation.to = target.into();
        observation.confidence = Confidence::Exact;
        observation.provenance = Provenance::Compiler;
    }
    repository_contribution
        .diagnostics
        .retain(|diagnostic| diagnostic.code != "rust.receiver_method_resolution_unavailable");
    let mut unresolved_by_path = BTreeMap::<PathBuf, (usize, Option<u32>)>::new();
    for observation in repository_contribution
        .observations
        .iter()
        .filter(|observation| observation.to.as_str().starts_with("rust-method://"))
    {
        let (path, line) = evidence_location(observation.evidence.as_str());
        unresolved_by_path
            .entry(path)
            .and_modify(|(count, first_line)| {
                *count += 1;
                *first_line = (*first_line).min(line);
            })
            .or_insert((1, line));
    }
    for (path, (count, line)) in unresolved_by_path {
        repository_contribution
            .diagnostics
            .push(AnalysisDiagnostic {
                code: "rust.receiver_method_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path,
                line,
                detail: Some(format!(
                    "{count} receiver method calls remain unresolved after compiler analysis"
                )),
            });
    }
    repository_contribution
        .diagnostics
        .extend(compiler_diagnostics);
    Ok(())
}

fn cargo_features(
    selected: Option<&str>,
    all_features: bool,
    no_default_features: bool,
) -> CargoFeatures {
    if all_features {
        return CargoFeatures::All;
    }
    CargoFeatures::Selected {
        features: selected
            .into_iter()
            .flat_map(|features| features.split(','))
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(str::to_owned)
            .collect(),
        no_default_features,
    }
}

fn environment_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

struct CallSite {
    file_id: FileId,
    from: String,
    unresolved: String,
    evidence: String,
    offset: usize,
}

fn line(source: &str, offset: usize) -> usize {
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn evidence_location(evidence: &str) -> (PathBuf, Option<u32>) {
    if let Some((path, line)) = evidence.rsplit_once(':')
        && let Ok(line) = line.parse()
    {
        return (PathBuf::from(path), Some(line));
    }
    (PathBuf::from(evidence), None)
}

fn text_size(offset: usize) -> Result<TextSize, Box<dyn Error + Send + Sync>> {
    Ok(TextSize::from(u32::try_from(offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{EnrichmentSnapshot, InputKind, RepositoryInput, RepositorySnapshot};
    use beholder_protocol::{
        analyze_requests, contribution_from_events,
        worker_v1::{AnalysisPhase, analyze_event, analyzer_worker_client::AnalyzerWorkerClient},
    };
    use std::{
        fs,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[tokio::test]
    async fn streams_compiler_resolved_snapshot_calls() {
        let base = std::env::temp_dir().join(format!(
            "beholder-rust-worker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(base.join("src")).unwrap();
        fs::create_dir_all(base.join("dependency/src")).unwrap();
        fs::create_dir_all(base.join(".cargo")).unwrap();
        let manifest = "[package]\nname = \"worker-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [dependencies]\ncontext = { path = \"dependency\" }\n";
        let dependency_manifest =
            "[package]\nname = \"context\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        let lockfile = "version = 4\n\n[[package]]\nname = \"context\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"worker-test\"\nversion = \"0.1.0\"\ndependencies = [\n \"context\",\n]\n";
        let dependency_source = "pub fn external() {}\n";
        let cargo_config = "[build]\ntarget-dir = \"target\"\n";
        let source = r#"
mod broken;
mod inner;
mod api { pub use crate::inner::renamed; }
use api::renamed as call_me;
use context::external;
trait Run { fn run(&self); }
struct Thing;
impl Run for Thing { fn run(&self) {} }
impl Thing { fn inherent(&self) {} }
fn generic<T: Run>(value: &T) { value.run(); }
macro_rules! generate { () => { fn generated() {} }; }
generate!();
fn caller() {
    call_me();
    generic(&Thing);
    let thing = Thing;
    thing.inherent();
    let boxed = Box::new(Thing);
    boxed.inherent();
    generated();
    external();
}
"#;
        let inner = "pub fn renamed() {}\n";
        let broken = "fn broken( {\n";
        fs::write(base.join("Cargo.toml"), "not the snapshot manifest").unwrap();
        fs::write(base.join("Cargo.lock"), "not the snapshot lockfile").unwrap();
        fs::write(
            base.join(".cargo/config.toml"),
            "not snapshot configuration",
        )
        .unwrap();
        fs::write(
            base.join("src/lib.rs"),
            "mod inner; pub fn stale_disk_source() {}",
        )
        .unwrap();
        fs::write(base.join("src/inner.rs"), "pub fn stale_inner() {}").unwrap();
        fs::write(base.join("src/broken.rs"), "pub fn stale_broken() {}").unwrap();
        fs::write(base.join("dependency/Cargo.toml"), dependency_manifest).unwrap();
        fs::write(
            base.join("dependency/src/lib.rs"),
            "pub fn stale_external() {}",
        )
        .unwrap();
        let snapshot = EnrichmentSnapshot {
            target_repository: "example/repo".into(),
            workspace: WorkspaceSnapshot {
                name: "test".into(),
                repositories: vec![
                    RepositorySnapshot {
                        base: base.clone(),
                        state: RepositoryState {
                            repository: LogicalRepository {
                                identity: "example/repo".into(),
                            },
                            head: None,
                            fingerprint: "state".into(),
                        },
                        inputs: vec![
                            RepositoryInput {
                                path: "Cargo.toml".into(),
                                content: Arc::from(manifest.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "Cargo.lock".into(),
                                content: Arc::from(lockfile.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: ".cargo/config.toml".into(),
                                content: Arc::from(cargo_config.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/lib.rs".into(),
                                content: Arc::from(source.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/inner.rs".into(),
                                content: Arc::from(inner.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/broken.rs".into(),
                                content: Arc::from(broken.as_bytes()),
                                kind: InputKind::Source,
                            },
                        ],
                    },
                    RepositorySnapshot {
                        base: base.join("dependency"),
                        state: RepositoryState {
                            repository: LogicalRepository {
                                identity: "example/context".into(),
                            },
                            head: None,
                            fingerprint: "context-state".into(),
                        },
                        inputs: vec![
                            RepositoryInput {
                                path: "Cargo.toml".into(),
                                content: Arc::from(dependency_manifest.as_bytes()),
                                kind: InputKind::Source,
                            },
                            RepositoryInput {
                                path: "src/lib.rs".into(),
                                content: Arc::from(dependency_source.as_bytes()),
                                kind: InputKind::Source,
                            },
                        ],
                    },
                ],
            },
        };
        let socket = PathBuf::from(format!(
            "/tmp/beholder-worker-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let server = tokio::spawn({
            let socket = socket.clone();
            let cache = base.join("cache");
            async move { serve(&socket, cache).await }
        });
        let endpoint = format!("unix:{}", socket.display());
        let mut client = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match AnalyzerWorkerClient::connect(endpoint.clone()).await {
                    Ok(client) => break client,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .unwrap();
        let mut stream = client
            .analyze(tokio_stream::iter(analyze_requests(snapshot)))
            .await
            .unwrap()
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.unwrap() {
            events.push(event);
        }
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match &event.event {
                    Some(analyze_event::Event::Progress(progress)) => {
                        AnalysisPhase::try_from(progress.phase).ok()
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [AnalysisPhase::ReceivingSnapshot, AnalysisPhase::Analyzing]
        );
        let contribution = contribution_from_events(events).unwrap();

        let overrides = &contribution.overrides;
        assert!(overrides.iter().any(|override_| {
            override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                && override_.resolved_to.as_str() == "repo://example/repo/rust/inner/renamed"
                && override_.provenance == Provenance::Compiler
                && override_.confidence == Confidence::Exact
        }));
        assert!(overrides.iter().any(|override_| {
            override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                && override_.resolved_to.as_str() == "repo://example/context/rust/lib/external"
                && override_.provenance == Provenance::Compiler
        }));
        assert!(
            overrides.iter().any(|override_| {
                override_.from.as_str() == "repo://example/repo/rust/lib/generic"
                    && override_.resolved_to.as_str() == "repo://example/repo/rust/lib/run"
                    && override_.provenance == Provenance::Compiler
                    && override_.confidence == Confidence::Exact
            }),
            "generic overrides: {:#?}",
            overrides
                .iter()
                .filter(|override_| override_.from.as_str().ends_with("/generic"))
                .collect::<Vec<_>>()
        );
        assert!(
            overrides.iter().any(|override_| {
                override_.from.as_str() == "repo://example/repo/rust/lib/caller"
                    && override_.resolved_to.as_str()
                        == "repo://example/repo/rust/lib/impl/Thing/inherent"
                    && override_.provenance == Provenance::Compiler
                    && override_.confidence == Confidence::Exact
            }),
            "caller overrides: {:#?}\ndiagnostics: {:#?}",
            overrides
                .iter()
                .filter(|override_| override_.from.as_str().ends_with("/caller"))
                .collect::<Vec<_>>(),
            contribution.repositories[0].diagnostics
        );
        assert!(contribution.repositories[0].observations.is_empty());
        assert!(
            contribution.diagnostics.iter().all(|(_, diagnostic)| {
                diagnostic.code != "rust.semantic_resolution_unavailable"
            })
        );
        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic.code == "rust.compiler_target_unrepresented"
                        && diagnostic.path == Path::new("src/lib.rs")
                        && diagnostic.line.is_some()
                })
        );
        assert!(
            contribution.repositories[0]
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic.code == "rust.receiver_method_resolution_unavailable"
                        && diagnostic.path == Path::new("src/lib.rs")
                        && diagnostic.line.is_some()
                })
        );
        server.abort();
        let _ = server.await;
        fs::remove_file(socket).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn selects_topmost_cargo_roots() {
        let repository = RepositorySnapshot {
            base: PathBuf::from("repo"),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repo".into(),
                },
                head: None,
                fingerprint: "state".into(),
            },
            inputs: [
                "services/one/Cargo.toml",
                "services/one/crates/member/Cargo.toml",
                "tools/two/Cargo.toml",
            ]
            .into_iter()
            .map(|path| RepositoryInput {
                path: path.into(),
                content: Arc::from(&b""[..]),
                kind: InputKind::Source,
            })
            .collect(),
        };

        assert_eq!(
            cargo_roots(&repository, Path::new("repo")),
            [
                PathBuf::from("repo/services/one"),
                PathBuf::from("repo/tools/two")
            ]
        );
    }

    #[test]
    fn selects_explicit_cargo_features() {
        assert_eq!(cargo_features(None, true, false), CargoFeatures::All);
        assert_eq!(
            cargo_features(Some("api, serde ,"), false, true),
            CargoFeatures::Selected {
                features: vec!["api".into(), "serde".into()],
                no_default_features: true,
            }
        );
    }
}
