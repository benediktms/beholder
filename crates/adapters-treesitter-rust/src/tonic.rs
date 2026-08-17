use super::model::{RustAnalysis, TonicAnalysis, TonicBinding, TonicGeneratedMethod};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, GrpcBindingCandidate,
    GrpcBindingRole, Provenance, RpcCardinality,
};
use std::{collections::BTreeMap, path::Path};
use tree_sitter::Node;

fn words(text: &str) -> Vec<&str> {
    text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
        .collect()
}

fn snake_to_pascal(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect()
}

fn imported_services(
    root: Node<'_>,
    source: &[u8],
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut clients = BTreeMap::new();
    let mut servers = BTreeMap::new();
    walk(root, &mut |node| {
        if node.kind() != "use_declaration" {
            return;
        }
        let Ok(text) = node.utf8_text(source) else {
            return;
        };
        let tokens = words(text);
        for (index, module) in tokens.iter().enumerate() {
            let (service, suffix, target) = if let Some(service) = module.strip_suffix("_client") {
                (service, "Client", &mut clients)
            } else if let Some(service) = module.strip_suffix("_server") {
                (service, "", &mut servers)
            } else {
                continue;
            };
            let service = snake_to_pascal(service);
            let type_name = format!("{service}{suffix}");
            if tokens[index + 1..].contains(&type_name.as_str()) {
                target.insert(type_name, service);
            }
        }
    });
    (clients, servers)
}

fn walk(node: Node<'_>, visit: &mut impl FnMut(Node<'_>)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

fn has_only_wrapper_calls(node: Node<'_>, source: &[u8]) -> bool {
    let mut valid = true;
    walk(node, &mut |candidate| {
        if candidate.kind() != "call_expression" {
            return;
        }
        let Some(function) = candidate.child_by_field_name("function") else {
            return;
        };
        if function.kind() != "field_expression" {
            return;
        }
        let method = function
            .child_by_field_name("field")
            .and_then(|field| field.utf8_text(source).ok());
        valid &= matches!(method, Some("expect" | "unwrap"));
    });
    valid
}

