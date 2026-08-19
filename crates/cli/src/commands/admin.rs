use super::{BenchmarkArgs, BenchmarkQueryArgs, CacheCommand, InspectSubject, WorkspaceCommand};
use crate::stdout;
use beholder_adapters_mnestic::SemanticStore;
use beholder_daemon_client::{clear_cache, garbage_collect, list_workspaces, register_workspace};
use std::{error::Error, path::Path};

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
        CacheCommand::Gc => {
            let collected = garbage_collect().await?;
            stdout(format_args!(
                "removed {} repository states · {} -> {} bytes",
                collected.repository_states_removed, collected.bytes_before, collected.bytes_after
            ))?;
        }
    }
    Ok(())
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
