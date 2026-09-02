use beholder_dto::DEFAULT_MAX_HOPS;
use beholder_presentation::{OutputMode, RenderOptions};
use clap::{Parser, Subcommand, ValueEnum};
use std::{error::Error, path::PathBuf};

mod admin;
mod daemon;
mod enrich;
mod gui;
mod index;
mod job;
mod query;

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
    /// Start the daemon when needed and launch the desktop graph explorer.
    Gui,
    /// Control the centralized Beholder daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Inspect durable jobs.
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    /// Index one Rust source file.
    IndexRust {
        /// Rust source file to analyze.
        source: PathBuf,
        /// Mnestic SQLite database path.
        #[arg(short, long)]
        database: PathBuf,
    },
    /// Enqueue durable indexing for a registered workspace or repository.
    Index {
        /// Exact registered workspace name or repository identity.
        target: String,
        /// Restrict a repository target to one exact workspace.
        #[arg(short, long)]
        workspace: Option<String>,
    },
    /// Enqueue durable enrichment for a registered repository.
    Enrich {
        /// Exact registered repository identity.
        repository: String,
        /// Restrict the repository target to one exact workspace.
        #[arg(short, long)]
        workspace: Option<String>,
        /// Run only these exact worker IDs.
        #[arg(short, long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// Manage registered workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Manage trusted runtime analyzer plugins.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Manage independently indexed repositories.
    Repository {
        #[command(subcommand)]
        command: RepositoryCommand,
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
    Benchmark(BenchmarkArgs),
    /// Query an existing synthetic benchmark database.
    BenchmarkQuery(BenchmarkQueryArgs),
    /// Show direct incoming and outgoing relationships for an entity.
    Context(QueryEntity),
    /// Show everything transitively affected by an entity.
    Impact(TraversalEntityQuery),
    /// Show transitive dependencies of an entity.
    Dependencies(TraversalEntityQuery),
    /// Find an evidence-backed path between two entities.
    Trace(QueryPath),
    /// Explain why one entity depends on another.
    Why(QueryPath),
}

#[derive(clap::Args)]
struct BenchmarkArgs {
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
}

#[derive(clap::Args)]
struct BenchmarkQueryArgs {
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
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Clear in-memory and persistent analysis caches.
    Clear,
    /// Remove obsolete indexed states and reclaim database space.
    Gc {
        /// Report the current background garbage-collection status.
        #[arg(long)]
        status: bool,
    },
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
enum JobCommand {
    /// List active jobs and the newest terminal history.
    List {
        /// Opaque token returned by the preceding page.
        #[arg(long)]
        page_token: Option<String>,
    },
    /// Show durable lifecycle details for one job.
    Get { id: String },
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
        /// Compiled FileDescriptorSet paths inside the registered repositories.
        #[arg(long = "protobuf-descriptor")]
        protobuf_descriptors: Vec<PathBuf>,
        /// Installed plugin IDs enabled for this workspace.
        #[arg(long = "plugin")]
        plugins: Vec<String>,
    },
    /// Enable an installed plugin for a workspace.
    EnablePlugin { workspace: String, plugin: String },
    /// Disable a plugin for a workspace.
    DisablePlugin { workspace: String, plugin: String },
    /// List registered workspaces.
    List,
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Discover and install a trusted plugin executable. The daemon must be stopped.
    Install { executable: PathBuf },
    /// Replace an installed plugin with a newly discovered executable. The daemon must be stopped.
    Replace { executable: PathBuf },
    /// List installed plugins.
    List,
    /// Remove a plugin from the registry. The daemon must be stopped.
    Remove { id: String },
}

#[derive(Subcommand)]
enum RepositoryCommand {
    /// Register a repository without attaching it to a workspace.
    Register {
        /// Repository root.
        path: PathBuf,
    },
    /// Forget an unreferenced repository and clean up its graph data.
    Delete {
        /// Logical repository identity.
        identity: String,
    },
    /// Show registration and latest completed revision state.
    Show {
        /// Logical repository identity.
        identity: String,
    },
}

#[derive(Clone, Eq, PartialEq, ValueEnum)]
enum InspectSubject {
    GrpcBindings,
    Relations,
    Revisions,
    Observations,
}

#[derive(clap::Args)]
struct QueryEntity {
    /// Workspace to query.
    #[arg(short, long, default_value = "main")]
    workspace: String,
    #[command(flatten)]
    output: OutputArgs,
    /// Canonical semantic entity ID.
    entity: String,
}

#[derive(clap::Args)]
struct TraversalEntityQuery {
    /// Workspace to query.
    #[arg(short, long, default_value = "main")]
    workspace: String,
    /// Limit traversal results to this many relationships.
    #[arg(long, default_value_t = DEFAULT_MAX_HOPS)]
    max_hops: u32,
    #[command(flatten)]
    output: OutputArgs,
    /// Canonical semantic entity ID.
    entity: String,
}

#[derive(clap::Args)]
struct QueryPath {
    /// Workspace to query.
    #[arg(short, long, default_value = "main")]
    workspace: String,
    /// Limit path search to this many relationships.
    #[arg(long, default_value_t = DEFAULT_MAX_HOPS)]
    max_hops: u32,
    #[command(flatten)]
    output: OutputArgs,
    /// Starting semantic entity ID.
    from: String,
    /// Destination semantic entity ID.
    to: String,
}

#[derive(clap::Args, Default)]
#[group(id = "output-format", multiple = false)]
struct OutputArgs {
    /// Emit stable, versioned Beholder JSON.
    #[arg(long, group = "output-format")]
    json: bool,
    /// Emit indented stable, versioned Beholder JSON.
    #[arg(long, group = "output-format")]
    json_pretty: bool,
    /// Emit the full uncollapsed semantic graph and evidence.
    #[arg(long, group = "output-format")]
    raw: bool,
    /// Include test and spec symbols in compact human output.
    #[arg(long)]
    include_tests: bool,
    /// Show individual analysis diagnostics in human-readable output.
    #[arg(short, long)]
    verbose: bool,
}

impl OutputArgs {
    fn mode(&self) -> OutputMode {
        if self.json {
            OutputMode::Json
        } else if self.json_pretty {
            OutputMode::JsonPretty
        } else if self.raw {
            OutputMode::Raw
        } else {
            OutputMode::Human
        }
    }

    fn options(&self) -> RenderOptions {
        RenderOptions {
            mode: self.mode(),
            include_tests: self.include_tests,
            include_diagnostics: self.verbose,
        }
    }
}

pub(super) async fn run() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Some(Command::Gui) => gui::run().await?,
        Some(Command::Daemon { command }) => daemon::run(command).await?,
        Some(Command::Job { command }) => job::run(command).await?,
        Some(Command::IndexRust { source, database }) => {
            index::print_result(index::rust(&source, &database)?)?;
        }
        Some(Command::Index { target, workspace }) => index::submit(target, workspace).await?,
        Some(Command::Enrich {
            repository,
            workspace,
            only,
        }) => enrich::submit(repository, workspace, only).await?,
        Some(Command::Workspace { command }) => admin::workspace(command).await?,
        Some(Command::Plugin { command }) => admin::plugin(command).await?,
        Some(Command::Repository { command }) => admin::repository(command).await?,
        Some(Command::Cache { command }) => admin::cache(command).await?,
        Some(Command::Inspect {
            subject,
            database,
            relation,
        }) => admin::inspect(subject, &database, relation.as_deref())?,
        Some(Command::Benchmark(args)) => admin::benchmark(args)?,
        Some(Command::BenchmarkQuery(args)) => admin::benchmark_query(args)?,
        Some(Command::Context(query)) => query::context(query).await?,
        Some(Command::Impact(query)) => query::impact(query).await?,
        Some(Command::Dependencies(query)) => query::dependencies(query).await?,
        Some(Command::Trace(query)) => query::trace(query).await?,
        Some(Command::Why(query)) => query::why(query).await?,
        None => unreachable!("Clap requires a subcommand"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_adapters_mnestic::SemanticStore;
    use beholder_presentation::OutputMode;
    use std::{env, fs};

    #[test]
    fn workspace_smoke() {
        assert!(Cli::try_parse_from(["beholder"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["beholder", "job", "list"])
                .unwrap()
                .command,
            Some(Command::Job {
                command: JobCommand::List { page_token: None }
            })
        ));
        assert!(Cli::try_parse_from(["beholder", "jobs", "list"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["beholder", "job", "get", "01M0XH82E7NSXFZPQEXS3W2310"])
                .unwrap()
                .command,
            Some(Command::Job {
                command: JobCommand::Get { id }
            }) if id == "01M0XH82E7NSXFZPQEXS3W2310"
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "cache", "clear"])
                .unwrap()
                .command,
            Some(Command::Cache {
                command: CacheCommand::Clear
            })
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "repository",
                "delete",
                "github.com/example/repo"
            ])
            .unwrap()
            .command,
            Some(Command::Repository {
                command: RepositoryCommand::Delete { identity }
            }) if identity == "github.com/example/repo"
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "cache", "gc"])
                .unwrap()
                .command,
            Some(Command::Cache {
                command: CacheCommand::Gc { status: false }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "cache", "gc", "--status"])
                .unwrap()
                .command,
            Some(Command::Cache {
                command: CacheCommand::Gc { status: true }
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
            Cli::try_parse_from(["beholder", "plugin", "install", "./plugin"])
                .unwrap()
                .command,
            Some(Command::Plugin {
                command: PluginCommand::Install { executable }
            }) if executable == std::path::Path::new("./plugin")
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "workspace",
                "enable-plugin",
                "main",
                "example.kafka"
            ])
            .unwrap()
            .command,
            Some(Command::Workspace {
                command: WorkspaceCommand::EnablePlugin { workspace, plugin }
            }) if workspace == "main" && plugin == "example.kafka"
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "workspace",
                "register",
                "main",
                "repo-a",
                "repo-b",
                "--protobuf-descriptor",
                "repo-a/contracts.bin",
            ])
            .unwrap()
            .command,
            Some(Command::Workspace {
                command: WorkspaceCommand::Register { repositories, protobuf_descriptors, .. }
            }) if repositories.len() == 2 && protobuf_descriptors.len() == 1
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "repository", "register", "repo-a"])
                .unwrap()
                .command,
            Some(Command::Repository {
                command: RepositoryCommand::Register { path }
            }) if path == std::path::Path::new("repo-a")
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "index", "github.com/example/repo", "--workspace", "main"])
                .unwrap()
                .command,
            Some(Command::Index { target, workspace: Some(workspace) })
                if target == "github.com/example/repo" && workspace == "main"
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "beholder",
                "enrich",
                "github.com/example/repo",
                "-w",
                "main",
                "-o",
                "rust,plugin"
            ])
            .unwrap()
            .command,
            Some(Command::Enrich { repository, workspace: Some(workspace), only })
                if repository == "github.com/example/repo"
                    && workspace == "main"
                    && only == ["rust", "plugin"]
        ));
        assert!(Cli::try_parse_from(["beholder", "repository", "index", "repo"]).is_err());
        assert!(Cli::try_parse_from(["beholder", "repository", "refresh", "repo"]).is_err());
        assert!(Cli::try_parse_from(["beholder", "reindex-workspace", "main"]).is_err());
        assert!(Cli::try_parse_from(["beholder", "index-rust-workspace", "main"]).is_err());
        assert!(Cli::try_parse_from(["beholder", "index-rust", "src/main.rs"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["beholder", "trace", "--json", "a", "b"])
                .unwrap()
                .command,
            Some(Command::Trace(QueryPath { output, max_hops: DEFAULT_MAX_HOPS, .. }))
                if output.mode() == OutputMode::Json
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "impact", "--max-hops", "3", "a"])
                .unwrap()
                .command,
            Some(Command::Impact(TraversalEntityQuery { max_hops: 3, .. }))
        ));
        assert!(matches!(
            Cli::try_parse_from(["beholder", "impact", "--include-tests", "a"])
                .unwrap()
                .command,
            Some(Command::Impact(TraversalEntityQuery { output, .. })) if output.include_tests
        ));
        assert!(Cli::try_parse_from(["beholder", "trace", "--json", "--raw", "a", "b"]).is_err());

        let store = SemanticStore::memory().unwrap();
        let result = store
            .trace(
                "main",
                "web/CheckoutPage",
                "cache/update_price",
                DEFAULT_MAX_HOPS,
            )
            .unwrap();
        assert_eq!(result.paths.len(), 1);
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.evidence.iter().any(|evidence| {
                    evidence.path.as_deref() == Some("CheckoutPage.tsx")
                        && evidence.line == Some(12)
                }))
        );
        assert_eq!(
            store
                .impact("main", "rpc/Pricing.GetPrice", DEFAULT_MAX_HOPS)
                .unwrap()
                .affected
                .len(),
            4
        );
        assert_eq!(
            store
                .context("main", "rpc/Pricing.GetPrice")
                .unwrap()
                .edges
                .len(),
            2
        );
        assert_eq!(
            store
                .dependencies("main", "rpc/Pricing.GetPrice", DEFAULT_MAX_HOPS)
                .unwrap()
                .dependencies
                .len(),
            3
        );

        let path = env::temp_dir().join(format!("beholder-dogfood-{}.db", std::process::id()));
        let _ = fs::remove_file(&path);
        let source = env::temp_dir().join(format!("beholder-dogfood-{}.rs", std::process::id()));
        fs::write(&source, "fn first() { second(); } fn second() {}").unwrap();
        assert!(index::rust(&source, &path).unwrap().1);
        fs::write(&source, "fn first() {}").unwrap();
        assert!(index::rust(&source, &path).unwrap().1);
        assert_eq!(index::rust(&source, &path).unwrap(), (0, false));
        let indexed = SemanticStore::persistent(&path, false).unwrap();
        let stored_calls = indexed.inspect_observations(Some("calls")).unwrap();
        assert_eq!(stored_calls.rows.len(), 1);
        let caller = stored_calls.rows[0][1].as_str().unwrap();
        assert!(
            indexed
                .dependencies(index::MAIN_VIEW, caller, DEFAULT_MAX_HOPS)
                .unwrap()
                .dependencies
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
