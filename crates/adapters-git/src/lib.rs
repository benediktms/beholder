use beholder_domain::{GitClone, GitTopology, LogicalRepository, RepositoryState, WorkingTree};
use gix::bstr::ByteSlice;
use sha2::{Digest, Sha256};
use std::{error::Error, path::Path, process::Command};

pub fn canonical_remote(remote: &str) -> Option<String> {
    let remote = gix::url::parse(remote.trim()).ok()?;
    canonical_url(&remote)
}

fn canonical_url(remote: &gix::Url) -> Option<String> {
    let host = remote.host()?;
    let path = std::str::from_utf8(remote.path.as_ref())
        .ok()?
        .trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| format!("{host}/{path}"))
}

pub fn repository_identity(root: &Path) -> Result<String, Box<dyn Error>> {
    match gix::discover(root) {
        Ok(repository) => repository_identity_from(&repository, root),
        Err(_) => local_repository_identity(root),
    }
}

fn repository_identity_from(
    repository: &gix::Repository,
    root: &Path,
) -> Result<String, Box<dyn Error>> {
    let remote_name = repository.remote_default_name(gix::remote::Direction::Fetch);
    let identity = remote_name
        .and_then(|name| repository.find_remote(name).ok())
        .and_then(|remote| remote.url(gix::remote::Direction::Fetch).cloned())
        .and_then(|remote| canonical_url(&remote));
    if let Some(identity) = identity {
        return Ok(identity);
    }
    local_repository_identity(root)
}

fn local_repository_identity(root: &Path) -> Result<String, Box<dyn Error>> {
    // ponytail: directory name is the local-only fallback; canonical Git remotes come with registration.
    root.canonicalize()?
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot derive repository identity from {}", root.display()).into())
}

pub fn repository_state(
    root: &Path,
    sources: &[(std::path::PathBuf, String)],
) -> Result<RepositoryState, Box<dyn Error>> {
    let (repository, head) = match gix::discover(root) {
        Ok(git) => {
            let topology = topology_from(git, root)?;
            let root = root.canonicalize()?;
            let selected = topology
                .working_trees
                .iter()
                .filter(|worktree| root.starts_with(&worktree.path))
                .max_by_key(|worktree| worktree.path.components().count())
                .ok_or_else(|| {
                    format!("{} is not inside a discovered working tree", root.display())
                })?;
            (topology.repository.identity, Some(selected.head.clone()))
        }
        Err(_) => (local_repository_identity(root)?, None),
    };

    Ok(RepositoryState {
        fingerprint: state_fingerprint(&repository, head.as_deref(), sources),
        repository: LogicalRepository {
            identity: repository,
        },
        head,
    })
}

