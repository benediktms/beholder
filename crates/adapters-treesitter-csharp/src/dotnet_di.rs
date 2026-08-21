use super::{
    model::{Call, Definition, DefinitionKind, Parameter},
    project::{CsharpProject, assembly_visibility},
    resolution::{CsharpSource, id, simple_type_name, type_name},
};
use beholder_domain::{DependencyRelation, Observation};
use std::collections::BTreeMap;

pub(super) struct Registration<'a> {
    pub service: &'a str,
    pub implementation: &'a str,
}

fn is_service_collection(receiver: &str, parameters: &[Parameter]) -> bool {
    // ponytail: `.Services` is the standard .NET builder surface; replace this with
    // property-type resolution if a real repository produces collisions.
    if receiver.ends_with(".Services") {
        return true;
    }
    parameters.iter().any(|parameter| {
        parameter.name == receiver
            && parameter
                .type_name
                .trim_end_matches('?')
                .rsplit('.')
                .next()
                .is_some_and(|name| name == "IServiceCollection")
    })
}

pub(super) fn registration<'a>(caller: &Definition, call: &'a Call) -> Option<Registration<'a>> {
    let [service, implementation] = call.type_arguments.as_slice() else {
        return None;
    };
    let receiver = call.receiver.as_deref()?;
    let service_collection_registration = is_service_collection(receiver, &caller.parameters)
        && matches!(
            call.name.as_str(),
            "AddSingleton"
                | "AddScoped"
                | "AddTransient"
                | "TryAddSingleton"
                | "TryAddScoped"
                | "TryAddTransient"
        );
    let descriptor_registration = receiver == "ServiceDescriptor"
        && matches!(call.name.as_str(), "Singleton" | "Scoped" | "Transient");
    (service_collection_registration || descriptor_registration).then_some(Registration {
        service,
        implementation,
    })
}

pub(super) fn observations(
    repository: &str,
    projects: &[CsharpProject],
    sources: &[CsharpSource<'_>],
) -> Vec<Observation> {
    let visibility = assembly_visibility(projects);
    let mut types = BTreeMap::<&str, Vec<(&CsharpSource<'_>, &Definition)>>::new();
    for source in sources {
        for definition in &source.analysis.definitions {
            if definition.kind == DefinitionKind::Type {
                types
                    .entry(type_name(definition))
                    .or_default()
                    .push((source, definition));
            }
        }
    }

    let mut observations = Vec::new();
    for source in sources {
        let visible = visibility
            .get(source.assembly)
            .cloned()
            .unwrap_or_else(|| std::collections::BTreeSet::from([source.assembly.into()]));
        for caller in source
            .analysis
            .definitions
            .iter()
            .filter(|definition| definition.kind == DefinitionKind::Callable)
        {
            for call in caller.calls.iter().rev() {
                let Some(registration) = registration(caller, call) else {
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
