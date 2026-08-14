use beholder_domain::{
    Confidence, DependencyOverride, DependencyRelation, EntityId, Observation, Provenance,
    SemanticRelation, StructuralRelation,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{error::Error, path::Path};
use tree_sitter::{Node, Parser};

pub const FRONTEND_VERSION: &str = "4";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ElixirAnalysis {
    modules: Vec<ElixirModule>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirModule {
    name: String,
    line: usize,
    functions: Vec<ElixirFunction>,
    references: Vec<ElixirModuleReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirFunction {
    name: String,
    arity: usize,
    line: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ElixirModuleReferenceKind {
    Import,
    Require,
    Use,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirModuleReference {
    name: String,
    kind: ElixirModuleReferenceKind,
    line: usize,
}

fn text<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn call_target<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    text(node.child_by_field_name("target")?, source)
}

fn arguments(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "arguments")
}

fn function_head<'a>(node: Node<'a>, source: &'a [u8]) -> Option<(&'a str, usize, usize)> {
    match node.kind() {
        "identifier" => Some((text(node, source)?, 0, 0)),
        "call" => {
            let name = call_target(node, source)?;
            let arguments = arguments(node);
            let max_arity = arguments.map_or(0, |arguments| arguments.named_child_count());
            let defaults = arguments.map_or(0, |arguments| {
                let mut cursor = arguments.walk();
                arguments
                    .named_children(&mut cursor)
                    .filter(|argument| {
                        argument.kind() == "binary_operator"
                            && argument
                                .child_by_field_name("operator")
                                .and_then(|operator| text(operator, source))
                                == Some("\\\\")
                    })
                    .count()
            });
            Some((name, max_arity - defaults, max_arity))
        }
        "binary_operator" => function_head(node.child_by_field_name("left")?, source),
        _ => None,
    }
}

fn collect(node: Node<'_>, source: &[u8], module: Option<usize>, modules: &mut Vec<ElixirModule>) {
    if node.kind() == "call" {
        match call_target(node, source) {
            Some("defmodule") => {
                if let Some(name) = arguments(node)
                    .and_then(|arguments| arguments.named_child(0))
                    .filter(|name| name.kind() == "alias")
                    .and_then(|name| text(name, source))
                {
                    let name = if let Some(name) = name.strip_prefix("Elixir.") {
                        name.into()
                    } else if let Some(parent) = module {
                        format!("{}.{}", modules[parent].name, name)
                    } else {
                        name.into()
                    };
                    let module = modules.len();
                    modules.push(ElixirModule {
                        name,
                        line: node.start_position().row + 1,
                        functions: Vec::new(),
                        references: Vec::new(),
                    });
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        collect(child, source, Some(module), modules);
                    }
                    return;
                }
            }
            Some("def" | "defp" | "defdelegate") => {
                if let Some(module) = module
                    && let Some((name, min_arity, max_arity)) = arguments(node)
                        .and_then(|arguments| arguments.named_child(0))
                        .and_then(|head| function_head(head, source))
                {
                    let functions = &mut modules[module].functions;
                    for arity in min_arity..=max_arity {
                        // ponytail: retain first-clause evidence until storage supports multiple
                        // evidence records for one semantic edge.
                        if !functions
                            .iter()
                            .any(|function| function.name == name && function.arity == arity)
                        {
                            functions.push(ElixirFunction {
                                name: name.into(),
                                arity,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                return;
            }
            Some(target @ ("import" | "require" | "use")) => {
                if let Some(module) = module
                    && let Some(name) = arguments(node)
                        .and_then(|arguments| arguments.named_child(0))
                        .filter(|name| name.kind() == "alias")
                        .and_then(|name| text(name, source))
                {
                    let kind = match target {
                        "import" => ElixirModuleReferenceKind::Import,
                        "require" => ElixirModuleReferenceKind::Require,
                        _ => ElixirModuleReferenceKind::Use,
                    };
                    let references = &mut modules[module].references;
                    // ponytail: retain first reference evidence until storage supports multiple
                    // evidence records for one semantic edge.
                    if !references
                        .iter()
                        .any(|reference| reference.name == name && reference.kind == kind)
                    {
                        references.push(ElixirModuleReference {
                            name: name.into(),
                            kind,
                            line: node.start_position().row + 1,
                        });
                    }
                }
                return;
            }
            Some("defmacro" | "defmacrop" | "quote") => return,
            _ => {}
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, module, modules);
    }
}

pub fn analyze(source: &str) -> Result<ElixirAnalysis, Box<dyn Error>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_elixir::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Elixir parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err("failed to parse Elixir source".into());
    }

    let mut modules = Vec::new();
    collect(tree.root_node(), source.as_bytes(), None, &mut modules);
    Ok(ElixirAnalysis { modules })
}

pub fn observations_from_analysis(
    repository: &str,
    analysis: &ElixirAnalysis,
    path: &Path,
) -> Vec<Observation> {
    let source_id = format!(
        "repo://{repository}/elixir-source/{}",
        path.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let mut observations = Vec::new();
    for module in &analysis.modules {
        let module_id = format!("repo://{repository}/elixir/{}", module.name);
        observations.push(Observation::structural(
            source_id.clone(),
            StructuralRelation::Defines,
            module_id.clone(),
            format!("{}:{}", path.display(), module.line),
        ));
        observations.extend(module.functions.iter().map(|function| {
            Observation::structural(
                module_id.clone(),
                StructuralRelation::Defines,
                format!("{module_id}/{}/{}", function.name, function.arity),
                format!("{}:{}", path.display(), function.line),
            )
        }));
        observations.extend(module.references.iter().map(|reference| {
            Observation::dependency(
                module_id.clone(),
                match reference.kind {
                    ElixirModuleReferenceKind::Import => DependencyRelation::Imports,
                    ElixirModuleReferenceKind::Require => DependencyRelation::Requires,
                    ElixirModuleReferenceKind::Use => DependencyRelation::Uses,
                },
                format!("elixir-module://{}", reference.name),
                format!("{}:{}", path.display(), reference.line),
            )
        }));
    }
    observations
}

pub fn resolve_workspace_modules(observations: &[Observation]) -> Vec<DependencyOverride> {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
    }) {
        let Some(name) = observation
            .to
            .as_str()
            .split_once("/elixir/")
            .map(|(_, name)| name)
            .filter(|name| !name.contains('/'))
        else {
            continue;
        };
        definitions
            .entry(name.into())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(observation.to.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.to.as_str().into()));
    }

    observations
        .iter()
        .filter_map(|observation| {
            let relation = match observation.relation {
                SemanticRelation::Dependency(relation @ DependencyRelation::Imports)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Requires)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Uses) => relation,
                _ => return None,
            };
            let name = observation.to.as_str().strip_prefix("elixir-module://")?;
            let target = definitions.get(name)?.as_ref()?;
            Some(DependencyOverride {
                from: observation.from.clone(),
                relation,
                unresolved_to: observation.to.clone(),
                resolved_to: EntityId::from(target.clone()),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Exact,
                provenance: Provenance::Ast,
            })
        })
        .collect()
}

