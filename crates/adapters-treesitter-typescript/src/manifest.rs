use beholder_domain::{RepositoryDependencyCandidate, RepositoryDependencyKind};
use beholder_indexing::{AnalysisInputKind, AnalyzerError, RepositorySnapshot, WorkspaceSnapshot};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectConfig {
    #[serde(default)]
    extends: OneOrMany,
    #[serde(default)]
    references: Vec<ProjectReference>,
}

#[derive(Clone, Deserialize)]
struct ProjectReference {
    path: PathBuf,
}

#[derive(Clone, Default, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
    #[default]
    None,
}

impl OneOrMany {
    fn values(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
            Self::None => &[],
        }
    }
}

pub fn typescript_analysis_input_kind(path: &Path) -> Option<AnalysisInputKind> {
    if crate::SourceLanguage::from_path(path).is_some()
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "graphql" | "gql"))
    {
        return Some(AnalysisInputKind::Source);
    }
    let name = path.file_name().and_then(|name| name.to_str())?;
    if is_project_config_name(name) {
        return Some(AnalysisInputKind::Configuration);
    }
    matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "pnpm-workspace.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "deno.lock"
    )
    .then_some(AnalysisInputKind::Dependency)
}

fn is_project_config_name(name: &str) -> bool {
    ((name.starts_with("tsconfig.") || name.starts_with("jsconfig.")) && name.ends_with(".json"))
        || matches!(name, "tsconfig.json" | "jsconfig.json")
}

/// Returns each captured project configuration and its immutable local
/// inheritance chain. Paths not present in the repository snapshot are never
/// read from the live filesystem.
pub fn typescript_config_chains(
    repository: &RepositorySnapshot,
) -> BTreeMap<PathBuf, Vec<PathBuf>> {
    let configs = project_configs(repository);
    configs
        .keys()
        .map(|path| {
            let mut chain = BTreeSet::new();
            collect_config_chain(path, &configs, &mut BTreeSet::new(), &mut chain);
            (path.clone(), chain.into_iter().collect())
        })
        .collect()
}

