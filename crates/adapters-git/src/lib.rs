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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_smoke() {
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
    }
}
