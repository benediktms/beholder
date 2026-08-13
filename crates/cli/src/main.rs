use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{observations, resolve_repository_calls, source_files};
use beholder_daemon_client::{get_status, stop};
use beholder_domain::{RepositoryState, WorkspaceView};
use clap::{Parser, Subcommand, ValueEnum};
use std::{error::Error, fs, path::Path, path::PathBuf};

const MAIN_VIEW: &str = "main";
type RustSources = Vec<(PathBuf, String)>;

fn index_rust(path: &Path, database_path: &Path) -> Result<(usize, bool), Box<dyn Error>> {
    let sources = vec![(path.to_path_buf(), fs::read_to_string(path)?)];
    let state = repository_state(path.parent().unwrap_or_else(|| Path::new(".")), &sources)?;
    let view = WorkspaceView::new(MAIN_VIEW, vec![state.clone()])?;
    let store = SemanticStore::persistent(database_path, true)?;
    if store.view_matches(&view)? {
        return Ok((0, false));
    }
    let observations = observations(&state.repository.identity, &sources[0].1, path)?;
    store.publish(&view, &observations)?;
    Ok((observations.len(), true))
}

fn index_rust_repository(
    root: &Path,
    database_path: &Path,
) -> Result<(usize, bool), Box<dyn Error>> {
    index_rust_workspace(&[root.to_path_buf()], database_path)
}

fn rust_repository_sources(root: &Path) -> Result<(RepositoryState, RustSources), Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("repository does not exist: {}", root.display()).into());
    }
    let mut files = Vec::new();
    source_files(root, &mut files)?;
    files.sort();

    let sources = files
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            Ok((relative_path, fs::read_to_string(path)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let state = repository_state(root, &sources)?;
    Ok((state, sources))
}

fn index_rust_workspace(
    roots: &[PathBuf],
    database_path: &Path,
) -> Result<(usize, bool), Box<dyn Error>> {
    if roots.is_empty() {
        return Err("workspace must contain a repository".into());
    }
    let repositories = roots
        .iter()
        .map(|root| rust_repository_sources(root))
        .collect::<Result<Vec<_>, _>>()?;
    let view = WorkspaceView::new(
        MAIN_VIEW,
        repositories
            .iter()
            .map(|(state, _)| state.clone())
            .collect(),
    )?;
    let store = SemanticStore::persistent(database_path, true)?;
    if store.view_matches(&view)? {
        return Ok((0, false));
    }

    let mut all_observations = Vec::new();
    for (state, sources) in repositories {
        for (path, source) in sources {
            all_observations.extend(observations(&state.repository.identity, &source, &path)?);
        }
    }
    resolve_repository_calls(&mut all_observations);
    store.publish(&view, &all_observations)?;
    Ok((all_observations.len(), true))
}

#[derive(Parser)]
#[command(
    name = "beholder",
    version,
    about = "Multi-repository architecture intelligence"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Control the centralized Beholder daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Index one Rust source file.
    IndexRust {
        /// Rust source file to analyze.
        source: PathBuf,
        /// Mnestic SQLite database path.
        #[arg(short, long)]
        database: PathBuf,
    },
    /// Index every Rust source file in one repository.
    IndexRustRepo {
        /// Repository root to analyze.
        repository: PathBuf,
        /// Mnestic SQLite database path.
        #[arg(short, long)]
        database: PathBuf,
    },
    /// Index multiple repositories as one coherent workspace view.
    IndexRustWorkspace {
        /// Repository roots to analyze together.
        #[arg(required = true, num_args = 1..)]
        repositories: Vec<PathBuf>,
        /// Mnestic SQLite database path.
        #[arg(short, long)]
        database: PathBuf,
    },
    /// Inspect persisted Mnestic state through Beholder DTOs.
    Inspect {
        /// Persisted data to display.
        #[arg(value_enum)]
        subject: InspectSubject,
        /// Mnestic SQLite database path.
        #[arg(short, long)]
        database: PathBuf,
        /// Limit observations to this relationship.
        #[arg(short, long)]
        relation: Option<String>,
    },
    /// Generate a synthetic graph and measure ingestion and queries.
    Benchmark {
        /// Storage backend: mem or sqlite.
        #[arg(short, long)]
        storage: String,
        /// Synthetic shape: linear, tree, dag, or corpus.
        #[arg(short, long)]
        topology: String,
        /// Number of entities to generate.
        #[arg(short, long)]
        entities: i64,
        /// Outgoing relationships per entity where applicable.
        #[arg(short, long)]
        fanout: i64,
        /// Maximum traversal depth.
        #[arg(long)]
        depth: i64,
        /// Required for SQLite; omitted for memory storage.
        #[arg(short, long)]
        database: Option<PathBuf>,
    },
    /// Query an existing synthetic benchmark database.
    BenchmarkQuery {
        /// Storage backend of the existing benchmark.
        #[arg(short, long)]
        storage: String,
        /// Synthetic shape used to create the benchmark.
        #[arg(short, long)]
        topology: String,
        /// Number of entities in the benchmark.
        #[arg(short, long)]
        entities: i64,
        /// Maximum traversal depth.
        #[arg(long)]
        depth: i64,
        /// Existing benchmark database path.
        #[arg(short, long)]
        database: PathBuf,
    },
    /// Show direct incoming and outgoing relationships for an entity.
    Context(QueryEntity),
    /// Show everything transitively affected by an entity.
    Impact(QueryEntity),
    /// Show transitive dependencies of an entity.
    Dependencies(QueryEntity),
    /// Find an evidence-backed path between two entities.
    Trace(QueryPath),
    /// Explain why one entity depends on another.
    Why(QueryPath),
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Report daemon readiness, PID, and protocol version.
    Status,
    /// Gracefully stop the daemon. Succeeds when it is already stopped.
    Stop,
}

