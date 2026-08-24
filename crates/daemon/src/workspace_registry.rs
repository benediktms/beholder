use crate::repository_registry::{self, RegisteredRepository, RepositoryRegistry};
use beholder_domain::{
    LogicalRepository, ProtobufDescriptorSource, Workspace, WorkspaceRepository,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct StoredWorkspace {
    name: String,
    repositories: Vec<StoredRepository>,
    #[serde(default)]
    protobuf_descriptors: Vec<StoredProtobufDescriptor>,
}

#[derive(Deserialize, Serialize)]
struct StoredProtobufDescriptor {
    repository: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredRepository {
    Path(PathBuf),
    Registered {
        identity: String,
        display_name: String,
        base: PathBuf,
        alternatives: Vec<PathBuf>,
    },
    Selected {
        identity: String,
        base: PathBuf,
    },
    Reference {
        identity: String,
    },
}

#[derive(Serialize)]
struct StoredWorkspaceOutput {
    name: String,
    repositories: Vec<StoredRepositoryOutput>,
    protobuf_descriptors: Vec<StoredProtobufDescriptor>,
}

#[derive(Serialize)]
struct StoredRepositoryOutput {
    identity: String,
    base: PathBuf,
}

pub struct WorkspaceRegistry {
    path: PathBuf,
    repositories: RepositoryRegistry,
    workspaces: BTreeMap<String, Workspace>,
}

impl WorkspaceRegistry {
    pub fn open(path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let state_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let repository_path = repository_registry::registry_path(state_dir);
        let mut repositories = RepositoryRegistry::open(repository_path.clone())?;
        let mut workspaces = BTreeMap::new();
        let mut migrated = false;
        let mut repositories_changed = false;
        if path.exists() {
            for stored in serde_json::from_reader::<_, Vec<StoredWorkspace>>(File::open(&path)?)? {
                let selections = stored
                    .repositories
                    .into_iter()
                    .map(|repository| {
                        let (selection, current) = stored_selection(repository, &repositories)?;
                        migrated |= !current;
                        repositories_changed |= repositories.remember_selection(selection.clone());
                        Ok::<_, Box<dyn Error>>(selection)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let workspace = Workspace::new(stored.name, selections)?
                    .with_protobuf_descriptors(
                        stored
                            .protobuf_descriptors
                            .into_iter()
                            .map(|descriptor| ProtobufDescriptorSource {
                                repository: LogicalRepository {
                                    identity: descriptor.repository,
                                },
                                path: descriptor.path,
                            })
                            .collect(),
                    )?;
                workspaces.insert(workspace.name.clone(), workspace);
            }
        }
        if !workspaces.is_empty() && (migrated || repositories_changed || !repository_path.exists())
        {
            repositories.persist()?;
            persist_workspaces(&path, &workspaces)?;
        }
        let registry = Self {
            path,
            repositories,
            workspaces,
        };
        Ok(registry)
    }

    pub fn register(
        &mut self,
        name: String,
        repositories: Vec<PathBuf>,
        protobuf_descriptors: Vec<PathBuf>,
    ) -> Result<Workspace, Box<dyn Error>> {
        let repositories = repositories
            .into_iter()
            .map(RepositoryRegistry::selection)
            .collect::<Result<Vec<_>, _>>()?;
        let descriptors = protobuf_descriptors
            .into_iter()
            .map(|path| {
                let path = fs::canonicalize(&path).map_err(|error| {
                    format!("invalid protobuf descriptor {}: {error}", path.display())
                })?;
                if !path.is_file() {
                    return Err(format!(
                        "protobuf descriptor is not a file: {}",
                        path.display()
                    ));
                }
                let repository = repositories
                    .iter()
                    .filter(|repository| path.starts_with(&repository.base))
                    .max_by_key(|repository| repository.base.components().count())
                    .ok_or_else(|| {
                        format!(
                            "protobuf descriptor is outside registered repositories: {}",
                            path.display()
                        )
                    })?;
                Ok(ProtobufDescriptorSource {
                    repository: repository.repository.clone(),
                    path,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let workspace =
            Workspace::new(name, repositories)?.with_protobuf_descriptors(descriptors)?;
        let mut registered_repositories = self.repositories.clone();
        for repository in &workspace.repositories {
            registered_repositories.remember_selection(repository.clone());
        }
        let mut workspaces = self.workspaces.clone();
        workspaces.insert(workspace.name.clone(), workspace.clone());
        registered_repositories.persist()?;
        self.persist(&workspaces)?;
        self.repositories = registered_repositories;
        self.workspaces = workspaces;
        Ok(workspace)
    }

    pub fn get(&self, name: &str) -> Option<&Workspace> {
        self.workspaces.get(name)
    }

    pub fn list(&self) -> Vec<Workspace> {
        self.workspaces.values().cloned().collect()
    }

    pub fn register_repository(
        &mut self,
        path: PathBuf,
    ) -> Result<RegisteredRepository, Box<dyn Error>> {
        self.repositories.register(path)
    }

    pub fn repository(&self, identity: &str) -> Option<&RegisteredRepository> {
        self.repositories.get(identity)
    }

    pub fn workspace_referencing_repository(&self, identity: &str) -> Option<&str> {
        self.workspaces
            .values()
            .find(|workspace| {
                workspace
                    .repositories
                    .iter()
                    .any(|repository| repository.repository.identity == identity)
            })
            .map(|workspace| workspace.name.as_str())
    }

    pub fn remove_repository(&mut self, identity: &str) -> Result<bool, Box<dyn Error>> {
        self.repositories.remove(identity)
    }

    fn persist(&self, workspaces: &BTreeMap<String, Workspace>) -> Result<(), Box<dyn Error>> {
        persist_workspaces(&self.path, workspaces)
    }
}

fn stored_selection(
    stored: StoredRepository,
    repositories: &RepositoryRegistry,
) -> Result<(WorkspaceRepository, bool), Box<dyn Error>> {
    match stored {
        StoredRepository::Path(path) => Ok((RepositoryRegistry::selection(path)?, false)),
        StoredRepository::Registered {
            identity,
            display_name,
            base,
            alternatives,
        } => Ok((
            WorkspaceRepository {
                repository: LogicalRepository { identity },
                display_name,
                base,
                alternatives,
            },
            false,
        )),
        StoredRepository::Selected { identity, base } => {
            Ok((select_checkout(repositories, &identity, base)?, true))
        }
        StoredRepository::Reference { identity } => repositories
            .get(&identity)
            .map(|registered| (registered.selection.clone(), false))
            .ok_or_else(|| format!("repository not registered: {identity}").into()),
    }
}

fn select_checkout(
    repositories: &RepositoryRegistry,
    identity: &str,
    base: PathBuf,
) -> Result<WorkspaceRepository, Box<dyn Error>> {
    if let Some(registered) = repositories.get(identity) {
        let mut selection = registered.selection.clone();
        if selection.base == base {
            return Ok(selection);
        }
        if let Some(index) = selection
            .alternatives
            .iter()
            .position(|alternative| alternative == &base)
        {
            selection.alternatives.remove(index);
            selection.alternatives.push(selection.base);
            selection.base = base;
            selection.display_name = selection
                .base
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("repository checkout has no UTF-8 display name")?
                .into();
            return Ok(selection);
        }
    }
    let selection = RepositoryRegistry::selection(base)?;
    if selection.repository.identity != identity {
        return Err(format!(
            "repository checkout identity changed: expected {identity}, found {}",
            selection.repository.identity
        )
        .into());
    }
    Ok(selection)
}

fn persist_workspaces(
    path: &Path,
    workspaces: &BTreeMap<String, Workspace>,
) -> Result<(), Box<dyn Error>> {
    let temporary = path.with_extension("json.tmp");
    let file = File::create(&temporary)?;
    serde_json::to_writer_pretty(
        &file,
        &workspaces
            .values()
            .map(|workspace| StoredWorkspaceOutput {
                name: workspace.name.clone(),
                repositories: workspace
                    .repositories
                    .iter()
                    .map(|repository| StoredRepositoryOutput {
                        identity: repository.repository.identity.clone(),
                        base: repository.base.clone(),
                    })
                    .collect(),
                protobuf_descriptors: workspace
                    .protobuf_descriptors
                    .iter()
                    .map(|descriptor| StoredProtobufDescriptor {
                        repository: descriptor.repository.identity.clone(),
                        path: descriptor.path.clone(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>(),
    )?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("workspaces.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn workspace_persists_repository_selection_by_identity_and_base() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-workspace-registry-{unique}"));
        let repository = state.join("repository");
        fs::create_dir_all(&repository).unwrap();

        let path = registry_path(&state);
        let mut registry = WorkspaceRegistry::open(path.clone()).unwrap();
        let workspace = registry
            .register("main".into(), vec![repository.clone()], Vec::new())
            .unwrap();
        let identity = &workspace.repositories[0].repository.identity;

        let stored: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(stored[0]["repositories"][0]["identity"], identity.as_str());
        assert_eq!(
            stored[0]["repositories"][0]["base"],
            repository
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(stored[0]["repositories"][0].as_object().unwrap().len(), 2);

        let repositories: serde_json::Value = serde_json::from_reader(
            File::open(repository_registry::registry_path(&state)).unwrap(),
        )
        .unwrap();
        assert_eq!(repositories[0]["identity"], identity.as_str());
        assert!(repositories[0].get("base").is_some());

        let reloaded = WorkspaceRegistry::open(path).unwrap();
        assert_eq!(reloaded.get("main"), Some(&workspace));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn migrates_inline_workspace_repositories_into_the_registry() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-workspace-migration-{unique}"));
        let repository = state.join("repository");
        fs::create_dir_all(&repository).unwrap();
        let path = registry_path(&state);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!([{
                "name": "main",
                "repositories": [{
                    "identity": "example/repository",
                    "display_name": "repository",
                    "base": repository,
                    "alternatives": []
                }],
                "protobuf_descriptors": []
            }]))
            .unwrap(),
        )
        .unwrap();

        let registry = WorkspaceRegistry::open(path.clone()).unwrap();
        assert_eq!(
            registry.get("main").unwrap().repositories[0]
                .repository
                .identity,
            "example/repository"
        );
        let repositories: serde_json::Value = serde_json::from_reader(
            File::open(repository_registry::registry_path(&state)).unwrap(),
        )
        .unwrap();
        assert_eq!(repositories[0]["identity"], "example/repository");
        let workspaces: serde_json::Value =
            serde_json::from_reader(File::open(path).unwrap()).unwrap();
        assert_eq!(
            workspaces[0]["repositories"][0].as_object().unwrap().len(),
            2
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn migrates_path_only_workspace_repositories_into_the_registry() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-workspace-path-{unique}"));
        let repository = state.join("repository");
        fs::create_dir_all(&repository).unwrap();
        let path = registry_path(&state);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!([{
                "name": "main",
                "repositories": [repository.clone()],
                "protobuf_descriptors": []
            }]))
            .unwrap(),
        )
        .unwrap();

        let registry = WorkspaceRegistry::open(path).unwrap();
        assert_eq!(
            registry.get("main").unwrap().repositories[0].base,
            repository.canonicalize().unwrap()
        );
        assert!(repository_registry::registry_path(&state).exists());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn preserves_workspace_checkout_selection_across_restart() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-workspace-selection-{unique}"));
        let first = state.join("first");
        let second = state.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(
            repository_registry::registry_path(&state),
            serde_json::to_vec_pretty(&serde_json::json!([{
                "identity": "example/repository",
                "display_name": "first",
                "base": first,
                "alternatives": [second]
            }]))
            .unwrap(),
        )
        .unwrap();
        let path = registry_path(&state);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!([
                {
                    "name": "first",
                    "repositories": [{"identity": "example/repository", "base": first}],
                    "protobuf_descriptors": []
                },
                {
                    "name": "second",
                    "repositories": [{"identity": "example/repository", "base": second}],
                    "protobuf_descriptors": []
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let registry = WorkspaceRegistry::open(path).unwrap();
        assert_eq!(registry.get("first").unwrap().repositories[0].base, first);
        assert_eq!(registry.get("second").unwrap().repositories[0].base, second);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn failed_workspace_registration_does_not_persist_repositories() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-workspace-rollback-{unique}"));
        let repository = state.join("repository");
        fs::create_dir_all(&repository).unwrap();
        let path = registry_path(&state);
        let mut registry = WorkspaceRegistry::open(path.clone()).unwrap();

        assert!(
            registry
                .register(
                    "main".into(),
                    vec![repository, state.join("missing")],
                    Vec::new()
                )
                .is_err()
        );
        assert!(!path.exists());
        assert!(!repository_registry::registry_path(&state).exists());
        fs::remove_dir_all(state).unwrap();
    }
}
