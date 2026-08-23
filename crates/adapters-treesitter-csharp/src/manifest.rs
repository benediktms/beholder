use beholder_domain::{RepositoryDependencyCandidate, RepositoryDependencyKind};
use beholder_indexing::{AnalysisInputKind, AnalyzerError, WorkspaceSnapshot};
use quick_xml::{Reader, events::Event};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub fn csharp_analysis_input_kind(path: &Path) -> Option<AnalysisInputKind> {
    let name = path.file_name().and_then(|name| name.to_str())?;
    if path.extension().is_some_and(|extension| extension == "cs")
        || name.ends_with(".prefab")
        || name.ends_with(".cs.meta")
        || name.ends_with(".prefab.meta")
    {
        return Some(AnalysisInputKind::Source);
    }
    if name == "global.json" || name == "ProjectVersion.txt" && under(path, "ProjectSettings") {
        return Some(AnalysisInputKind::Toolchain);
    }
    if name.ends_with(".csproj")
        || name.ends_with(".sln")
        || name.ends_with(".slnx")
        || name.ends_with(".asmdef")
        || name.ends_with(".asmref")
        || name.ends_with(".asmdef.meta")
        || matches!(name, "Directory.Packages.props" | "packages.lock.json")
        || matches!(name, "manifest.json" | "packages-lock.json") && under(path, "Packages")
    {
        return Some(AnalysisInputKind::Dependency);
    }
    if name.ends_with(".props")
        || name.ends_with(".targets")
        || matches!(name, "Directory.Build.rsp" | "NuGet.config")
        || path
            .extension()
            .is_some_and(|extension| extension == "asset")
            && under(path, "ProjectSettings")
    {
        return Some(AnalysisInputKind::Configuration);
    }
    None
}

pub(crate) fn csharp_repository_dependencies(
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
    let assemblies = assembly_owners(snapshot);
    let mut candidates = Vec::new();
    for repository in &snapshot.repositories {
        let repository_id = &repository.state.repository.identity;
        for input in &repository.inputs {
            let name = input.path.file_name().and_then(|name| name.to_str());
            if name.is_some_and(|name| name.ends_with(".csproj")) {
                let Ok(source) = std::str::from_utf8(&input.content) else {
                    continue;
                };
                let directory = absolute_lexical(
                    &repository
                        .base
                        .join(input.path.parent().unwrap_or_else(|| Path::new(""))),
                );
                for reference in xml_paths(source, b"ProjectReference", b"Include") {
                    add_path_candidate(
                        &mut candidates,
                        repository_id,
                        &resolve_path(&directory, &reference),
                        &roots,
                        RepositoryDependencyKind::ProjectReference,
                        format!(
                            "{}: project reference {}",
                            input.path.display(),
                            reference.display()
                        ),
                    );
                }
                for import in xml_paths(source, b"Import", b"Project") {
                    add_path_candidate(
                        &mut candidates,
                        repository_id,
                        &resolve_path(&directory, &import),
                        &roots,
                        RepositoryDependencyKind::CompilerDiscovered,
                        format!(
                            "{}: MSBuild import {}",
                            input.path.display(),
                            import.display()
                        ),
                    );
                }
            } else if name.is_some_and(|name| name.ends_with(".asmdef")) {
                let Ok(definition) = serde_json::from_slice::<AssemblyDefinition>(&input.content)
                else {
                    continue;
                };
                for reference in definition.references {
                    let reference = reference.strip_prefix("GUID:").unwrap_or(&reference);
                    if let Some(owner) = assemblies.get(reference)
                        && owner != repository_id
                    {
                        candidates.push(RepositoryDependencyCandidate {
                            from: repository_id.clone(),
                            to: owner.clone(),
                            analyzer: "csharp".into(),
                            kind: RepositoryDependencyKind::ProjectReference,
                            evidence: format!(
                                "{}: Unity assembly reference {reference}",
                                input.path.display()
                            ),
                        });
                    }
                }
            } else if name == Some("manifest.json")
                && input.path.ends_with("Packages/manifest.json")
            {
                let Ok(manifest) = serde_json::from_slice::<Value>(&input.content) else {
                    continue;
                };
                let directory = absolute_lexical(
                    &repository
                        .base
                        .join(input.path.parent().unwrap_or_else(|| Path::new(""))),
                );
                for (package, specification) in manifest
                    .get("dependencies")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(package, value)| value.as_str().map(|value| (package, value)))
                {
                    let Some(path) = specification.strip_prefix("file:") else {
                        continue;
                    };
                    add_path_candidate(
                        &mut candidates,
                        repository_id,
                        &resolve_path(&directory, Path::new(path)),
                        &roots,
                        RepositoryDependencyKind::PathDependency,
                        format!(
                            "{}: Unity package {package} = {specification}",
                            input.path.display()
                        ),
                    );
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

#[derive(Deserialize)]
struct AssemblyDefinition {
    name: String,
    #[serde(default)]
    references: Vec<String>,
}

fn assembly_owners(snapshot: &WorkspaceSnapshot) -> BTreeMap<String, String> {
    let mut owners = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for repository in &snapshot.repositories {
        for input in repository.inputs.iter().filter(|input| {
            input
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".asmdef") || name.ends_with(".asmdef.meta"))
        }) {
            let keys = if input
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".asmdef.meta"))
            {
                unity_guid(&input.content).into_iter().collect()
            } else {
                serde_json::from_slice::<AssemblyDefinition>(&input.content)
                    .ok()
                    .map(|definition| vec![definition.name])
                    .unwrap_or_default()
            };
            for key in keys {
                if ambiguous.contains(&key) {
                    continue;
                }
                let owner = repository.state.repository.identity.clone();
                if owners.insert(key.clone(), owner).is_some() {
                    owners.remove(&key);
                    ambiguous.insert(key);
                }
            }
        }
    }
    owners
}

