use super::model::*;
use beholder_domain::{DependencyRelation, Observation, Provenance};
use std::collections::BTreeSet;
use std::{error::Error, path::Path};
use tree_sitter::{Node, Parser};

pub(super) fn text<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

pub(super) fn call_target<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    text(node.child_by_field_name("target")?, source)
}

pub(super) fn arguments(node: Node<'_>) -> Option<Node<'_>> {
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

pub(super) fn keyword_value<'a>(node: Node<'a>, source: &'a [u8], key: &str) -> Option<Node<'a>> {
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

fn reference_definition(
    node: Node<'_>,
    source: &[u8],
    kind: ElixirModuleReferenceKind,
    aliases: &[ElixirAlias],
    current_module: &str,
) -> Option<ElixirModuleReference> {
    let raw_name = arguments(node)
        .and_then(|arguments| arguments.named_child(0))
        .filter(|name| name.kind() == "alias")
        .and_then(|name| text(name, source))?;
    Some(ElixirModuleReference {
        name: expand_alias(raw_name, aliases, current_module),
        kind,
        line: node.start_position().row + 1,
        only: (kind == ElixirModuleReferenceKind::Import)
            .then(|| function_filter(node, source, "only"))
            .flatten(),
        except: (kind == ElixirModuleReferenceKind::Import)
            .then(|| function_filter(node, source, "except"))
            .flatten()
            .unwrap_or_default(),
    })
}

fn function_imports(
    node: Node<'_>,
    source: &[u8],
    aliases: &[ElixirAlias],
    current_module: &str,
) -> Vec<ElixirModuleReference> {
    let Some(block) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "do_block")
    else {
        return Vec::new();
    };
    block
        .named_children(&mut block.walk())
        .filter(|child| child.kind() == "call" && call_target(*child, source) == Some("import"))
        .filter_map(|child| {
            reference_definition(
                child,
                source,
                ElixirModuleReferenceKind::Import,
                aliases,
                current_module,
            )
        })
        .collect()
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
    let mut imports = references
        .iter()
        .filter(|reference| reference.kind == ElixirModuleReferenceKind::Import)
        .cloned()
        .collect::<Vec<_>>();
    for import in function_imports(node, source, aliases, current_module) {
        if !imports.contains(&import) {
            imports.push(import);
        }
    }
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
            for import in &imports {
                if !function.imports.contains(import) {
                    function.imports.push(import.clone());
                }
            }
            for r#use in struct_uses {
                if !function
                    .struct_uses
                    .iter()
                    .any(|existing| existing.module == r#use.module)
                {
                    function.struct_uses.push(r#use);
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
            imports: imports.clone(),
        });
    }
}

fn collect_quoted_semantics(
    node: Node<'_>,
    source: &[u8],
    functions: &mut Vec<ElixirFunction>,
    implements: &mut Vec<ElixirModuleReference>,
    aliases: &[ElixirAlias],
    references: &[ElixirModuleReference],
    current_module: &str,
) {
    // ponytail: literal top-level definitions only; use compiler expansion when dynamic macros
    // must be modelled.
    if node.kind() == "unary_operator"
        && let Some(mut behaviour) = behaviour_reference(node, source)
    {
        behaviour.name = expand_alias(&behaviour.name, aliases, current_module);
        implements.push(behaviour);
        return;
    }
    if node.kind() == "call" {
        if matches!(call_target(node, source), Some("def" | "defp")) {
            push_function(functions, node, source, aliases, references, current_module);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_quoted_semantics(
            child,
            source,
            functions,
            implements,
            aliases,
            references,
            current_module,
        );
    }
}

fn collect_using_semantics(
    node: Node<'_>,
    source: &[u8],
    functions: &mut Vec<ElixirFunction>,
    implements: &mut Vec<ElixirModuleReference>,
    aliases: &[ElixirAlias],
    references: &[ElixirModuleReference],
    current_module: &str,
) {
    if node.kind() == "call" {
        if call_target(node, source) == Some("quote") {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_quoted_semantics(
                    child,
                    source,
                    functions,
                    implements,
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
        collect_using_semantics(
            child,
            source,
            functions,
            implements,
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

pub(super) fn expand_alias(module: &str, aliases: &[ElixirAlias], current_module: &str) -> String {
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

pub(super) fn module_target(name: &str) -> String {
    name.strip_prefix(':').map_or_else(
        || format!("elixir-module://{name}"),
        |name| format!("erlang-module://{name}"),
    )
}

pub(super) fn call_observations(
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
        enclosing_module: None,
        line,
        functions: Vec::new(),
        callbacks: Vec::new(),
        using_functions: Vec::new(),
        using_implements: Vec::new(),
        struct_fields: Vec::new(),
        implements: Vec::new(),
        aliases: inherited_aliases,
        references: Vec::new(),
        grpc: Default::default(),
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
        if let Some(module) = module {
            let aliases = modules[module].aliases.clone();
            let name = modules[module].name.clone();
            super::grpc::observe_call(&mut modules[module].grpc, node, source, &aliases, &name);
        }
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
                    let enclosing_module = module.map(|parent| modules[parent].name.clone());
                    let implementation = push_module(
                        modules,
                        format!("{protocol}.{type}"),
                        node.start_position().row + 1,
                        aliases.clone(),
                    );
                    modules[implementation].enclosing_module = enclosing_module;
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
                if let Some(module) = module {
                    let kind = match target {
                        "import" => ElixirModuleReferenceKind::Import,
                        "require" => ElixirModuleReferenceKind::Require,
                        _ => ElixirModuleReferenceKind::Use,
                    };
                    let Some(reference) = reference_definition(
                        node,
                        source,
                        kind,
                        &modules[module].aliases,
                        &modules[module].name,
                    ) else {
                        return;
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
                    let mut implements = Vec::new();
                    let aliases = modules[module].aliases.clone();
                    let references = modules[module].references.clone();
                    let name = modules[module].name.clone();
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        collect_using_semantics(
                            child,
                            source,
                            &mut functions,
                            &mut implements,
                            &aliases,
                            &references,
                            &name,
                        );
                    }
                    modules[module].using_functions = functions;
                    modules[module].using_implements = implements;
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
