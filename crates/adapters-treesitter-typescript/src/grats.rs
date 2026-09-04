use super::{SourceLanguage, analysis::source_stem, graphql::GraphqlResolverSource};
use beholder_adapters_graphql::GraphqlFacts;
use beholder_domain::{DependencyRelation, EntityFact, EntityKind, Observation};
use std::{borrow::Cow, collections::BTreeMap};
use tree_sitter::{Node, Parser};

struct Resolver {
    parent_type: String,
    field: String,
    definition: String,
    line: usize,
}

enum Annotation {
    Root(&'static str, Option<String>),
    Field(Option<String>),
}

fn annotation(comment: &str) -> Option<Annotation> {
    for line in comment.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        for (tag, root_type) in [
            ("@gqlQueryField", "Query"),
            ("@gqlMutationField", "Mutation"),
            ("@gqlSubscriptionField", "Subscription"),
        ] {
            if let Some(field) = line.strip_prefix(tag).map(str::trim) {
                return Some(Annotation::Root(
                    root_type,
                    (!field.is_empty()).then(|| field.into()),
                ));
            }
        }
        if let Some(field) = line.strip_prefix("@gqlField") {
            let field = field.trim();
            return Some(Annotation::Field((!field.is_empty()).then(|| field.into())));
        }
    }
    None
}

fn leading_comment<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let mut declaration = node;
    while let Some(parent) = declaration
        .parent()
        .filter(|parent| matches!(parent.kind(), "lexical_declaration" | "export_statement"))
    {
        declaration = parent;
    }
    let start = declaration.start_byte();
    let prefix = std::str::from_utf8(source.get(..start)?).ok()?;
    let comment = prefix.rsplit_once("/**")?.1;
    let (comment, trailing) = comment.rsplit_once("*/")?;
    (trailing.trim().is_empty() || trailing.trim_start().starts_with('@')).then_some(comment)
}

fn parameter_type<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let node = node
        .child_by_field_name("value")
        .filter(|value| matches!(value.kind(), "arrow_function" | "function_expression"))
        .unwrap_or(node);
    let parameter = node.child_by_field_name("parameters")?.named_child(0)?;
    let annotation = parameter.child_by_field_name("type")?;
    annotation
        .utf8_text(source)
        .ok()?
        .trim()
        .strip_prefix(':')
        .map(str::trim)
}

fn resolver(
    node: Node<'_>,
    source: &[u8],
    scope: &[String],
    types: &BTreeMap<String, String>,
) -> Option<Resolver> {
    let name = node.child_by_field_name("name")?.utf8_text(source).ok()?;
    let annotation = annotation(leading_comment(node, source)?)?;
    let (parent_type, field) = match annotation {
        Annotation::Root(parent, field) => (parent.into(), field.unwrap_or_else(|| name.into())),
        Annotation::Field(field) => {
            let owner = scope
                .last()
                .map(String::as_str)
                .or_else(|| parameter_type(node, source))?;
            (
                types.get(owner)?.clone(),
                field.unwrap_or_else(|| name.into()),
            )
        }
    };
    Some(Resolver {
        parent_type,
        field,
        definition: scope
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(name))
            .collect::<Vec<_>>()
            .join("/"),
        line: node.start_position().row + 1,
    })
}

fn collect(
    node: Node<'_>,
    source: &[u8],
    scope: &mut Vec<String>,
    types: &BTreeMap<String, String>,
    resolvers: &mut Vec<Resolver>,
) {
    match node.kind() {
        "class_declaration" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
            else {
                return;
            };
            scope.push(name.into());
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    collect(child, source, scope, types, resolvers);
                }
            }
            scope.pop();
            return;
        }
        "method_definition" | "function_declaration" | "generator_function_declaration" => {
            if let Some(resolver) = resolver(node, source, scope, types) {
                resolvers.push(resolver);
            }
            return;
        }
        "variable_declarator"
            if node.child_by_field_name("value").is_some_and(|value| {
                matches!(value.kind(), "arrow_function" | "function_expression")
            }) =>
        {
            if let Some(resolver) = resolver(node, source, scope, types) {
                resolvers.push(resolver);
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, scope, types, resolvers);
    }
}

fn type_annotation(comment: &str, code_name: &str) -> Option<String> {
    comment.lines().find_map(|line| {
        let line = line.trim().trim_start_matches('*').trim();
        ["@gqlType", "@gqlInterface"].iter().find_map(|tag| {
            line.strip_prefix(tag).map(|name| {
                let name = name.trim();
                if name.is_empty() { code_name } else { name }.into()
            })
        })
    })
}

fn collect_types(node: Node<'_>, source: &[u8], types: &mut Vec<(String, String)>) {
    if matches!(
        node.kind(),
        "class_declaration" | "interface_declaration" | "type_alias_declaration"
    ) && let Some(name) = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        && let Some(comment) = leading_comment(node, source)
        && let Some(graphql_name) = type_annotation(comment, name)
    {
        types.push((name.into(), graphql_name));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_types(child, source, types);
    }
}

pub(super) fn types(input: &GraphqlResolverSource<'_>) -> Vec<(String, String)> {
    let Some((mut parser, source)) = parser_and_source(input) else {
        return Vec::new();
    };
    let Some(tree) = parser.parse(source.as_ref(), None) else {
        return Vec::new();
    };
    let mut types = Vec::new();
    collect_types(tree.root_node(), source.as_bytes(), &mut types);
    types
}

