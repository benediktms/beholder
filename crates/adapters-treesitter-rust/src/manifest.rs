use beholder_domain::{
    RepositoryDependencyCandidate, RepositoryDependencyKind, SourceAnalysisError,
};
use beholder_indexing::{AnalysisInputKind, AnalyzerError, WorkspaceSnapshot};
use std::{
    path::{Component, Path, PathBuf},
    str,
};

pub fn rust_analysis_input_kind(path: &Path) -> Option<AnalysisInputKind> {
    if path.extension().is_some_and(|extension| extension == "rs") {
        return Some(AnalysisInputKind::Source);
    }
    match path.file_name().and_then(|name| name.to_str()) {
        Some("Cargo.toml" | "Cargo.lock") => Some(AnalysisInputKind::Dependency),
        Some("rust-toolchain" | "rust-toolchain.toml") => Some(AnalysisInputKind::Toolchain),
        Some("config" | "config.toml")
            if path
                .parent()
                .is_some_and(|parent| parent.ends_with(".cargo")) =>
        {
            Some(AnalysisInputKind::Configuration)
        }
        _ => None,
    }
}

pub(crate) fn cargo_repository_dependencies(
    snapshot: &WorkspaceSnapshot,
) -> Result<Vec<RepositoryDependencyCandidate>, AnalyzerError> {
    let roots = snapshot
        .repositories
        .iter()
        .map(|repository| {
            (
                repository.state.repository.identity.as_str(),
                absolute_lexical(&repository.base),
            )
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for repository in &snapshot.repositories {
        for input in repository.inputs.iter().filter(|input| {
            input
                .path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        }) {
            let Ok(text) = str::from_utf8(&input.content) else {
                continue;
            };
            let Ok(manifest) = toml::from_str::<toml::Value>(text) else {
                continue;
            };
            let manifest_directory = absolute_lexical(
                &repository
                    .base
                    .join(input.path.parent().unwrap_or_else(|| Path::new(""))),
            );
            for dependency in dependency_paths(&manifest) {
                let target = resolve_declared_path(&manifest_directory, &dependency);
                if let Some(owner) = repository_owner(&target, &roots)
                    && owner != repository.state.repository.identity
                {
                    candidates.push(RepositoryDependencyCandidate {
                        from: repository.state.repository.identity.clone(),
                        to: owner.to_owned(),
                        analyzer: "rust".into(),
                        kind: RepositoryDependencyKind::PathDependency,
                        evidence: format!(
                            "{}: path dependency {}",
                            input.path.display(),
                            dependency.display()
                        ),
                    });
                }
            }
            for member in workspace_members(&manifest) {
                if contains_glob(&member) {
                    continue;
                }
                let target = resolve_declared_path(&manifest_directory, &member);
                if let Some(owner) = repository_owner(&target, &roots)
                    && owner != repository.state.repository.identity
                {
                    candidates.push(RepositoryDependencyCandidate {
                        from: repository.state.repository.identity.clone(),
                        to: owner.to_owned(),
                        analyzer: "rust".into(),
                        kind: RepositoryDependencyKind::WorkspaceMember,
                        evidence: format!(
                            "{}: workspace member {}",
                            input.path.display(),
                            member.display()
                        ),
                    });
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

pub fn validate_immutable_rust_inputs(snapshot: &WorkspaceSnapshot) -> Result<(), AnalyzerError> {
    for repository in &snapshot.repositories {
        for input in &repository.inputs {
            let is_manifest = input
                .path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml");
            let is_cargo_configuration = matches!(
                rust_analysis_input_kind(&input.path),
                Some(AnalysisInputKind::Configuration)
            );
            if !is_manifest && !is_cargo_configuration {
                continue;
            }
            let text = str::from_utf8(&input.content)
                .map_err(|error| SourceAnalysisError::from_source(&input.path, Box::new(error)))?;
            let document = toml::from_str::<toml::Value>(text)
                .map_err(|error| SourceAnalysisError::from_source(&input.path, Box::new(error)))?;
            reject_absolute_local_paths(&document, &input.path)?;
        }
    }
    Ok(())
}

fn reject_absolute_local_paths(value: &toml::Value, source: &Path) -> Result<(), AnalyzerError> {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                if key == "path"
                    && value
                        .as_str()
                        .is_some_and(|path| Path::new(path).is_absolute())
                {
                    return Err(format!(
                        "{} declares absolute local path {}; immutable Rust analysis requires relative snapshot paths",
                        source.display(),
                        value.as_str().unwrap_or_default()
                    )
                    .into());
                }
                if key == "paths"
                    && value.as_array().is_some_and(|paths| {
                        paths
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .any(|path| Path::new(path).is_absolute())
                    })
                {
                    return Err(format!(
                        "{} declares an absolute Cargo path override; immutable Rust analysis requires relative snapshot paths",
                        source.display()
                    )
                    .into());
                }
                reject_absolute_local_paths(value, source)?;
            }
        }
        toml::Value::Array(values) => {
            for value in values {
                reject_absolute_local_paths(value, source)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn dependency_paths(manifest: &toml::Value) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(root) = manifest.as_table() {
        for name in [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "replace",
        ] {
            collect_paths_from_dependency_table(root.get(name), &mut paths);
        }
        if let Some(workspace) = root.get("workspace").and_then(toml::Value::as_table) {
            collect_paths_from_dependency_table(workspace.get("dependencies"), &mut paths);
        }
        if let Some(targets) = root.get("target").and_then(toml::Value::as_table) {
            for target in targets.values().filter_map(toml::Value::as_table) {
                for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    collect_paths_from_dependency_table(target.get(name), &mut paths);
                }
            }
        }
        if let Some(patches) = root.get("patch").and_then(toml::Value::as_table) {
            for patch in patches.values() {
                collect_paths_from_dependency_table(Some(patch), &mut paths);
            }
        }
    }
    paths
}

fn collect_paths_from_dependency_table(value: Option<&toml::Value>, paths: &mut Vec<PathBuf>) {
    let Some(table) = value.and_then(toml::Value::as_table) else {
        return;
    };
    for dependency in table.values() {
        if let Some(path) = dependency
            .as_table()
            .and_then(|dependency| dependency.get("path"))
            .and_then(toml::Value::as_str)
        {
            paths.push(PathBuf::from(path));
        }
    }
}

fn workspace_members(manifest: &toml::Value) -> Vec<PathBuf> {
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(PathBuf::from)
        .collect()
}

fn contains_glob(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']'))
}

fn resolve_declared_path(manifest_directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        absolute_lexical(path)
    } else {
        absolute_lexical(&manifest_directory.join(path))
    }
}

fn repository_owner<'a>(path: &Path, roots: &[(&'a str, PathBuf)]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(identity, _)| *identity)
}

