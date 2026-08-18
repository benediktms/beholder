use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CsharpProject {
    pub path: PathBuf,
    pub assembly: String,
    pub references: Vec<PathBuf>,
    pub includes: Vec<PathBuf>,
    pub removes: Vec<PathBuf>,
}

fn normalized_path(path: &str) -> PathBuf {
    PathBuf::from(path.replace('\\', "/"))
}

fn attribute(event: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<PathBuf> {
    event
        .attributes()
        .filter_map(Result::ok)
        .find(|attribute| attribute.key.as_ref() == name)
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| normalized_path(&value))
}

pub fn parse_project(path: &Path, source: &str) -> Result<CsharpProject, Box<dyn Error>> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(true);
    let mut assembly = None;
    let mut references = Vec::new();
    let mut includes = Vec::new();
    let mut removes = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(event) if event.name().as_ref() == b"AssemblyName" => {
                assembly = Some(reader.read_text(event.name())?.into_owned());
            }
            Event::Start(event) | Event::Empty(event)
                if event.name().as_ref() == b"ProjectReference" =>
            {
                if let Some(reference) = attribute(&event, b"Include") {
                    references.push(reference);
                }
            }
            Event::Start(event) | Event::Empty(event) if event.name().as_ref() == b"Compile" => {
                if let Some(include) = attribute(&event, b"Include") {
                    includes.push(include);
                }
                if let Some(remove) = attribute(&event, b"Remove") {
                    removes.push(remove);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("C# project path has no file stem")?;
    references.sort();
    references.dedup();
    includes.sort();
    includes.dedup();
    removes.sort();
    removes.dedup();
    Ok(CsharpProject {
        path: path.into(),
        assembly: assembly
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback.into()),
        references,
        includes,
        removes,
    })
}

fn resolve_relative(base: &Path, path: &Path) -> Option<PathBuf> {
    let path_text = path.to_string_lossy();
    if path_text.contains("$(") || path_text.contains('*') || path_text.contains('?') {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    Some(resolved)
}

pub(crate) fn assembly_visibility(
    projects: &[CsharpProject],
) -> BTreeMap<String, BTreeSet<String>> {
    let paths = projects
        .iter()
        .map(|project| (project.path.clone(), project.assembly.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visibility = projects
        .iter()
        .map(|project| {
            let directory = project.path.parent().unwrap_or(Path::new(""));
            let references = project
                .references
                .iter()
                .filter_map(|reference| resolve_relative(directory, reference))
                .filter_map(|reference| paths.get(&reference).cloned())
                .chain(std::iter::once(project.assembly.clone()))
                .collect::<BTreeSet<_>>();
            (project.assembly.clone(), references)
        })
        .collect::<BTreeMap<_, _>>();
    loop {
        let previous = visibility.clone();
        for visible in visibility.values_mut() {
            for assembly in visible.clone() {
                if let Some(transitive) = previous.get(&assembly) {
                    visible.extend(transitive.iter().cloned());
                }
            }
        }
        if visibility == previous {
            break;
        }
    }
    visibility
}

pub fn source_assemblies(projects: &[CsharpProject], path: &Path) -> Vec<String> {
    let mut owners = projects
        .iter()
        .filter_map(|project| {
            let directory = project.path.parent().unwrap_or(Path::new(""));
            path.starts_with(directory)
                .then_some((directory.components().count(), project))
        })
        .collect::<Vec<_>>();
    owners.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    let mut assemblies = owners
        .into_iter()
        .find(|(_, project)| {
            let directory = project.path.parent().unwrap_or(Path::new(""));
            !project
                .removes
                .iter()
                .filter_map(|remove| resolve_relative(directory, remove))
                .any(|remove| remove == path)
        })
        .map(|(_, project)| vec![project.assembly.clone()])
        .unwrap_or_default();
    assemblies.extend(projects.iter().filter_map(|project| {
        let directory = project.path.parent().unwrap_or(Path::new(""));
        project
            .includes
            .iter()
            .filter_map(|include| resolve_relative(directory, include))
            .any(|include| include == path)
            .then(|| project.assembly.clone())
    }));
    assemblies.sort();
    assemblies.dedup();
    if assemblies.is_empty() {
        assemblies.push("default".into());
    }
    assemblies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_static_project_boundaries_and_assigns_sources() {
        let project = parse_project(
            Path::new("src/App/App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
                <PropertyGroup><AssemblyName>Example.App</AssemblyName></PropertyGroup>
                <ItemGroup>
                    <ProjectReference Include="..\Core\Core.csproj" />
                    <Compile Include="..\Shared.cs" />
                    <Compile Remove="Removed.cs" />
                </ItemGroup>
            </Project>"#,
        )
        .unwrap();
        let core = parse_project(
            Path::new("src/Core/Core.csproj"),
            "<Project><PropertyGroup><AssemblyName>Example.Core</AssemblyName></PropertyGroup></Project>",
        )
        .unwrap();
        assert_eq!(project.assembly, "Example.App");
        assert_eq!(project.references, [PathBuf::from("../Core/Core.csproj")]);
        assert_eq!(
            assembly_visibility(&[project.clone(), core])["Example.App"],
            BTreeSet::from(["Example.App".into(), "Example.Core".into()])
        );
        assert_eq!(
            source_assemblies(
                std::slice::from_ref(&project),
                Path::new("src/App/Program.cs")
            ),
            ["Example.App"]
        );
        assert_eq!(
            source_assemblies(std::slice::from_ref(&project), Path::new("src/Shared.cs")),
            ["Example.App"]
        );
        assert_eq!(
            source_assemblies(
                std::slice::from_ref(&project),
                Path::new("src/App/Removed.cs")
            ),
            ["default"]
        );
    }
}
