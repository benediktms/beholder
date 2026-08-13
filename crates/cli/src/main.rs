use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{FRONTEND_VERSION, observations};
use beholder_daemon_client::{
    clear_cache, context as daemon_context, dependencies as daemon_dependencies, get_status,
    impact as daemon_impact, index_rust_workspace as daemon_index_workspace, list_workspaces,
    register_workspace, state_dir, stop, trace as daemon_trace, why as daemon_why,
};
use beholder_domain::WorkspaceView;
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::Duration,
};

mod service;

const MAIN_VIEW: &str = "main";

fn index_rust(path: &Path, database_path: &Path) -> Result<(usize, bool), Box<dyn Error>> {
    let sources = vec![(path.to_path_buf(), fs::read_to_string(path)?)];
    let state = repository_state(path.parent().unwrap_or_else(|| Path::new(".")), &sources)?;
    let view = WorkspaceView::new(
        MAIN_VIEW,
        format!("rust:{FRONTEND_VERSION}:single-file:1"),
        vec![state.clone()],
    )?;
    let store = SemanticStore::persistent(database_path, true)?;
    if store.view_matches(&view)? {
        return Ok((0, false));
    }
    let observations = observations(&state.repository.identity, &sources[0].1, path)?;
    let _changes = store.publish(&view, &observations)?;
    Ok((observations.len(), true))
}

#[derive(Parser)]
#[command(
    name = "beholder",
    version,
    about = "Multi-repository architecture intelligence",
    arg_required_else_help = true
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
    /// Index a registered workspace as one coherent view.
    IndexRustWorkspace {
        /// Registered workspace name.
        workspace: String,
    },
    /// Manage registered workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Manage disposable analysis caches.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
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
enum CacheCommand {
    /// Clear in-memory and persistent analysis caches.
    Clear,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Install and start the user-level daemon service.
    Install,
    /// Stop and remove the user-level daemon service.
    Uninstall,
    /// Start the daemon in the background.
    Start,
    /// Run the daemon in the foreground.
    Run,
    /// Report daemon readiness, PID, and protocol version.
    Status,
    /// Gracefully stop the daemon. Succeeds when it is already stopped.
    Stop,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Register or replace a workspace configuration.
    Register {
        /// Stable workspace name.
        name: String,
        /// Repository roots in the workspace.
        #[arg(required = true, num_args = 1..)]
        repositories: Vec<PathBuf>,
    },
    /// List registered workspaces.
    List,
}

#[derive(Clone, Eq, PartialEq, ValueEnum)]
enum InspectSubject {
    Relations,
    Revisions,
    Observations,
}

#[derive(clap::Args)]
struct QueryEntity {
    /// Workspace to query.
    #[arg(short, long, default_value = "main")]
    workspace: String,
    /// Canonical semantic entity ID.
    entity: String,
}

#[derive(clap::Args)]
struct QueryPath {
    /// Workspace to query.
    #[arg(short, long, default_value = "main")]
    workspace: String,
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

fn daemon_binary() -> Result<PathBuf, Box<dyn Error>> {
    let binary = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "beholderd.exe"
    } else {
        "beholderd"
    });
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!("beholderd not found next to CLI at {}", binary.display()).into())
    }
}

async fn start_daemon() -> Result<(), Box<dyn Error>> {
    if let Ok(status) = get_status().await {
        println!("already running (pid {})", status.pid);
        return Ok(());
    }
    let state = state_dir()?;
    fs::create_dir_all(&state)?;
    let log_path = state.join("beholderd.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut child = ProcessCommand::new(daemon_binary()?)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    for _ in 0..50 {
        if let Ok(status) = get_status().await {
            println!("started (pid {})", status.pid);
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(
                format!("beholderd exited with {status}; see {}", log_path.display()).into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("beholderd did not become ready; see {}", log_path.display()).into())
}

fn run_daemon() -> Result<(), Box<dyn Error>> {
    let binary = daemon_binary()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(ProcessCommand::new(binary).exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = ProcessCommand::new(binary).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("beholderd exited with {status}").into())
        }
    }
}

