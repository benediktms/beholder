#![deny(missing_docs)]
//! Public Rust SDK for Beholder runtime analyzer plugins.
//!
//! A plugin is a trusted executable which serves one gRPC request on the Unix
//! socket supplied by Beholder. Implement [`Analyzer`], declare the immutable
//! inputs and semantic kinds the plugin uses, then call [`serve`]. The callback
//! is synchronous and runs on a blocking thread inside the plugin process; the
//! SDK keeps gRPC and OpenTelemetry work on its asynchronous runtime.
//!
//! ```no_run
//! use beholder_plugin_sdk::{
//!     AnalysisContext, AnalysisError, AnalysisInputKind, Analyzer, Descriptor,
//!     EntityKind, Output, PluginInputScope, PluginInputSelector, PluginPathMatcher,
//!     PLUGIN_API_VERSION,
//! };
//! use std::collections::BTreeSet;
//!
//! struct Example;
//!
//! impl Analyzer for Example {
//!     fn descriptor(&self) -> Descriptor {
//!         Descriptor {
//!             id: "example".into(),
//!             api_version: PLUGIN_API_VERSION,
//!             inputs: vec![PluginInputSelector {
//!                 scope: PluginInputScope::Target,
//!                 matcher: PluginPathMatcher::Extension("rs".into()),
//!                 kind: AnalysisInputKind::Source,
//!             }],
//!             semantic_entities: BTreeSet::from([EntityKind::Callable]),
//!             semantic_relations: BTreeSet::new(),
//!             produces_entities: BTreeSet::new(),
//!             produces_relations: BTreeSet::new(),
//!         }
//!     }
//!
//!     fn analyze(
//!         &self,
//!         context: &AnalysisContext,
//!         _output: &mut Output,
//!     ) -> Result<(), AnalysisError> {
//!         for input in context.inputs() {
//!             let _source = input.text()?;
//!         }
//!         Ok(())
//!     }
//! }
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! beholder_plugin_sdk::serve(Example).await?;
//! # Ok(())
//! # }
//! ```

use beholder_domain::{
    AnalysisDiagnostic, EntityFact, EntityId, Evidence as DomainEvidence, Observation,
};
use beholder_indexing::{
    AnalysisCompleteness, AnalyzerContribution, AnalyzerMetadata, CacheStatistics,
    EnrichmentSnapshot, RepositoryContribution,
};
use beholder_protocol::{
    WorkspaceSnapshotBuilder, analyze_events, descriptor_to_wire,
    worker_v1::{
        AnalyzeEvent, AnalyzeRequest, DescribeRequest, DescribeResponse,
        analyzer_plugin_server::{AnalyzerPlugin, AnalyzerPluginServer},
        analyzer_worker_server::{AnalyzerWorker, AnalyzerWorkerServer},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::{Request, Response, Status, Streaming};

/// Canonical semantic kinds accepted by Beholder.
pub use beholder_domain::{
    AnalysisDiagnosticSeverity, Confidence, DependencyRelation, EntityKind, EntityMetadata,
    Provenance, SemanticRelation, StructuralRelation,
};
/// The current plugin API version and descriptor types.
pub use beholder_indexing::{
    AnalysisInputKind, PLUGIN_API_VERSION, PluginDescriptor as Descriptor, PluginInputScope,
    PluginInputSelector, PluginPathMatcher,
};

/// Error returned by plugin recognition code.
pub type AnalysisError = Box<dyn Error + Send + Sync>;

/// Error returned while starting or serving a plugin process.
pub type ServeError = Box<dyn Error + Send + Sync>;

/// Synchronous recognition callback implemented by a runtime plugin.
pub trait Analyzer: Send + Sync + 'static {
    /// Describes the plugin's inputs and permitted semantic outputs.
    fn descriptor(&self) -> Descriptor;

    /// Recognizes semantic facts from one immutable target snapshot.
    fn analyze(&self, context: &AnalysisContext, output: &mut Output) -> Result<(), AnalysisError>;
}

/// One immutable file selected by the plugin descriptor.
#[derive(Clone, Debug)]
pub struct Input {
    repository: String,
    target: bool,
    path: PathBuf,
    content: Arc<[u8]>,
}

impl Input {
    /// Logical repository which owns this input.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Whether the input belongs to the contribution target.
    pub fn is_target(&self) -> bool {
        self.target
    }

    /// Repository-relative path supplied by Beholder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Exact immutable bytes supplied by Beholder.
    pub fn bytes(&self) -> &[u8] {
        &self.content
    }

    /// Reads this input as UTF-8 text.
    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.content)
    }
}

