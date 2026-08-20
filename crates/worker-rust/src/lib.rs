use beholder_adapters_treesitter_rust::{RustAnalyzer, analyze, source_entity_id};
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
use ra_ap_project_model::CargoConfig;
use ra_ap_vfs::VfsPath;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::{Request, Response, Status, Streaming};

const ANALYZER_VERSION: &str = "7:6:rust.tonic:1:rust-analyzer-0.0.348:worker-5";
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

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
        let mut stream = request.into_inner();
        let analyzer = self.analyzer.clone();
        let analysis_pool = self.analysis_pool.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
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
            if send_progress(&sender, AnalysisPhase::Analyzing).await {
                return;
            }
            let result = tokio::task::spawn_blocking(
                move || -> Result<Vec<AnalyzeEvent>, Box<dyn Error + Send + Sync>> {
                    analysis_pool.install(|| {
                        let mut contribution = analyzer.analyze(&snapshot)?;
                        enrich_semantics(&snapshot, &mut contribution);
                        retain_semantic_enrichment(&mut contribution);
                        contribution.metadata.version = ANALYZER_VERSION.into();
                        analyze_events(contribution).map_err(Into::into)
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
        });
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

fn enrich_semantics(snapshot: &WorkspaceSnapshot, contribution: &mut AnalyzerContribution) {
    for repository in &snapshot.repositories {
        let cargo_roots = cargo_roots(repository);
        if cargo_roots.is_empty() {
            continue;
        }
        let mut enriched = contribution.clone();
        let result = (|| {
            manifests_match_snapshot(repository)?;
            for cargo_root in cargo_roots {
                enrich_repository(repository, &cargo_root, &mut enriched)?;
            }
            manifests_match_snapshot(repository)
        })();
        if let Err(error) = result {
            contribution.diagnostics.push((
                repository.state.repository.identity.clone(),
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
}

fn cargo_roots(repository: &beholder_indexing::RepositorySnapshot) -> Vec<PathBuf> {
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
        .map(|directory| repository.base.join(directory))
        .collect()
}

fn manifests_match_snapshot(
    repository: &beholder_indexing::RepositorySnapshot,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    for input in repository.inputs.iter().filter(|input| {
        input
            .path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml")
    }) {
        if fs::read(repository.base.join(&input.path))?.as_slice() != input.content.as_ref() {
            return Err(
                format!("{} changed during compiler analysis", input.path.display()).into(),
            );
        }
    }
    Ok(())
}

fn retain_semantic_enrichment(contribution: &mut AnalyzerContribution) {
    contribution
        .overrides
        .retain(|override_| override_.provenance == Provenance::Compiler);
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
    repository: &beholder_indexing::RepositorySnapshot,
    cargo_root: &Path,
    contribution: &mut AnalyzerContribution,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let cargo = CargoConfig {
        set_test: true,
        no_deps: true,
        ..CargoConfig::default()
    };
    let load = LoadCargoConfig {
        load_out_dirs_from_check: false,
        with_proc_macro_server: ProcMacroServerChoice::None,
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 1,
    };
    let (mut database, vfs, _) = load_workspace_at(cargo_root, &cargo, &load, &|_| {})?;
    let mut change = ChangeWithProcMacros::default();
    let mut files = BTreeMap::<PathBuf, (FileId, String)>::new();
    for input in repository.inputs.iter().filter(|input| {
        input
            .path
            .extension()
            .is_some_and(|extension| extension == "rs")
    }) {
        let source = std::str::from_utf8(&input.content)?.to_owned();
        let absolute = repository.base.join(&input.path);
        let path = VfsPath::new_real_path(absolute.to_string_lossy().into_owned());
        let Some((file_id, _)) = vfs.file_id(&path) else {
            continue;
        };
        change.change_file(file_id, Some(source.clone()));
        files.insert(input.path.clone(), (file_id, source));
    }
    database.apply_change(change);
    let analysis = AnalysisHost::with_database(database).analysis();
    let mut definitions = BTreeMap::new();
    let mut call_sites = Vec::new();
    for (path, (file_id, source)) in &files {
        let syntax = match analyze(source) {
            Ok(syntax) => syntax,
            Err(error) if error.downcast_ref::<UnsafeTreeRecovery>().is_some() => continue,
            Err(error) => return Err(error),
        };
        let source_id = source_entity_id(&repository.state.repository.identity, path);
        for function in syntax.functions() {
            let function_id = format!("{source_id}/{}", function.qualified_name());
            definitions.insert(
                (*file_id, text_size(function.name_offset())?),
                function_id.clone(),
            );
            call_sites.extend(function.calls().map(|call| CallSite {
                file_id: *file_id,
                from: function_id.clone(),
                unresolved: if call.receiver_method() {
                    format!("rust-method://{}", call.name())
                } else {
                    format!("rust-call://{}", call.name())
                },
                evidence: format!("{}:{}", path.display(), line(source, call.offset())),
                offset: call.offset(),
            }));
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
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};
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
        let manifest =
            "[package]\nname = \"worker-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        let source = r#"
mod broken;
mod inner;
mod api { pub use crate::inner::renamed; }
use api::renamed as call_me;
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
}
"#;
        let inner = "pub fn renamed() {}\n";
        let broken = "fn broken( {\n";
        fs::write(base.join("Cargo.toml"), manifest).unwrap();
        fs::write(
            base.join("src/lib.rs"),
            "mod inner; pub fn stale_disk_source() {}",
        )
        .unwrap();
        fs::write(base.join("src/inner.rs"), "pub fn stale_inner() {}").unwrap();
        fs::write(base.join("src/broken.rs"), "pub fn stale_broken() {}").unwrap();
        let snapshot = WorkspaceSnapshot {
            name: "test".into(),
            repositories: vec![RepositorySnapshot {
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
            }],
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
            cargo_roots(&repository),
            [
                PathBuf::from("repo/services/one"),
                PathBuf::from("repo/tools/two")
            ]
        );
    }
}