pub(crate) fn typescript_repository_dependencies(
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
    let packages = package_owners(snapshot);
    let mut candidates = Vec::new();

    for repository in &snapshot.repositories {
        let repository_id = &repository.state.repository.identity;
        for (path, config) in project_configs(repository) {
            let config_directory = absolute_lexical(
                &repository
                    .base
                    .join(path.parent().unwrap_or_else(|| Path::new(""))),
            );
            for reference in config.references {
                let target = resolve_declared_path(&config_directory, &reference.path);
                add_path_candidate(
                    &mut candidates,
                    repository_id,
                    &target,
                    &roots,
                    RepositoryDependencyKind::ProjectReference,
                    format!(
                        "{}: project reference {}",
                        path.display(),
                        reference.path.display()
                    ),
                );
            }
            for extended in config.extends.values().iter().filter(|path| is_local(path)) {
                let target = resolve_declared_path(&config_directory, Path::new(extended));
                add_path_candidate(
                    &mut candidates,
                    repository_id,
                    &target,
                    &roots,
                    RepositoryDependencyKind::CompilerDiscovered,
                    format!("{}: extends {extended}", path.display()),
                );
            }
        }

        for input in repository.inputs.iter().filter(|input| {
            input.path.file_name().and_then(|name| name.to_str()) == Some("package.json")
        }) {
            let Ok(manifest) = serde_json::from_slice::<Value>(&input.content) else {
                continue;
            };
            let manifest_directory = absolute_lexical(
                &repository
                    .base
                    .join(input.path.parent().unwrap_or_else(|| Path::new(""))),
            );
            for workspace in workspace_patterns(&manifest) {
                if workspace.starts_with('!') {
                    continue;
                }
                let pattern = resolve_declared_path(&manifest_directory, Path::new(workspace));
                for (owner, root) in &roots {
                    if *owner != repository_id && path_pattern_matches(&pattern, root) {
                        candidates.push(RepositoryDependencyCandidate {
                            from: repository_id.clone(),
                            to: (*owner).to_owned(),
                            analyzer: "typescript".into(),
                            kind: RepositoryDependencyKind::WorkspaceMember,
                            evidence: format!(
                                "{}: workspace member {workspace}",
                                input.path.display()
                            ),
                        });
                    }
                }
            }
            for (section, name, specification) in dependency_specifications(&manifest) {
                if specification.starts_with("workspace:") {
                    if let Some(owner) = packages.get(name)
                        && owner != repository_id
                    {
                        candidates.push(RepositoryDependencyCandidate {
                            from: repository_id.clone(),
                            to: owner.clone(),
                            analyzer: "typescript".into(),
                            kind: RepositoryDependencyKind::WorkspaceMember,
                            evidence: format!(
                                "{}: {section} {name} = {specification}",
                                input.path.display()
                            ),
                        });
                    }
                    continue;
                }
                let Some(path) = local_package_path(specification) else {
                    continue;
                };
                let target = resolve_declared_path(&manifest_directory, Path::new(path));
                add_path_candidate(
                    &mut candidates,
                    repository_id,
                    &target,
                    &roots,
                    RepositoryDependencyKind::PathDependency,
                    format!(
                        "{}: {section} {name} = {specification}",
                        input.path.display()
                    ),
                );
            }
        }
    }

    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn project_configs(repository: &RepositorySnapshot) -> BTreeMap<PathBuf, ProjectConfig> {
    repository
        .inputs
        .iter()
        .filter(|input| {
            input
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_project_config_name)
        })
        .filter_map(|input| {
            jsonc_parser::parse_to_serde_value::<ProjectConfig>(
                std::str::from_utf8(&input.content).ok()?,
                &Default::default(),
            )
            .ok()
            .map(|config| (normalized(&input.path), config))
        })
        .collect()
}

fn collect_config_chain(
    path: &Path,
    configs: &BTreeMap<PathBuf, ProjectConfig>,
    visiting: &mut BTreeSet<PathBuf>,
    chain: &mut BTreeSet<PathBuf>,
) {
    let path = normalized(path);
    if !visiting.insert(path.clone()) || !chain.insert(path.clone()) {
        return;
    }
    let Some(config) = configs.get(&path) else {
        return;
    };
    let directory = path.parent().unwrap_or_else(|| Path::new(""));
    for extended in config.extends.values().iter().filter(|path| is_local(path)) {
        let mut parent = normalized(&directory.join(extended));
        if parent.extension().is_none() {
            parent.set_extension("json");
        }
        if configs.contains_key(&parent) {
            collect_config_chain(&parent, configs, visiting, chain);
        }
    }
    visiting.remove(&path);
}

fn package_owners(snapshot: &WorkspaceSnapshot) -> BTreeMap<String, String> {
    let mut packages = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for repository in &snapshot.repositories {
        for input in repository.inputs.iter().filter(|input| {
            input.path.file_name().and_then(|name| name.to_str()) == Some("package.json")
        }) {
            let Ok(manifest) = serde_json::from_slice::<Value>(&input.content) else {
                continue;
            };
            let Some(name) = manifest.get("name").and_then(Value::as_str) else {
                continue;
            };
            if ambiguous.contains(name) {
                continue;
            }
            let owner = repository.state.repository.identity.clone();
            if packages.insert(name.to_owned(), owner).is_some() {
                packages.remove(name);
                ambiguous.insert(name.to_owned());
            }
        }
    }
    packages
}