#[derive(Clone, Eq, PartialEq, ValueEnum)]
enum InspectSubject {
    Relations,
    Revisions,
    Observations,
}

#[derive(clap::Args)]
struct QueryEntity {
    /// Canonical semantic entity ID.
    entity: String,
    /// Persisted database; omit to query the in-memory example.
    #[arg(short, long)]
    database: Option<PathBuf>,
}

#[derive(clap::Args)]
struct QueryPath {
    /// Starting semantic entity ID.
    from: String,
    /// Destination semantic entity ID.
    to: String,
    /// Persisted database; omit to query the in-memory example.
    #[arg(short, long)]
    database: Option<PathBuf>,
}

fn query_store(database: Option<&Path>) -> Result<SemanticStore, Box<dyn Error>> {
    match database {
        Some(path) => SemanticStore::persistent(path, false),
        None => SemanticStore::memory(),
    }
}

fn print_index_result((count, published): (usize, bool)) {
    println!(
        "{}",
        if published {
            format!("indexed {count} Rust observations")
        } else {
            "unchanged; kept current analysis revision".into()
        }
    );
}

fn database_argument(database: Option<&Path>) -> Result<Option<&str>, Box<dyn Error>> {
    database
        .map(|path| {
            path.to_str()
                .ok_or_else(|| "database path is not UTF-8".into())
        })
        .transpose()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Some(Command::Daemon {
            command: DaemonCommand::Status,
        }) => {
            let status = get_status().await?;
            println!(
                "{} (pid {}, protocol v{})",
                status.status, status.pid, status.protocol_version
            );
        }
        Some(Command::Daemon {
            command: DaemonCommand::Stop,
        }) => println!(
            "{}",
            if stop().await? {
                "stopped"
            } else {
                "not running"
            }
        ),
        Some(Command::IndexRust { source, database }) => {
            print_index_result(index_rust(&source, &database)?);
        }
        Some(Command::IndexRustRepo {
            repository,
            database,
        }) => print_index_result(index_rust_repository(&repository, &database)?),
        Some(Command::IndexRustWorkspace {
            repositories,
            database,
        }) => print_index_result(index_rust_workspace(&repositories, &database)?),
        Some(Command::Inspect {
            subject,
            database,
            relation,
        }) => {
            if relation.is_some() && subject != InspectSubject::Observations {
                return Err("--relation is only valid for observations".into());
            }
            let store = SemanticStore::persistent(&database, false)?;
            let result = match subject {
                InspectSubject::Relations => store.inspect_relations()?,
                InspectSubject::Revisions => store.inspect_revisions()?,
                InspectSubject::Observations => store.inspect_observations(relation.as_deref())?,
            };
            println!("{result:#?}");
        }
        Some(Command::Benchmark {
            storage,
            topology,
            entities,
            fanout,
            depth,
            database,
        }) => {
            let store =
                SemanticStore::benchmark_store(&storage, database_argument(database.as_deref())?)?;
            println!("{}", store.benchmark(&topology, entities, fanout, depth)?);
        }
        Some(Command::BenchmarkQuery {
            storage,
            topology,
            entities,
            depth,
            database,
        }) => {
            let store =
                SemanticStore::benchmark_store(&storage, database_argument(Some(&database))?)?;
            println!("{}", store.benchmark_queries(&topology, entities, depth));
        }
        Some(Command::Context(query)) => {
            println!(
                "{:#?}",
                query_store(query.database.as_deref())?.context(&query.entity)?
            );
        }
        Some(Command::Impact(query)) => {
            println!(
                "{:#?}",
                query_store(query.database.as_deref())?.impact(&query.entity)?
            );
        }
        Some(Command::Dependencies(query)) => println!(
            "{:#?}",
            query_store(query.database.as_deref())?.dependencies(&query.entity)?
        ),
        Some(Command::Trace(query) | Command::Why(query)) => println!(
            "{:#?}",
            query_store(query.database.as_deref())?.trace(&query.from, &query.to)?
        ),
        None => println!(
            "{:#?}",
            SemanticStore::memory()?.trace("web/CheckoutPage", "cache/update_price")?
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn workspace_smoke() {
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "index-rust-workspace",
                "-d",
                "beholder.db",
                "repo-a",
                "repo-b",
            ])
            .unwrap()
            .command,
            Some(Command::IndexRustWorkspace { repositories, .. }) if repositories.len() == 2
        ));
        assert!(Cli::try_parse_from(["beholder", "index-rust", "src/main.rs"]).is_err());

        let store = SemanticStore::memory().unwrap();

        let result = store
            .trace("web/CheckoutPage", "cache/update_price")
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(format!("{result:?}").contains("CheckoutPage.tsx:12"));

        assert_eq!(store.impact("rpc/Pricing.GetPrice").unwrap().rows.len(), 3);
        assert_eq!(store.context("rpc/Pricing.GetPrice").unwrap().rows.len(), 2);
        assert_eq!(
            store
                .dependencies("rpc/Pricing.GetPrice")
                .unwrap()
                .rows
                .len(),
            3
        );

        let path = env::temp_dir().join(format!("beholder-dogfood-{}.db", std::process::id()));
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let _ = fs::remove_file(&path);
        let (count, published) = index_rust_repository(&root, &path).unwrap();
        assert!(published && count > 0);
        assert_eq!(index_rust_repository(&root, &path).unwrap(), (0, false));
        let indexed = SemanticStore::persistent(&path, false).unwrap();
        let entity = "repo://beholder/rust/crates/adapters-mnestic/src/lib/trace";
        assert!(format!("{:?}", indexed.context(entity).unwrap()).contains("query"));
        assert!(format!("{:?}", indexed.inspect_relations().unwrap()).contains("observation"));
        let calls = indexed.inspect_observations(Some("calls")).unwrap();
        assert!(!calls.rows.is_empty());
        assert!(
            calls
                .rows
                .iter()
                .all(|row| row[2].as_str() == Some("calls"))
        );
        drop(indexed);

        let multi_root =
            env::temp_dir().join(format!("beholder-multi-repository-{}", std::process::id()));
        let _ = fs::remove_dir_all(&multi_root);
        let first = multi_root.join("repo-a");
        let second = multi_root.join("repo-b");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::create_dir_all(second.join("src")).unwrap();
        fs::write(first.join("src/lib.rs"), "fn caller() { helper(); }").unwrap();
        fs::write(second.join("src/lib.rs"), "fn helper() {}").unwrap();
        let multi_database = multi_root.join("beholder.db");
        assert!(
            index_rust_workspace(&[first.clone(), second.clone()], &multi_database)
                .unwrap()
                .1
        );
        assert_eq!(
            index_rust_workspace(&[second.clone(), first.clone()], &multi_database).unwrap(),
            (0, false)
        );
        let indexed = SemanticStore::persistent(&multi_database, false).unwrap();
        assert_eq!(indexed.inspect_revisions().unwrap().rows.len(), 2);
        assert!(
            format!(
                "{:?}",
                indexed.context("repo://repo-a/rust/lib/caller").unwrap()
            )
            .contains("repo://repo-b/rust/lib/helper")
        );
        drop(indexed);
        assert!(
            index_rust_workspace(&[first.clone(), first], &multi_database)
                .unwrap_err()
                .to_string()
                .contains("duplicate repository repo-a")
        );
        fs::remove_dir_all(multi_root).unwrap();

        let source = env::temp_dir().join(format!("beholder-dogfood-{}.rs", std::process::id()));
        fs::write(&source, "fn first() { second(); } fn second() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        fs::write(&source, "fn first() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        assert_eq!(index_rust(&source, &path).unwrap(), (0, false));
        let indexed = SemanticStore::persistent(&path, false).unwrap();
        assert!(
            indexed
                .inspect_observations(Some("calls"))
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            indexed.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(3)
        );
        drop(indexed);
        fs::remove_file(source).unwrap();
        fs::remove_file(path).unwrap();
    }
}
