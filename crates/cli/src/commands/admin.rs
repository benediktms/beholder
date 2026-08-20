use super::{BenchmarkArgs, BenchmarkQueryArgs, CacheCommand, InspectSubject, WorkspaceCommand};
use crate::stdout;
use beholder_adapters_mnestic::SemanticStore;
use beholder_daemon_client::{
    clear_cache, garbage_collect, get_garbage_collection_status, list_workspaces,
    register_workspace,
};
use beholder_dto::{GarbageCollectionEvent, GarbageCollectionPhase, GarbageCollectionProgress};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};

const GARBAGE_COLLECTION_HEARTBEAT: Duration = Duration::from_secs(10);

pub(super) async fn workspace(command: WorkspaceCommand) -> Result<(), Box<dyn Error>> {
    match command {
        WorkspaceCommand::Register {
            name,
            repositories,
            protobuf_descriptors,
        } => stdout(format_args!(
            "{:#?}",
            register_workspace(name, &repositories, &protobuf_descriptors).await?
        ))?,
        WorkspaceCommand::List => {
            for workspace in list_workspaces().await? {
                stdout(format_args!(
                    "{}\t{}",
                    workspace.name,
                    workspace.repositories.len()
                ))?;
            }
        }
    }
    Ok(())
}

pub(super) async fn cache(command: CacheCommand) -> Result<(), Box<dyn Error>> {
    match command {
        CacheCommand::Clear => {
            clear_cache().await?;
            stdout(format_args!("cleared analysis cache"))?;
        }
        CacheCommand::Gc { status: true } => {
            let status = get_garbage_collection_status().await?;
            stdout(format_args!(
                "{} · {} repository states queued",
                if status.running { "running" } else { "idle" },
                status.repository_states_queued,
            ))?;
            if let Some(progress) = status.progress {
                stdout(format_args!(
                    "{}",
                    garbage_collection_progress_action(&progress)
                ))?;
            }
        }
        CacheCommand::Gc { status: false } => {
            let mut events = garbage_collect().await?;
            let mut current_action = None;
            let mut phase_started = Instant::now();
            loop {
                let event = tokio::select! {
                    event = events.message() => event?,
                    () = tokio::time::sleep(GARBAGE_COLLECTION_HEARTBEAT), if current_action.is_some() => {
                        let elapsed = phase_started.elapsed().as_secs();
                        let action = current_action.as_deref().expect("heartbeat requires a garbage collection action");
                        eprintln!(
                            "still {action}; {}m {:02}s elapsed...",
                            elapsed / 60,
                            elapsed % 60,
                        );
                        continue;
                    }
                };
                let Some(event) = event else {
                    break;
                };
                match event {
                    GarbageCollectionEvent::Progress(progress) => {
                        phase_started = Instant::now();
                        let action = garbage_collection_progress_action(&progress);
                        eprintln!("{action}...");
                        current_action = Some(action);
                    }
                    GarbageCollectionEvent::Completed(collected) => {
                        current_action = None;
                        stdout(format_args!(
                            "queued {} obsolete repository states for background cleanup",
                            collected.repository_states_queued,
                        ))?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn garbage_collection_action(phase: GarbageCollectionPhase) -> &'static str {
    match phase {
        GarbageCollectionPhase::ClaimingObsoleteStates => "claiming obsolete repository states",
        GarbageCollectionPhase::SweepingObsoleteStates => "sweeping obsolete repository states",
    }
}

fn garbage_collection_progress_action(progress: &GarbageCollectionProgress) -> String {
    let Some(step) = progress.step.as_deref() else {
        return garbage_collection_action(progress.phase).into();
    };
    let target = match (
        progress.completed_rows,
        progress.rows,
        progress.stale_states,
        progress.repositories,
    ) {
        (Some(completed), Some(rows), Some(states), Some(repositories)) => format!(
            "{step} across {states} repository states in {repositories} repositories \
             ({completed}/{rows} rows removed)"
        ),
        (Some(completed), None, Some(states), Some(repositories)) => format!(
            "{step} across {states} repository states in {repositories} repositories \
             ({completed} rows removed)"
        ),
        _ => step.into(),
    };
    format!(
        "removing {target} (step {}/{})",
        progress.completed_steps + 1,
        progress.total_steps,
    )
}

pub(super) fn inspect(
    subject: InspectSubject,
    database: &Path,
    relation: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    if relation.is_some() && subject != InspectSubject::Observations {
        return Err("--relation is only valid for observations".into());
    }
    let store = SemanticStore::persistent(database, false)?;
    let result = match subject {
        InspectSubject::GrpcBindings => store.inspect_grpc_bindings()?,
        InspectSubject::Relations => store.inspect_relations()?,
        InspectSubject::Revisions => store.inspect_revisions()?,
        InspectSubject::Observations => store.inspect_observations(relation)?,
    };
    stdout(format_args!("{result:#?}"))?;
    Ok(())
}

pub(super) fn benchmark(args: BenchmarkArgs) -> Result<(), Box<dyn Error>> {
    let store = SemanticStore::benchmark_store(
        &args.storage,
        database_argument(args.database.as_deref())?,
    )?;
    stdout(format_args!(
        "{}",
        store.benchmark(&args.topology, args.entities, args.fanout, args.depth)?
    ))?;
    Ok(())
}

pub(super) fn benchmark_query(args: BenchmarkQueryArgs) -> Result<(), Box<dyn Error>> {
    let store =
        SemanticStore::benchmark_store(&args.storage, database_argument(Some(&args.database))?)?;
    stdout(format_args!(
        "{}",
        store.benchmark_queries(&args.topology, args.entities, args.depth)
    ))?;
    Ok(())
}

fn database_argument(database: Option<&Path>) -> Result<Option<&str>, Box<dyn Error>> {
    database
        .map(|path| {
            path.to_str()
                .ok_or_else(|| "database path is not UTF-8".into())
        })
        .transpose()
}
