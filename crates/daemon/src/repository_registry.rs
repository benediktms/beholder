use beholder_adapters_git::workspace_repository;
use beholder_domain::{LogicalRepository, WorkspaceRepository};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredRepository {
    pub selection: WorkspaceRepository,
}

#[derive(Deserialize, Serialize)]
struct StoredRepository {
    identity: String,
    display_name: String,
    base: PathBuf,
    alternatives: Vec<PathBuf>,
}

pub struct RepositoryRegistry {
    path: PathBuf,
    repositories: BTreeMap<String, RegisteredRepository>,
}

impl RepositoryRegistry {
    pub fn open(path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let repositories = if path.exists() {
            serde_json::from_reader::<_, Vec<StoredRepository>>(File::open(&path)?)?
                .into_iter()
                .map(|stored| {
                    let repository = LogicalRepository {
                        identity: stored.identity,
                    };
                    let registered = RegisteredRepository {
                        selection: WorkspaceRepository {
                            repository: repository.clone(),
                            display_name: stored.display_name,
                            base: stored.base,
                            alternatives: stored.alternatives,
                        },
                    };
                    (registered.selection.repository.identity.clone(), registered)
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        Ok(Self { path, repositories })
    }

    pub fn register(&mut self, path: PathBuf) -> Result<RegisteredRepository, Box<dyn Error>> {
        let path = fs::canonicalize(&path)
            .map_err(|error| format!("invalid repository {}: {error}", path.display()))?;
        if !path.is_dir() {
            return Err(format!("repository is not a directory: {}", path.display()).into());
        }
        let selection = workspace_repository(&path)?;
        self.register_selection(selection)
    }

    fn register_selection(
        &mut self,
        selection: WorkspaceRepository,
    ) -> Result<RegisteredRepository, Box<dyn Error>> {
        let identity = selection.repository.identity.clone();
        let registered = RegisteredRepository { selection };
        let mut repositories = self.repositories.clone();
        repositories.insert(identity, registered.clone());
        self.persist(&repositories)?;
        self.repositories = repositories;
        Ok(registered)
    }

    pub fn get(&self, identity: &str) -> Option<&RegisteredRepository> {
        self.repositories.get(identity)
    }

    fn persist(
        &self,
        repositories: &BTreeMap<String, RegisteredRepository>,
    ) -> Result<(), Box<dyn Error>> {
        let temporary = self.path.with_extension("json.tmp");
        let file = File::create(&temporary)?;
        serde_json::to_writer_pretty(
            &file,
            &repositories
                .values()
                .map(|registered| StoredRepository {
                    identity: registered.selection.repository.identity.clone(),
                    display_name: registered.selection.display_name.clone(),
                    base: registered.selection.base.clone(),
                    alternatives: registered.selection.alternatives.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        file.sync_all()?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

pub fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("repositories.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn registers_and_reloads_a_repository_without_a_workspace() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state = std::env::temp_dir().join(format!("beholder-repository-registry-{unique}"));
        let repository = state.join("repository");
        fs::create_dir_all(&repository).unwrap();

        let path = registry_path(&state);
        let mut registry = RepositoryRegistry::open(path.clone()).unwrap();
        let registered = registry
            .register(repository.canonicalize().unwrap())
            .unwrap();

        let reloaded = RepositoryRegistry::open(path).unwrap();
        let reloaded = reloaded
            .get(&registered.selection.repository.identity)
            .unwrap();
        assert_eq!(reloaded.selection.base, repository.canonicalize().unwrap());
        fs::remove_dir_all(state).unwrap();
    }
}