pub(crate) fn absolute_lexical(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};
    use std::sync::Arc;

    fn repository(identity: &str, base: &str, manifest: &str) -> RepositorySnapshot {
        RepositorySnapshot {
            base: base.into(),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: identity.into(),
                },
                head: None,
                fingerprint: identity.into(),
            },
            inputs: vec![RepositoryInput {
                path: "Cargo.toml".into(),
                content: Arc::from(manifest.as_bytes()),
                kind: InputKind::Source,
            }],
        }
    }

    #[test]
    fn classifies_rust_semantic_inputs() {
        assert_eq!(
            rust_analysis_input_kind(Path::new("src/lib.rs")),
            Some(AnalysisInputKind::Source)
        );
        assert_eq!(
            rust_analysis_input_kind(Path::new("Cargo.lock")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            rust_analysis_input_kind(Path::new(".cargo/config.toml")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            rust_analysis_input_kind(Path::new("rust-toolchain.toml")),
            Some(AnalysisInputKind::Toolchain)
        );
        assert_eq!(rust_analysis_input_kind(Path::new("README.md")), None);
    }

    #[test]
    fn discovers_cross_repository_path_and_workspace_dependencies() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "example/a",
                    "/workspace/a",
                    "[workspace]\nmembers = [\"../b/member\"]\n[dependencies]\nb = { path = \"../b\" }\n",
                ),
                repository("example/b", "/workspace/b", "[package]\nname = \"b\"\n"),
            ],
        };

        let dependencies = cargo_repository_dependencies(&snapshot).unwrap();

        assert_eq!(dependencies.len(), 2);
        assert!(dependencies.iter().all(|dependency| {
            dependency.from == "example/a"
                && dependency.to == "example/b"
                && dependency.analyzer == "rust"
        }));
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.kind == RepositoryDependencyKind::PathDependency)
        );
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.kind == RepositoryDependencyKind::WorkspaceMember)
        );
    }

    #[test]
    fn rejects_absolute_local_paths_for_immutable_compilation() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![repository(
                "example/a",
                "/workspace/a",
                "[dependencies]\nexternal = { path = \"/live/external\" }\n",
            )],
        };

        let error = validate_immutable_rust_inputs(&snapshot).unwrap_err();

        assert!(error.to_string().contains("absolute local path"));
    }
}