fn unity_guid(content: &[u8]) -> Option<String> {
    std::str::from_utf8(content).ok()?.lines().find_map(|line| {
        line.trim_start_matches('\u{feff}')
            .trim()
            .strip_prefix("guid:")
            .map(|guid| guid.trim().to_owned())
    })
}

fn xml_paths(source: &str, element: &[u8], attribute: &[u8]) -> Vec<PathBuf> {
    let mut reader = Reader::from_str(source);
    let mut paths = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event)) if event.name().as_ref() == element => {
                paths.extend(
                    event
                        .attributes()
                        .filter_map(Result::ok)
                        .filter_map(|value| {
                            (value.key.as_ref() == attribute)
                                .then(|| value.unescape_value().ok())
                                .flatten()
                                .map(|value| PathBuf::from(value.replace('\\', "/")))
                                .filter(|path| !dynamic(path))
                        }),
                );
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    paths
}

fn dynamic(path: &Path) -> bool {
    path.to_string_lossy()
        .bytes()
        .any(|byte| matches!(byte, b'$' | b'*' | b'?'))
}

fn under(path: &Path, directory: &str) -> bool {
    path.ancestors()
        .skip(1)
        .any(|parent| parent.ends_with(directory))
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
            from: repository.into(),
            to: owner.into(),
            analyzer: "csharp".into(),
            kind,
            evidence,
        });
    }
}

fn repository_owner<'a>(path: &Path, roots: &[(&'a str, PathBuf)]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(identity, _)| *identity)
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        absolute_lexical(path)
    } else {
        absolute_lexical(&base.join(path))
    }
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.into()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| ".".into())
            .join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput, RepositorySnapshot};
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
    fn classifies_dotnet_and_unity_inputs_without_generated_outputs() {
        assert_eq!(
            csharp_analysis_input_kind(Path::new("src/App.cs")),
            Some(AnalysisInputKind::Source)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("App.csproj")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("global.json")),
            Some(AnalysisInputKind::Toolchain)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("Directory.Build.props")),
            Some(AnalysisInputKind::Configuration)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("ProjectSettings/ProjectVersion.txt")),
            Some(AnalysisInputKind::Toolchain)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("Packages/packages-lock.json")),
            Some(AnalysisInputKind::Dependency)
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("obj/project.assets.json")),
            None
        );
        assert_eq!(
            csharp_analysis_input_kind(Path::new("Library/ArtifactDB")),
            None
        );
    }

    #[test]
    fn discovers_project_import_assembly_and_local_package_edges() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "app",
                    "/workspace/app",
                    &[
                        (
                            "App.csproj",
                            r#"<Project><Import Project="../build/shared.props"/><ItemGroup><ProjectReference Include="../core/Core.csproj"/></ItemGroup></Project>"#,
                        ),
                        (
                            "Assets/App.asmdef",
                            r#"{"name":"App","references":["Core"]}"#,
                        ),
                        (
                            "Packages/manifest.json",
                            r#"{"dependencies":{"tools":"file:../../tools"}}"#,
                        ),
                    ],
                ),
                repository(
                    "core",
                    "/workspace/core",
                    &[
                        ("Core.csproj", "<Project/>"),
                        ("Core.asmdef", r#"{"name":"Core"}"#),
                    ],
                ),
                repository(
                    "build",
                    "/workspace/build",
                    &[("shared.props", "<Project/>")],
                ),
                repository("tools", "/workspace/tools", &[("package.json", "{}")]),
            ],
        };
        let dependencies = csharp_repository_dependencies(&snapshot).unwrap();
        assert_eq!(dependencies.len(), 4);
        assert!(
            dependencies
                .iter()
                .all(|dependency| dependency.from == "app" && dependency.analyzer == "csharp")
        );
        assert!(dependencies.iter().any(|dependency| dependency.to == "core"
            && dependency.kind == RepositoryDependencyKind::ProjectReference));
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.to == "build"
                    && dependency.kind == RepositoryDependencyKind::CompilerDiscovered)
        );
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.to == "tools"
                    && dependency.kind == RepositoryDependencyKind::PathDependency)
        );
    }

    #[test]
    fn resolves_guid_assembly_references_and_deduplicates_them() {
        let snapshot = WorkspaceSnapshot {
            name: "main".into(),
            repositories: vec![
                repository(
                    "app",
                    "/workspace/app",
                    &[(
                        "App.asmdef",
                        r#"{"name":"App","references":["GUID:abc","GUID:abc"]}"#,
                    )],
                ),
                repository(
                    "core",
                    "/workspace/core",
                    &[("Core.asmdef.meta", "guid: abc\n")],
                ),
            ],
        };
        let dependencies = csharp_repository_dependencies(&snapshot).unwrap();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].to, "core");
    }
}