/// Opaque checked reference to a canonical semantic entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityRef {
    id: EntityId,
    kind: EntityKind,
}

impl EntityRef {
    /// Canonical content-addressed entity ID.
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    /// Canonical entity kind associated with the ID.
    pub fn kind(&self) -> EntityKind {
        self.kind
    }
}

/// One immutable observation supplied from the target's baseline graph.
#[derive(Clone, Debug)]
pub struct BaselineObservation {
    observation: Observation,
}

impl BaselineObservation {
    /// Canonical source entity ID.
    pub fn from(&self) -> &str {
        self.observation.from.as_str()
    }

    /// Canonical semantic relation.
    pub fn relation(&self) -> SemanticRelation {
        self.observation.relation
    }

    /// Canonical destination entity ID.
    pub fn to(&self) -> &str {
        self.observation.to.as_str()
    }

    /// Source evidence carried by the baseline observation.
    pub fn evidence(&self) -> &str {
        self.observation.evidence.as_str()
    }
}

/// Immutable inputs and baseline facts for one analyzer callback.
pub struct AnalysisContext {
    workspace: String,
    target_repository: String,
    inputs: Vec<Input>,
    entities: BTreeMap<String, EntityRef>,
    observations: Vec<BaselineObservation>,
    cancelled: Arc<AtomicBool>,
}

impl AnalysisContext {
    /// Semantic workspace being enriched.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Logical repository which will own the plugin contribution.
    pub fn target_repository(&self) -> &str {
        &self.target_repository
    }

    /// Selected target and context files.
    pub fn inputs(&self) -> impl Iterator<Item = &Input> {
        self.inputs.iter()
    }

    /// Selected canonical baseline entities.
    pub fn entities(&self) -> impl Iterator<Item = &EntityRef> {
        self.entities.values()
    }

    /// Looks up one selected baseline entity by canonical ID.
    pub fn entity(&self, id: &str) -> Option<&EntityRef> {
        self.entities.get(id)
    }

    /// Selected canonical baseline observations.
    pub fn observations(&self) -> impl Iterator<Item = &BaselineObservation> {
        self.observations.iter()
    }

    /// Whether Beholder has abandoned the current request.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Structured source evidence for a plugin-produced relationship.
#[derive(Clone, Debug)]
pub struct Evidence {
    path: PathBuf,
    line: Option<u32>,
    detail: Option<String>,
}

impl Evidence {
    /// Creates evidence tied to an input Beholder supplied to the plugin.
    pub fn new(input: &Input, line: Option<u32>, detail: Option<String>) -> Self {
        Self {
            path: input.path.clone(),
            line,
            detail,
        }
    }

    fn render(&self) -> DomainEvidence {
        let mut rendered = self.path.to_string_lossy().into_owned();
        if let Some(line) = self.line {
            rendered.push(':');
            rendered.push_str(&line.to_string());
        }
        if let Some(detail) = &self.detail {
            rendered.push_str(" · ");
            rendered.push_str(detail);
        }
        rendered.into()
    }
}

/// Validated contribution builder for one target repository.
pub struct Output {
    descriptor: Descriptor,
    target_repository: String,
    known: BTreeMap<String, EntityRef>,
    entities: Vec<EntityFact>,
    observations: Vec<Observation>,
    diagnostics: Vec<AnalysisDiagnostic>,
    replaced_diagnostic_codes: BTreeSet<String>,
    incomplete: bool,
}

impl Output {
    /// Defines a canonical entity owned by this plugin contribution.
    pub fn define(
        &mut self,
        id: impl Into<String>,
        kind: EntityKind,
        metadata: Option<EntityMetadata>,
    ) -> Result<EntityRef, AnalysisError> {
        let id = id.into();
        if !self.descriptor.produces_entities.contains(&kind) {
            return Err(format!("plugin did not declare output entity kind {kind:?}").into());
        }
        validate_address(kind, &id)?;
        let entity = EntityFact::new(id.clone(), kind, metadata)?;
        let reference = EntityRef {
            id: entity.id.clone(),
            kind,
        };
        if self.known.insert(id, reference.clone()).is_some() {
            return Err("plugin defined the same entity more than once".into());
        }
        self.entities.push(entity);
        Ok(reference)
    }

