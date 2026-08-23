use beholder_domain::{RepositoryDependencyCandidate, RepositoryDependencyKind};
use beholder_indexing::{AnalysisInputKind, AnalyzerError, WorkspaceSnapshot};
use std::path::{Component, Path, PathBuf};
use tree_sitter::{Node, Parser};

pub fn elixir_analysis_input_kind(path: &Path) -> Option<AnalysisInputKind> {
    let file_name = path.file_name().and_then(|name| name.to_str());
    if file_name == Some("mix.exs") || file_name == Some("mix.lock") {
        return Some(AnalysisInputKind::Dependency);
    }
    if file_name == Some("runtime.exs") && under_config_directory(path) {
        return None;
    }
    if is_compile_configuration(path) {
        return Some(AnalysisInputKind::Configuration);
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "ex" | "exs"))
        .then_some(AnalysisInputKind::Source)
}

fn is_compile_configuration(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("exs")
        && under_config_directory(path)
        && path.file_name().and_then(|name| name.to_str()) != Some("runtime.exs")
}

fn under_config_directory(path: &Path) -> bool {
    path.ancestors()
        .skip(1)
        .any(|parent| parent.ends_with("config"))
}

pub(crate) fn mix_repository_dependencies(
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
                .is_some_and(|name| name == "mix.exs")
        }) {
            let Ok(source) = std::str::from_utf8(&input.content) else {
                continue;
            };
            let project_base = absolute_lexical(
                &repository
                    .base
                    .join(input.path.parent().unwrap_or_else(|| Path::new(""))),
            );
            for dependency in keyword_paths(source, "path") {
                let target = resolve_declared_path(&project_base, &dependency);
                if let Some(owner) = repository_owner(&target, &roots)
                    && owner != repository.state.repository.identity
                {
                    candidates.push(RepositoryDependencyCandidate {
                        from: repository.state.repository.identity.clone(),
                        to: owner.to_owned(),
                        analyzer: "elixir".into(),
                        kind: RepositoryDependencyKind::PathDependency,
                        evidence: format!(
                            "{}: path dependency {}",
                            input.path.display(),
                            dependency.display()
                        ),
                    });
                }
            }
            for apps_path in keyword_paths(source, "apps_path") {
                let apps_root = resolve_declared_path(&project_base, &apps_path);
                for (member, base) in &roots {
                    if *member != repository.state.repository.identity
                        && base.starts_with(&apps_root)
                    {
                        candidates.push(RepositoryDependencyCandidate {
                            from: repository.state.repository.identity.clone(),
                            to: (*member).to_owned(),
                            analyzer: "elixir".into(),
                            kind: RepositoryDependencyKind::WorkspaceMember,
                            evidence: format!(
                                "{}: umbrella member under {}",
                                input.path.display(),
                                apps_path.display()
                            ),
                        });
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn keyword_paths(source: &str, keyword: &str) -> Vec<PathBuf> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_elixir::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    collect_keyword_paths(tree.root_node(), source.as_bytes(), keyword, &mut paths);
    paths
}

fn collect_keyword_paths(node: Node<'_>, source: &[u8], keyword: &str, paths: &mut Vec<PathBuf>) {
    if node.kind() == "keywords" {
        let mut cursor = node.walk();
        for pair in node.named_children(&mut cursor) {
            let key = pair
                .child_by_field_name("key")
                .and_then(|key| key.utf8_text(source).ok())
                .map(|key| key.trim().trim_end_matches(':'));
            if key != Some(keyword) {
                continue;
            }
            let Some(value) = pair
                .child_by_field_name("value")
                .and_then(|value| value.utf8_text(source).ok())
                .and_then(literal_string)
            else {
                continue;
            };
            paths.push(PathBuf::from(value));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_keyword_paths(child, source, keyword, paths);
    }
}

fn literal_string(value: &str) -> Option<&str> {
    let quote = value.chars().next().filter(|quote| matches!(quote, '\'' | '"'))?;
    let value = value.strip_prefix(quote)?.strip_suffix(quote)?;
    (!value.contains("#{")).then_some(value)
}

fn resolve_declared_path(project_base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        absolute_lexical(path)
    } else {
        absolute_lexical(&project_base.join(path))
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
                path: "mix.exs".into(),
                content: Arc::from(manifest.as_bytes()),
                kind: InputKind::Source,
            }],
        }
    }

    #[test]
    fn classifies_elixir_semantic_inputs() {
        assert_eq!(
            elixir_analysis_input_kind(Path::new("lib/app.ex")),
            Some(AnalysisInputKind::Source)
        );
        assert_eq!(
            elixir_analysis_input_kind(Path::new("mix.lock")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            elixir_analysis_input_kind(Path::new("config/config.exs")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            elixir_analysis_input_kind(Path::new("apps/api/config/nested/dev.exs")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            elixir_analysis_input_kind(Path::new("config/runtime.exs")),
            None
        );
    }

    #[test]
    fn discovers_mix_path_and_umbrella_dependencies() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "example/root",
                    "/workspace/root",
                    "def project, do: [apps_path: \"../services\"]\ndefp deps, do: [{:shared, path: \"../shared\"}]\n",
                ),
                repository("example/service", "/workspace/services/api", "def project, do: []\n"),
                repository("example/shared", "/workspace/shared", "def project, do: []\n"),
            ],
        };

        let dependencies = mix_repository_dependencies(&snapshot).unwrap();

        assert_eq!(dependencies.len(), 2);
        assert!(dependencies.iter().any(|dependency| {
            dependency.to == "example/service"
                && dependency.kind == RepositoryDependencyKind::WorkspaceMember
        }));
        assert!(dependencies.iter().any(|dependency| {
            dependency.to == "example/shared"
                && dependency.kind == RepositoryDependencyKind::PathDependency
        }));
    }

    #[test]
    fn ignores_commented_dynamic_and_unrelated_mix_paths() {
        let source = r#"
# defp deps, do: [{:commented, path: "../commented"}]
def project, do: [some_path: "../unrelated"]
defp deps, do: [{:dynamic, path: dependency_path()}]
"#;

        assert!(keyword_paths(source, "path").is_empty());
        assert!(keyword_paths(source, "apps_path").is_empty());
    }
}