fn state_fingerprint(
    repository: &str,
    head: Option<&str>,
    sources: &[(std::path::PathBuf, String)],
) -> String {
    let mut digest = Sha256::new();
    digest.update((repository.len() as u64).to_le_bytes());
    digest.update(repository.as_bytes());
    digest.update([u8::from(head.is_some())]);
    if let Some(head) = head {
        digest.update((head.len() as u64).to_le_bytes());
        digest.update(head.as_bytes());
    }
    for (path, source) in sources {
        let path = path.to_string_lossy();
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((source.len() as u64).to_le_bytes());
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub fn discover_topology(root: &Path) -> Result<GitTopology, Box<dyn Error>> {
    let repository = gix::discover(root)?;
    topology_from(repository, root)
}

fn topology_from(repository: gix::Repository, root: &Path) -> Result<GitTopology, Box<dyn Error>> {
    let main_repository = repository.main_repo()?;
    let common_directory = main_repository.common_dir().canonicalize()?;
    let prunable = prunable_worktrees(root)?;
    let mut working_trees = Vec::new();
    if main_repository.worktree().is_some() {
        working_trees.push(working_tree(&main_repository)?);
    }
    for worktree in main_repository.worktrees()? {
        if prunable.contains(&worktree.base()?) {
            continue;
        }
        working_trees.push(working_tree(&worktree.into_repo()?)?);
    }
    working_trees.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(GitTopology {
        repository: LogicalRepository {
            identity: repository_identity_from(&repository, root)?,
        },
        clone: GitClone { common_directory },
        working_trees,
    })
}

fn prunable_worktrees(root: &Path) -> Result<Vec<std::path::PathBuf>, Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git worktree list failed for {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    let mut paths = Vec::new();
    let mut worktree = None;
    let mut prunable = false;
    for field in output.stdout.split(|byte| *byte == 0) {
        if field.is_empty() {
            if prunable && let Some(path) = worktree.take() {
                paths.push(path);
            }
            worktree = None;
            prunable = false;
        } else if let Some(path) = field.strip_prefix(b"worktree ") {
            worktree = Some(gix::path::from_bstr(path.as_bstr()).into_owned());
        } else if field == b"prunable" || field.starts_with(b"prunable ") {
            prunable = true;
        }
    }
    Ok(paths)
}

fn working_tree(repository: &gix::Repository) -> Result<WorkingTree, Box<dyn Error>> {
    Ok(WorkingTree {
        path: repository
            .worktree()
            .ok_or("repository has no working tree")?
            .base()
            .canonicalize()?,
        head: repository.head_id()?.to_string(),
        branch: repository
            .head_name()?
            .map(|name| name.shorten().to_str().map(str::to_owned))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

    fn required_output(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, Box<dyn Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(output.stdout)
    }

    struct TestDirectory(std::path::PathBuf);

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn workspace_smoke() -> Result<(), Box<dyn Error>> {
        for remote in [
            "git@github.com:company/payments.git",
            "https://github.com/company/payments.git",
            "ssh://git@github.com/company/payments.git",
        ] {
            assert_eq!(
                canonical_remote(remote),
                Some("github.com/company/payments".into())
            );
        }

        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let fixture = TestDirectory(std::env::temp_dir().join(format!(
            "beholder-git-topology-{}-{unique}",
            std::process::id()
        )));
        let main = fixture.0.join("main");
        let linked = fixture.0.join("linked");
        fs::create_dir_all(&main)?;
        required_output(&main, &["init", "-b", "main"])?;
        fs::write(main.join("README.md"), "fixture")?;
        required_output(&main, &["add", "README.md"])?;
        required_output(
            &main,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:company/payments.git",
            ],
        )?;
        let tree = String::from_utf8(required_output(&main, &["write-tree"])?)?;
        let commit = String::from_utf8(required_output(
            &main,
            &[
                "-c",
                "user.name=Beholder Test",
                "-c",
                "user.email=beholder@example.com",
                "commit-tree",
                tree.trim(),
                "-m",
                "fixture",
            ],
        )?)?;
        required_output(&main, &["update-ref", "refs/heads/main", commit.trim()])?;
        required_output(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                linked.to_str().ok_or("non-UTF-8 fixture path")?,
                "HEAD",
            ],
        )?;

        let main_topology = discover_topology(&main)?;
        let linked_topology = discover_topology(&linked)?;
        assert_eq!(main_topology.repository, linked_topology.repository);
        assert_eq!(main_topology.clone, linked_topology.clone);
        assert_eq!(main_topology.working_trees, linked_topology.working_trees);
        assert_eq!(main_topology.working_trees.len(), 2);
        assert_ne!(
            main_topology.working_trees[0].path,
            main_topology.working_trees[1].path
        );
        let branches = main_topology
            .working_trees
            .iter()
            .filter_map(|worktree| worktree.branch.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(branches, ["feature", "main"].into());

        let source = |root: &Path| -> Result<Vec<(std::path::PathBuf, String)>, std::io::Error> {
            Ok(vec![(
                std::path::PathBuf::from("README.md"),
                fs::read_to_string(root.join("README.md"))?,
            )])
        };
        let main_state = repository_state(&main, &source(&main)?)?;
        let linked_state = repository_state(&linked, &source(&linked)?)?;
        assert_eq!(main_state, linked_state);

        fs::write(linked.join("README.md"), "feature fixture")?;
        required_output(&linked, &["add", "README.md"])?;
        let tree = String::from_utf8(required_output(&linked, &["write-tree"])?)?;
        let commit = String::from_utf8(required_output(
            &linked,
            &[
                "-c",
                "user.name=Beholder Test",
                "-c",
                "user.email=beholder@example.com",
                "commit-tree",
                tree.trim(),
                "-p",
                linked_state.head.as_deref().unwrap(),
                "-m",
                "feature",
            ],
        )?)?;
        required_output(
            &linked,
            &["update-ref", "refs/heads/feature", commit.trim()],
        )?;
        let linked_topology = discover_topology(&linked)?;
        let nested = linked.join("src");
        fs::create_dir(&nested)?;
        let feature_state = repository_state(&nested, &source(&linked)?)?;
        assert_ne!(feature_state.head, main_state.head);
        assert_eq!(
            feature_state.head.as_deref(),
            linked_topology
                .working_trees
                .iter()
                .find(|worktree| worktree.path == linked.canonicalize().unwrap())
                .map(|worktree| worktree.head.as_str())
        );

        fs::write(linked.join("README.md"), "dirty fixture")?;
        let dirty_state = repository_state(&linked, &source(&linked)?)?;
        assert_eq!(dirty_state.repository, feature_state.repository);
        assert_eq!(dirty_state.head, feature_state.head);
        assert_ne!(dirty_state.fingerprint, feature_state.fingerprint);

        required_output(
            &main,
            &[
                "worktree",
                "lock",
                "--reason",
                "temporarily unavailable",
                linked.to_str().ok_or("non-UTF-8 fixture path")?,
            ],
        )?;
        fs::remove_dir_all(&linked)?;
        let inaccessible = discover_topology(&main).unwrap_err().to_string();
        assert!(inaccessible.contains("is inaccessible"), "{inaccessible}");

        fs::remove_file(main.join(".git/worktrees/linked/locked"))?;
        let topology = discover_topology(&main)?;
        assert_eq!(topology.working_trees.len(), 1);
        assert_eq!(topology.working_trees[0].path, main.canonicalize()?);
        Ok(())
    }
}