    /// Adds a canonical relationship between checked entity references.
    pub fn relate(
        &mut self,
        from: &EntityRef,
        relation: SemanticRelation,
        to: &EntityRef,
        evidence: Evidence,
        confidence: Confidence,
        provenance: Provenance,
    ) -> Result<(), AnalysisError> {
        if !self.descriptor.produces_relations.contains(&relation) {
            return Err(format!("plugin did not declare output relation {relation:?}").into());
        }
        if !self.known.contains_key(from.id()) || !self.known.contains_key(to.id()) {
            return Err("plugin relationship references an unknown entity".into());
        }
        validate_relation(from.kind, relation, to.kind)?;
        self.observations.push(Observation {
            from: from.id.clone(),
            relation,
            to: to.id.clone(),
            evidence: evidence.render(),
            confidence,
            provenance,
        });
        Ok(())
    }

    /// Emits a non-fatal diagnostic tied to a supplied input.
    pub fn diagnostic(
        &mut self,
        code: impl Into<String>,
        severity: AnalysisDiagnosticSeverity,
        input: &Input,
        line: Option<u32>,
        detail: Option<String>,
    ) {
        self.diagnostics.push(AnalysisDiagnostic {
            code: code.into(),
            severity,
            path: input.path.clone(),
            line,
            detail,
        });
    }

    /// Replaces one baseline diagnostic code while this contribution is selected.
    pub fn replace_diagnostic_code(&mut self, code: impl Into<String>) {
        self.replaced_diagnostic_codes.insert(code.into());
    }