pub fn observations(
    repository: &str,
    source: &str,
    path: &Path,
) -> Result<Vec<Observation>, Box<dyn Error>> {
    Ok(observations_from_analysis(
        repository,
        &analyze(source)?,
        path,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::SemanticRelation;

    #[test]
    fn emits_stable_module_and_function_definitions() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Payments do
              def create_payment(account, amount), do: {:ok, account, amount}
              defp normalize(value \\ nil), do: value
              defdelegate lookup(id, opts \\ []), to: Backend
              def create_payment(account, amount) when amount > 0, do: {:ok, account, amount}
            end
            "#,
            Path::new("lib/my_app/payments.ex"),
        )
        .unwrap();

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir-source/lib/my_app/payments.ex"
                && observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Payments"
                && observation.evidence.as_str() == "lib/my_app/payments.ex:2"
        }));
        let functions = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str() == "repo://payments/elixir/MyApp.Payments"
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            functions,
            vec![
                "repo://payments/elixir/MyApp.Payments/create_payment/2",
                "repo://payments/elixir/MyApp.Payments/normalize/0",
                "repo://payments/elixir/MyApp.Payments/normalize/1",
                "repo://payments/elixir/MyApp.Payments/lookup/1",
                "repo://payments/elixir/MyApp.Payments/lookup/2",
            ]
        );
    }

    #[test]
    fn models_module_references_without_expanding_macros() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Macro do
              defmacro __using__(_) do
                quote do
                  def generated, do: :ok
                end
              end
            end
            defmodule MyApp do
              defmodule Consumer do
                use MyApp.Macro, mode: :strict
                import External.Helpers, only: [help: 1]
                require External.Macros, as: Macros
                def own, do: :ok
              end
            end
            "#,
            Path::new("lib/my_app/consumer.ex"),
        )
        .unwrap();

        assert!(!observations.iter().any(|observation| {
            observation.to.as_str().ends_with("/__using__/1")
                || observation.to.as_str().ends_with("/generated/0")
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Uses)
                && observation.to.as_str() == "elixir-module://MyApp.Macro"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:11"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Imports)
                && observation.to.as_str() == "elixir-module://External.Helpers"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:12"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::Requires)
                && observation.to.as_str() == "elixir-module://External.Macros"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:13"
        }));

        let overrides = resolve_workspace_modules(&observations);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].relation, DependencyRelation::Uses);
        assert_eq!(
            overrides[0].resolved_to.as_str(),
            "repo://payments/elixir/MyApp.Macro"
        );
    }
}
