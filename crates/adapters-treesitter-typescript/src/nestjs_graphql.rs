use super::{SourceLanguage, analysis::source_stem, graphql::GraphqlResolverSource};
use beholder_adapters_graphql::GraphqlFacts;
use beholder_domain::{DependencyRelation, EntityFact, EntityKind, Observation};
use std::collections::BTreeMap;
use tree_sitter::{Node, Parser};

struct Resolver {
    parent: String,
    field: String,
    definition: String,
    line: usize,
}

fn parser(language: SourceLanguage) -> Option<Parser> {
    let mut parser = Parser::new();
    let grammar = match language {
        SourceLanguage::JavaScript | SourceLanguage::Jsx => tree_sitter_javascript::LANGUAGE,
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        SourceLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    parser.set_language(&grammar.into()).ok()?;
    Some(parser)
}

fn decorators(node: Node<'_>) -> Vec<Node<'_>> {
    let mut decorators = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "decorator")
        .collect::<Vec<_>>();
    let mut sibling = node.prev_named_sibling();
    while let Some(decorator) = sibling.filter(|sibling| sibling.kind() == "decorator") {
        decorators.push(decorator);
        sibling = decorator.prev_named_sibling();
    }
    decorators
}

fn call<'tree>(
    node: Node<'tree>,
    source: &[u8],
    aliases: &BTreeMap<String, String>,
) -> Option<(String, Node<'tree>)> {
    let call = node
        .named_child(0)?
        .kind()
        .eq("call_expression")
        .then(|| node.named_child(0))??;
    let name = call
        .child_by_field_name("function")?
        .utf8_text(source)
        .ok()?;
    Some((aliases.get(name)?.clone(), call))
}

fn string(node: Node<'_>, source: &[u8]) -> Option<String> {
    matches!(node.kind(), "string" | "template_string")
        .then(|| node.utf8_text(source).ok())
        .flatten()?
        .trim_matches(['\'', '"', '`'])
        .to_owned()
        .into()
}

fn named_option(node: Node<'_>, source: &[u8], option: &str) -> Option<String> {
    if node.kind() == "pair"
        && node
            .child_by_field_name("key")?
            .utf8_text(source)
            .ok()?
            .trim_matches(['\'', '"'])
            == option
    {
        return string(node.child_by_field_name("value")?, source);
    }
    node.named_children(&mut node.walk())
        .find_map(|child| named_option(child, source, option))
}

fn arguments(call: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    call.child_by_field_name("arguments")
        .into_iter()
        .flat_map(|arguments| {
            arguments
                .named_children(&mut arguments.walk())
                .collect::<Vec<_>>()
        })
}

fn field_name(call: Node<'_>, source: &[u8], fallback: &str) -> String {
    arguments(call)
        .find_map(|argument| {
            string(argument, source).or_else(|| named_option(argument, source, "name"))
        })
        .unwrap_or_else(|| fallback.into())
}

fn resolver_type(call: Node<'_>, source: &[u8]) -> Option<String> {
    let argument = arguments(call).next()?;
    if let Some(name) = string(argument, source) {
        return Some(name);
    }
    let body = (argument.kind() == "arrow_function")
        .then(|| argument.child_by_field_name("body"))
        .flatten()?;
    body.utf8_text(source)
        .ok()?
        .trim()
        .trim_matches(['[', ']'])
        .split('.')
        .next_back()
        .map(str::to_owned)
}

fn resolver_parent(
    node: Node<'_>,
    source: &[u8],
    aliases: &BTreeMap<String, String>,
) -> Option<String> {
    decorators(node).into_iter().find_map(|decorator| {
        let (name, call) = call(decorator, source, aliases)?;
        (name == "Resolver")
            .then(|| resolver_type(call, source))
            .flatten()
    })
}

fn collect(
    node: Node<'_>,
    source: &[u8],
    aliases: &BTreeMap<String, String>,
    resolvers: &mut Vec<Resolver>,
) {
    if node.kind() == "class_declaration" {
        let Some(class_name) = node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
        else {
            return;
        };
        let class_parent = resolver_parent(node, source, aliases);
        if let Some(body) = node.child_by_field_name("body") {
            for method in body
                .named_children(&mut body.walk())
                .filter(|method| method.kind() == "method_definition")
            {
                let Some(method_name) = method
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                else {
                    continue;
                };
                let method_parent = resolver_parent(method, source, aliases);
                let binding = decorators(method).into_iter().find_map(|decorator| {
                    let (name, call) = call(decorator, source, aliases)?;
                    let parent = match name.as_str() {
                        "Query" => "Query".into(),
                        "Mutation" => "Mutation".into(),
                        "Subscription" => "Subscription".into(),
                        "ResolveField" => method_parent.clone().or_else(|| class_parent.clone())?,
                        _ => return None,
                    };
                    Some((parent, field_name(call, source, method_name)))
                });
                if let Some((parent, field)) = binding {
                    resolvers.push(Resolver {
                        parent,
                        field,
                        definition: format!("{class_name}/{method_name}"),
                        line: method.start_position().row + 1,
                    });
                }
            }
        }
        return;
    }
    for child in node.named_children(&mut node.walk()) {
        collect(child, source, aliases, resolvers);
    }
}