    /// Marks the target contribution incomplete while preserving valid facts.
    pub fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    fn finish(self, digest: String) -> AnalyzerContribution {
        AnalyzerContribution {
            metadata: AnalyzerMetadata {
                id: self.descriptor.id,
                version: digest,
            },
            active_repositories: vec![self.target_repository.clone()],
            repositories: vec![RepositoryContribution {
                repository: self.target_repository,
                completeness: if self.incomplete {
                    AnalysisCompleteness::Incomplete
                } else {
                    AnalysisCompleteness::Complete
                },
                entities: self.entities,
                grpc_bindings: Vec::new(),
                observations: self.observations,
                semantic_candidates: Vec::new(),
                diagnostics: self.diagnostics,
                replaced_diagnostic_codes: self.replaced_diagnostic_codes,
                fact_shards: Vec::new(),
            }],
            overrides: Vec::new(),
            candidate_overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        }
    }
}

struct PluginService<A> {
    analyzer: Arc<A>,
    descriptor: Descriptor,
    shutdown: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

struct CancellationWatcher(tokio::task::JoinHandle<()>);

impl Drop for CancellationWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<A> Clone for PluginService<A> {
    fn clone(&self) -> Self {
        Self {
            analyzer: Arc::clone(&self.analyzer),
            descriptor: self.descriptor.clone(),
            shutdown: Arc::clone(&self.shutdown),
        }
    }
}

impl<A> PluginService<A> {
    fn stop(&self) {
        if let Ok(mut shutdown) = self.shutdown.lock()
            && let Some(shutdown) = shutdown.take()
        {
            let _ = shutdown.send(());
        }
    }
}

#[tonic::async_trait]
impl<A: Analyzer> AnalyzerPlugin for PluginService<A> {
    async fn describe(
        &self,
        _request: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        let response = DescribeResponse {
            descriptor: Some(descriptor_to_wire(self.descriptor.clone())),
        };
        self.stop();
        Ok(Response::new(response))
    }
}

type AnalyzeStream =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<AnalyzeEvent, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl<A: Analyzer> AnalyzerWorker for PluginService<A> {
    type AnalyzeStream = AnalyzeStream;

    async fn analyze(
        &self,
        request: Request<Streaming<AnalyzeRequest>>,
    ) -> Result<Response<Self::AnalyzeStream>, Status> {
        let span = tracing::info_span!(
            "plugin.analyze",
            plugin = self.descriptor.id,
            workspace = tracing::field::Empty,
            target_repository = tracing::field::Empty,
        );
        beholder_observability::set_parent_from_metadata(&span, request.metadata());
        let mut stream = request.into_inner();
        let analyzer = Arc::clone(&self.analyzer);
        let descriptor = self.descriptor.clone();
        let shutdown = self.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&cancelled);
        let closed = sender.clone();
        let cancellation_watcher = CancellationWatcher(tokio::spawn(async move {
            closed.closed().await;
            cancellation.store(true, Ordering::Release);
        }));
        tokio::spawn(
            async move {
                let _cancellation_watcher = cancellation_watcher;
                let mut snapshot = WorkspaceSnapshotBuilder::default();
                loop {
                    match stream.message().await {
                        Ok(Some(request)) => {
                            if let Err(error) = snapshot.push(request) {
                                let _ = sender.send(Err(Status::invalid_argument(error))).await;
                                shutdown.stop();
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            shutdown.stop();
                            return;
                        }
                    }
                }
                let snapshot = match snapshot.finish() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let _ = sender.send(Err(Status::invalid_argument(error))).await;
                        shutdown.stop();
                        return;
                    }
                };
                let context = context(snapshot, Arc::clone(&cancelled));
                tracing::Span::current().record("workspace", context.workspace());
                tracing::Span::current().record("target_repository", context.target_repository());
                let mut output = Output {
                    descriptor: descriptor.clone(),
                    target_repository: context.target_repository.clone(),
                    known: context.entities.clone(),
                    entities: Vec::new(),
                    observations: Vec::new(),
                    diagnostics: Vec::new(),
                    replaced_diagnostic_codes: BTreeSet::new(),
                    incomplete: false,
                };
                let digest = std::env::var("BEHOLDER_PLUGIN_DIGEST")
                    .unwrap_or_else(|_| "development".into());
                let analysis_span = tracing::Span::current();
                let result = tokio::task::spawn_blocking(move || {
                    analysis_span.in_scope(|| {
                        analyzer.analyze(&context, &mut output).and_then(|()| {
                            analyze_events(output.finish(digest)).map_err(Into::into)
                        })
                    })
                })
                .await;
                let events = match result {
                    Ok(Ok(events)) => events,
                    Ok(Err(error)) => {
                        let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                        shutdown.stop();
                        return;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                        shutdown.stop();
                        return;
                    }
                };
                for event in events {
                    if sender.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                shutdown.stop();
            }
            .instrument(span),
        );
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

/// Serves exactly one descriptor or analysis request on Beholder's private socket.
pub async fn serve(analyzer: impl Analyzer) -> Result<(), ServeError> {
    let descriptor = analyzer.descriptor();
    descriptor.validate()?;
    let socket = socket_argument()?;
    let _observability = beholder_observability::init(
        &format!("beholder-worker-{}", descriptor.id),
        beholder_observability::LogOutput::Stderr,
    );
    let listener = UnixListener::bind(&socket)?;
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let service = PluginService {
        analyzer: Arc::new(analyzer),
        descriptor,
        shutdown: Arc::new(Mutex::new(Some(shutdown))),
    };
    tonic::transport::Server::builder()
        .add_service(AnalyzerPluginServer::new(service.clone()))
        .add_service(AnalyzerWorkerServer::new(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
            let _ = stopped.await;
        })
        .await?;
    Ok(())
}

fn socket_argument() -> Result<PathBuf, ServeError> {
    let mut arguments = std::env::args_os().skip(1);
    let mut socket = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--socket") => socket = arguments.next().map(PathBuf::from),
            Some("--cache-dir") => {
                let _ = arguments.next().ok_or("missing --cache-dir value")?;
            }
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy()).into()),
        }
    }
    socket.ok_or_else(|| "missing --socket".into())
}

