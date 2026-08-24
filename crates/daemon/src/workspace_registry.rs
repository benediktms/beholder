use crate::repository_registry::{self, RegisteredRepository, RepositoryRegistry};
use beholder_domain::{LogicalRepository, ProtobufDescriptorSource, Workspace};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
struct StoredRepository {
    identity: String,
}

pub struct WorkspaceRegistry {
    path: PathBuf,
    repositories: RepositoryRegistry,
    workspaces: BTreeMap<String, Workspace>,
}

impl WorkspaceRegistry {
    pub fn open(path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let state_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let repositories = RepositoryRegistry::open(repository_registry::registry_path(state_dir))?;
        let mut workspaces = BTreeMap::new();
        if path.exists() {
            for stored in serde_json::from_reader::<_, Vec<StoredWorkspace>>(File::open(&path)?)? {
                let selections = stored
                    .repositories
                    .into_iter()
                    .map(|repository| {
                        repositories
                            .get(&repository.identity)
                            .map(|registered| registered.selection.clone())
                            .ok_or_else(|| {
                                format!("repository not registered: {}", repository.identity)
                            })
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
            .map(|path| {
                self.repositories
                    .register(path)
                    .map(|entry| entry.selection)
            })
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
        let mut workspaces = self.workspaces.clone();
        workspaces.insert(workspace.name.clone(), workspace.clone());
        self.persist(&workspaces)?;
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

    fn persist(&self, workspaces: &BTreeMap<String, Workspace>) -> Result<(), Box<dyn Error>> {
        let temporary = self.path.with_extension("json.tmp");
        let file = File::create(&temporary)?;
        serde_json::to_writer_pretty(
            &file,
            &workspaces
                .values()
                .map(|workspace| StoredWorkspace {
                    name: workspace.name.clone(),
                    repositories: workspace
                        .repositories
                        .iter()
                        .map(|repository| StoredRepository {
                            identity: repository.repository.identity.clone(),
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
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("workspaces.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn workspace_persists_repository_membership_by_identity() {
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
            .register("main".into(), vec![repository], Vec::new())
            .unwrap();
        let identity = &workspace.repositories[0].repository.identity;

        let stored: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(stored[0]["repositories"][0]["identity"], identity.as_str());
        assert_eq!(stored[0]["repositories"][0].as_object().unwrap().len(), 1);

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
}
