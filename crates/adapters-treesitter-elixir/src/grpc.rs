use super::analysis::{arguments, call_target, expand_alias, keyword_value, text};
use super::model::{ElixirAlias, ElixirAnalysis};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, GrpcBindingCandidate,
    GrpcBindingRole, Provenance, RpcCardinality,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct GrpcModule {
    service_name: Option<(String, usize)>,
    service: Option<(String, usize)>,
    stub: Option<String>,
    server: bool,
    methods: Vec<GrpcMethod>,
    issues: Vec<GrpcIssue>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct GrpcMethod {
    proto_name: String,
    elixir_name: String,
    line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct GrpcIssue {
    line: usize,
    detail: String,
}

fn literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    let value = text(node, source)?;
    match node.kind() {
        "alias" => Some(value.into()),
        "atom" => Some(value.trim_start_matches(':').into()),
        "string" => Some(value.trim_matches('"').into()),
        _ => None,
    }
}

fn pascal_to_snake(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut result = String::new();
    for (index, character) in characters.iter().copied().enumerate() {
        if character.is_uppercase()
            && index > 0
            && (characters[index - 1].is_lowercase()
                || characters
                    .get(index + 1)
                    .is_some_and(|character| character.is_lowercase()))
        {
            result.push('_');
        }
        result.extend(character.to_lowercase());
    }
    result
}

fn option(
    node: Node<'_>,
    source: &[u8],
    key: &str,
    aliases: &[ElixirAlias],
    current_module: &str,
) -> Option<String> {
    let value = keyword_value(node, source, key)?;
    literal(value, source).map(|value| expand_alias(&value, aliases, current_module))
}

pub(super) fn observe_call(
    grpc: &mut GrpcModule,
    node: Node<'_>,
    source: &[u8],
    aliases: &[ElixirAlias],
    current_module: &str,
) {
    let line = node.start_position().row + 1;
    match call_target(node, source) {
        Some("use") => {
            let Some(target) = arguments(node)
                .and_then(|arguments| arguments.named_child(0))
                .and_then(|target| literal(target, source))
            else {
                return;
            };
            let service = option(node, source, "service", aliases, current_module)
                .map(|service| (service, line));
            match target.as_str() {
                "GRPC.Service" => {
                    if let Some(name) = option(node, source, "name", aliases, current_module) {
                        grpc.service_name = Some((name, line));
                    } else {
                        grpc.issues.push(GrpcIssue {
                            line,
                            detail: "GRPC.Service requires a literal name option".into(),
                        });
                    }
                }
                "GRPC.Stub" => {
                    grpc.service = service;
                    if grpc.service.is_none() {
                        grpc.issues.push(GrpcIssue {
                            line,
                            detail: "GRPC.Stub requires a literal service option".into(),
                        });
                    }
                }
                "GRPC.Server" => {
                    grpc.service = service;
                    grpc.server = true;
                    if grpc.service.is_none() {
                        grpc.issues.push(GrpcIssue {
                            line,
                            detail: "GRPC.Server requires a literal service option".into(),
                        });
                    }
                }
                _ => {
                    if let (Some(service), Some(stub)) = (
                        service,
                        option(node, source, "stub", aliases, current_module),
                    ) {
                        grpc.service = Some(service);
                        grpc.stub = Some(stub);
                    }
                }
            }
        }
        Some("rpc") if grpc.service_name.is_some() => {
            let Some(arguments) = arguments(node) else {
                return;
            };
            let Some(proto_name) = arguments
                .named_child(0)
                .and_then(|name| literal(name, source))
            else {
                grpc.issues.push(GrpcIssue {
                    line,
                    detail: "gRPC method name must be a literal atom".into(),
                });
                return;
            };
            if arguments.named_child_count() != 3 {
                grpc.issues.push(GrpcIssue {
                    line,
                    detail: format!("streaming gRPC method {proto_name} is not supported"),
                });
                return;
            }
            if [1, 2].into_iter().any(|index| {
                arguments
                    .named_child(index)
                    .is_some_and(|argument| call_target(argument, source) == Some("stream"))
            }) {
                grpc.issues.push(GrpcIssue {
                    line,
                    detail: format!("streaming gRPC method {proto_name} is not supported"),
                });
                return;
            }
            grpc.methods.push(GrpcMethod {
                elixir_name: pascal_to_snake(&proto_name),
                proto_name,
                line,
            });
        }
        _ => {}
    }
}

fn local_symbol(repository: &str, module: &str, function: &str, arity: usize) -> String {
    format!("repo://{repository}/elixir/{module}/{function}/{arity}")
}

fn candidate(
    local_symbol: &str,
    role: GrpcBindingRole,
    service: &str,
    method: &GrpcMethod,
    evidence: String,
    confidence: Confidence,
    provenance: Provenance,
) -> GrpcBindingCandidate {
    GrpcBindingCandidate {
        local_symbol: local_symbol.into(),
        role,
        service: service.into(),
        method: method.proto_name.clone(),
        cardinality: RpcCardinality::Unary,
        evidence: evidence.into(),
        confidence,
        provenance,
    }
}

pub fn bindings(
    repository: &str,
    sources: &[(&Path, &ElixirAnalysis)],
) -> (Vec<GrpcBindingCandidate>, Vec<AnalysisDiagnostic>) {
    let services = sources
        .iter()
        .flat_map(|(path, analysis)| {
            analysis.modules.iter().filter_map(move |module| {
                module
                    .grpc
                    .service_name
                    .as_ref()
                    .map(|(name, _)| (module.name.clone(), (name.clone(), *path, &module.grpc)))
            })
        })
        .collect::<BTreeMap<_, _>>();
    let stubs = sources
        .iter()
        .flat_map(|(_, analysis)| &analysis.modules)
        .filter(|module| !module.grpc.server && module.grpc.stub.is_none())
        .filter_map(|module| {
            module
                .grpc
                .service
                .as_ref()
                .map(|(service, _)| (module.name.clone(), service.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut emitted = BTreeSet::new();

    for (path, analysis) in sources {
        for module in &analysis.modules {
            diagnostics.extend(module.grpc.issues.iter().map(|issue| AnalysisDiagnostic {
                code: "elixir.grpc_literal_unresolved".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: (*path).into(),
                line: u32::try_from(issue.line).ok(),
                detail: Some(issue.detail.clone()),
            }));

            let configured_service = module.grpc.service.as_ref().map(|(service, _)| service);
            let server_service = module.grpc.server.then_some(configured_service).flatten();
            let wrapper_service = module.grpc.stub.as_ref().and_then(|stub| stubs.get(stub));

            if let Some((service, line)) = &module.grpc.service
                && module.grpc.stub.is_none()
                && !services.contains_key(service)
            {
                diagnostics.push(AnalysisDiagnostic {
                    code: "elixir.grpc_service_unresolved".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path: (*path).into(),
                    line: u32::try_from(*line).ok(),
                    detail: Some(format!("gRPC service module {service} was not found")),
                });
            }

            for function in &module.functions {
                let function_id =
                    local_symbol(repository, &module.name, &function.name, function.arity);
                let mut bindings = Vec::new();
                if let Some(service_module) = server_service
                    && let Some(service) = services.get(service_module)
                    && let Some(method) = service
                        .2
                        .methods
                        .iter()
                        .find(|method| method.elixir_name == function.name)
                {
                    bindings.push((GrpcBindingRole::Server, service, method, function.line));
                }
                if let Some(service_module) = wrapper_service
                    && let Some(service) = services.get(service_module)
                    && let Some(method) =
                        service.2.methods.iter().find(|method| {
                            method.elixir_name == function.name.trim_end_matches('!')
                        })
                {
                    bindings.push((GrpcBindingRole::Client, service, method, function.line));
                }
                for call in &function.calls {
                    let Some(service_module) =
                        call.module.as_ref().and_then(|module| stubs.get(module))
                    else {
                        continue;
                    };
                    let Some(service) = services.get(service_module) else {
                        continue;
                    };
                    let Some(method) = service
                        .2
                        .methods
                        .iter()
                        .find(|method| method.elixir_name == call.name)
                    else {
                        diagnostics.push(AnalysisDiagnostic {
                            code: "elixir.grpc_method_unresolved".into(),
                            severity: AnalysisDiagnosticSeverity::KnownLimitation,
                            path: (*path).into(),
                            line: u32::try_from(call.line).ok(),
                            detail: Some(format!(
                                "gRPC method {} was not found on {service_module}",
                                call.name
                            )),
                        });
                        continue;
                    };
                    bindings.push((GrpcBindingRole::Client, service, method, call.line));
                }

                for (role, (service, service_path, _), method, line) in bindings {
                    let key = (
                        function_id.clone(),
                        role.as_str(),
                        service.clone(),
                        method.proto_name.clone(),
                    );
                    if !emitted.insert(key) {
                        continue;
                    }
                    candidates.push(candidate(
                        &function_id,
                        role,
                        service,
                        method,
                        format!("{}:{line}", path.display()),
                        Confidence::Inferred,
                        Provenance::Ast,
                    ));
                    if service_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".pb.ex"))
                    {
                        candidates.push(candidate(
                            &function_id,
                            role,
                            service,
                            method,
                            format!("{}:{}", service_path.display(), method.line),
                            Confidence::Exact,
                            Provenance::Generated,
                        ));
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
    use crate::analyze;

    fn resolved(sources: &[(&str, &str)]) -> (Vec<GrpcBindingCandidate>, Vec<AnalysisDiagnostic>) {
        let analyses = sources
            .iter()
            .map(|(path, source)| ((*path).into(), analyze(source).unwrap()))
            .collect::<Vec<(std::path::PathBuf, ElixirAnalysis)>>();
        let sources = analyses
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        bindings("example", &sources)
    }

    const SERVICE: &str = r#"
        defmodule Pricing.V1.PricingService.Service do
          use GRPC.Service, name: "pricing.v1.PricingService"
          rpc :GetQuote, Pricing.V1.Request, Pricing.V1.Response
        end

        defmodule Pricing.V1.PricingService.Stub do
          use GRPC.Stub, service: Pricing.V1.PricingService.Service
        end
    "#;

    #[test]
    fn resolves_direct_wrapped_and_delegated_clients() {
        let (candidates, diagnostics) = resolved(&[
            ("lib/pricing.pb.ex", SERVICE),
            (
                "lib/client.ex",
                r#"
                defmodule Pricing.Client do
                  alias Pricing.V1.PricingService.Stub
                  def direct(channel, request), do: Stub.get_quote(channel, request)
                  def wrapped(channel, request) do
                    Stub.get_quote(channel, request)
                  end
                  defdelegate delegated(channel, request), to: Stub, as: :get_quote
                  def message, do: %Pricing.V1.Request{}
                end
                "#,
            ),
        ]);

        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        for function in ["direct/2", "wrapped/2", "delegated/2"] {
            let symbol = format!("repo://example/elixir/Pricing.Client/{function}");
            assert!(candidates.iter().any(|candidate| {
                candidate.local_symbol.as_str() == symbol
                    && candidate.role == GrpcBindingRole::Client
                    && candidate.service == "pricing.v1.PricingService"
                    && candidate.method == "GetQuote"
                    && candidate.confidence == Confidence::Exact
                    && candidate.provenance == Provenance::Generated
            }));
        }
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.local_symbol.as_str().ends_with("/message/0"))
        );
    }

    #[test]
    fn resolves_server_callbacks_and_generated_client_wrappers() {
        let (candidates, diagnostics) = resolved(&[
            ("lib/pricing.pb.ex", SERVICE),
            (
                "lib/server.ex",
                r#"
                defmodule Pricing.Server do
                  alias Pricing.V1.PricingService.Service
                  use GRPC.Server, service: Service
                  defdelegate get_quote(request, stream), to: Pricing.Action, as: :call
                end

                defmodule Pricing.Client do
                  use ClientMacro,
                    service: Pricing.V1.PricingService,
                    stub: Pricing.V1.PricingService.Stub
                  def get_quote(request, opts \\ []), do: call(request, :get_quote, opts)
                end
                "#,
            ),
        ]);

        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        assert!(candidates.iter().any(|candidate| {
            candidate.local_symbol.as_str() == "repo://example/elixir/Pricing.Server/get_quote/2"
                && candidate.role == GrpcBindingRole::Server
                && candidate.confidence == Confidence::Exact
        }));
        for arity in [1, 2] {
            assert!(candidates.iter().any(|candidate| {
                candidate.local_symbol.as_str()
                    == format!("repo://example/elixir/Pricing.Client/get_quote/{arity}")
                    && candidate.role == GrpcBindingRole::Client
                    && candidate.confidence == Confidence::Exact
            }));
        }
    }

    #[test]
    fn diagnoses_dynamic_and_unsupported_grpc_shapes() {
        let (candidates, diagnostics) = resolved(&[
            ("lib/pricing.pb.ex", SERVICE),
            (
                "lib/dynamic.ex",
                r#"
            defmodule Dynamic.Service do
              use GRPC.Service, name: service_name()
              rpc dynamic_method(), Request, Response
            end

            defmodule Streaming.Service do
              use GRPC.Service, name: "pricing.v1.Streaming"
              rpc :Watch, Request, stream(Response)
            end

            defmodule Dynamic.Server do
              use GRPC.Server, service: Missing.Service
              def missing(request, stream), do: {request, stream}
            end

            defmodule Dynamic.Client do
              alias Pricing.V1.PricingService.Stub
              def missing(channel, request), do: Stub.missing(channel, request)
            end
            "#,
            ),
        ]);

        assert!(candidates.is_empty());
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "elixir.grpc_literal_unresolved"
                && diagnostic.detail.as_deref()
                    == Some("GRPC.Service requires a literal name option")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.detail.as_deref()
                == Some("gRPC service module Missing.Service was not found")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.detail.as_deref() == Some("streaming gRPC method Watch is not supported")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.detail.as_deref()
                == Some("gRPC method missing was not found on Pricing.V1.PricingService.Service")
        }));
    }
}
