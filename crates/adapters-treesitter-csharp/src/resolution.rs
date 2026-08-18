use super::{
    analysis::source_stem,
    dotnet_di,
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

fn type_name(definition: &Definition) -> &str {
    definition
        .qualified_name
        .rsplit('/')
        .next()
        .unwrap_or(&definition.qualified_name)
}

fn simple_type_name(name: &str) -> &str {
    name.trim_end_matches('?')
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .split('<')
        .next()
        .unwrap_or(name)
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
    let mut types = BTreeMap::<&str, Vec<(&CsharpSource<'_>, &Definition)>>::new();
    for source in sources {
        for definition in &source.analysis.definitions {
            match definition.kind {
                DefinitionKind::Callable => definitions
                    .entry(callable_name(definition))
                    .or_default()
                    .push((source, definition)),
                DefinitionKind::Type => types
                    .entry(type_name(definition))
                    .or_default()
                    .push((source, definition)),
                DefinitionKind::Namespace => {}
            }
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
                    .map(|candidate| {
                        (
                            (
                                candidate.0.assembly,
                                candidate.0.path,
                                candidate.1.qualified_name.as_str(),
                            ),
                            candidate,
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
                    .into_values()
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
                let Some(registration) = dotnet_di::registration(caller, call) else {
                    continue;
                };
                let resolve_type = |name: &str| {
                    types
                        .get(simple_type_name(name))
                        .into_iter()
                        .flatten()
                        .filter(|(candidate, _)| visible.contains(candidate.assembly))
                        .copied()
                        .map(|candidate| {
                            (
                                (
                                    candidate.0.assembly,
                                    candidate.0.path,
                                    candidate.1.qualified_name.as_str(),
                                ),
                                candidate,
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                        .into_values()
                        .collect::<Vec<_>>()
                };
                let (service, implementation) = (
                    resolve_type(registration.service),
                    resolve_type(registration.implementation),
                );
                let ([(service_source, service)], [(implementation_source, implementation)]) =
                    (service.as_slice(), implementation.as_slice())
                else {
                    continue;
                };
                let evidence = format!("{}:{}", source.path.display(), call.line);
                observations.push(Observation::dependency(
                    id(repository, service_source, service),
                    DependencyRelation::ResolvedBy,
                    id(repository, implementation_source, implementation),
                    evidence.clone(),
                ));
                observations.push(Observation::dependency(
                    id(repository, source, caller),
                    DependencyRelation::Selects,
                    id(repository, implementation_source, implementation),
                    evidence,
                ));
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

    #[test]
    fn resolves_dotnet_service_collection_registrations() {
        let app_project = parse_project(
            Path::new("App/App.csproj"),
            r#"<Project><ItemGroup><ProjectReference Include="../Core/Core.csproj" /></ItemGroup></Project>"#,
        )
        .unwrap();
        let core_project = parse_project(Path::new("Core/Core.csproj"), "<Project />").unwrap();
        let app = analyze("static class Setup { static void Configure(IServiceCollection services) { services.TryAddSingleton<IClock, SystemClock>(); } static void ConfigureBuilder(WebApplicationBuilder builder) { builder.Services.AddSingleton<IQueue, Queue>(); } }")
            .unwrap();
        let core = analyze(
            "#if PUBLIC\npublic interface IClock {}\npublic sealed class SystemClock : IClock {}\n#else\ninternal interface IClock {}\ninternal sealed class SystemClock : IClock {}\n#endif\ninterface IQueue {} sealed class Queue : IQueue {}",
        )
        .unwrap();
        let sources = [
            CsharpSource {
                path: Path::new("App/Setup.cs"),
                assembly: "App",
                analysis: &app,
            },
            CsharpSource {
                path: Path::new("Core/Clock.cs"),
                assembly: "Core",
                analysis: &core,
            },
        ];

        let observations =
            resolve_repository_calls("example", &[app_project, core_project], &sources);

        assert!(
            observations.iter().any(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                    && observation.from.as_str().ends_with("/Core/Clock/IClock")
                    && observation.to.as_str().ends_with("/Core/Clock/SystemClock")
            }),
            "{observations:#?}"
        );
        assert!(
            observations.iter().any(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Selects)
                    && observation
                        .from
                        .as_str()
                        .ends_with("/App/Setup/Setup/Configure(IServiceCollection)")
                    && observation.to.as_str().ends_with("/Core/Clock/SystemClock")
            }),
            "{observations:#?}"
        );
        assert!(
            observations.iter().any(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                    && observation.from.as_str().ends_with("/Core/Clock/IQueue")
                    && observation.to.as_str().ends_with("/Core/Clock/Queue")
            }),
            "{observations:#?}"
        );
        assert_eq!(observations.len(), 4, "{observations:#?}");
    }
}