async fn wait_for_daemon_lock() -> Result<(), Box<dyn Error>> {
    let path = state_dir()?.join("beholderd.pid");
    for _ in 0..100 {
        if !path.exists() {
            return Ok(());
        }
        let file = fs::File::options().read(true).write(true).open(&path)?;
        if file.try_lock().is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("beholderd did not release its single-instance lock".into())
}

async fn stop_for_service_change() -> Result<(), Box<dyn Error>> {
    if matches!(
        tokio::time::timeout(Duration::from_millis(500), get_status()).await,
        Ok(Ok(_))
    ) {
        tokio::time::timeout(Duration::from_secs(2), stop())
            .await
            .map_err(|_| "timed out stopping beholderd")??;
    }
    wait_for_daemon_lock().await
}

async fn install_daemon_service() -> Result<(), Box<dyn Error>> {
    stop_for_service_change().await?;
    let state = state_dir()?;
    let outcome = service::install(&service::installed_daemon_path()?, &state)?;
    if std::env::var("BEHOLDER_LAUNCHER").as_deref() != Ok("fake") {
        for _ in 0..50 {
            if get_status().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        get_status().await.map_err(|_| {
            format!(
                "installed beholderd did not become ready; see {}",
                state.join("beholderd.log").display()
            )
        })?;
    }
    println!(
        "installed {} ({})",
        outcome.manifest_path.display(),
        if outcome.manifest_changed {
            "updated"
        } else {
            "unchanged"
        }
    );
    Ok(())
}

async fn uninstall_daemon_service() -> Result<(), Box<dyn Error>> {
    stop_for_service_change().await?;
    let outcome = service::uninstall()?;
    println!(
        "{} {}",
        if outcome.manifest_existed {
            "removed"
        } else {
            "already absent"
        },
        outcome.manifest_path.display()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Some(Command::Daemon {
            command: DaemonCommand::Install,
        }) => install_daemon_service().await?,
        Some(Command::Daemon {
            command: DaemonCommand::Uninstall,
        }) => uninstall_daemon_service().await?,
        Some(Command::Daemon {
            command: DaemonCommand::Start,
        }) => start_daemon().await?,
        Some(Command::Daemon {
            command: DaemonCommand::Run,
        }) => run_daemon()?,
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
        Some(Command::IndexRustWorkspace { workspace }) => {
            print_index_result(daemon_index_workspace(workspace).await?)
        }
        Some(Command::Workspace {
            command: WorkspaceCommand::Register { name, repositories },
        }) => println!("{:#?}", register_workspace(name, &repositories).await?),
        Some(Command::Workspace {
            command: WorkspaceCommand::List,
        }) => {
            for workspace in list_workspaces().await? {
                println!("{}\t{}", workspace.name, workspace.repositories.len());
            }
        }
        Some(Command::Cache {
            command: CacheCommand::Clear,
        }) => {
            clear_cache().await?;
            println!("cleared analysis cache");
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
            println!(
                "{:#?}",
                daemon_context(query.workspace, query.entity).await?
            );
        }
        Some(Command::Impact(query)) => {
            println!("{:#?}", daemon_impact(query.workspace, query.entity).await?);
        }
        Some(Command::Dependencies(query)) => {
            println!(
                "{:#?}",
                daemon_dependencies(query.workspace, query.entity).await?
            )
        }
        Some(Command::Trace(query)) => {
            println!(
                "{:#?}",
                daemon_trace(query.workspace, query.from, query.to).await?
            )
        }
        Some(Command::Why(query)) => println!(
            "{:#?}",
            daemon_why(query.workspace, query.from, query.to).await?
        ),
        None => unreachable!("Clap requires a subcommand"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn workspace_smoke() {
        assert!(Cli::try_parse_from(["beholder"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["beholder", "cache", "clear"])
                .unwrap()
                .command,
            Some(Command::Cache {
                command: CacheCommand::Clear
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "daemon", "install"])
                .unwrap()
                .command,
            Some(Command::Daemon {
                command: DaemonCommand::Install
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "daemon", "start"])
                .unwrap()
                .command,
            Some(Command::Daemon {
                command: DaemonCommand::Start
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "daemon", "run"])
                .unwrap()
                .command,
            Some(Command::Daemon {
                command: DaemonCommand::Run
            })
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "workspace",
                "register",
                "main",
                "repo-a",
                "repo-b",
            ])
            .unwrap()
            .command,
            Some(Command::Workspace {
                command: WorkspaceCommand::Register { repositories, .. }
            }) if repositories.len() == 2
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "index-rust-workspace", "main"])
                .unwrap()
                .command,
            Some(Command::IndexRustWorkspace { workspace }) if workspace == "main"
        ));
        assert!(Cli::try_parse_from(["beholder", "index-rust", "src/main.rs"]).is_err());

        let store = SemanticStore::memory().unwrap();

        let result = store
            .trace("main", "web/CheckoutPage", "cache/update_price")
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(format!("{result:?}").contains("CheckoutPage.tsx:12"));

        assert_eq!(
            store
                .impact("main", "rpc/Pricing.GetPrice")
                .unwrap()
                .rows
                .len(),
            4
        );
        assert_eq!(
            store
                .context("main", "rpc/Pricing.GetPrice")
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(
            store
                .dependencies("main", "rpc/Pricing.GetPrice")
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