fn workspace_patterns(manifest: &Value) -> Vec<&str> {
    let Some(workspaces) = manifest.get("workspaces") else {
        return Vec::new();
    };
    let values = workspaces
        .as_array()
        .or_else(|| workspaces.get("packages").and_then(Value::as_array));
    values
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn dependency_specifications(manifest: &Value) -> Vec<(&str, &str, &str)> {
    let mut dependencies = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(values) = manifest.get(section).and_then(Value::as_object) else {
            continue;
        };
        dependencies.extend(values.iter().filter_map(|(name, value)| {
            value
                .as_str()
                .map(|specification| (section, name.as_str(), specification))
        }));
    }
    dependencies
}

fn local_package_path(specification: &str) -> Option<&str> {
    for protocol in ["file:", "link:", "portal:"] {
        if let Some(path) = specification.strip_prefix(protocol) {
            return Some(path);
        }
    }
    is_local(specification).then_some(specification)
}

fn is_local(path: &str) -> bool {
    path.starts_with("./") || path.starts_with("../") || Path::new(path).is_absolute()
}

fn add_path_candidate(
    candidates: &mut Vec<RepositoryDependencyCandidate>,
    repository: &str,
    target: &Path,
    roots: &[(&str, PathBuf)],
    kind: RepositoryDependencyKind,
    evidence: String,
) {
    if let Some(owner) = repository_owner(target, roots)
        && owner != repository
    {
        candidates.push(RepositoryDependencyCandidate {
            from: repository.to_owned(),
            to: owner.to_owned(),
            analyzer: "typescript".into(),
            kind,
            evidence,
        });
    }
}

fn path_pattern_matches(pattern: &Path, path: &Path) -> bool {
    let pattern = pattern
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let path = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[String], path: &[String]) -> bool {
    match (pattern.split_first(), path.split_first()) {
        (None, None) => true,
        (Some((segment, rest)), _) if segment == "**" => {
            match_segments(rest, path)
                || path
                    .split_first()
                    .is_some_and(|(_, tail)| match_segments(pattern, tail))
        }
        (Some((segment, rest)), Some((value, tail))) if segment_matches(segment, value) => {
            match_segments(rest, tail)
        }
        _ => false,
    }
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut remainder = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 {
            let Some(rest) = remainder.strip_prefix(part) else {
                return false;
            };
            remainder = rest;
        } else if index + 1 == parts.len() {
            return remainder.ends_with(part);
        } else if let Some(position) = remainder.find(part) {
            remainder = &remainder[position + part.len()..];
        } else {
            return false;
        }
    }
    parts.last().is_some_and(|part| part.is_empty()) || remainder.is_empty()
}

fn resolve_declared_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        absolute_lexical(path)
    } else {
        absolute_lexical(&base.join(path))
    }
}

fn repository_owner<'a>(path: &Path, roots: &[(&'a str, PathBuf)]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(identity, _)| *identity)
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalized(&path)
}