fn context(snapshot: EnrichmentSnapshot, cancelled: Arc<AtomicBool>) -> AnalysisContext {
    let target_repository = snapshot.target_repository;
    let workspace = snapshot.workspace.name;
    let inputs = snapshot
        .workspace
        .repositories
        .into_iter()
        .flat_map(|repository| {
            let identity = repository.state.repository.identity;
            let target = identity == target_repository;
            repository.inputs.into_iter().map(move |input| Input {
                repository: identity.clone(),
                target,
                path: input.path,
                content: input.content,
            })
        })
        .collect();
    let entities = snapshot
        .baseline
        .entities
        .into_iter()
        .map(|entity| {
            let id = entity.id.to_string();
            (
                id,
                EntityRef {
                    id: entity.id,
                    kind: entity.kind,
                },
            )
        })
        .collect();
    let observations = snapshot
        .baseline
        .observations
        .into_iter()
        .map(|observation| BaselineObservation { observation })
        .collect();
    AnalysisContext {
        workspace,
        target_repository,
        inputs,
        entities,
        observations,
        cancelled,
    }
}

fn validate_address(kind: EntityKind, id: &str) -> Result<(), AnalysisError> {
    let valid = match kind {
        EntityKind::Callable | EntityKind::Namespace => id.starts_with("repo://"),
        EntityKind::GraphqlArgument => id.starts_with("graphql-argument://"),
        EntityKind::GraphqlEnumValue => id.starts_with("graphql-enum-value://"),
        EntityKind::GraphqlField => id.starts_with("graphql-field://"),
        EntityKind::GraphqlOperation => id.starts_with("graphql-operation://"),
        EntityKind::GraphqlType => id.starts_with("graphql-type://"),
        EntityKind::GrpcOperation => id.starts_with("grpc://"),
        EntityKind::KafkaTopic => id.starts_with("kafka-topic://"),
        EntityKind::ProtoField => id.starts_with("proto-field://"),
        EntityKind::ProtoMethod => id.starts_with("proto-method://"),
        EntityKind::ProtoService => id.starts_with("proto-service://"),
        EntityKind::ProtoType => id.starts_with("proto-type://"),
        EntityKind::Service => id.starts_with("service://"),
        EntityKind::UnityPrefab => id.starts_with("repo://") || id.starts_with("unity://"),
    };
    if valid
        && id
            .split_once("://")
            .is_some_and(|(_, value)| !value.is_empty())
    {
        Ok(())
    } else {
        Err(format!("entity address {id:?} does not match kind {kind:?}").into())
    }
}

fn validate_relation(
    from: EntityKind,
    relation: SemanticRelation,
    to: EntityKind,
) -> Result<(), AnalysisError> {
    let valid = match relation {
        SemanticRelation::Dependency(DependencyRelation::Publishes) => {
            from == EntityKind::Callable && to == EntityKind::KafkaTopic
        }
        SemanticRelation::Dependency(DependencyRelation::ConsumedBy) => {
            from == EntityKind::KafkaTopic && to == EntityKind::Callable
        }
        SemanticRelation::Dependency(DependencyRelation::CallsRpc) => {
            from == EntityKind::Callable && to == EntityKind::GrpcOperation
        }
        SemanticRelation::Dependency(DependencyRelation::ImplementedBy) => {
            from == EntityKind::GrpcOperation && to == EntityKind::Callable
        }
        SemanticRelation::Dependency(DependencyRelation::Implements) => {
            from == EntityKind::Callable && to == EntityKind::GrpcOperation
        }
        SemanticRelation::Dependency(DependencyRelation::BindsContract) => {
            matches!(
                from,
                EntityKind::Callable | EntityKind::GrpcOperation | EntityKind::KafkaTopic
            ) && matches!(to, EntityKind::ProtoMethod | EntityKind::ProtoType)
        }
        _ => true,
    };
    valid
        .then_some(())
        .ok_or_else(|| format!("invalid {relation:?} endpoints {from:?} -> {to:?}").into())
}

impl fmt::Debug for AnalysisContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnalysisContext")
            .field("workspace", &self.workspace)
            .field("target_repository", &self.target_repository)
            .field("inputs", &self.inputs.len())
            .field("entities", &self.entities.len())
            .field("observations", &self.observations.len())
            .finish()
    }
}

use tracing::Instrument as _;
