use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{FRONTEND_VERSION, observations};
use beholder_daemon_client::reindex_workspace;
use beholder_domain::{RepositoryFacts, WorkspaceView};
use std::{error::Error, fs, path::Path};

pub(super) const MAIN_VIEW: &str = "main";

pub(super) fn rust(path: &Path, database_path: &Path) -> Result<(usize, bool), Box<dyn Error>> {
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
    store.publish(
        &view,
        &[RepositoryFacts {
            state,
            analysis_identity: format!("rust:{FRONTEND_VERSION}:single-file:1"),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations: observations.clone(),
        }],
        &[],
    )?;
    store.checkpoint()?;
    Ok((observations.len(), true))
}

pub(super) async fn workspace(name: String) -> Result<(), Box<dyn Error>> {
    print_result(reindex_workspace(name).await?);
    Ok(())
}

pub(super) fn print_result((count, published): (usize, bool)) {
    println!(
        "{}",
        if published {
            format!("indexed {count} observations")
        } else {
            "unchanged; kept current analysis revision".into()
        }
    );
}
