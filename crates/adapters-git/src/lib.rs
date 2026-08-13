use beholder_domain::{GitClone, GitTopology, LogicalRepository, WorkingTree};
use sha2::{Digest, Sha256};
use std::{error::Error, path::Path, process::Command};

pub fn canonical_remote(remote: &str) -> Option<String> {
    let remote = gix_url::parse(remote.trim().into()).ok()?;
    let host = remote.host()?;
    let path = std::str::from_utf8(remote.path.as_ref())
        .ok()?
        .trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| format!("{host}/{path}"))
}

fn output(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

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

pub fn repository_identity(root: &Path) -> Result<String, Box<dyn Error>> {
    let remote = output(root, &["remote", "get-url", "origin"]).or_else(|| {
        let remotes = output(root, &["remote"])?;
        let mut remotes = remotes.lines();
        let only = remotes.next()?;
        remotes
            .next()
            .is_none()
            .then(|| output(root, &["remote", "get-url", only]))
            .flatten()
    });
    if let Some(identity) = remote.as_deref().and_then(canonical_remote) {
        return Ok(identity);
    }
    // ponytail: directory name is the local-only fallback; canonical Git remotes come with registration.
    root.canonicalize()?
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot derive repository identity from {}", root.display()).into())
}

pub fn source_fingerprint(repository: &str, sources: &[(std::path::PathBuf, String)]) -> String {
    let mut digest = Sha256::new();
    digest.update((repository.len() as u64).to_le_bytes());
    digest.update(repository.as_bytes());
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
    let common_directory = String::from_utf8(required_output(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?;
    let common_directory = Path::new(common_directory.trim()).canonicalize()?;
    let worktrees = required_output(root, &["worktree", "list", "--porcelain", "-z"])?;

    Ok(GitTopology {
        repository: LogicalRepository {
            identity: repository_identity(root)?,
        },
        clone: GitClone { common_directory },
        working_trees: parse_worktrees(&worktrees)?,
    })
}

fn parse_worktrees(output: &[u8]) -> Result<Vec<WorkingTree>, Box<dyn Error>> {
    let mut worktrees = Vec::new();
    let mut path = None;
    let mut head = None;
    let mut branch = None;

    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let (Some(path), Some(head)) = (path.take(), head.take()) {
                worktrees.push(WorkingTree {
                    path,
                    head,
                    branch: branch.take(),
                });
            }
            continue;
        }
        let field = std::str::from_utf8(field)?;
        if let Some(value) = field.strip_prefix("worktree ") {
            path = Some(Path::new(value).canonicalize()?);
        } else if let Some(value) = field.strip_prefix("HEAD ") {
            head = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("branch ") {
            branch = Some(
                value
                    .strip_prefix("refs/heads/")
                    .unwrap_or(value)
                    .to_owned(),
            );
        }
    }

    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

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
        assert_eq!(
            main_topology
                .working_trees
                .iter()
                .map(|worktree| worktree.branch.as_deref())
                .collect::<Vec<_>>(),
            [Some("main"), Some("feature")]
        );
        Ok(())
    }
}
