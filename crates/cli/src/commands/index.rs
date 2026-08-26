use super::job::{enum_name, target as job_target};
use crate::stdout;
use beholder_adapters_git::repository_state;
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{FRONTEND_VERSION, observations};
use beholder_daemon_client::{IndexTarget, get_repository, list_workspaces, submit_index};
use beholder_domain::{
    BeholderError, BeholderErrorKind, RepositoryFacts, Workspace, WorkspaceView,
};
use beholder_protocol::v1::JobStatus;
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
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations: observations.clone(),
        }],
        &[],
    )?;
    store.checkpoint()?;
    Ok((observations.len(), true))
}

pub(super) async fn submit(
    target: String,
    workspace_scope: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let workspaces = list_workspaces().await?;
    let repository = match get_repository(target.clone()).await {
        Ok(_) => true,
        Err(error)
            if error
                .downcast_ref::<BeholderError>()
                .is_some_and(|error| error.kind() == BeholderErrorKind::NotFound) =>
        {
            false
        }
        Err(error) => return Err(error),
    };
    let index_target = resolve_target(target, workspace_scope, &workspaces, repository)
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let response = submit_index(index_target).await?;
    let job = response.job.ok_or("daemon returned no submitted job")?;
    stdout(format_args!("enqueued {}\t{}", job.id, job_target(&job)))?;
    for overlap in response.overlapping_jobs {
        stdout(format_args!(
            "overlaps {}\t{}\t{}",
            overlap.id,
            enum_name::<JobStatus>(overlap.status),
            job_target(&overlap),
        ))?;
    }
    Ok(())
}

fn resolve_target(
    target: String,
    workspace_scope: Option<String>,
    workspaces: &[Workspace],
    repository: bool,
) -> Result<IndexTarget, String> {
    if let Some(workspace_scope) = workspace_scope {
        if !repository {
            return Err(format!("repository not registered: {target}"));
        }
        let workspace = workspaces
            .iter()
            .find(|workspace| workspace.name == workspace_scope)
            .ok_or_else(|| format!("workspace not registered: {workspace_scope}"))?;
        if !workspace
            .repositories
            .iter()
            .any(|repository| repository.repository.identity == target)
        {
            return Err(format!(
                "repository {target} is not in workspace {workspace_scope}"
            ));
        }
        Ok(IndexTarget::Repository {
            repository: target,
            workspace_scope: Some(workspace_scope),
        })
    } else {
        let workspace = workspaces.iter().any(|workspace| workspace.name == target);
        match (workspace, repository) {
            (true, true) => Err(format!(
                "index target is ambiguous: {target} is both a workspace and a repository"
            )),
            (true, false) => Ok(IndexTarget::Workspace(target)),
            (false, true) => Ok(IndexTarget::Repository {
                repository: target,
                workspace_scope: None,
            }),
            (false, false) => Err(format!("workspace or repository not registered: {target}")),
        }
    }
}

pub(super) fn print_result((count, published): (usize, bool)) -> Result<(), Box<dyn Error>> {
    stdout(format_args!(
        "{}",
        if published {
            format!("indexed {count} observations")
        } else {
            "unchanged; kept current analysis revision".into()
        }
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, WorkspaceRepository};

    fn workspace(name: &str, repositories: &[&str]) -> Workspace {
        Workspace::new(
            name,
            repositories
                .iter()
                .map(|identity| WorkspaceRepository {
                    repository: LogicalRepository {
                        identity: (*identity).into(),
                    },
                    display_name: (*identity).into(),
                    base: (*identity).into(),
                    alternatives: Vec::new(),
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn resolves_exact_workspace_repository_scope_and_ambiguity() {
        let workspaces = vec![workspace("main", &["repo"]), workspace("repo", &["other"])];
        assert_eq!(
            resolve_target("main".into(), None, &workspaces, false).unwrap(),
            IndexTarget::Workspace("main".into())
        );
        assert_eq!(
            resolve_target("repo".into(), None, &workspaces[..1], true).unwrap(),
            IndexTarget::Repository {
                repository: "repo".into(),
                workspace_scope: None,
            }
        );
        assert_eq!(
            resolve_target("repo".into(), Some("main".into()), &workspaces, true).unwrap(),
            IndexTarget::Repository {
                repository: "repo".into(),
                workspace_scope: Some("main".into()),
            }
        );
        assert!(
            resolve_target("repo".into(), None, &workspaces, true)
                .unwrap_err()
                .contains("ambiguous")
        );
        assert!(
            resolve_target("missing".into(), None, &workspaces, false)
                .unwrap_err()
                .contains("not registered")
        );
        assert!(
            resolve_target("other".into(), Some("main".into()), &workspaces, true)
                .unwrap_err()
                .contains("not in workspace")
        );
    }
}
