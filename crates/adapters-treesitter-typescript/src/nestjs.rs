use super::{
    grpc::{GeneratedGrpcMethod, literal, module_id},
    model::{Call, TypescriptAnalysis},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyRelation,
    GrpcBindingCandidate, GrpcBindingRole, Observation, Provenance, RpcCardinality,
    SemanticRelation,
};
use std::{collections::BTreeSet, path::Path};

fn capitalize(name: &str) -> String {
    let mut name = name.to_owned();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

fn grpc_method(call: &Call, class: &str, handler: &str) -> Option<Result<(String, String), ()>> {
    if call.receiver.is_some() || call.name != "GrpcMethod" {
        return None;
    }
    let service = match call.arguments.first() {
        Some(service) => literal(service).map(str::to_owned).ok_or(()),
        None => Ok(class.to_owned()),
    };
    let method = match call.arguments.get(1) {
        Some(method) => literal(method).map(str::to_owned).ok_or(()),
        None => Ok(capitalize(handler)),
    };
    Some(service.and_then(|service| method.map(|method| (service, method))))
}

fn matching_service<'a>(
    generated: &'a [GeneratedGrpcMethod<'_>],
    short_service: &str,
    source_method: &str,
) -> BTreeSet<&'a str> {
    generated
        .iter()
        .filter(|generated| {
            generated.short_service == short_service
                && generated.source_method == source_method
                && generated.service.ends_with(&format!(".{short_service}"))
        })
        .map(|generated| generated.service.as_str())
        .collect()
}

fn matching_rpc<'a>(
    generated: &'a [GeneratedGrpcMethod<'_>],
    short_service: &str,
    method: &str,
) -> BTreeSet<&'a str> {
    generated
        .iter()
        .filter(|generated| generated.short_service == short_service && generated.method == method)
        .map(|generated| generated.service.as_str())
        .collect()
}

fn inferred_services(
    sources: &[(&Path, &TypescriptAnalysis)],
    method: &super::model::Definition,
    short_service: &str,
) -> BTreeSet<String> {
    let Some(request_type) = method.bindings.first().map(|binding| &binding.type_name) else {
        return BTreeSet::new();
    };
    sources
        .iter()
        .filter(|(_, analysis)| {
            analysis
                .definitions
                .iter()
                .filter(|definition| definition.qualified_name == *request_type)
                .count()
                > 1
        })
        .filter_map(|(_, analysis)| {
            analysis
                .string_constants
                .iter()
                .find(|constant| constant.name == "protobufPackage")
                .and_then(|constant| literal(&constant.value))
        })
        .map(|package| format!("{package}.{short_service}"))
        .collect()
}

fn candidate(
    local_symbol: &str,
    role: GrpcBindingRole,
    service: &str,
    method: &str,
    path: &Path,
    line: usize,
) -> GrpcBindingCandidate {
    GrpcBindingCandidate {
        local_symbol: local_symbol.into(),
        role,
        service: service.into(),
        method: method.into(),
        cardinality: RpcCardinality::Unary,
        evidence: format!("{}:{line}", path.display()).into(),
        confidence: Confidence::Exact,
        provenance: Provenance::Ast,
    }
}

