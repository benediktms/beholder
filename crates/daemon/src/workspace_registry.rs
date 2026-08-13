use beholder_domain::Workspace;
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
    repositories: Vec<PathBuf>,
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
                    Workspace::new(stored.name, stored.repositories)
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
                Ok(path)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let workspace = Workspace::new(name, repositories)?;
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
                    repositories: workspace.repositories.clone(),
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
