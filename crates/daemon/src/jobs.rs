use apalis_codec::json::JsonCodec;
use apalis_sqlite::{CompactType, Config, SqliteStorage, fetcher::SqliteFetcher};
use serde::{Deserialize, Serialize};
use sqlx::{
    Connection,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{error::Error, path::Path};

const INDEX_QUEUE: &str = "index";
const ENRICHMENT_QUEUE: &str = "enrichment";

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

pub type IndexJobStorage = SqliteStorage<IndexJob, JsonCodec<CompactType>, SqliteFetcher>;
pub type EnrichmentJobStorage = SqliteStorage<EnrichmentJob, JsonCodec<CompactType>, SqliteFetcher>;

#[derive(Debug)]
pub struct JobQueue {
    pub index_jobs: IndexJobStorage,
    pub enrichment_jobs: EnrichmentJobStorage,
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
            index_jobs: SqliteStorage::new_with_config(&pool, &Config::new(INDEX_QUEUE)),
            enrichment_jobs: SqliteStorage::new_with_config(&pool, &Config::new(ENRICHMENT_QUEUE)),
        })
    }

    #[cfg(test)]
    async fn close(self) {
        self.index_jobs.pool().close().await;
    }
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
    }
}
