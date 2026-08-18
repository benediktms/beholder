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

fn inferred_argument_type<'a>(caller: &'a Definition, expression: &str) -> Option<&'a str> {
    caller
        .parameters
        .iter()
        .find(|parameter| parameter.name == expression)
        .map(|parameter| parameter.type_name.as_str())
        .or_else(|| {
            caller
                .locals
                .iter()
                .find(|binding| binding.name == expression)
                .map(|binding| binding.type_name.as_str())
        })
        .or_else(|| expression.starts_with('"').then_some("string"))
}

fn call_is_applicable(target: &Definition, call: &Call) -> bool {
    let parameters = if call.kind == CallKind::Member
        && target
            .parameters
            .first()
            .is_some_and(|parameter| parameter.is_extension)
    {
        &target.parameters[1..]
    } else {
        target.parameters.as_slice()
    };
    if call.arguments.len() > parameters.len()
        || call.arguments.len()
            < parameters
                .iter()
                .filter(|parameter| !parameter.is_optional)
                .count()
    {
        return false;
    }
    call.arguments.iter().enumerate().all(|(index, argument)| {
        argument
            .name
            .as_deref()
            .and_then(|name| parameters.iter().find(|parameter| parameter.name == name))
            .or_else(|| parameters.get(index))
            .is_some()
    })
}

fn argument_match_score(caller: &Definition, target: &Definition, call: &Call) -> usize {
    let parameters = if call.kind == CallKind::Member
        && target
            .parameters
            .first()
            .is_some_and(|parameter| parameter.is_extension)
    {
        &target.parameters[1..]
    } else {
        target.parameters.as_slice()
    };
    call.arguments
        .iter()
        .enumerate()
        .filter(|(index, argument)| {
            let parameter = argument
                .name
                .as_deref()
                .and_then(|name| parameters.iter().find(|parameter| parameter.name == name))
                .or_else(|| parameters.get(*index));
            parameter
                .zip(inferred_argument_type(caller, &argument.expression))
                .is_some_and(|(parameter, argument_type)| {
                    simple_type_name(argument_type) == simple_type_name(&parameter.type_name)
                })
        })
        .count()
}

fn target_matches(caller: &Definition, target: &Definition, call: &Call) -> bool {
    if callable_name(target)
        != if call.kind == CallKind::Constructor {
            simple_type_name(&call.name)
        } else {
            &call.name
        }
    {
        return false;
    }
    let owner_matches = match call.kind {
        CallKind::Direct => parent(&target.qualified_name) == parent(&caller.qualified_name),
        CallKind::Constructor => parent(&target.qualified_name)
            .rsplit('/')
            .next()
            .is_some_and(|type_name| type_name == simple_type_name(&call.name)),
        CallKind::Member if call.receiver.as_deref() == Some("this") => {
            parent(&target.qualified_name) == parent(&caller.qualified_name)
        }
        CallKind::Member => {
            let receiver = call.receiver.as_deref().unwrap_or_default();
            caller
                .parameters
                .iter()
                .map(|parameter| (&parameter.name, &parameter.type_name))
                .chain(
                    caller
                        .locals
                        .iter()
                        .map(|binding| (&binding.name, &binding.type_name)),
                )
                .find(|(name, _)| name.as_str() == receiver)
                .zip(target.parameters.first())
                .is_some_and(|((_, receiver_type), first)| {
                    first.is_extension
                        && simple_type_name(&first.type_name) == simple_type_name(receiver_type)
                })
                || caller
                    .locals
                    .iter()
                    .find(|binding| binding.name == receiver)
                    .is_some_and(|binding| {
                        parent(&target.qualified_name)
                            .rsplit('/')
                            .next()
                            .is_some_and(|owner| owner == simple_type_name(&binding.type_name))
                    })
                || parent(&target.qualified_name)
                    .rsplit('/')
                    .next()
                    .is_some_and(|owner| owner == simple_type_name(receiver))
        }
    };
    owner_matches && call_is_applicable(target, call)
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
                let lookup_name = if call.kind == CallKind::Constructor {
                    simple_type_name(&call.name)
                } else {
                    &call.name
                };
                let mut candidates = definitions
                    .get(lookup_name)
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
                if let Some(best_score) = candidates
                    .iter()
                    .map(|(_, target)| argument_match_score(caller, target, call))
                    .max()
                {
                    candidates.retain(|(_, target)| {
                        argument_match_score(caller, target, call) == best_score
                    });
                }
                if let [(target_source, target)] = candidates.as_slice() {
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
        assert_eq!(observations.len(), 2, "{observations:#?}");
    }

    #[test]
    fn resolves_overloads_and_static_calls_from_argument_types() {
        let app_project = parse_project(
            Path::new("App/App.csproj"),
            r#"<Project><ItemGroup><ProjectReference Include="../Core/Core.csproj" /></ItemGroup></Project>"#,
        )
        .unwrap();
        let core_project = parse_project(Path::new("Core/Core.csproj"), "<Project />").unwrap();
        let app = analyze("static class Syntax { static void Start(string text) { Parse(text, flag: null); SourceText.From(text); var source = new Core.SourceText(); source.Read(); var derived = new Core.Derived(); Accept(derived); } static void Parse(string text, object? flag) {} static void Parse(SourceText text, object? flag) {} static void Accept(Core.Base value) {} }").unwrap();
        let core = analyze(
            "namespace Core { class SourceText { public static SourceText From(string text) { return new SourceText(); } public SourceText() {} public void Read() {} } class Base {} class Derived : Base {} }",
        )
        .unwrap();
        let sources = [
            CsharpSource {
                path: Path::new("App/Syntax.cs"),
                assembly: "App",
                analysis: &app,
            },
            CsharpSource {
                path: Path::new("Core/SourceText.cs"),
                assembly: "Core",
                analysis: &core,
            },
        ];

        let observations =
            resolve_repository_calls("example", &[app_project, core_project], &sources);

        assert!(
            observations.iter().any(|observation| {
                observation.from.as_str().ends_with("/Syntax/Start(string)")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/Syntax/Parse(string,object?)")
            }),
            "{observations:#?}"
        );
        assert!(
            observations.iter().any(|observation| {
                observation.from.as_str().ends_with("/Syntax/Start(string)")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/SourceText/From(string)")
            }),
            "{observations:#?}"
        );
        for target in ["/Core/SourceText/SourceText()", "/Core/SourceText/Read()"] {
            assert!(
                observations.iter().any(|observation| {
                    observation.from.as_str().ends_with("/Syntax/Start(string)")
                        && observation.to.as_str().ends_with(target)
                }),
                "missing {target}: {observations:#?}"
            );
        }
        assert!(
            observations.iter().any(|observation| {
                observation.from.as_str().ends_with("/Syntax/Start(string)")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/Syntax/Accept(Core.Base)")
            }),
            "{observations:#?}"
        );
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