pub(super) fn bindings(
    repository: &str,
    sources: &[(&Path, &TypescriptAnalysis)],
    generated: &[GeneratedGrpcMethod<'_>],
    observations: &[Observation],
) -> (Vec<GrpcBindingCandidate>, Vec<AnalysisDiagnostic>) {
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut emitted = BTreeSet::new();
    for (path, analysis) in sources {
        let module = module_id(repository, path, analysis);
        for definition in &analysis.definitions {
            let (class, handler) = definition
                .qualified_name
                .rsplit_once('/')
                .unwrap_or((&definition.qualified_name, &definition.qualified_name));
            let class = class.rsplit('/').next().unwrap_or(class);
            for call in &definition.calls {
                if matches!(call.name.as_str(), "GrpcStreamMethod" | "GrpcStreamCall") {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "typescript.nestjs_grpc_streaming_unsupported".into(),
                        severity: AnalysisDiagnosticSeverity::KnownLimitation,
                        path: (*path).into(),
                        line: u32::try_from(call.line).ok(),
                        detail: Some(format!("@{} requires streaming cardinality", call.name)),
                    });
                    continue;
                }
                let Some(binding) = grpc_method(call, class, handler) else {
                    continue;
                };
                let Ok((service_name, method)) = binding else {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "typescript.nestjs_grpc_literal_unresolved".into(),
                        severity: AnalysisDiagnosticSeverity::KnownLimitation,
                        path: (*path).into(),
                        line: u32::try_from(call.line).ok(),
                        detail: Some("NestJS gRPC decorator arguments must be literals".into()),
                    });
                    continue;
                };
                let matches = matching_rpc(generated, &service_name, &method);
                if matches.len() != 1 {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "typescript.nestjs_grpc_service_unresolved".into(),
                        severity: AnalysisDiagnosticSeverity::KnownLimitation,
                        path: (*path).into(),
                        line: u32::try_from(call.line).ok(),
                        detail: Some(format!(
                            "@GrpcMethod({service_name}, {method}) matched {} generated services",
                            matches.len()
                        )),
                    });
                    continue;
                }
                let local_symbol = format!("{module}/{}", definition.qualified_name);
                if emitted.insert((local_symbol.clone(), "server", method.clone())) {
                    candidates.push(candidate(
                        &local_symbol,
                        GrpcBindingRole::Server,
                        matches.first().expect("exactly one service matched"),
                        &method,
                        path,
                        call.line,
                    ));
                }
            }

            for call in &definition.calls {
                let Some(controller) = call.name.strip_suffix("ControllerMethods") else {
                    continue;
                };
                for method in analysis.definitions.iter().filter(|method| {
                    method
                        .qualified_name
                        .strip_prefix(&format!("{}/", definition.qualified_name))
                        .is_some_and(|name| !name.contains('/'))
                }) {
                    let source_method = method
                        .qualified_name
                        .rsplit_once('/')
                        .map(|(_, method)| method)
                        .expect("class member has a parent");
                    let matches = matching_service(generated, controller, source_method);
                    if matches.len() != 1 {
                        continue;
                    }
                    let generated_method = generated
                        .iter()
                        .find(|generated| {
                            generated.service == **matches.first().unwrap()
                                && generated.source_method == source_method
                        })
                        .expect("matched generated method exists");
                    let local_symbol = format!("{module}/{}", method.qualified_name);
                    if emitted.insert((
                        local_symbol.clone(),
                        "server",
                        generated_method.method.clone(),
                    )) {
                        candidates.push(candidate(
                            &local_symbol,
                            GrpcBindingRole::Server,
                            &generated_method.service,
                            &generated_method.method,
                            path,
                            call.line,
                        ));
                    }
                }
            }
        }
    }

    for (path, analysis) in sources {
        for definition in &analysis.definitions {
            for call in &definition.calls {
                if call.name != "getService" {
                    continue;
                }
                let Some(interface) = call.type_arguments.first() else {
                    continue;
                };
                let Some(service_name) = call.arguments.first().and_then(|value| literal(value))
                else {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "typescript.nestjs_grpc_client_service_unresolved".into(),
                        severity: AnalysisDiagnosticSeverity::KnownLimitation,
                        path: (*path).into(),
                        line: u32::try_from(call.line).ok(),
                        detail: Some(
                            "ClientGrpc.getService requires a literal service name".into(),
                        ),
                    });
                    continue;
                };
                for (method_path, method_analysis) in sources {
                    let method_module = module_id(repository, method_path, method_analysis);
                    for method in method_analysis.definitions.iter().filter(|method| {
                        method
                            .qualified_name
                            .strip_prefix(&format!("{interface}/"))
                            .is_some_and(|name| !name.contains('/'))
                    }) {
                        let source_method = method.qualified_name.rsplit_once('/').unwrap().1;
                        let matches = matching_service(generated, service_name, source_method);
                        let inferred = if matches.is_empty() {
                            inferred_services(sources, method, service_name)
                        } else {
                            BTreeSet::new()
                        };
                        if matches.len() + inferred.len() != 1 {
                            continue;
                        }
                        let generated_method = generated.iter().find(|generated| {
                            matches
                                .first()
                                .is_some_and(|service| generated.service == **service)
                                && generated.source_method == source_method
                        });
                        let service = generated_method
                            .map(|generated| generated.service.as_str())
                            .or_else(|| inferred.first().map(String::as_str))
                            .expect("exactly one service matched");
                        let rpc_method = generated_method
                            .map(|generated| generated.method.clone())
                            .unwrap_or_else(|| capitalize(source_method));
                        let local_symbol = format!("{method_module}/{}", method.qualified_name);
                        let used = observations.iter().any(|observation| {
                            observation.relation
                                == SemanticRelation::Dependency(DependencyRelation::Calls)
                                && observation.to.as_str() == local_symbol
                        });
                        if used
                            && emitted.insert((local_symbol.clone(), "client", rpc_method.clone()))
                        {
                            candidates.push(candidate(
                                &local_symbol,
                                GrpcBindingRole::Client,
                                service,
                                &rpc_method,
                                path,
                                call.line,
                            ));
                        }
                    }
                }
            }
        }
    }
    (candidates, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceLanguage, analyze, ts_proto};

    fn generated() -> TypescriptAnalysis {
        analyze(
            r#"
            export const protobufPackage = "example.checkout.v1";
            export interface RPCServiceClient {
              initializeOrder(request: Request): Observable<Response>;
            }
            "#,
            SourceLanguage::TypeScript,
        )
        .unwrap()
    }

    #[test]
    fn binds_explicit_and_defaulted_grpc_method_decorators() {
        let generated = generated();
        let server = analyze(
            r#"
            export class ExplicitController {
              @GrpcMethod('RPCService', 'InitializeOrder') explicit(message: Request) {}
            }
            export class NamedController {
              @GrpcMethod('RPCService') initializeOrder(message: Request) {}
            }
            export class RPCService {
              @GrpcMethod() initializeOrder(message: Request) {}
            }
            "#,
            SourceLanguage::TypeScript,
        )
        .unwrap();
        let generated = ts_proto::grpc_methods(
            "example",
            &[(Path::new("generated/checkout.ts"), &generated)],
        );

        let (bindings, diagnostics) = bindings(
            "example",
            &[(Path::new("src/controller.ts"), &server)],
            &generated,
            &[],
        );

        assert_eq!(bindings.len(), 3);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn reports_streaming_decorators_explicitly() {
        let server = analyze(
            "class RPCService { @GrpcStreamMethod() stream(messages: Observable<Request>) {} }",
            SourceLanguage::TypeScript,
        )
        .unwrap();

        let (_, diagnostics) = bindings(
            "example",
            &[(Path::new("src/controller.ts"), &server)],
            &[],
            &[],
        );

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "typescript.nestjs_grpc_streaming_unsupported"
        }));
    }

    #[test]
    fn binds_client_grpc_proxies_and_generated_controller_decorators() {
        let generated = generated();
        let source = analyze(
            r#"
            interface CheckoutProxy {
              initializeOrder(request: Request): Observable<Response>;
            }
            class CheckoutClient {
              private proxy: CheckoutProxy;
              onModuleInit() {
                this.proxy = this.client.getService<CheckoutProxy>('RPCService');
              }
              run() { return this.proxy.initializeOrder({}); }
            }
            @RPCServiceControllerMethods()
            class CheckoutController {
              initializeOrder(request: Request): Response { return request; }
            }
            "#,
            SourceLanguage::TypeScript,
        )
        .unwrap();
        let generated = ts_proto::grpc_methods(
            "example",
            &[(Path::new("generated/checkout.ts"), &generated)],
        );
        let proxy = "repo://example/typescript/src/checkout/CheckoutProxy/initializeOrder";
        let observations = vec![Observation::dependency(
            "repo://example/typescript/src/checkout/CheckoutClient/run",
            DependencyRelation::Calls,
            proxy,
            "src/checkout.ts:10",
        )];

        let (bindings, diagnostics) = bindings(
            "example",
            &[(Path::new("src/checkout.ts"), &source)],
            &generated,
            &observations,
        );

        assert!(diagnostics.is_empty());
        assert!(bindings.iter().any(|binding| {
            binding.local_symbol.as_str() == proxy && binding.role == GrpcBindingRole::Client
        }));
        assert!(bindings.iter().any(|binding| {
            binding.local_symbol.as_str()
                == "repo://example/typescript/src/checkout/CheckoutController/initializeOrder"
                && binding.role == GrpcBindingRole::Server
        }));
    }

    #[test]
    fn infers_handwritten_client_service_from_generated_request() {
        let generated_message = analyze(
            r#"
            export const protobufPackage = "rpc.service_fees.v1";
            export interface ListServiceFeeConfigurationsRequest {}
            export const ListServiceFeeConfigurationsRequest = {};
            "#,
            SourceLanguage::TypeScript,
        )
        .unwrap();
        let client = analyze(
            r#"
            interface ServiceFeesRpcService {
              listServiceFeeConfigurations(request: ListServiceFeeConfigurationsRequest): Observable<Response>;
            }
            class Client {
              onModuleInit() {
                this.service = this.client.getService<ServiceFeesRpcService>('RPCService');
              }
              run() { return this.service.listServiceFeeConfigurations({}); }
            }
            "#,
            SourceLanguage::TypeScript,
        )
        .unwrap();
        let proxy = "repo://example/typescript/src/client/ServiceFeesRpcService/listServiceFeeConfigurations";
        let observations = vec![Observation::dependency(
            "repo://example/typescript/src/client/Client/run",
            DependencyRelation::Calls,
            proxy,
            "src/client.ts:9",
        )];
        let sources = [
            (Path::new("src/client.ts"), &client),
            (
                Path::new("generated/list_service_fee_configurations.ts"),
                &generated_message,
            ),
        ];

        let (bindings, diagnostics) = bindings("example", &sources, &[], &observations);

        assert!(diagnostics.is_empty());
        assert!(bindings.iter().any(|binding| {
            binding.local_symbol.as_str() == proxy
                && binding.service == "rpc.service_fees.v1.RPCService"
                && binding.method == "ListServiceFeeConfigurations"
        }));
    }
}
