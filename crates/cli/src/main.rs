use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::observations;
use beholder_daemon_client::{
    context as daemon_context, dependencies as daemon_dependencies, get_status,
    impact as daemon_impact, index_rust_workspace as daemon_index_workspace, stop,
    trace as daemon_trace, why as daemon_why,
};
use beholder_domain::WorkspaceView;
use clap::{Parser, Subcommand, ValueEnum};
use std::{error::Error, fs, path::Path, path::PathBuf};

const MAIN_VIEW: &str = "main";

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
    },
    /// Index multiple repositories as one coherent workspace view.
    IndexRustWorkspace {
        /// Repository roots to analyze together.
        #[arg(required = true, num_args = 1..)]
        repositories: Vec<PathBuf>,
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
}

#[derive(clap::Args)]
struct QueryPath {
    /// Starting semantic entity ID.
    from: String,
    /// Destination semantic entity ID.
    to: String,
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
        Some(Command::IndexRustRepo { repository }) => {
            print_index_result(daemon_index_workspace(&[repository]).await?)
        }
        Some(Command::IndexRustWorkspace { repositories }) => {
            print_index_result(daemon_index_workspace(&repositories).await?)
        }
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
            println!("{:#?}", daemon_context(query.entity).await?);
        }
        Some(Command::Impact(query)) => {
            println!("{:#?}", daemon_impact(query.entity).await?);
        }
        Some(Command::Dependencies(query)) => {
            println!("{:#?}", daemon_dependencies(query.entity).await?)
        }
        Some(Command::Trace(query)) => {
            println!("{:#?}", daemon_trace(query.from, query.to).await?)
        }
        Some(Command::Why(query)) => println!("{:#?}", daemon_why(query.from, query.to).await?),
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
        let _ = fs::remove_file(&path);
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
            Some(2)
        );
        drop(indexed);
        fs::remove_file(source).unwrap();
        fs::remove_file(path).unwrap();
    }
}