pub(super) fn analyze(
    root: Node<'_>,
    source: &[u8],
    functions: &[(String, String, Node<'_>)],
) -> TonicAnalysis {
    let (clients, servers) = imported_services(root, source);
    let mut analysis = TonicAnalysis::default();
    walk(root, &mut |node| {
        if node.kind() == "macro_invocation"
            && let Ok(text) = node.utf8_text(source)
            && text.starts_with("tonic::include_proto!")
            && let Some(package) = text.split('"').nth(1)
        {
            analysis.packages.push(package.into());
        }
    });

    let returned_clients = functions
        .iter()
        .filter_map(|(name, _, function)| {
            let return_type = function
                .child_by_field_name("return_type")?
                .utf8_text(source)
                .ok()?;
            clients
                .iter()
                .find(|(type_name, _)| return_type.contains(type_name.as_str()))
                .map(|(_, service)| (name.clone(), service.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    for (_, qualified_name, function) in functions {
        let mut local_clients = BTreeMap::new();
        walk(*function, &mut |node| {
            if node.kind() != "let_declaration" {
                return;
            }
            let Some(pattern) = node.child_by_field_name("pattern") else {
                return;
            };
            let Some(value) = node.child_by_field_name("value") else {
                return;
            };
            let Ok(pattern) = pattern.utf8_text(source) else {
                return;
            };
            let Ok(text) = value.utf8_text(source) else {
                return;
            };
            if let Some((_, service)) = clients
                .iter()
                .find(|(type_name, _)| text.contains(type_name.as_str()))
                .or_else(|| {
                    has_only_wrapper_calls(value, source).then(|| {
                        returned_clients
                            .iter()
                            .find(|(name, _)| text.contains(&format!("{name}(")))
                    })?
                })
            {
                local_clients.insert(
                    pattern.trim_start_matches("mut ").to_owned(),
                    service.clone(),
                );
            }
        });
        walk(*function, &mut |node| {
            if node.kind() != "call_expression" {
                return;
            }
            let Some(field) = node.child_by_field_name("function") else {
                return;
            };
            if field.kind() != "field_expression" {
                return;
            }
            let Some(receiver_node) = field.child_by_field_name("value") else {
                return;
            };
            let Some(method) = field.child_by_field_name("field") else {
                return;
            };
            let (Ok(receiver), Ok(method)) =
                (receiver_node.utf8_text(source), method.utf8_text(source))
            else {
                return;
            };
            let service = local_clients.get(receiver).cloned().or_else(|| {
                (!matches!(method, "expect" | "unwrap")
                    && has_only_wrapper_calls(receiver_node, source))
                .then(|| {
                    returned_clients.iter().find_map(|(name, service)| {
                        receiver
                            .contains(&format!("{name}("))
                            .then(|| service.clone())
                    })
                })
                .flatten()
            });
            if let Some(service) = service {
                let line = node.start_position().row + 1;
                analysis.client_calls.push(TonicBinding {
                    function: qualified_name.clone(),
                    service,
                    method: method.into(),
                    line,
                });
                analysis
                    .recognized_receiver_calls
                    .push((line, method.into()));
            }
        });

        if let Some(owner) = qualified_name.strip_prefix("impl/")
            && let Some((trait_name, _)) = owner.split_once("-for-")
            && let Some(service) = servers.get(trait_name)
        {
            analysis.server_methods.push(TonicBinding {
                function: qualified_name.clone(),
                service: service.clone(),
                method: qualified_name.rsplit('/').next().unwrap_or_default().into(),
                line: function.start_position().row + 1,
            });
        }

        let mut parts = qualified_name.split('/');
        let Some(module) = parts.next() else { continue };
        let Some(service) = module.strip_suffix("_client").map(snake_to_pascal) else {
            continue;
        };
        if qualified_name.contains(&format!("impl/{service}Client")) {
            analysis.generated_methods.push(TonicGeneratedMethod {
                service,
                method: qualified_name.rsplit('/').next().unwrap_or_default().into(),
                line: function.start_position().row + 1,
            });
        }
    }
    analysis.packages.sort();
    analysis.packages.dedup();
    analysis
}

fn module_id(repository: &str, path: &Path) -> String {
    let module = path
        .strip_prefix("src")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    format!("repo://{repository}/rust/{module}")
}

pub fn bindings(
    repository: &str,
    sources: &[(&Path, &RustAnalysis)],
) -> (Vec<GrpcBindingCandidate>, Vec<AnalysisDiagnostic>) {
    let packages = sources
        .iter()
        .flat_map(|(_, analysis)| &analysis.tonic.packages)
        .collect::<std::collections::BTreeSet<_>>();
    let package = (packages.len() == 1)
        .then(|| packages.first().map(|package| package.as_str()))
        .flatten();
    let generated = sources
        .iter()
        .flat_map(|(path, analysis)| {
            analysis.tonic.generated_methods.iter().map(move |method| {
                (
                    (method.service.as_str(), method.method.as_str()),
                    (*path, method.line),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    for (path, analysis) in sources {
        for (role, binding) in analysis
            .tonic
            .client_calls
            .iter()
            .map(|binding| (GrpcBindingRole::Client, binding))
            .chain(
                analysis
                    .tonic
                    .server_methods
                    .iter()
                    .map(|binding| (GrpcBindingRole::Server, binding)),
            )
        {
            let Some(package) = package else {
                diagnostics.push(AnalysisDiagnostic {
                    code: "rust.tonic_package_unresolved".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path: (*path).into(),
                    line: u32::try_from(binding.line).ok(),
                    detail: Some(
                        "tonic binding requires exactly one include_proto! package".into(),
                    ),
                });
                continue;
            };
            let service = format!("{package}.{}", binding.service);
            let method = snake_to_pascal(&binding.method);
            let local_symbol = format!("{}/{}", module_id(repository, path), binding.function);
            candidates.push(GrpcBindingCandidate {
                local_symbol: local_symbol.as_str().into(),
                role,
                service: service.clone(),
                method: method.clone(),
                cardinality: RpcCardinality::Unary,
                evidence: format!("{}:{}", path.display(), binding.line).into(),
                confidence: Confidence::Inferred,
                provenance: Provenance::Ast,
            });
            if let Some((generated_path, line)) =
                generated.get(&(binding.service.as_str(), binding.method.as_str()))
            {
                candidates.push(GrpcBindingCandidate {
                    local_symbol: local_symbol.as_str().into(),
                    role,
                    service,
                    method,
                    cardinality: RpcCardinality::Unary,
                    evidence: format!("{}:{line}", generated_path.display()).into(),
                    confidence: Confidence::Exact,
                    provenance: Provenance::Generated,
                });
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
            .collect::<Vec<(std::path::PathBuf, RustAnalysis)>>();
        let sources = analyses
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        bindings("example", &sources)
    }

    #[test]
    fn infers_include_proto_client_and_server_bindings() {
        let (candidates, diagnostics) = resolved(&[
            ("src/protocol.rs", "tonic::include_proto!(\"pricing.v1\");"),
            (
                "src/client.rs",
                "use contract::pricing_client::PricingClient; \
                 async fn connect() -> Result<PricingClient<Channel>, Error> { todo!() } \
                 async fn quote() { \
                     let response = connect().await.unwrap().get_quote(()).await.unwrap(); \
                     response.into_inner().try_into(); \
                 }",
            ),
            (
                "src/server.rs",
                "use contract::pricing_server::{Pricing, PricingServer}; \
                 struct Handler; \
                 impl Pricing for Handler { async fn get_quote(&self) {} }",
            ),
        ]);

        assert!(diagnostics.is_empty());
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.role == GrpcBindingRole::Client)
                .count(),
            1
        );
        for (symbol, role) in [
            ("repo://example/rust/client/quote", GrpcBindingRole::Client),
            (
                "repo://example/rust/server/impl/Pricing-for-Handler/get_quote",
                GrpcBindingRole::Server,
            ),
        ] {
            assert!(candidates.iter().any(|candidate| {
                candidate.local_symbol.as_str() == symbol
                    && candidate.role == role
                    && candidate.service == "pricing.v1.Pricing"
                    && candidate.method == "GetQuote"
                    && candidate.confidence == Confidence::Inferred
            }));
        }
    }

    #[test]
    fn generated_client_method_corroborates_inferred_call() {
        let (candidates, _) = resolved(&[
            ("src/protocol.rs", "tonic::include_proto!(\"pricing.v1\");"),
            (
                "src/generated.rs",
                "mod pricing_client { \
                     pub struct PricingClient<T>(T); \
                     impl<T> PricingClient<T> { pub async fn get_quote(&mut self) {} } \
                 }",
            ),
            (
                "src/app.rs",
                "use crate::pricing_client::PricingClient; \
                 async fn quote() { \
                     let mut client = PricingClient::new(); \
                     client.get_quote().await; \
                 }",
            ),
        ]);
        let call = candidates
            .iter()
            .filter(|candidate| candidate.local_symbol.as_str() == "repo://example/rust/app/quote")
            .collect::<Vec<_>>();
        assert_eq!(call.len(), 2);
        assert!(call.iter().any(|candidate| {
            candidate.confidence == Confidence::Inferred && candidate.provenance == Provenance::Ast
        }));
        assert!(call.iter().any(|candidate| {
            candidate.confidence == Confidence::Exact
                && candidate.provenance == Provenance::Generated
                && candidate.evidence.as_str().starts_with("src/generated.rs:")
        }));
    }

    #[test]
    fn unrelated_receiver_method_remains_unresolved() {
        let (candidates, _) = resolved(&[
            ("src/protocol.rs", "tonic::include_proto!(\"pricing.v1\");"),
            (
                "src/app.rs",
                "async fn quote(client: OtherClient) { client.get_quote().await; }",
            ),
        ]);
        assert!(candidates.is_empty());
    }
}
