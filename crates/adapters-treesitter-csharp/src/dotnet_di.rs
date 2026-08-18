use super::model::{Call, Definition, Parameter};

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
