use super::{
    analysis::source_stem,
    model::{Call, CallKind, CsharpAnalysis, Definition, DefinitionKind},
    project::{CsharpProject, assembly_visibility},
};
use beholder_domain::{DependencyRelation, Observation};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub struct CsharpSource<'a> {
    pub path: &'a Path,
    pub assembly: &'a str,
    pub analysis: &'a CsharpAnalysis,
}

fn parent(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn callable_name(definition: &Definition) -> &str {
    definition
        .qualified_name
        .rsplit('/')
        .next()
        .unwrap_or(&definition.qualified_name)
        .split('(')
        .next()
        .unwrap_or_default()
}

fn target_matches(caller: &Definition, target: &Definition, call: &Call) -> bool {
    if callable_name(target) != call.name {
        return false;
    }
    match call.kind {
        CallKind::Direct => parent(&target.qualified_name) == parent(&caller.qualified_name),
        CallKind::Constructor => parent(&target.qualified_name)
            .rsplit('/')
            .next()
            .is_some_and(|type_name| type_name == call.name),
        CallKind::Member if call.receiver.as_deref() == Some("this") => {
            parent(&target.qualified_name) == parent(&caller.qualified_name)
        }
        CallKind::Member => call
            .receiver
            .as_deref()
            .and_then(|receiver| {
                caller
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name == receiver)
            })
            .zip(target.parameters.first())
            .is_some_and(|(receiver, first)| {
                first.is_extension && first.type_name == receiver.type_name
            }),
    }
}

fn id(repository: &str, source: &CsharpSource<'_>, definition: &Definition) -> String {
    format!(
        "repo://{repository}/csharp/{}/{}/{}",
        source.assembly,
        source_stem(source.path),
        definition.qualified_name
    )
}

pub fn resolve_repository_calls(
    repository: &str,
    projects: &[CsharpProject],
    sources: &[CsharpSource<'_>],
) -> Vec<Observation> {
    let visibility = assembly_visibility(projects);
    let mut definitions = BTreeMap::<&str, Vec<(&CsharpSource<'_>, &Definition)>>::new();
    for source in sources {
        for definition in source
            .analysis
            .definitions
            .iter()
            .filter(|definition| definition.kind == DefinitionKind::Callable)
        {
            definitions
                .entry(callable_name(definition))
                .or_default()
                .push((source, definition));
        }
    }
    let mut observations = Vec::new();
    for source in sources {
        let visible = visibility
            .get(source.assembly)
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([source.assembly.into()]));
        for caller in source
            .analysis
            .definitions
            .iter()
            .filter(|definition| definition.kind == DefinitionKind::Callable)
        {
            for call in &caller.calls {
                let candidates = definitions
                    .get(call.name.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|(candidate, target)| {
                        visible.contains(candidate.assembly) && target_matches(caller, target, call)
                    })
                    .copied()
                    .collect::<Vec<_>>();
                if let [(target_source, target)] = candidates.as_slice() {
                    let already_resolved_locally = target_source.path == source.path
                        && target_source.assembly == source.assembly
                        && !(call.kind == CallKind::Member
                            && call.receiver.as_deref() != Some("this"));
                    if already_resolved_locally {
                        continue;
                    }
                    observations.push(Observation::dependency(
                        id(repository, source, caller),
                        DependencyRelation::Calls,
                        id(repository, target_source, target),
                        format!("{}:{}", source.path.display(), call.line),
                    ));
                }
            }
        }
    }
    observations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, parse_project};
    use beholder_domain::{DependencyRelation, SemanticRelation};

    #[test]
    fn resolves_extension_calls_only_through_visible_projects() {
        let app_project = parse_project(
            Path::new("App/App.csproj"),
            r#"<Project><ItemGroup><ProjectReference Include="../Core/Core.csproj" /></ItemGroup></Project>"#,
        )
        .unwrap();
        let core_project = parse_project(Path::new("Core/Core.csproj"), "<Project />").unwrap();
        let app = analyze("static class App { static void Start(this IServiceCollection services) { Helper(); services.AddCore(); } static void Helper() {} }")
        .unwrap();
        let core = analyze(
            "static class Core { public static void AddCore(this IServiceCollection services) {} }",
        )
        .unwrap();
        let sources = [
            CsharpSource {
                path: Path::new("App/App.cs"),
                assembly: "App",
                analysis: &app,
            },
            CsharpSource {
                path: Path::new("Core/Core.cs"),
                assembly: "Core",
                analysis: &core,
            },
        ];
        let observations =
            resolve_repository_calls("example", &[app_project, core_project], &sources);
        assert!(
            observations.iter().any(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                    && observation
                        .from
                        .as_str()
                        .ends_with("/App/App/Start(IServiceCollection)")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/Core/Core/AddCore(IServiceCollection)")
            }),
            "{observations:#?}"
        );
        assert_eq!(observations.len(), 1, "{observations:#?}");
    }
}