pub(super) fn facts(repository: &str, input: &GraphqlResolverSource<'_>) -> GraphqlFacts {
    if !input.source.contains('@') {
        return GraphqlFacts::default();
    }
    let aliases = input
        .analysis
        .imports
        .iter()
        .filter(|import| import.source == "@nestjs/graphql")
        .flat_map(|import| {
            import
                .bindings
                .iter()
                .map(|binding| (binding.local.clone(), binding.imported.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    if aliases.is_empty() {
        return GraphqlFacts::default();
    }
    let Some(tree) =
        parser(input.analysis.language).and_then(|mut parser| parser.parse(input.source, None))
    else {
        return GraphqlFacts::default();
    };
    let mut resolvers = Vec::new();
    collect(
        tree.root_node(),
        input.source.as_bytes(),
        &aliases,
        &mut resolvers,
    );
    let module = format!(
        "repo://{repository}/{}/{}",
        input.analysis.language.id_segment(),
        source_stem(input.path)
    );
    let definitions = input
        .analysis
        .definitions
        .iter()
        .map(|definition| definition.qualified_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut facts = GraphqlFacts::default();
    for resolver in resolvers {
        if !definitions.contains(resolver.definition.as_str()) {
            continue;
        }
        let field = format!("graphql-field://{}/{}", resolver.parent, resolver.field);
        facts
            .entities
            .push(EntityFact::new(field.clone(), EntityKind::GraphqlField, None).unwrap());
        facts.observations.push(Observation::dependency(
            field,
            DependencyRelation::ResolvedBy,
            format!("{module}/{}", resolver.definition),
            format!("{}:{}", input.path.display(), resolver.line),
        ));
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphqlResolverInput, analyze, collect_graphql_resolvers};
    use beholder_domain::SemanticRelation;
    use std::path::Path;

    #[test]
    fn maps_root_and_nested_resolvers_when_nest_graphql_is_installed() {
        let source = r#"
            import {
              Mutation, Query as RootQuery, ResolveField, Resolver, Subscription
            } from '@nestjs/graphql';
            @Resolver(() => Author)
            export class AuthorResolver {
              @RootQuery(() => Author, { name: 'authorById' }) author() {}
              @Mutation() updateAuthor() {}
              @Subscription(() => Author) authorChanged() {}
              @ResolveField('posts') resolvePosts() {}
            }
        "#;
        let analysis = analyze(source, SourceLanguage::TypeScript).unwrap();
        let source = GraphqlResolverSource {
            path: Path::new("src/author.resolver.ts"),
            analysis: &analysis,
            source,
        };
        assert!(
            collect_graphql_resolvers(GraphqlResolverInput {
                repository: "example",
                sources: std::slice::from_ref(&source),
                manifests: &[],
            })
            .observations
            .is_empty()
        );
        let facts = collect_graphql_resolvers(GraphqlResolverInput {
            repository: "example",
            sources: std::slice::from_ref(&source),
            manifests: &[(
                Path::new("package.json"),
                r#"{"dependencies":{"@nestjs/graphql":"13.1.0"}}"#,
            )],
        });

        for (field, definition) in [
            ("graphql-field://Query/authorById", "AuthorResolver/author"),
            (
                "graphql-field://Mutation/updateAuthor",
                "AuthorResolver/updateAuthor",
            ),
            (
                "graphql-field://Subscription/authorChanged",
                "AuthorResolver/authorChanged",
            ),
            (
                "graphql-field://Author/posts",
                "AuthorResolver/resolvePosts",
            ),
        ] {
            assert!(
                facts.observations.iter().any(|observation| {
                    observation.from.as_str() == field
                        && observation.relation
                            == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                        && observation.to.as_str()
                            == format!("repo://example/typescript/src/author.resolver/{definition}")
                }),
                "missing {field} -> {definition}: {facts:#?}"
            );
        }
    }
}