fn normalized(path: &Path) -> PathBuf {
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
    use beholder_indexing::{InputKind, RepositoryInput};
    use std::sync::Arc;

    fn repository(identity: &str, base: &str, inputs: &[(&str, &str)]) -> RepositorySnapshot {
        RepositorySnapshot {
            base: base.into(),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: identity.into(),
                },
                head: None,
                fingerprint: identity.into(),
            },
            inputs: inputs
                .iter()
                .map(|(path, content)| RepositoryInput {
                    path: (*path).into(),
                    content: Arc::from(content.as_bytes()),
                    kind: InputKind::Source,
                })
                .collect(),
        }
    }

    #[test]
    fn classifies_typescript_semantic_inputs() {
        assert_eq!(
            typescript_analysis_input_kind(Path::new("src/app.ts")),
            Some(AnalysisInputKind::Source)
        );
        assert_eq!(
            typescript_analysis_input_kind(Path::new("tsconfig.base.json")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            typescript_analysis_input_kind(Path::new("pnpm-lock.yaml")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            typescript_analysis_input_kind(Path::new("unrelated.json")),
            None
        );
        assert_eq!(typescript_analysis_input_kind(Path::new(".env")), None);
    }

    #[test]
    fn declares_finite_deterministic_config_chains() {
        let repository = repository(
            "example/app",
            "/workspace/app",
            &[
                (
                    "tsconfig.json",
                    r#"{ "extends": "./tsconfig.base", "references": [{ "path": "../shared" }] }"#,
                ),
                (
                    "tsconfig.base.json",
                    r#"{ "extends": "./tsconfig.json", "compilerOptions": { "moduleResolution": "bundler" } }"#,
                ),
            ],
        );

        let chains = typescript_config_chains(&repository);

        assert_eq!(
            chains[Path::new("tsconfig.json")],
            vec![
                PathBuf::from("tsconfig.base.json"),
                PathBuf::from("tsconfig.json")
            ]
        );
        assert_eq!(
            chains[Path::new("tsconfig.base.json")],
            vec![
                PathBuf::from("tsconfig.base.json"),
                PathBuf::from("tsconfig.json")
            ]
        );
    }

    #[test]
    fn discovers_project_references_and_local_package_dependencies() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "example/app",
                    "/workspace/app",
                    &[
                        (
                            "tsconfig.json",
                            r#"{ "references": [{ "path": "../shared" }] }"#,
                        ),
                        (
                            "package.json",
                            r#"{
                                "name": "@example/app",
                                "dependencies": {
                                    "@example/shared": "workspace:*",
                                    "@example/tools": "file:../tools"
                                }
                            }"#,
                        ),
                    ],
                ),
                repository(
                    "example/shared",
                    "/workspace/shared",
                    &[("package.json", r#"{ "name": "@example/shared" }"#)],
                ),
                repository(
                    "example/tools",
                    "/workspace/tools",
                    &[("package.json", r#"{ "name": "@example/tools" }"#)],
                ),
            ],
        };

        let dependencies = typescript_repository_dependencies(&snapshot).unwrap();

        assert_eq!(dependencies.len(), 3);
        assert!(dependencies.iter().all(|dependency| {
            dependency.from == "example/app" && dependency.analyzer == "typescript"
        }));
        assert!(dependencies.iter().any(|dependency| {
            dependency.to == "example/shared"
                && dependency.kind == RepositoryDependencyKind::ProjectReference
        }));
        assert!(dependencies.iter().any(|dependency| {
            dependency.to == "example/shared"
                && dependency.kind == RepositoryDependencyKind::WorkspaceMember
        }));
        assert!(dependencies.iter().any(|dependency| {
            dependency.to == "example/tools"
                && dependency.kind == RepositoryDependencyKind::PathDependency
        }));
    }

    #[test]
    fn discovers_globbed_workspace_members_and_deduplicates_evidence() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "example/root",
                    "/workspace/root",
                    &[(
                        "package.json",
                        r#"{ "workspaces": ["packages/*", "packages/*"] }"#,
                    )],
                ),
                repository(
                    "example/package",
                    "/workspace/root/packages/api",
                    &[("package.json", r#"{ "name": "@example/api" }"#)],
                ),
            ],
        };

        let dependencies = typescript_repository_dependencies(&snapshot).unwrap();

        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].to, "example/package");
        assert_eq!(
            dependencies[0].kind,
            RepositoryDependencyKind::WorkspaceMember
        );
    }

    #[test]
    fn ignores_remote_ambiguous_and_unrelated_package_evidence() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "example/app",
                    "/workspace/app",
                    &[(
                        "package.json",
                        r#"{
                            "dependencies": {
                                "remote": "^1.0.0",
                                "duplicate": "workspace:*"
                            }
                        }"#,
                    )],
                ),
                repository(
                    "example/one",
                    "/workspace/one",
                    &[("package.json", r#"{ "name": "duplicate" }"#)],
                ),
                repository(
                    "example/two",
                    "/workspace/two",
                    &[("package.json", r#"{ "name": "duplicate" }"#)],
                ),
            ],
        };

        assert!(
            typescript_repository_dependencies(&snapshot)
                .unwrap()
                .is_empty()
        );
    }
}