fn parser_and_source<'a>(input: &GraphqlResolverSource<'a>) -> Option<(Parser, Cow<'a, str>)> {
    let mut parser = Parser::new();
    let grammar = match input.analysis.language {
        SourceLanguage::JavaScript => tree_sitter_javascript::LANGUAGE,
        SourceLanguage::Svelte => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        SourceLanguage::Jsx | SourceLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    parser.set_language(&grammar.into()).ok()?;
    let source = if input.analysis.language == SourceLanguage::Svelte {
        Cow::Owned(super::svelte::embedded_source(input.source)?)
    } else {
        Cow::Borrowed(input.source)
    };
    Some((parser, source))
}

pub(super) struct FactsInput<'a> {
    pub repository: &'a str,
    pub source: &'a GraphqlResolverSource<'a>,
    pub types: &'a BTreeMap<String, String>,
}

pub(super) fn facts(input: FactsInput<'_>) -> GraphqlFacts {
    let FactsInput {
        repository,
        source: input,
        types,
    } = input;
    if !input.source.contains("@gql") {
        return GraphqlFacts::default();
    }
    let Some((mut parser, source)) = parser_and_source(input) else {
        return GraphqlFacts::default();
    };
    let Some(tree) = parser.parse(source.as_ref(), None) else {
        return GraphqlFacts::default();
    };
    let mut resolvers = Vec::new();
    collect(
        tree.root_node(),
        source.as_bytes(),
        &mut Vec::new(),
        types,
        &mut resolvers,
    );
    let module_id = format!(
        "repo://{}/{}/{}",
        repository,
        input.analysis.language.id_segment(),
        source_stem(input.path)
    );
    let definitions = input
        .analysis
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.qualified_name.as_str(),
                format!("{module_id}/{}", definition.qualified_name),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut facts = GraphqlFacts::default();
    for resolver in resolvers {
        let Some(definition) = definitions.get(resolver.definition.as_str()) else {
            continue;
        };
        let field = format!(
            "graphql-field://{}/{}",
            resolver.parent_type, resolver.field
        );
        facts
            .entities
            .push(EntityFact::new(field.clone(), EntityKind::GraphqlField, None).unwrap());
        facts.observations.push(Observation::dependency(
            field,
            DependencyRelation::ResolvedBy,
            definition.clone(),
            format!("{}:{}", input.path.display(), resolver.line),
        ));
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;
    use beholder_domain::SemanticRelation;
    use std::path::Path;

    #[test]
    fn requires_package_and_maps_exported_subscription_generator() {
        let source = r#"
            /** @gqlSubscriptionField giftCardPaymentCompleted */
            export async function* subscriptionGiftCardPaymentCompleted() { yield null }

            /** @gqlQueryField */
            export function walletBalance() { return 10 }

            /** @gqlType Customer */
            export class CustomerModel {
              /** @gqlField displayName */
              name() { return "Ada" }
            }

            /** @gqlField emailAddress */
            export function email(customer: CustomerModel) { return customer.email }

            /** @gqlField loyaltyStatus */
            export const loyalty = (customer: CustomerModel) => customer.loyalty
        "#;
        let analysis = analyze(source, SourceLanguage::TypeScript).unwrap();
        let source = GraphqlResolverSource {
            analysis: &analysis,
            source,
            path: Path::new("src/subscription.ts"),
        };
        assert!(
            crate::collect_graphql_resolvers(crate::GraphqlResolverInput {
                repository: "example",
                sources: std::slice::from_ref(&source),
                manifests: &[],
            })
            .observations
            .is_empty()
        );
        let facts = crate::collect_graphql_resolvers(crate::GraphqlResolverInput {
            repository: "example",
            sources: std::slice::from_ref(&source),
            manifests: &[
                (
                    Path::new("packages/gateway/package.json"),
                    r#"{"devDependencies":{"grats":"0.0.34"}}"#,
                ),
                (Path::new("package.json"), r#"{"private":true}"#),
            ],
        });

        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str()
                == "graphql-field://Subscription/giftCardPaymentCompleted"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str()
                    == "repo://example/typescript/src/subscription/subscriptionGiftCardPaymentCompleted"
        }));
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Query/walletBalance"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str()
                    == "repo://example/typescript/src/subscription/walletBalance"
        }));
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Customer/displayName"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str()
                    == "repo://example/typescript/src/subscription/CustomerModel/name"
        }));
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Customer/emailAddress"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str() == "repo://example/typescript/src/subscription/email"
        }));
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Customer/loyaltyStatus"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str() == "repo://example/typescript/src/subscription/loyalty"
        }));
    }

    #[test]
    fn maps_resolvers_from_svelte_instance_scripts() {
        let annotation = ["/", "** @gqlQueryField */"].concat();
        let source = format!(
            r#"
            <script lang="ts">
              {annotation}
              export function greeting() { return "hello" }
            </script>
        "#
        );
        let analysis = analyze(&source, SourceLanguage::Svelte).unwrap();
        let source = GraphqlResolverSource {
            analysis: &analysis,
            source: &source,
            path: Path::new("src/greeting.svelte"),
        };

        let facts = crate::collect_graphql_resolvers(crate::GraphqlResolverInput {
            repository: "example",
            sources: std::slice::from_ref(&source),
            manifests: &[(
                Path::new("package.json"),
                r#"{"devDependencies":{"grats":"0.0.34"}}"#,
            )],
        });

        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Query/greeting"
                && observation.to.as_str()
                    == "repo://example/typescript/src/greeting.svelte/greeting"
        }));
    }
}
