use beholder_adapters_git::workspace_repository;
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
#[serde(untagged)]
enum StoredRepository {
    Path(PathBuf),
    Registered {
        identity: String,
        display_name: String,
        base: PathBuf,
        alternatives: Vec<PathBuf>,
    },
}

pub struct WorkspaceRegistry {
    path: PathBuf,
    workspaces: BTreeMap<String, Workspace>,
}

impl WorkspaceRegistry {
    pub fn open(path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let workspaces = if path.exists() {
            serde_json::from_reader::<_, Vec<StoredWorkspace>>(File::open(&path)?)?
                .into_iter()
                .map(|stored| {
                    let repositories = stored
                        .repositories
                        .into_iter()
                        .map(|repository| match repository {
                            StoredRepository::Path(path) => {
                                workspace_repository(&path).map_err(|error| error.to_string())
                            }
                            StoredRepository::Registered {
                                identity,
                                display_name,
                                base,
                                alternatives,
                            } => Ok(WorkspaceRepository {
                                repository: LogicalRepository { identity },
                                display_name,
                                base,
                                alternatives,
                            }),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Workspace::new(stored.name, repositories)?
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
                        )
                        .map(|workspace| (workspace.name.clone(), workspace))
                })
                .collect::<Result<_, _>>()?
        } else {
            BTreeMap::new()
        };
        Ok(Self { path, workspaces })
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
                let path = fs::canonicalize(&path)
                    .map_err(|error| format!("invalid repository {}: {error}", path.display()))?;
                if !path.is_dir() {
                    return Err(format!("repository is not a directory: {}", path.display()));
                }
                if path.to_str().is_none() {
                    return Err(format!("repository path is not UTF-8: {}", path.display()));
                }
                workspace_repository(&path).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
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
                        .map(|repository| StoredRepository::Registered {
                            identity: repository.repository.identity.clone(),
                            display_name: repository.display_name.clone(),
                            base: repository.base.clone(),
                            alternatives: repository.alternatives.clone(),
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
