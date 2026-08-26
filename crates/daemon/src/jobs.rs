use apalis::prelude::FromRequest;
use apalis::{
    layers::tracing::TracingContext,
    prelude::{
        Acknowledge, AcknowledgementExt, Attempt, Backend, BoxDynError, Data, IntervalStrategy,
        MetadataExt, StrategyBuilder, Task, TaskBuilder, TaskId, TaskSink, TaskStream,
        WorkerBuilder, WorkerContext,
    },
};
use apalis_codec::json::JsonCodec;
use apalis_sqlite::{
    CompactType, Config, SqliteContext, SqliteStorage, TaskBuilderExt, fetcher::SqliteFetcher,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{
    Connection, Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    collections::BTreeSet,
    convert::Infallible,
    error::Error,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tracing::Instrument;
use ulid::Ulid;

use crate::{indexing::IndexScheduler, workspace_registry::WorkspaceRegistry};
use beholder_adapters_mnestic::SemanticStore;
use beholder_domain::Workspace;

const INDEX_QUEUE: &str = "index";
const ENRICHMENT_QUEUE: &str = "enrichment";
pub const MAX_ATTEMPTS: u32 = 5;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobTrigger {
    Automatic,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexTarget {
    Workspace {
        workspace: String,
    },
    Repository {
        repository: String,
        workspace_scope: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryChange {
    SourcePath(String),
    ConfigurationPath(String),
    Head,
    Reconciliation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryIntent {
    pub repository: String,
    pub changes: Vec<RepositoryChange>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexJob {
    pub target: IndexTarget,
    pub trigger: JobTrigger,
    pub prerequisite_index_jobs: Vec<IndexJobId>,
    pub generation: Option<u64>,
    pub repository_intents: Vec<RepositoryIntent>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexJobId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexOutcome {
    Published,
    Unchanged,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexDestination {
    Workspace { workspace: String },
    StandaloneRepository { repository: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexDestinationResult {
    pub destination: IndexDestination,
    pub observation_count: usize,
    pub published: bool,
    pub outcome: IndexOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexJobResult {
    pub destinations: Vec<IndexDestinationResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentTarget {
    WorkspaceRepository {
        workspace: String,
        repository: String,
    },
    StandaloneRepository {
        repository: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnrichmentJob {
    pub target: EnrichmentTarget,
    pub worker_id: String,
    pub expected_worker_version: String,
    pub trigger: JobTrigger,
    pub prerequisite_index_jobs: Vec<IndexJobId>,
    pub input_fingerprint: Option<String>,
}

trait JobTelemetry {
    const KIND: &'static str;

    fn summary(&self) -> String;
}

impl JobTelemetry for IndexJob {
    const KIND: &'static str = "index";

    fn summary(&self) -> String {
        match &self.target {
            IndexTarget::Workspace { workspace } => format!("index workspace {workspace}"),
            IndexTarget::Repository {
                repository,
                workspace_scope: Some(workspace),
            } => format!("index repository {repository} in workspace {workspace}"),
            IndexTarget::Repository {
                repository,
                workspace_scope: None,
            } => format!("index repository {repository}"),
        }
    }
}

impl JobTelemetry for EnrichmentJob {
    const KIND: &'static str = "enrichment";

    fn summary(&self) -> String {
        match &self.target {
            EnrichmentTarget::WorkspaceRepository {
                workspace,
                repository,
            } => format!(
                "enrich repository {repository} in workspace {workspace} with worker {}",
                self.worker_id
            ),
            EnrichmentTarget::StandaloneRepository { repository } => format!(
                "enrich repository {repository} with worker {}",
                self.worker_id
            ),
        }
    }
}

pub type IndexJobStorage = SqliteStorage<IndexJob, JsonCodec<CompactType>, SqliteFetcher>;
pub type EnrichmentJobStorage = SqliteStorage<EnrichmentJob, JsonCodec<CompactType>, SqliteFetcher>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct EligibleAt {
    millis: i64,
}

#[derive(Clone, Debug)]
struct EligibleIndexStorage(IndexJobStorage);

struct IndexAttempt {
    attempt: Attempt,
    context: SqliteContext,
}

#[derive(Clone)]
struct AutomaticIndexAcknowledgement {
    queue: JobQueue,
    scheduler: Arc<IndexScheduler>,
}

impl Acknowledge<IndexJobResult, SqliteContext, Ulid> for AutomaticIndexAcknowledgement {
    type Error = sqlx::Error;
    type Future = futures_util::future::BoxFuture<'static, Result<(), Self::Error>>;

    fn ack(
        &mut self,
        _result: &Result<IndexJobResult, BoxDynError>,
        parts: &apalis::prelude::Parts<SqliteContext, Ulid>,
    ) -> Self::Future {
        let id = parts.task_id.map(|id| id.to_string());
        let queue = self.queue.clone();
        let scheduler = Arc::clone(&self.scheduler);
        Box::pin(async move {
            if let Some(id) = id
                && let Some((workspace, generation)) = queue.terminal_automatic_job(&id).await?
            {
                scheduler.automatic_job_finished(&workspace, generation);
            }
            Ok(())
        })
    }
}

impl FromRequest<Task<IndexJob, SqliteContext, Ulid>> for IndexAttempt {
    type Error = Infallible;

    async fn from_request(task: &Task<IndexJob, SqliteContext, Ulid>) -> Result<Self, Self::Error> {
        Ok(Self {
            attempt: task.parts.attempt.clone(),
            context: task.parts.ctx.clone(),
        })
    }
}

impl Backend for EligibleIndexStorage {
    type Args = <IndexJobStorage as Backend>::Args;
    type IdType = <IndexJobStorage as Backend>::IdType;
    type Context = <IndexJobStorage as Backend>::Context;
    type Error = <IndexJobStorage as Backend>::Error;
    type Stream = TaskStream<Task<IndexJob, SqliteContext, Ulid>, sqlx::Error>;
    type Beat = <IndexJobStorage as Backend>::Beat;
    type Layer = <IndexJobStorage as Backend>::Layer;

    fn heartbeat(&self, worker: &WorkerContext) -> Self::Beat {
        self.0.heartbeat(worker)
    }

    fn middleware(&self) -> Self::Layer {
        self.0.middleware()
    }

    fn poll(self, worker: &WorkerContext) -> Self::Stream {
        let buffer_size = self.0.config().buffer_size();
        self.0
            .poll(worker)
            .map(|item| async move {
                if let Ok(Some(task)) = &item {
                    let eligible: Result<EligibleAt, _> = task.parts.ctx.extract();
                    if let Ok(eligible) = eligible {
                        wait_until(eligible.millis).await;
                    }
                }
                item
            })
            .buffer_unordered(buffer_size)
            .boxed()
    }
}

#[derive(Clone, Debug)]
pub struct JobQueue {
    pub index_jobs: IndexJobStorage,
    pub enrichment_jobs: EnrichmentJobStorage,
    admission: Arc<Mutex<bool>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredJobKind {
    Index,
    Enrichment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredJobStatus {
    Queued,
    Waiting,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredJobTarget {
    Workspace(String),
    Repository {
        repository: String,
        workspace_scope: Option<String>,
        worker_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct StoredJob {
    pub id: String,
    pub kind: StoredJobKind,
    pub status: StoredJobStatus,
    pub trigger: JobTrigger,
    pub target: StoredJobTarget,
    pub attempts: u32,
    pub max_attempts: u32,
    pub submitted_at_ms: i64,
    pub eligible_at_ms: Option<i64>,
    pub lock_at: Option<i64>,
    pub done_at: Option<i64>,
    pub last_error: Option<String>,
    pub prerequisites: Vec<String>,
    pub result: Option<IndexJobResult>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Recovery {
    pub released_reservations: u64,
    pub interrupted_running: u64,
    pub immediate_retries: u64,
    pub terminal_failures: u64,
}

pub struct IndexWorker {
    pub context: WorkerContext,
    pub task: tokio::task::JoinHandle<Result<(), String>>,
}

impl JobQueue {
    pub async fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let is_fresh = !path.exists();
        if !is_fresh {
            quick_check(path).await?;
        }

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await?;
        SqliteStorage::setup(&pool).await?;
        if is_fresh {
            eprintln!(
                "created fresh job queue at {}; job history begins empty",
                path.display()
            );
        }

        Ok(Self {
            index_jobs: SqliteStorage::new_with_config(
                &pool,
                &Config::new(INDEX_QUEUE).with_poll_interval(
                    StrategyBuilder::new()
                        .apply(IntervalStrategy::new(Duration::from_millis(100)))
                        .build(),
                ),
            ),
            enrichment_jobs: SqliteStorage::new_with_config(&pool, &Config::new(ENRICHMENT_QUEUE)),
            admission: Arc::new(Mutex::new(true)),
        })
    }

    pub async fn close_admission(&self) {
        *self.admission.lock().await = false;
    }

    pub async fn enqueue_automatic_index(
        &self,
        job: IndexJob,
    ) -> Result<Option<IndexJobId>, Box<dyn Error + Send + Sync>> {
        let admitted = self.admission.lock().await;
        if !*admitted {
            return Err("job admission is closed".into());
        }
        let IndexTarget::Workspace { workspace } = &job.target else {
            return Err("automatic index jobs must target one workspace".into());
        };
        if self.automatic_index_active(workspace).await? {
            return Ok(None);
        }

        Ok(Some(self.push_index_job(job).await?))
    }

    pub async fn enqueue_manual_index(
        &self,
        job: IndexJob,
        workspaces: &[Workspace],
    ) -> Result<(IndexJobId, Vec<StoredJob>), Box<dyn Error + Send + Sync>> {
        let admitted = self.admission.lock().await;
        if !*admitted {
            return Err("job admission is closed".into());
        }
        if job.trigger != JobTrigger::Manual {
            return Err("manual index submission requires a manual trigger".into());
        }
        let requested = index_target_pairs(&job.target, workspaces);
        let rows = sqlx::query(
            "SELECT * FROM Jobs WHERE job_type = ? AND (status IN ('Pending', 'Queued', 'Running') OR (status = 'Failed' AND attempts < max_attempts)) ORDER BY id",
        )
        .bind(INDEX_QUEUE)
        .fetch_all(self.index_jobs.pool())
        .await?;
        let overlaps = rows
            .into_iter()
            .filter_map(|row| {
                let payload = row.get::<Vec<u8>, _>("job");
                let active = serde_json::from_slice::<IndexJob>(&payload).ok()?;
                (!requested.is_disjoint(&index_target_pairs(&active.target, workspaces)))
                    .then(|| decode_row(row))
                    .flatten()
            })
            .collect();
        let id = self.push_index_job(job).await?;
        Ok((id, overlaps))
    }

    async fn push_index_job(
        &self,
        job: IndexJob,
    ) -> Result<IndexJobId, Box<dyn Error + Send + Sync>> {
        let (workspace, repository) = match &job.target {
            IndexTarget::Workspace { workspace } => (Some(workspace.clone()), None),
            IndexTarget::Repository { repository, .. } => (None, Some(repository.clone())),
        };
        let trigger = match job.trigger {
            JobTrigger::Automatic => "automatic",
            JobTrigger::Manual => "manual",
        };

        let target = match &job.target {
            IndexTarget::Workspace { .. } => "workspace",
            IndexTarget::Repository { .. } => "repository",
        };
        let id = Ulid::new();
        let (task, operation) = queued_task(job, id, INDEX_QUEUE, unix_millis()?);
        let mut storage = self.index_jobs.clone();
        async move {
            async move {
                storage.push_task(task).await?;
                tracing::info!(job.id = %id, "index job enqueued");
                Ok::<(), Box<dyn Error + Send + Sync>>(())
            }
            .instrument(tracing::info_span!(
                "job.enqueue",
                job.trigger = trigger,
                job.target = target,
                job.workspace = workspace.as_deref(),
                job.repository = repository.as_deref(),
                messaging.system = "apalis",
                messaging.destination.name = INDEX_QUEUE,
                messaging.message.id = %id,
                messaging.operation.name = "send",
                messaging.operation.type = "send",
            ))
            .await
        }
        .instrument(operation)
        .await?;
        Ok(IndexJobId(id.to_string()))
    }

    pub async fn automatic_index_active(&self, workspace: &str) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT job FROM Jobs WHERE job_type = ? AND (status IN ('Pending', 'Queued', 'Running') OR (status = 'Failed' AND attempts < max_attempts))",
        )
        .bind(INDEX_QUEUE)
        .fetch_all(self.index_jobs.pool())
        .await?;
        Ok(rows.into_iter().any(|row| {
            serde_json::from_slice::<IndexJob>(row.get::<&[u8], _>("job")).is_ok_and(|job| {
                job.trigger == JobTrigger::Automatic
                    && matches!(job.target, IndexTarget::Workspace { workspace: active } if active == workspace)
            })
        }))
    }

    async fn terminal_automatic_job(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<u64>)>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT job FROM Jobs WHERE id = ? AND NOT (status IN ('Pending', 'Queued', 'Running') OR (status = 'Failed' AND attempts < max_attempts))",
        )
        .bind(id)
        .fetch_optional(self.index_jobs.pool())
        .await?;
        Ok(row.and_then(|row| {
            serde_json::from_slice::<IndexJob>(row.get::<&[u8], _>("job"))
                .ok()
                .and_then(|job| match (job.trigger, job.target) {
                    (JobTrigger::Automatic, IndexTarget::Workspace { workspace }) => {
                        Some((workspace, job.generation))
                    }
                    _ => None,
                })
        }))
    }

    pub async fn recover(&self) -> Result<Recovery, sqlx::Error> {
        let mut transaction = self.index_jobs.pool().begin().await?;
        let released_reservations = sqlx::query(
            "UPDATE Jobs SET status = 'Pending', lock_at = NULL, lock_by = NULL WHERE status = 'Queued' AND lock_by IS NOT NULL",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let interrupted =
            sqlx::query("SELECT id, attempts, max_attempts FROM Jobs WHERE status = 'Running'")
                .fetch_all(&mut *transaction)
                .await?;
        let mut recovery = Recovery {
            released_reservations,
            interrupted_running: interrupted.len() as u64,
            ..Recovery::default()
        };
        for row in interrupted {
            let id = row.get::<String, _>("id");
            let attempts = row.get::<i64, _>("attempts") + 1;
            let max_attempts = row.get::<i64, _>("max_attempts");
            let terminal = attempts >= max_attempts;
            let status = if terminal { "Killed" } else { "Failed" };
            recovery.immediate_retries += u64::from(!terminal);
            recovery.terminal_failures += u64::from(terminal);
            sqlx::query(
                "UPDATE Jobs SET status = ?, attempts = ?, run_at = strftime('%s', 'now'), done_at = strftime('%s', 'now'), lock_at = NULL, lock_by = NULL, last_result = ? WHERE id = ?",
            )
            .bind(status)
            .bind(attempts)
            .bind(r#"{"Err":"daemon stopped during job attempt"}"#)
            .bind(&id)
            .execute(&mut *transaction)
            .await?;
            tracing::warn!(job.id = %id, disposition = status, "interrupted index job recovered");
        }
        transaction.commit().await?;
        tracing::info!(
            released_reservations = recovery.released_reservations,
            interrupted_running = recovery.interrupted_running,
            immediate_retries = recovery.immediate_retries,
            terminal_failures = recovery.terminal_failures,
            "job queue recovery completed"
        );
        Ok(recovery)
    }

    pub async fn schedule_retry(&self, id: &str, attempt: usize) -> Result<(), sqlx::Error> {
        let delay_ms = retry_delay_ms(attempt);
        let eligible_at_ms = unix_millis()
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
            .saturating_add(delay_ms);
        sqlx::query(
            "UPDATE Jobs SET run_at = ? / 1000, metadata = json_set(COALESCE(metadata, '{}'), ?, json(?)) WHERE id = ?",
        )
        .bind(eligible_at_ms)
        .bind(eligible_metadata_path())
        .bind(serde_json::to_string(&EligibleAt { millis: eligible_at_ms }).expect("eligible time serializes"))
        .bind(id)
        .execute(self.index_jobs.pool())
        .await?;
        tracing::warn!(
            job.id = id,
            attempt,
            max_attempts = MAX_ATTEMPTS,
            delay_ms,
            eligible_at_ms,
            "index job retry scheduled"
        );
        Ok(())
    }

    pub async fn list(
        &self,
        cursor: Option<(i64, String)>,
    ) -> Result<(Vec<StoredJob>, Option<(i64, String)>), Box<dyn Error + Send + Sync>> {
        self.list_after_active(cursor, || {}).await
    }

    async fn list_after_active<F>(
        &self,
        cursor: Option<(i64, String)>,
        after_active: F,
    ) -> Result<(Vec<StoredJob>, Option<(i64, String)>), Box<dyn Error + Send + Sync>>
    where
        F: FnOnce(),
    {
        let mut transaction = self.index_jobs.pool().begin().await?;
        let mut jobs = Vec::new();
        if cursor.is_none() {
            let rows = sqlx::query(
                "SELECT * FROM Jobs WHERE status IN ('Pending', 'Queued', 'Running') OR (status = 'Failed' AND attempts < max_attempts) ORDER BY CASE WHEN status = 'Running' THEN 0 WHEN COALESCE(json_extract(metadata, ?), run_at * 1000) > ? THEN 1 ELSE 2 END, COALESCE(json_extract(metadata, ?), run_at * 1000), id",
            )
            .bind(eligible_millis_path())
            .bind(unix_millis()?)
            .bind(eligible_millis_path())
            .fetch_all(&mut *transaction)
            .await?;
            jobs.extend(rows.into_iter().filter_map(decode_row));
        }
        after_active();
        let terminal = match cursor {
            Some((done_at, id)) => {
                sqlx::query(
                    "SELECT * FROM Jobs WHERE (status IN ('Done', 'Killed') OR (status = 'Failed' AND attempts >= max_attempts)) AND (done_at < ? OR (done_at = ? AND id < ?)) ORDER BY done_at DESC, id DESC LIMIT 16",
                )
                .bind(done_at)
                .bind(done_at)
                .bind(id)
                .fetch_all(&mut *transaction)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT * FROM Jobs WHERE status IN ('Done', 'Killed') OR (status = 'Failed' AND attempts >= max_attempts) ORDER BY done_at DESC, id DESC LIMIT 16",
                )
                .fetch_all(&mut *transaction)
                .await?
            }
        };
        let has_more = terminal.len() > 15;
        jobs.extend(terminal.into_iter().take(15).filter_map(decode_row));
        let next = has_more.then(|| {
            let last = jobs.last().expect("terminal page contains a last row");
            (last.done_at.unwrap_or_default(), last.id.clone())
        });
        transaction.commit().await?;
        Ok((jobs, next))
    }

    pub async fn get(&self, id: &str) -> Result<Option<StoredJob>, Box<dyn Error + Send + Sync>> {
        Ok(sqlx::query("SELECT * FROM Jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(self.index_jobs.pool())
            .await?
            .and_then(decode_row))
    }

    #[cfg(test)]
    async fn close(self) {
        self.index_jobs.pool().close().await;
    }
}

fn retry_delay_ms(attempt: usize) -> i64 {
    250_i64.saturating_mul(1_i64 << attempt.saturating_sub(1))
}

fn eligible_metadata_path() -> String {
    format!("$.\"{}\"", std::any::type_name::<EligibleAt>())
}

fn eligible_millis_path() -> String {
    format!("{}.millis", eligible_metadata_path())
}

fn unix_millis() -> Result<i64, std::time::SystemTimeError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64)
}

async fn wait_until(eligible_at_ms: i64) {
    let Ok(now_ms) = unix_millis() else {
        return;
    };
    if let Some(wait_ms) = eligible_at_ms.checked_sub(now_ms).filter(|wait| *wait > 0) {
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms as u64)).await;
    }
}

fn queue_wait_ms(eligible_at_ms: i64, started_at_ms: i64) -> u64 {
    started_at_ms.saturating_sub(eligible_at_ms).max(0) as u64
}

fn queued_task<Args: JobTelemetry>(
    args: Args,
    id: Ulid,
    queue: &'static str,
    eligible_at_ms: i64,
) -> (Task<Args, SqliteContext, Ulid>, tracing::Span) {
    let summary = args.summary();
    let operation = tracing::info_span!(
        "job.operation",
        job.id = %id,
        job.kind = Args::KIND,
        job.name = Args::KIND,
        job.queue = queue,
        job.summary = %summary,
        messaging.system = "apalis",
        messaging.destination.name = queue,
        messaging.message.id = %id,
    );
    let task = operation.in_scope(|| {
        task_with_trace_context(
            args,
            id,
            eligible_at_ms,
            beholder_observability::current_w3c_trace_context(),
        )
    });
    (task, operation)
}

fn task_with_trace_context<Args>(
    args: Args,
    id: Ulid,
    eligible_at_ms: i64,
    context: Option<beholder_observability::W3cTraceContext>,
) -> Task<Args, SqliteContext, Ulid> {
    TaskBuilder::new(args)
        .with_task_id(TaskId::new(id))
        .max_attempts(MAX_ATTEMPTS)
        .meta(task_tracing_context(context))
        .meta(EligibleAt {
            millis: eligible_at_ms,
        })
        .build()
}

fn task_tracing_context(
    context: Option<beholder_observability::W3cTraceContext>,
) -> TracingContext {
    context.map_or_else(TracingContext::new, |context| {
        let mut tracing = TracingContext::new()
            .with_trace_id(context.trace_id)
            .with_span_id(context.span_id)
            .with_trace_flags(context.trace_flags);
        if let Some(state) = context.trace_state {
            tracing = tracing.with_trace_state(state);
        }
        tracing
    })
}

fn continue_task_trace(span: &tracing::Span, context: &SqliteContext) {
    let tracing_context: Result<TracingContext, _> = context.extract();
    if let Ok(context) = tracing_context
        && let (Some(trace_id), Some(span_id)) = (context.trace_id(), context.span_id())
    {
        beholder_observability::set_parent_from_w3c(
            span,
            &beholder_observability::W3cTraceContext {
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
                trace_flags: context.trace_flags().unwrap_or_default(),
                trace_state: context.trace_state().clone(),
            },
        );
    }
}

fn index_target_pairs(
    target: &IndexTarget,
    workspaces: &[Workspace],
) -> BTreeSet<(Option<String>, String)> {
    match target {
        IndexTarget::Workspace { workspace } => workspaces
            .iter()
            .find(|candidate| candidate.name == *workspace)
            .into_iter()
            .flat_map(|workspace| {
                workspace.repositories.iter().map(|repository| {
                    (
                        Some(workspace.name.clone()),
                        repository.repository.identity.clone(),
                    )
                })
            })
            .collect(),
        IndexTarget::Repository {
            repository,
            workspace_scope: Some(workspace),
        } => BTreeSet::from([(Some(workspace.clone()), repository.clone())]),
        IndexTarget::Repository {
            repository,
            workspace_scope: None,
        } => {
            let mut pairs = workspaces
                .iter()
                .filter(|workspace| {
                    workspace
                        .repositories
                        .iter()
                        .any(|candidate| candidate.repository.identity == *repository)
                })
                .map(|workspace| (Some(workspace.name.clone()), repository.clone()))
                .collect::<BTreeSet<_>>();
            if pairs.is_empty() {
                pairs.insert((None, repository.clone()));
            }
            pairs
        }
    }
}

pub fn start_index_worker(
    queue: JobQueue,
    scheduler: Arc<IndexScheduler>,
    store: Arc<SemanticStore>,
    workspaces: Arc<std::sync::Mutex<WorkspaceRegistry>>,
) -> IndexWorker {
    let name = format!("beholder-index-{}-{}", std::process::id(), Ulid::new());
    let acknowledgement = AutomaticIndexAcknowledgement {
        queue: queue.clone(),
        scheduler: Arc::clone(&scheduler),
    };
    let worker = WorkerBuilder::new(&name)
        .backend(EligibleIndexStorage(queue.index_jobs.clone()))
        .ack_with(acknowledgement)
        .data(scheduler)
        .data(store)
        .data(workspaces)
        .data(queue)
        .build(run_index_job);
    let mut context = WorkerContext::new::<()>(&name);
    context.start().expect("new index worker starts once");
    let handle = context.clone();
    let task = tokio::spawn(async move {
        worker
            .run_with_ctx(&mut context)
            .await
            .map_err(|e| e.to_string())
    });
    IndexWorker {
        context: handle,
        task,
    }
}

async fn run_index_job(
    job: IndexJob,
    scheduler: Data<Arc<IndexScheduler>>,
    store: Data<Arc<SemanticStore>>,
    workspaces: Data<Arc<std::sync::Mutex<WorkspaceRegistry>>>,
    queue: Data<JobQueue>,
    id: TaskId<Ulid>,
    index_attempt: IndexAttempt,
) -> Result<IndexJobResult, BoxDynError> {
    let IndexAttempt { attempt, context } = index_attempt;
    let (workspace, repository) = match &job.target {
        IndexTarget::Workspace { workspace } => (Some(workspace.clone()), None),
        IndexTarget::Repository { repository, .. } => (None, Some(repository.clone())),
    };
    let trigger = match job.trigger {
        JobTrigger::Automatic => "automatic",
        JobTrigger::Manual => "manual",
    };
    let attempt_started_at_ms = unix_millis().unwrap_or_default();
    let eligible: Result<EligibleAt, _> = context.extract();
    let queue_wait_ms = queue_wait_ms(
        eligible.map_or(attempt_started_at_ms, |eligible| eligible.millis),
        attempt_started_at_ms,
    );
    let span = tracing::info_span!(
        "job.attempt",
        job.id = %id,
        job.kind = "index",
        job.trigger = trigger,
        job.queue = INDEX_QUEUE,
        job.workspace = workspace.as_deref(),
        job.repository = repository.as_deref(),
        attempt = attempt.current(),
        max_attempts = MAX_ATTEMPTS,
        queue_wait_ms,
        job.outcome = tracing::field::Empty,
        messaging.system = "apalis",
        messaging.destination.name = INDEX_QUEUE,
        messaging.message.id = %id,
        messaging.operation.name = "process",
        messaging.operation.type = "process",
    );
    continue_task_trace(&span, &context);
    async move {
        tracing::info!(job.id = %id, attempt = attempt.current(), queue_wait_ms, "index job attempt started");
        let scheduler_for_job = Arc::clone(&scheduler);
        let store_for_job = Arc::clone(&store);
        let workspaces_for_job = Arc::clone(&workspaces);
        let job_for_run = job.clone();
        let parent = tracing::Span::current();
        let result = tokio::task::spawn_blocking(move || {
            parent.in_scope(|| {
                scheduler_for_job.run_index_job(&store_for_job, &workspaces_for_job, &job_for_run)
            })
        })
        .await
        .map_err(|error| error.to_string())?;
        match result {
            Ok(result) => {
                if result.destinations.iter().any(|result| result.published) {
                    scheduler.schedule_checkpoint(Arc::clone(&store));
                }
                let outcome = if result
                    .destinations
                    .iter()
                    .all(|result| result.outcome == IndexOutcome::Superseded)
                {
                    "superseded"
                } else if result.destinations.iter().any(|result| result.published) {
                    "published"
                } else {
                    "unchanged"
                };
                tracing::Span::current().record("job.outcome", outcome);
                tracing::info!(job.id = %id, outcome, "index job completed");
                Ok(result)
            }
            Err(error) => {
                let terminal = attempt.current() >= MAX_ATTEMPTS as usize;
                if job.trigger == JobTrigger::Automatic
                    && let Some(workspace) = workspace.as_deref()
                {
                    scheduler.automatic_job_failed(workspace, job.generation, terminal);
                }
                if terminal {
                    tracing::Span::current().record("job.outcome", "failed");
                    tracing::error!(job.id = %id, %error, attempt = attempt.current(), max_attempts = MAX_ATTEMPTS, "index job failed");
                } else {
                    queue.schedule_retry(&id.to_string(), attempt.current()).await?;
                    tracing::warn!(job.id = %id, %error, attempt = attempt.current(), max_attempts = MAX_ATTEMPTS, "index job attempt failed");
                }
                Err(error.into())
            }
        }
    }
    .instrument(span)
    .await
}

fn decode_row(row: sqlx::sqlite::SqliteRow) -> Option<StoredJob> {
    let id = row.get::<String, _>("id");
    let queue = row.get::<String, _>("job_type");
    let raw_status = row.get::<String, _>("status");
    let attempts = (row.get::<i64, _>("attempts") + i64::from(raw_status == "Running")) as u32;
    let max_attempts = row.get::<i64, _>("max_attempts") as u32;
    let run_at = row.get::<Option<i64>, _>("run_at");
    let eligible_at_ms = row
        .get::<Option<String>, _>("metadata")
        .and_then(|metadata| serde_json::from_str::<serde_json::Value>(&metadata).ok())
        .and_then(|metadata| metadata.get(std::any::type_name::<EligibleAt>()).cloned())
        .and_then(|eligible| serde_json::from_value::<EligibleAt>(eligible).ok())
        .map(|eligible| eligible.millis)
        .or_else(|| run_at.map(|seconds| seconds.saturating_mul(1_000)));
    let now_ms = unix_millis().ok()?;
    let status = match raw_status.as_str() {
        "Running" => StoredJobStatus::Running,
        "Done" => StoredJobStatus::Completed,
        "Killed" => StoredJobStatus::Failed,
        "Failed" if attempts >= max_attempts => StoredJobStatus::Failed,
        "Pending" | "Queued" | "Failed"
            if eligible_at_ms.is_some_and(|eligible| eligible > now_ms) =>
        {
            StoredJobStatus::Waiting
        }
        "Pending" | "Queued" | "Failed" => StoredJobStatus::Queued,
        _ => return None,
    };
    let done_at = matches!(status, StoredJobStatus::Completed | StoredJobStatus::Failed)
        .then(|| row.get::<Option<i64>, _>("done_at"))
        .flatten();
    let payload = row.get::<Vec<u8>, _>("job");
    let (kind, trigger, target, prerequisites) = if queue == INDEX_QUEUE {
        let job = serde_json::from_slice::<IndexJob>(&payload).ok()?;
        let target = match &job.target {
            IndexTarget::Workspace { workspace } => StoredJobTarget::Workspace(workspace.clone()),
            IndexTarget::Repository {
                repository,
                workspace_scope,
            } => StoredJobTarget::Repository {
                repository: repository.clone(),
                workspace_scope: workspace_scope.clone(),
                worker_id: None,
            },
        };
        (
            StoredJobKind::Index,
            job.trigger,
            target,
            job.prerequisite_index_jobs
                .into_iter()
                .map(|id| id.0)
                .collect(),
        )
    } else if queue == ENRICHMENT_QUEUE {
        let job = serde_json::from_slice::<EnrichmentJob>(&payload).ok()?;
        let target = match &job.target {
            EnrichmentTarget::WorkspaceRepository {
                workspace,
                repository,
            } => StoredJobTarget::Repository {
                repository: repository.clone(),
                workspace_scope: Some(workspace.clone()),
                worker_id: Some(job.worker_id.clone()),
            },
            EnrichmentTarget::StandaloneRepository { repository } => StoredJobTarget::Repository {
                repository: repository.clone(),
                workspace_scope: None,
                worker_id: Some(job.worker_id.clone()),
            },
        };
        (
            StoredJobKind::Enrichment,
            job.trigger,
            target,
            job.prerequisite_index_jobs
                .into_iter()
                .map(|id| id.0)
                .collect(),
        )
    } else {
        return None;
    };
    let last_result = row.get::<Option<String>, _>("last_result");
    let last_error = last_result.as_deref().and_then(|result| {
        serde_json::from_str::<serde_json::Value>(result)
            .ok()?
            .get("Err")?
            .as_str()
            .map(str::to_owned)
    });
    let result = last_result.as_deref().and_then(|result| {
        serde_json::from_str::<serde_json::Value>(result)
            .ok()?
            .get("Ok")
            .cloned()
            .and_then(decode_index_result)
    });
    Some(StoredJob {
        submitted_at_ms: Ulid::from_string(&id).ok()?.timestamp_ms() as i64,
        id,
        kind,
        status,
        trigger,
        target,
        attempts,
        max_attempts,
        eligible_at_ms,
        lock_at: (raw_status != "Pending" && raw_status != "Queued")
            .then(|| row.get::<Option<i64>, _>("lock_at"))
            .flatten(),
        done_at,
        last_error,
        prerequisites,
        result,
    })
}

fn decode_index_result(result: serde_json::Value) -> Option<IndexJobResult> {
    #[derive(Deserialize)]
    struct LegacyIndexJobResult {
        workspace: String,
        observation_count: usize,
        published: bool,
        outcome: IndexOutcome,
    }

    serde_json::from_value(result.clone()).ok().or_else(|| {
        let legacy = serde_json::from_value::<LegacyIndexJobResult>(result).ok()?;
        Some(IndexJobResult {
            destinations: vec![IndexDestinationResult {
                destination: IndexDestination::Workspace {
                    workspace: legacy.workspace,
                },
                observation_count: legacy.observation_count,
                published: legacy.published,
                outcome: legacy.outcome,
            }],
        })
    })
}

async fn quick_check(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(path).read_only(true),
    )
    .await
    .map_err(|error| {
        format!(
            "queue quick-check failed for {} while opening read-only SQLite: {error}",
            path.display()
        )
    })?;
    let result = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
        .fetch_all(&mut connection)
        .await
        .map_err(|error| {
            format!(
                "queue quick-check failed for {} while running PRAGMA quick_check: {error}",
                path.display()
            )
        })?;
    if result != ["ok"] {
        return Err(format!(
            "queue quick-check failed for {}: PRAGMA quick_check returned {}",
            path.display(),
            result.join(", ")
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod durable_tests {
    use super::*;
    use beholder_domain::{LogicalRepository, WorkspaceRepository};
    use std::{fs, time::Duration};

    fn automatic_job(workspace: &str) -> IndexJob {
        IndexJob {
            target: IndexTarget::Workspace {
                workspace: workspace.into(),
            },
            trigger: JobTrigger::Automatic,
            prerequisite_index_jobs: Vec::new(),
            generation: Some(1),
            repository_intents: Vec::new(),
        }
    }

    fn queue_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "beholder-jobs-{name}-{}-{}.sqlite",
            std::process::id(),
            Ulid::new()
        ))
    }

    fn workspace(name: &str, repositories: &[&str]) -> Workspace {
        Workspace::new(
            name,
            repositories
                .iter()
                .map(|repository| WorkspaceRepository {
                    repository: LogicalRepository {
                        identity: (*repository).into(),
                    },
                    display_name: (*repository).into(),
                    base: repository.into(),
                    alternatives: Vec::new(),
                })
                .collect(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn automatic_jobs_coalesce_and_remain_inspectable() {
        let path = queue_path("inspect");
        let queue = JobQueue::open(&path).await.unwrap();
        let id = queue
            .enqueue_automatic_index(automatic_job("main"))
            .await
            .unwrap()
            .unwrap();
        assert!(
            queue
                .enqueue_automatic_index(automatic_job("main"))
                .await
                .unwrap()
                .is_none()
        );
        let (jobs, next) = queue.list(None).await.unwrap();
        assert_eq!(next, None);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, id.0);
        assert_eq!(jobs[0].status, StoredJobStatus::Queued);
        assert_eq!(jobs[0].max_attempts, MAX_ATTEMPTS);
        assert!(queue.get(&id.0).await.unwrap().is_some());

        queue.close_admission().await;
        assert!(
            queue
                .enqueue_automatic_index(automatic_job("secondary"))
                .await
                .is_err()
        );
        queue.close().await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn manual_jobs_always_enqueue_and_report_every_overlapping_index_job() {
        let path = queue_path("manual-overlap");
        let queue = JobQueue::open(&path).await.unwrap();
        let workspaces = vec![
            workspace("first", &["shared", "first-only"]),
            workspace("second", &["shared"]),
            workspace("third", &["third-only"]),
        ];
        let automatic_first = queue
            .enqueue_automatic_index(automatic_job("first"))
            .await
            .unwrap()
            .unwrap();
        let automatic_second = queue
            .enqueue_automatic_index(automatic_job("second"))
            .await
            .unwrap()
            .unwrap();
        let automatic_third = queue
            .enqueue_automatic_index(automatic_job("third"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE Jobs SET status = 'Running' WHERE id = ?")
            .bind(&automatic_second.0)
            .execute(queue.index_jobs.pool())
            .await
            .unwrap();
        let manual = || IndexJob {
            target: IndexTarget::Repository {
                repository: "shared".into(),
                workspace_scope: None,
            },
            trigger: JobTrigger::Manual,
            prerequisite_index_jobs: Vec::new(),
            generation: None,
            repository_intents: Vec::new(),
        };

        let (first, overlaps) = queue
            .enqueue_manual_index(manual(), &workspaces)
            .await
            .unwrap();
        assert_eq!(overlaps.len(), 2);
        assert_eq!(
            overlaps
                .iter()
                .find(|job| job.id == automatic_first.0)
                .unwrap()
                .status,
            StoredJobStatus::Queued
        );
        assert_eq!(
            overlaps
                .iter()
                .find(|job| job.id == automatic_second.0)
                .unwrap()
                .status,
            StoredJobStatus::Running
        );
        assert!(!overlaps.iter().any(|job| job.id == automatic_third.0));
        sqlx::query("UPDATE Jobs SET status = 'Failed', attempts = 1 WHERE id = ?")
            .bind(&first.0)
            .execute(queue.index_jobs.pool())
            .await
            .unwrap();
        queue.schedule_retry(&first.0, 4).await.unwrap();
        let (second, overlaps) = queue
            .enqueue_manual_index(manual(), &workspaces)
            .await
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(overlaps.len(), 3);
        assert!(overlaps.iter().any(|job| job.id == automatic_first.0));
        assert!(overlaps.iter().any(|job| job.id == automatic_second.0));
        assert!(!overlaps.iter().any(|job| job.id == automatic_third.0));
        assert_eq!(
            overlaps
                .iter()
                .find(|job| job.id == first.0)
                .unwrap()
                .status,
            StoredJobStatus::Waiting
        );
        assert_eq!(
            queue.get(&second.0).await.unwrap().unwrap().trigger,
            JobTrigger::Manual
        );

        queue.close().await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn first_page_reads_active_and_terminal_jobs_from_one_snapshot() {
        let path = queue_path("snapshot");
        let queue = JobQueue::open(&path).await.unwrap();
        let id = queue
            .enqueue_automatic_index(automatic_job("main"))
            .await
            .unwrap()
            .unwrap();
        let pool = queue.index_jobs.pool().clone();
        let completed_id = id.0.clone();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let completion = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    sqlx::query(
                        "UPDATE Jobs SET status = 'Done', done_at = strftime('%s', 'now') WHERE id = ?",
                    )
                    .bind(completed_id)
                    .execute(&pool)
                    .await
                    .unwrap();
                });
            done_tx.send(()).unwrap();
        });

        let (jobs, _) = queue
            .list_after_active(None, || {
                start_tx.send(()).unwrap();
                done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            })
            .await
            .unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, id.0);
        assert_eq!(jobs[0].status, StoredJobStatus::Queued);
        completion.join().unwrap();
        assert_eq!(
            queue.get(&id.0).await.unwrap().unwrap().status,
            StoredJobStatus::Completed
        );
        queue.close().await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn startup_recovery_distinguishes_reserved_and_running_jobs() {
        let path = queue_path("recovery");
        let queue = JobQueue::open(&path).await.unwrap();
        let reserved = queue
            .enqueue_automatic_index(automatic_job("reserved"))
            .await
            .unwrap()
            .unwrap();
        let running = queue
            .enqueue_automatic_index(automatic_job("running"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "INSERT INTO Workers (id, worker_type, storage_name) VALUES ('fetcher', 'index', 'sqlite'), ('worker', 'index', 'sqlite')",
        )
        .execute(queue.index_jobs.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE Jobs SET status = 'Queued', lock_by = 'fetcher' WHERE id = ?")
            .bind(&reserved.0)
            .execute(queue.index_jobs.pool())
            .await
            .unwrap();
        sqlx::query(
            "UPDATE Jobs SET status = 'Running', attempts = 3, lock_by = 'worker' WHERE id = ?",
        )
        .bind(&running.0)
        .execute(queue.index_jobs.pool())
        .await
        .unwrap();

        let recovery = queue.recover().await.unwrap();
        assert_eq!(recovery.released_reservations, 1);
        assert_eq!(recovery.interrupted_running, 1);
        assert_eq!(recovery.immediate_retries, 1);
        let reserved = queue.get(&reserved.0).await.unwrap().unwrap();
        assert_eq!(reserved.status, StoredJobStatus::Queued);
        assert_eq!(reserved.attempts, 0);
        let running = queue.get(&running.0).await.unwrap().unwrap();
        assert_eq!(running.status, StoredJobStatus::Queued);
        assert_eq!(running.attempts, 4);
        assert!(running.last_error.unwrap().contains("daemon stopped"));

        assert_eq!(
            (1..MAX_ATTEMPTS as usize)
                .map(retry_delay_ms)
                .collect::<Vec<_>>(),
            [250, 500, 1_000, 2_000]
        );
        queue.close().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn retry_schedule_persists_subsecond_eligibility() {
        let path = queue_path("subsecond-retry");
        let queue = JobQueue::open(&path).await.unwrap();
        let id = queue
            .enqueue_automatic_index(automatic_job("main"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE Jobs SET status = 'Failed', attempts = 1 WHERE id = ?")
            .bind(&id.0)
            .execute(queue.index_jobs.pool())
            .await
            .unwrap();
        let before = unix_millis().unwrap();
        queue.schedule_retry(&id.0, 1).await.unwrap();
        let after = unix_millis().unwrap();

        let stored = queue.get(&id.0).await.unwrap().unwrap();
        let eligible_at_ms = stored.eligible_at_ms.unwrap();
        assert!(eligible_at_ms >= before + 250);
        assert!(eligible_at_ms <= after + 250);
        assert_eq!(stored.status, StoredJobStatus::Waiting);
        assert_eq!(queue_wait_ms(eligible_at_ms, eligible_at_ms + 37), 37);
        assert_eq!(queue_wait_ms(eligible_at_ms, eligible_at_ms - 1), 0);

        let mut worker = WorkerContext::new::<()>("subsecond-retry-test");
        worker.start().unwrap();
        let mut stream = EligibleIndexStorage(queue.index_jobs.clone()).poll(&worker);
        let delivered = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(task) = stream.next().await.unwrap().unwrap() {
                    break task;
                }
            }
        })
        .await
        .expect("retry was not delivered after its eligible time");
        assert_eq!(delivered.parts.task_id.unwrap().to_string(), id.0);
        assert!(unix_millis().unwrap() >= eligible_at_ms);

        queue.close().await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn idle_index_queue_keeps_bounded_pickup_latency() {
        let path = queue_path("idle-pickup");
        let queue = JobQueue::open(&path).await.unwrap();
        let storage = EligibleIndexStorage(queue.index_jobs.clone());
        let delivery = tokio::spawn(async move {
            let mut worker = WorkerContext::new::<()>("idle-pickup-test");
            worker.start().unwrap();
            let mut stream = storage.poll(&worker);
            loop {
                if let Some(task) = stream.next().await.unwrap().unwrap() {
                    break task;
                }
            }
        });

        tokio::time::sleep(Duration::from_secs(2)).await;
        let id = queue
            .enqueue_automatic_index(automatic_job("main"))
            .await
            .unwrap()
            .unwrap();
        let delivered = tokio::time::timeout(Duration::from_millis(500), delivery)
            .await
            .expect("idle index queue did not poll within its bounded interval")
            .unwrap();
        assert_eq!(delivered.parts.task_id.unwrap().to_string(), id.0);

        queue.close().await;
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn terminal_history_uses_fifteen_row_keyset_pages() {
        let path = queue_path("pagination");
        let queue = JobQueue::open(&path).await.unwrap();
        for index in 0..17 {
            let id = queue
                .enqueue_automatic_index(automatic_job(&format!("workspace-{index}")))
                .await
                .unwrap()
                .unwrap();
            sqlx::query("UPDATE Jobs SET status = 'Done', done_at = ? WHERE id = ?")
                .bind(index)
                .bind(id.0)
                .execute(queue.index_jobs.pool())
                .await
                .unwrap();
        }

        let (first, cursor) = queue.list(None).await.unwrap();
        assert_eq!(first.len(), 15);
        let (second, next) = queue.list(cursor).await.unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(next, None);

        queue.close().await;
        fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "beholder-queue-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn creates_reopens_migrates_and_recreates_an_externally_deleted_queue() {
        let state = temp_dir("lifecycle");
        let path = state.join("queue.sqlite");

        let queue = JobQueue::open(&path).await.unwrap();
        assert!(path.is_file());
        assert_eq!(queue.index_jobs.config().queue().as_ref(), INDEX_QUEUE);
        assert_eq!(
            queue.enrichment_jobs.config().queue().as_ref(),
            ENRICHMENT_QUEUE
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'Jobs'",
            )
            .fetch_one(queue.index_jobs.pool())
            .await
            .unwrap(),
            "Jobs"
        );
        queue.close().await;

        JobQueue::open(&path).await.unwrap().close().await;
        fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        JobQueue::open(&path).await.unwrap().close().await;
        assert!(path.is_file());

        fs::remove_dir_all(state).unwrap();
    }

    #[tokio::test]
    async fn rejects_and_preserves_a_corrupt_existing_queue() {
        let state = temp_dir("corrupt");
        let path = state.join("queue.sqlite");
        let corrupt = b"not a SQLite database";
        fs::write(&path, corrupt).unwrap();

        let error = JobQueue::open(&path).await.unwrap_err().to_string();
        assert!(error.contains("quick-check"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
        assert_eq!(fs::read(&path).unwrap(), corrupt);

        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn typed_payloads_round_trip_without_source_or_graph_state() {
        let index = IndexJob {
            target: IndexTarget::Repository {
                repository: "example/repository".into(),
                workspace_scope: None,
            },
            trigger: JobTrigger::Automatic,
            prerequisite_index_jobs: vec![IndexJobId("01J00000000000000000000000".into())],
            generation: Some(7),
            repository_intents: vec![RepositoryIntent {
                repository: "example/repository".into(),
                changes: vec![RepositoryChange::SourcePath("src/lib.rs".into())],
            }],
        };
        let enrichment = EnrichmentJob {
            target: EnrichmentTarget::WorkspaceRepository {
                workspace: "main".into(),
                repository: "example/repository".into(),
            },
            worker_id: "rust".into(),
            expected_worker_version: "1".into(),
            trigger: JobTrigger::Automatic,
            prerequisite_index_jobs: vec![IndexJobId("01K00000000000000000000000".into())],
            input_fingerprint: Some("fingerprint".into()),
        };

        assert_eq!(
            serde_json::from_slice::<IndexJob>(&serde_json::to_vec(&index).unwrap()).unwrap(),
            index
        );
        assert_eq!(
            serde_json::from_slice::<EnrichmentJob>(&serde_json::to_vec(&enrichment).unwrap())
                .unwrap(),
            enrichment
        );
        assert_eq!(
            enrichment.summary(),
            "enrich repository example/repository in workspace main with worker rust"
        );
        assert!(!enrichment.summary().contains("fingerprint"));
    }

    #[test]
    fn queued_tasks_preserve_generic_w3c_context() {
        let expected = beholder_observability::W3cTraceContext {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            trace_flags: 1,
            trace_state: Some("vendor=value".into()),
        };
        let task = task_with_trace_context((), Ulid::new(), 42, Some(expected.clone()));
        let stored: TracingContext = task.parts.ctx.extract().unwrap();

        assert_eq!(stored.trace_id(), &Some(expected.trace_id));
        assert_eq!(stored.span_id(), &Some(expected.span_id));
        assert_eq!(stored.trace_flags(), &Some(expected.trace_flags));
        assert_eq!(stored.trace_state(), &expected.trace_state);
    }
}
