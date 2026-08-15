use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, EntityId, Observation, Provenance, SemanticRelation, StructuralRelation,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::{error::Error, path::Path};
use tree_sitter::{Node, Parser};

pub const FRONTEND_VERSION: &str = "9";
pub const RESOLVER_VERSION: &str = "4";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ElixirAnalysis {
    modules: Vec<ElixirModule>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirModule {
    name: String,
    line: usize,
    functions: Vec<ElixirFunction>,
    callbacks: Vec<ElixirFunction>,
    using_functions: Vec<ElixirFunction>,
    struct_fields: Vec<ElixirStructField>,
    implements: Vec<ElixirModuleReference>,
    aliases: Vec<ElixirAlias>,
    references: Vec<ElixirModuleReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirFunction {
    name: String,
    arity: usize,
    line: usize,
    calls: Vec<ElixirCall>,
    struct_uses: Vec<ElixirStructUse>,
    imports: Vec<ElixirModuleReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirCall {
    module: Option<String>,
    name: String,
    arity: usize,
    line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirAlias {
    name: String,
    target: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirStructField {
    name: String,
    line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirStructUse {
    module: String,
    line: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ElixirModuleReferenceKind {
    Behaviour,
    Import,
    Require,
    Use,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ElixirModuleReference {
    name: String,
    kind: ElixirModuleReferenceKind,
    line: usize,
    only: Option<BTreeSet<String>>,
    except: BTreeSet<String>,
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

fn has_do_block(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| child.kind() == "do_block")
}

fn piped_argument(node: Node<'_>, source: &[u8]) -> usize {
    node.parent()
        .filter(|parent| parent.kind() == "binary_operator")
        .filter(|parent| {
            parent
                .child_by_field_name("operator")
                .and_then(|operator| text(operator, source))
                == Some("|>")
        })
        .and_then(|parent| parent.child_by_field_name("right"))
        .is_some_and(|right| right == node) as usize
}

fn parsed_call(node: Node<'_>, source: &[u8]) -> Option<ElixirCall> {
    let target = node.child_by_field_name("target")?;
    let (module, name) = match target.kind() {
        "identifier" => (None, text(target, source)?.to_owned()),
        "dot" => {
            let left = target.child_by_field_name("left")?;
            let right = target.child_by_field_name("right")?;
            if left.kind() != "alias" && text(left, source) != Some("__MODULE__") {
                return None;
            }
            (
                Some(text(left, source)?.to_owned()),
                text(right, source)?.to_owned(),
            )
        }
        _ => return None,
    };
    let arity = arguments(node).map_or(0, |arguments| arguments.named_child_count())
        + piped_argument(node, source);
    Some(ElixirCall {
        module,
        name,
        arity,
        line: node.start_position().row + 1,
    })
}

fn collect_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<ElixirCall>) {
    if node.kind() == "call" {
        let target = call_target(node, source);
        if target == Some("quote") {
            return;
        }
        if !has_do_block(node)
            && !matches!(
                target,
                Some(
                    "alias"
                        | "def"
                        | "defdelegate"
                        | "defmacro"
                        | "defmacrop"
                        | "defmodule"
                        | "defp"
                        | "import"
                        | "require"
                        | "use"
                )
            )
            && let Some(call) = parsed_call(node, source)
        {
            calls.push(call);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, calls);
    }
}

fn function_calls(node: Node<'_>, source: &[u8]) -> Vec<ElixirCall> {
    let mut calls = Vec::new();
    if let Some(arguments) = arguments(node) {
        let mut cursor = arguments.walk();
        for body in arguments.named_children(&mut cursor).skip(1) {
            collect_calls(body, source, &mut calls);
        }
    }
    let mut cursor = node.walk();
    for body in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "do_block")
    {
        collect_calls(body, source, &mut calls);
    }
    calls
}

fn keyword_token<'a>(node: Node<'a>, source: &'a [u8], key: &str) -> Option<&'a str> {
    let arguments = text(arguments(node)?, source)?;
    let value = arguments.split_once(&format!("{key}:"))?.1.trim_start();
    let value = value.strip_prefix(':').unwrap_or(value);
    let end = value
        .find(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '_' | '.' | '!' | '?')
        })
        .unwrap_or(value.len());
    (end > 0).then_some(&value[..end])
}

fn keyword_value<'a>(node: Node<'a>, source: &'a [u8], key: &str) -> Option<Node<'a>> {
    let arguments = arguments(node)?;
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "keywords")
        .flat_map(|keywords| {
            let mut cursor = keywords.walk();
            keywords.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .find(|pair| {
            pair.child_by_field_name("key")
                .and_then(|key_node| text(key_node, source))
                .is_some_and(|name| name.trim().trim_end_matches(':') == key)
        })
        .and_then(|pair| pair.child_by_field_name("value"))
}

fn function_filter(node: Node<'_>, source: &[u8], key: &str) -> Option<BTreeSet<String>> {
    let value = keyword_value(node, source, key)?;
    if value.kind() == "atom" {
        return (text(value, source) != Some(":functions")).then(BTreeSet::new);
    }
    let mut functions = BTreeSet::new();
    let mut cursor = value.walk();
    for pair in value
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "keywords")
        .flat_map(|keywords| {
            let mut cursor = keywords.walk();
            keywords.named_children(&mut cursor).collect::<Vec<_>>()
        })
    {
        let Some(name) = pair
            .child_by_field_name("key")
            .and_then(|key| text(key, source))
            .map(|name| name.trim().trim_end_matches(':'))
        else {
            continue;
        };
        let Some(arity) = pair
            .child_by_field_name("value")
            .and_then(|arity| text(arity, source))
            .and_then(|arity| arity.parse::<usize>().ok())
        else {
            continue;
        };
        functions.insert(format!("{name}/{arity}"));
    }
    Some(functions)
}

fn alias_names(node: Node<'_>, source: &[u8]) -> Vec<String> {
    match node.kind() {
        "alias" => text(node, source).map_or_else(Vec::new, |name| vec![name.into()]),
        "list" | "tuple" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .flat_map(|child| alias_names(child, source))
                .collect()
        }
        "dot" => {
            let Some(prefix) = node
                .child_by_field_name("left")
                .and_then(|left| text(left, source))
            else {
                return Vec::new();
            };
            let Some(children) = node.child_by_field_name("right") else {
                return Vec::new();
            };
            alias_names(children, source)
                .into_iter()
                .map(|name| format!("{prefix}.{name}"))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn collect_struct_uses(node: Node<'_>, source: &[u8], uses: &mut Vec<ElixirStructUse>) {
    if node.kind() == "struct"
        && let Some(module) = node
            .named_child(0)
            .filter(|module| module.kind() == "alias")
            .and_then(|module| text(module, source))
    {
        uses.push(ElixirStructUse {
            module: module.into(),
            line: node.start_position().row + 1,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_struct_uses(child, source, uses);
    }
}

fn function_struct_uses(node: Node<'_>, source: &[u8]) -> Vec<ElixirStructUse> {
    let mut uses = Vec::new();
    collect_struct_uses(node, source, &mut uses);
    uses
}

fn push_function(
    functions: &mut Vec<ElixirFunction>,
    node: Node<'_>,
    source: &[u8],
    aliases: &[ElixirAlias],
    references: &[ElixirModuleReference],
    current_module: &str,
) {
    let Some((name, min_arity, max_arity)) = arguments(node)
        .and_then(|arguments| arguments.named_child(0))
        .and_then(|head| function_head(head, source))
        .filter(|(name, _, _)| *name != "unquote")
    else {
        return;
    };
    let delegate = if call_target(node, source) == Some("defdelegate") {
        keyword_token(node, source, "to").map(|module| {
            (
                module.to_owned(),
                keyword_token(node, source, "as").unwrap_or(name).to_owned(),
            )
        })
    } else {
        None
    };
    for arity in min_arity..=max_arity {
        let mut calls = delegate.as_ref().map_or_else(
            || function_calls(node, source),
            |(module, name)| {
                vec![ElixirCall {
                    module: Some(module.clone()),
                    name: name.clone(),
                    arity,
                    line: node.start_position().row + 1,
                }]
            },
        );
        for call in &mut calls {
            if let Some(module) = &mut call.module {
                *module = expand_alias(module, aliases, current_module);
            }
        }
        let mut struct_uses = function_struct_uses(node, source);
        for r#use in &mut struct_uses {
            r#use.module = expand_alias(&r#use.module, aliases, current_module);
        }
        // ponytail: retain first-clause evidence until storage supports multiple
        // evidence records for one semantic edge.
        if let Some(index) = functions
            .iter()
            .position(|function| function.name == name && function.arity == arity)
        {
            let function = &mut functions[index];
            for call in calls {
                if !function.calls.iter().any(|existing| {
                    existing.module == call.module
                        && existing.name == call.name
                        && existing.arity == call.arity
                }) {
                    function.calls.push(call);
                }
            }
            for import in references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Import)
            {
                if !function.imports.contains(import) {
                    function.imports.push(import.clone());
                }
            }
            continue;
        }
        functions.push(ElixirFunction {
            name: name.into(),
            arity,
            line: node.start_position().row + 1,
            calls,
            struct_uses,
            imports: references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Import)
                .cloned()
                .collect(),
        });
    }
}

fn collect_quoted_functions(
    node: Node<'_>,
    source: &[u8],
    functions: &mut Vec<ElixirFunction>,
    aliases: &[ElixirAlias],
    references: &[ElixirModuleReference],
    current_module: &str,
) {
    // ponytail: literal top-level definitions only; use compiler expansion when dynamic macros
    // must be modelled.
    if node.kind() == "call" {
        if matches!(call_target(node, source), Some("def" | "defp")) {
            push_function(functions, node, source, aliases, references, current_module);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_quoted_functions(
            child,
            source,
            functions,
            aliases,
            references,
            current_module,
        );
    }
}

fn collect_using_functions(
    node: Node<'_>,
    source: &[u8],
    functions: &mut Vec<ElixirFunction>,
    aliases: &[ElixirAlias],
    references: &[ElixirModuleReference],
    current_module: &str,
) {
    if node.kind() == "call" {
        if call_target(node, source) == Some("quote") {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_quoted_functions(
                    child,
                    source,
                    functions,
                    aliases,
                    references,
                    current_module,
                );
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_using_functions(
            child,
            source,
            functions,
            aliases,
            references,
            current_module,
        );
    }
}

fn alias_definitions(node: Node<'_>, source: &[u8]) -> Vec<ElixirAlias> {
    let Some(arguments) = arguments(node) else {
        return Vec::new();
    };
    let Some(target) = arguments.named_child(0) else {
        return Vec::new();
    };
    let targets = alias_names(target, source);
    let explicit_name = keyword_value(node, source, "as")
        .and_then(|name| text(name, source))
        .map(|name| name.trim_start_matches("Elixir."));
    targets
        .into_iter()
        .map(|target| ElixirAlias {
            name: explicit_name
                .unwrap_or_else(|| target.rsplit('.').next().unwrap_or(&target))
                .into(),
            target,
        })
        .collect()
}

fn expand_alias(module: &str, aliases: &[ElixirAlias], current_module: &str) -> String {
    if module == "__MODULE__" {
        return current_module.into();
    }
    let (first, rest) = module.split_once('.').unwrap_or((module, ""));
    let Some(alias) = aliases.iter().rev().find(|alias| alias.name == first) else {
        return module.into();
    };
    if rest.is_empty() {
        alias.target.clone()
    } else {
        format!("{}.{}", alias.target, rest)
    }
}

fn module_target(name: &str) -> String {
    name.strip_prefix(':').map_or_else(
        || format!("elixir-module://{name}"),
        |name| format!("erlang-module://{name}"),
    )
}

fn call_observations(
    repository: &str,
    module_name: &str,
    functions: &[ElixirFunction],
    definitions: &BTreeSet<String>,
    path: &Path,
    generated: bool,
) -> Vec<Observation> {
    let module_id = format!("repo://{repository}/elixir/{module_name}");
    let mut observations = Vec::new();
    for function in functions {
        let function_id = format!("{module_id}/{}/{}", function.name, function.arity);
        let mut targets = BTreeSet::new();
        for call in &function.calls {
            let target = if let Some(target_module) = &call.module {
                let candidate = format!(
                    "repo://{repository}/elixir/{target_module}/{}/{}",
                    call.name, call.arity
                );
                if definitions.contains(&candidate) {
                    candidate
                } else {
                    format!("elixir-call://{target_module}/{}/{}", call.name, call.arity)
                }
            } else {
                let candidate = format!("{module_id}/{}/{}", call.name, call.arity);
                if definitions.contains(&candidate) {
                    candidate
                } else {
                    format!("elixir-call://{}/{}", call.name, call.arity)
                }
            };
            if !targets.insert(target.clone()) {
                continue;
            }
            let mut observation = Observation::dependency(
                function_id.clone(),
                DependencyRelation::Calls,
                target,
                format!("{}:{}", path.display(), call.line),
            );
            if generated {
                observation.provenance = Provenance::Generated;
            }
            observations.push(observation);
        }
    }
    observations
}

fn collect_struct_fields(node: Node<'_>, source: &[u8], fields: &mut Vec<ElixirStructField>) {
    let name = match node.kind() {
        "atom" => text(node, source).map(|name| name.trim_start_matches(':')),
        "pair" => node
            .child_by_field_name("key")
            .and_then(|key| text(key, source))
            .map(|name| name.trim().trim_end_matches(':')),
        _ => None,
    };
    if let Some(name) = name {
        fields.push(ElixirStructField {
            name: name.into(),
            line: node.start_position().row + 1,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_struct_fields(child, source, fields);
    }
}

fn struct_fields(node: Node<'_>, source: &[u8]) -> Vec<ElixirStructField> {
    let mut fields = Vec::new();
    if let Some(fields_node) = arguments(node).and_then(|arguments| arguments.named_child(0)) {
        collect_struct_fields(fields_node, source, &mut fields);
    }
    fields
}

fn callback_definition(node: Node<'_>, source: &[u8]) -> Option<ElixirFunction> {
    let call = node
        .child_by_field_name("operand")
        .filter(|call| call.kind() == "call")?;
    if !matches!(
        call_target(call, source),
        Some("callback" | "macrocallback")
    ) {
        return None;
    }
    let (name, min_arity, max_arity) = arguments(call)
        .and_then(|arguments| arguments.named_child(0))
        .and_then(|signature| function_head(signature, source))?;
    (min_arity == max_arity).then(|| ElixirFunction {
        name: name.into(),
        arity: max_arity,
        line: node.start_position().row + 1,
        calls: Vec::new(),
        struct_uses: Vec::new(),
        imports: Vec::new(),
    })
}

fn behaviour_reference(node: Node<'_>, source: &[u8]) -> Option<ElixirModuleReference> {
    let call = node
        .child_by_field_name("operand")
        .filter(|call| call.kind() == "call")?;
    if call_target(call, source) != Some("behaviour") {
        return None;
    }
    let name = arguments(call)
        .and_then(|arguments| arguments.named_child(0))
        .filter(|name| matches!(name.kind(), "alias" | "atom"))
        .and_then(|name| text(name, source))?;
    Some(ElixirModuleReference {
        name: name.into(),
        kind: ElixirModuleReferenceKind::Behaviour,
        line: node.start_position().row + 1,
        only: None,
        except: BTreeSet::new(),
    })
}

fn push_module(
    modules: &mut Vec<ElixirModule>,
    name: String,
    line: usize,
    inherited_aliases: Vec<ElixirAlias>,
) -> usize {
    let module = modules.len();
    modules.push(ElixirModule {
        name,
        line,
        functions: Vec::new(),
        callbacks: Vec::new(),
        using_functions: Vec::new(),
        struct_fields: Vec::new(),
        implements: Vec::new(),
        aliases: inherited_aliases,
        references: Vec::new(),
    });
    module
}

fn collect(node: Node<'_>, source: &[u8], module: Option<usize>, modules: &mut Vec<ElixirModule>) {
    if node.kind() == "unary_operator"
        && let Some(module) = module
    {
        if let Some(callback) = callback_definition(node, source) {
            modules[module].callbacks.push(callback);
            return;
        }
        if let Some(behaviour) = behaviour_reference(node, source) {
            let mut behaviour = behaviour;
            behaviour.name = expand_alias(
                &behaviour.name,
                &modules[module].aliases,
                &modules[module].name,
            );
            modules[module].implements.push(behaviour);
            return;
        }
    }

    if node.kind() == "call" {
        match call_target(node, source) {
            Some("defmodule" | "defprotocol") => {
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
                    let inherited_aliases = module
                        .map(|parent| modules[parent].aliases.clone())
                        .unwrap_or_default();
                    let module = push_module(
                        modules,
                        name,
                        node.start_position().row + 1,
                        inherited_aliases,
                    );
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        collect(child, source, Some(module), modules);
                    }
                    return;
                }
            }
            Some("defimpl") => {
                let Some(protocol) = arguments(node)
                    .and_then(|arguments| arguments.named_child(0))
                    .filter(|protocol| protocol.kind() == "alias")
                    .and_then(|protocol| text(protocol, source))
                else {
                    return;
                };
                let aliases = module
                    .map(|parent| modules[parent].aliases.clone())
                    .unwrap_or_default();
                let current_module = module
                    .map(|parent| modules[parent].name.clone())
                    .unwrap_or_default();
                let protocol = expand_alias(protocol, &aliases, &current_module);
                let types = keyword_value(node, source, "for")
                    .map(|value| alias_names(value, source))
                    .filter(|types| !types.is_empty())
                    .or_else(|| keyword_token(node, source, "for").map(|name| vec![name.into()]))
                    .or_else(|| module.map(|parent| vec![modules[parent].name.clone()]))
                    .unwrap_or_default();
                for r#type in types {
                    let r#type = expand_alias(&r#type, &aliases, &current_module);
                    let implementation = push_module(
                        modules,
                        format!("{protocol}.{type}"),
                        node.start_position().row + 1,
                        aliases.clone(),
                    );
                    modules[implementation]
                        .implements
                        .push(ElixirModuleReference {
                            name: protocol.clone(),
                            kind: ElixirModuleReferenceKind::Behaviour,
                            line: node.start_position().row + 1,
                            only: None,
                            except: BTreeSet::new(),
                        });
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        collect(child, source, Some(implementation), modules);
                    }
                }
                return;
            }
            Some("def" | "defp" | "defdelegate") => {
                if let Some(module) = module {
                    let aliases = modules[module].aliases.clone();
                    let references = modules[module].references.clone();
                    let name = modules[module].name.clone();
                    push_function(
                        &mut modules[module].functions,
                        node,
                        source,
                        &aliases,
                        &references,
                        &name,
                    );
                }
                return;
            }
            Some("defstruct") => {
                if let Some(module) = module {
                    modules[module]
                        .struct_fields
                        .extend(struct_fields(node, source));
                }
                return;
            }
            Some("alias") => {
                if let Some(module) = module {
                    let aliases = modules[module].aliases.clone();
                    let name = modules[module].name.clone();
                    let definitions =
                        alias_definitions(node, source)
                            .into_iter()
                            .map(|mut alias| {
                                alias.target = expand_alias(&alias.target, &aliases, &name);
                                alias
                            });
                    modules[module].aliases.extend(definitions);
                }
                return;
            }
            Some(target @ ("import" | "require" | "use")) => {
                if let Some(module) = module
                    && let Some(raw_name) = arguments(node)
                        .and_then(|arguments| arguments.named_child(0))
                        .filter(|name| name.kind() == "alias")
                        .and_then(|name| text(name, source))
                {
                    let name =
                        expand_alias(raw_name, &modules[module].aliases, &modules[module].name);
                    let kind = match target {
                        "import" => ElixirModuleReferenceKind::Import,
                        "require" => ElixirModuleReferenceKind::Require,
                        _ => ElixirModuleReferenceKind::Use,
                    };
                    let reference = ElixirModuleReference {
                        name,
                        kind,
                        line: node.start_position().row + 1,
                        only: (kind == ElixirModuleReferenceKind::Import)
                            .then(|| function_filter(node, source, "only"))
                            .flatten(),
                        except: (kind == ElixirModuleReferenceKind::Import)
                            .then(|| function_filter(node, source, "except"))
                            .flatten()
                            .unwrap_or_default(),
                    };
                    let references = &mut modules[module].references;
                    // ponytail: retain first reference evidence until storage supports multiple
                    // evidence records for one semantic edge.
                    if let Some(existing) = references.iter().position(|existing| {
                        existing.name == reference.name && existing.kind == reference.kind
                    }) {
                        if kind == ElixirModuleReferenceKind::Import {
                            references[existing].only = reference.only;
                            references[existing].except = reference.except;
                        }
                    } else {
                        references.push(reference);
                    }
                    if target == "require" && keyword_value(node, source, "as").is_some() {
                        let aliases = modules[module].aliases.clone();
                        let module_name = modules[module].name.clone();
                        let definitions =
                            alias_definitions(node, source)
                                .into_iter()
                                .map(|mut alias| {
                                    alias.target =
                                        expand_alias(&alias.target, &aliases, &module_name);
                                    alias
                                });
                        modules[module].aliases.extend(definitions);
                    }
                }
                return;
            }
            Some("defmacro") => {
                if let Some(module) = module
                    && arguments(node)
                        .and_then(|arguments| arguments.named_child(0))
                        .and_then(|head| function_head(head, source))
                        .is_some_and(|(name, _, _)| name == "__using__")
                {
                    let mut functions = Vec::new();
                    let aliases = modules[module].aliases.clone();
                    let references = modules[module].references.clone();
                    let name = modules[module].name.clone();
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        collect_using_functions(
                            child,
                            source,
                            &mut functions,
                            &aliases,
                            &references,
                            &name,
                        );
                    }
                    modules[module].using_functions = functions;
                }
                return;
            }
            Some("defmacrop" | "quote") => return,
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
    let definitions = analysis
        .modules
        .iter()
        .flat_map(|module| {
            let module_id = format!("repo://{repository}/elixir/{}", module.name);
            module
                .functions
                .iter()
                .map(move |function| format!("{module_id}/{}/{}", function.name, function.arity))
        })
        .collect::<BTreeSet<_>>();
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
        observations.extend(module.callbacks.iter().map(|callback| {
            Observation::structural(
                module_id.clone(),
                StructuralRelation::Defines,
                format!("{module_id}/callback/{}/{}", callback.name, callback.arity),
                format!("{}:{}", path.display(), callback.line),
            )
        }));
        observations.extend(module.struct_fields.iter().map(|field| {
            Observation::structural(
                format!("{module_id}/field/{}", field.name),
                StructuralRelation::FieldOf,
                module_id.clone(),
                format!("{}:{}", path.display(), field.line),
            )
        }));
        observations.extend(call_observations(
            repository,
            &module.name,
            &module.functions,
            &definitions,
            path,
            false,
        ));
        for function in &module.functions {
            let function_id = format!("{module_id}/{}/{}", function.name, function.arity);
            let mut targets = BTreeSet::new();
            for r#use in &function.struct_uses {
                let target = r#use.module.clone();
                if targets.insert(target.clone()) {
                    observations.push(Observation::dependency(
                        function_id.clone(),
                        DependencyRelation::Uses,
                        module_target(&target),
                        format!("{}:{}", path.display(), r#use.line),
                    ));
                }
            }
        }
        observations.extend(module.implements.iter().map(|implementation| {
            Observation::dependency(
                module_id.clone(),
                DependencyRelation::Implements,
                module_target(&implementation.name),
                format!("{}:{}", path.display(), implementation.line),
            )
        }));
        observations.extend(module.references.iter().map(|reference| {
            Observation::dependency(
                module_id.clone(),
                match reference.kind {
                    ElixirModuleReferenceKind::Behaviour => DependencyRelation::Implements,
                    ElixirModuleReferenceKind::Import => DependencyRelation::Imports,
                    ElixirModuleReferenceKind::Require => DependencyRelation::Requires,
                    ElixirModuleReferenceKind::Use => DependencyRelation::Uses,
                },
                module_target(&reference.name),
                format!("{}:{}", path.display(), reference.line),
            )
        }));
    }
    observations
}

pub fn diagnostics_from_analysis(
    analysis: &ElixirAnalysis,
    path: &Path,
) -> Vec<AnalysisDiagnostic> {
    analysis
        .modules
        .iter()
        .flat_map(|module| {
            module
                .references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Use)
                .map(|reference| AnalysisDiagnostic {
                    code: "elixir.macro_expansion_incomplete".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path: path.into(),
                    line: u32::try_from(reference.line).ok(),
                    detail: Some(format!(
                        "use {} is indexed without compiler macro expansion",
                        reference.name
                    )),
                })
        })
        .collect()
}

pub fn generated_observations(
    repository: &str,
    sources: &[(&Path, &ElixirAnalysis)],
    observations: &[Observation],
) -> Vec<Observation> {
    let mut macros = BTreeMap::<&str, Option<(&Path, &ElixirModule)>>::new();
    for (path, analysis) in sources {
        for module in &analysis.modules {
            if module.using_functions.is_empty() {
                continue;
            }
            macros
                .entry(&module.name)
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some((path, module)));
        }
    }

    let mut definition_edges = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .map(|observation| {
            (
                observation.from.as_str().to_owned(),
                observation.to.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut definitions = definition_edges
        .iter()
        .map(|(_, definition)| definition.clone())
        .collect::<BTreeSet<_>>();
    let mut generated = Vec::new();
    for (_, analysis) in sources {
        for module in &analysis.modules {
            let module_id = format!("repo://{repository}/elixir/{}", module.name);
            for reference in module
                .references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Use)
            {
                let target = reference.name.clone();
                let Some(Some((path, macro_module))) = macros.get(target.as_str()) else {
                    continue;
                };
                let mut emitted_functions = Vec::new();
                for function in &macro_module.using_functions {
                    let function_id = format!("{module_id}/{}/{}", function.name, function.arity);
                    if definition_edges.insert((module_id.clone(), function_id.clone())) {
                        definitions.insert(function_id.clone());
                        emitted_functions.push(function.clone());
                        generated.push(Observation::generated(
                            module_id.clone(),
                            StructuralRelation::Defines,
                            function_id,
                            format!("{}:{}", path.display(), function.line),
                        ));
                    }
                }
                generated.extend(call_observations(
                    repository,
                    &module.name,
                    &emitted_functions,
                    &definitions,
                    path,
                    true,
                ));
            }
        }
    }
    generated
}

pub fn resolve_workspace_modules(observations: &[Observation]) -> Vec<DependencyOverride> {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
    }) {
        let Some(name) = observation
            .to
            .as_str()
            .rsplit_once("/elixir/")
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
                SemanticRelation::Dependency(relation @ DependencyRelation::Implements)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Imports)
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

pub fn resolve_repository_calls(
    observations: &mut [Observation],
    sources: &[(&Path, &ElixirAnalysis)],
) {
    let definitions = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .filter_map(|observation| {
            let symbol = observation.to.as_str().rsplit_once("/elixir/")?.1;
            symbol
                .contains('/')
                .then(|| (symbol.to_owned(), observation.to.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let imports = sources
        .iter()
        .flat_map(|(_, analysis)| &analysis.modules)
        .flat_map(|module| {
            module.functions.iter().map(|function| {
                (
                    format!("{}/{}/{}", module.name, function.name, function.arity),
                    &function.imports,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        let Some(symbol) = observation.to.as_str().strip_prefix("elixir-call://") else {
            continue;
        };
        let target = if symbol.matches('/').count() >= 2 {
            definitions.get(symbol)
        } else {
            let Some((function, arity)) = symbol.rsplit_once('/') else {
                continue;
            };
            let Some(scoped_function) = observation
                .from
                .as_str()
                .rsplit_once("/elixir/")
                .map(|(_, function)| function)
            else {
                continue;
            };
            let caller = scoped_function;
            let Some((scoped_function, _)) = scoped_function.rsplit_once('/') else {
                continue;
            };
            let Some((module, _)) = scoped_function.rsplit_once('/') else {
                continue;
            };
            let signature = format!("{function}/{arity}");
            if let Some(target) = definitions.get(&format!("{module}/{signature}")) {
                observation.to = target.clone();
                continue;
            }
            let candidates = imports
                .get(caller)
                .copied()
                .into_iter()
                .flatten()
                .filter(|import| {
                    import
                        .only
                        .as_ref()
                        .is_none_or(|only| only.contains(&signature))
                        && !import.except.contains(&signature)
                })
                .filter_map(|import| definitions.get(&format!("{}/{signature}", import.name)))
                .collect::<BTreeSet<_>>();
            if candidates.len() == 1 {
                candidates.into_iter().next()
            } else {
                None
            }
        };
        if let Some(target) = target {
            observation.to = target.clone();
        }
    }
}

pub fn observations(
    repository: &str,
    source: &str,
    path: &Path,
) -> Result<Vec<Observation>, Box<dyn Error>> {
    let analysis = analyze(source)?;
    let mut observations = observations_from_analysis(repository, &analysis, path);
    observations.extend(generated_observations(
        repository,
        &[(path, &analysis)],
        &observations,
    ));
    resolve_repository_calls(&mut observations, &[(path, &analysis)]);
    Ok(observations)
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
    fn resolves_local_and_aliased_calls_without_emitting_control_macros() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Payments do
              def before_alias, do: Late.run()
              alias MyApp.{Ledger, Unused}
              alias MyApp.Late, as: Late
              def after_alias, do: Late.run()
              import MyApp.Helpers, only: [audit: 0]
              require MyApp.Macros, as: Macros

              def create(amount) do
                amount |> normalize()
                Ledger.record(amount)
                if amount > 0 do
                  audit()
                  hidden()
                  Macros.expand(amount)
                end
              end

              def create(:fallback), do: fallback()
              defp normalize(amount), do: amount
              defp fallback, do: :ok
              defdelegate delegate(amount), to: Ledger, as: :record
            end

            defmodule MyApp.Helpers do
              def audit, do: :ok
              def hidden, do: :ok
            end

            defmodule MyApp.Late do
              def run, do: :ok
            end
            "#,
            Path::new("lib/my_app/payments.ex"),
        )
        .unwrap();

        let caller = "repo://payments/elixir/MyApp.Payments/create/1";
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str() == caller
                    && observation.relation
                        == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                "repo://payments/elixir/MyApp.Payments/normalize/1",
                "elixir-call://MyApp.Ledger/record/1",
                "repo://payments/elixir/MyApp.Helpers/audit/0",
                "elixir-call://hidden/0",
                "elixir-call://MyApp.Macros/expand/1",
                "repo://payments/elixir/MyApp.Payments/fallback/0",
            ]
        );
        assert!(!calls.iter().any(|call| call.contains("/if/")));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/delegate/1"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "elixir-call://MyApp.Ledger/record/1"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/before_alias/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "elixir-call://Late/run/0"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/after_alias/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Late/run/0"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::Requires)
                && observation.to.as_str() == "elixir-module://MyApp.Macros"
        }));
    }

    #[test]
    fn resolves_repository_calls_and_generated_function_calls() {
        let macro_analysis = analyze(
            r#"
            defmodule MyApp.ServerMacro do
              alias MyApp.Backend, as: Backend
              defmacro __using__(_) do
                quote do
                  def generated(value), do: Backend.work(value)
                end
              end
            end
            "#,
        )
        .unwrap();
        let consumer_analysis = analyze(
            r#"
            defmodule MyApp.Consumer do
              use MyApp.ServerMacro
            end

            defmodule MyApp.Backend do
              def work(value), do: value
            end
            "#,
        )
        .unwrap();
        let macro_path = Path::new("lib/my_app/server_macro.ex");
        let consumer_path = Path::new("lib/my_app/consumer.ex");
        let mut observations = observations_from_analysis("payments", &macro_analysis, macro_path);
        observations.extend(observations_from_analysis(
            "payments",
            &consumer_analysis,
            consumer_path,
        ));
        observations.extend(generated_observations(
            "payments",
            &[
                (macro_path, &macro_analysis),
                (consumer_path, &consumer_analysis),
            ],
            &observations,
        ));
        resolve_repository_calls(
            &mut observations,
            &[
                (macro_path, &macro_analysis),
                (consumer_path, &consumer_analysis),
            ],
        );

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer/generated/1"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Backend/work/1"
                && observation.evidence.as_str() == "lib/my_app/server_macro.ex:6"
                && observation.provenance == Provenance::Generated
        }));
    }

    #[test]
    fn models_literal_using_definitions_as_generated() {
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

        assert!(
            !observations
                .iter()
                .any(|observation| observation.to.as_str().ends_with("/__using__/1"))
        );
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Consumer/generated/0"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:5"
                && observation.provenance == Provenance::Generated
        }));
        assert!(!observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Macro"
                && observation.to.as_str().ends_with("/generated/0")
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

    #[test]
    fn models_compiler_defined_behaviours_protocols_and_structs() {
        let observations = observations(
            "github.com/example/elixir",
            r#"
            defmodule Example.Worker do
              @callback run(term()) :: term()
            end

            defmodule Example.Data do
              @behaviour Example.Worker
              @behaviour :gen_server
              defstruct [:name, active: true]

              alias Example.Data, as: Data
              def build(name), do: %Data{name: name}
            end

            defprotocol Example.Printable do
              def print(value)
            end

            defimpl Example.Printable, for: Example.Data do
              def print(value), do: value.name
            end
            "#,
            Path::new("lib/example.ex"),
        )
        .unwrap();
        let triples = observations
            .iter()
            .map(|observation| {
                (
                    observation.from.as_str(),
                    observation.relation.as_str(),
                    observation.to.as_str(),
                )
            })
            .collect::<BTreeSet<_>>();

        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Worker",
            "defines",
            "repo://github.com/example/elixir/elixir/Example.Worker/callback/run/1",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data",
            "implements",
            "elixir-module://Example.Worker",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data",
            "implements",
            "erlang-module://gen_server",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data/field/name",
            "field_of",
            "repo://github.com/example/elixir/elixir/Example.Data",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data/build/1",
            "uses",
            "elixir-module://Example.Data",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Printable",
            "defines",
            "repo://github.com/example/elixir/elixir/Example.Printable/print/1",
        )));
        assert!(
            triples.contains(&(
                "repo://github.com/example/elixir/elixir/Example.Printable.Example.Data",
                "implements",
                "elixir-module://Example.Printable",
            )),
            "{triples:#?}"
        );
        let overrides = resolve_workspace_modules(&observations);
        assert!(overrides.iter().any(|override_| {
            override_.unresolved_to.as_str() == "elixir-module://Example.Printable"
                && override_.resolved_to.as_str()
                    == "repo://github.com/example/elixir/elixir/Example.Printable"
                && override_.relation == DependencyRelation::Implements
        }));
    }

    #[test]
    fn reports_macro_expansion_as_a_known_limitation() {
        let analysis = analyze(
            r#"
            defmodule MyApp.Consumer do
              alias MyApp.ServerMacro, as: Server
              use Server, mode: :strict
            end
            "#,
        )
        .unwrap();

        assert_eq!(
            diagnostics_from_analysis(&analysis, Path::new("lib/my_app/consumer.ex")),
            vec![AnalysisDiagnostic {
                code: "elixir.macro_expansion_incomplete".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: std::path::PathBuf::from("lib/my_app/consumer.ex"),
                line: Some(4),
                detail: Some(
                    "use MyApp.ServerMacro is indexed without compiler macro expansion".into(),
                ),
            }]
        );
    }
}
