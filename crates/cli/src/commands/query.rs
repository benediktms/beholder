use super::{QueryEntity, QueryPath, TraversalEntityQuery};
use crate::stdout;
use beholder_daemon_client::{context as fetch_context, dependencies as fetch_dependencies};
use beholder_daemon_client::{impact as fetch_impact, trace as fetch_trace, why as fetch_why};
use beholder_presentation::{
    context as render_context, dependencies as render_dependencies, impact as render_impact,
    trace as render_trace, why as render_why,
};
use std::error::Error;

pub(super) async fn context(query: QueryEntity) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        render_context(
            &fetch_context(query.workspace, query.entity).await?,
            query.output.options(),
        )?
    ))?;
    Ok(())
}

pub(super) async fn impact(query: TraversalEntityQuery) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        render_impact(
            &fetch_impact(query.workspace, query.entity, query.max_hops).await?,
            query.output.options(),
        )?
    ))?;
    Ok(())
}

pub(super) async fn dependencies(query: TraversalEntityQuery) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        render_dependencies(
            &fetch_dependencies(query.workspace, query.entity, query.max_hops).await?,
            query.output.options(),
        )?
    ))?;
    Ok(())
}

pub(super) async fn trace(query: QueryPath) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        render_trace(
            &fetch_trace(query.workspace, query.from, query.to, query.max_hops).await?,
            query.output.options(),
        )?
    ))?;
    Ok(())
}

pub(super) async fn why(query: QueryPath) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        render_why(
            &fetch_why(query.workspace, query.from, query.to, query.max_hops).await?,
            query.output.options(),
        )?
    ))?;
    Ok(())
}
