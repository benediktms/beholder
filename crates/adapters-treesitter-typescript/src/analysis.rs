use super::{
    model::*,
    plugin::{TypescriptLanguage, built_in_plugins},
};
use beholder_adapters_treesitter::{ErrorDisposition, Recovery, RecoveryFailure, recover_with};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, Provenance, SemanticCandidate, SourcePosition, SourceSpan, StructuralRelation,
    UnsafeTreeRecovery,
};
use beholder_indexing::{LanguageAnalyzer, SourceRecognitionInput};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, path::Path};
use tree_sitter::{Node, Parser};

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn qualified(scope: &[String], name: &str) -> String {
    scope
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(name))
        .collect::<Vec<_>>()
        .join("/")
}

fn callable_value(node: Node<'_>) -> bool {
    matches!(node.kind(), "arrow_function" | "function_expression")
}

fn is_exported(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.kind() == "export_statement" {
            return true;
        }
        node = parent;
    }
    false
}

fn collect_identifiers(node: Node<'_>, source: &[u8], identifiers: &mut Vec<String>) {
    if node.kind() == "identifier" {
        if let Some(identifier) = text(node, source) {
            identifiers.push(identifier.into());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifiers(child, source, identifiers);
    }
}

fn lsp_position(node: Node<'_>, source: &[u8], end: bool) -> Option<SourcePosition> {
    let point = if end {
        node.end_position()
    } else {
        node.start_position()
    };
    let byte = if end {
        node.end_byte()
    } else {
        node.start_byte()
    };
    let line_start = byte.checked_sub(point.column)?;
    let character = std::str::from_utf8(source.get(line_start..byte)?)
        .ok()?
        .encode_utf16()
        .count()
        .try_into()
        .ok()?;
    Some(SourcePosition {
        line: point.row.try_into().ok()?,
        character,
    })
}

fn call_target(node: Node<'_>, target: Node<'_>, source: &[u8], kind: CallKind) -> Option<Call> {
    let (receiver, name, kind, selection): (Option<String>, String, CallKind, Node<'_>) =
        match target.kind() {
            "identifier" => (None, text(target, source)?.into(), kind, target),
            "member_expression" => {
                let property = target.child_by_field_name("property")?;
                (
                    target
                        .child_by_field_name("object")
                        .and_then(|node| text(node, source))
                        .map(str::to_owned),
                    text(property, source)?.into(),
                    if kind == CallKind::Constructor {
                        CallKind::Constructor
                    } else {
                        CallKind::Member
                    },
                    property,
                )
            }
            _ => return None,
        };
    let preserve_all_arguments = matches!(
        name.as_str(),
        "GrpcMethod" | "GrpcStreamMethod" | "GrpcStreamCall" | "getService"
    ) || (name == "request"
        && receiver.as_deref() == Some("this.rpc"));
    let preserve_type_arguments = name == "getService";
    let start = lsp_position(selection, source, false)?;
    let end = lsp_position(selection, source, true)?;
    Some(Call {
        kind,
        receiver,
        returned_receiver: (name == "load")
            .then(|| {
                target
                    .child_by_field_name("object")
                    .filter(|object| object.kind() == "call_expression")
                    .filter(|object| {
                        // ponytail: broaden when receiver types and invoking methods can be proven.
                        object
                            .child_by_field_name("function")
                            .and_then(|function| text(function, source))
                            == Some("context.get")
                    })
                    .and_then(|object| object.child_by_field_name("arguments"))
                    .and_then(|arguments| {
                        let arguments = arguments
                            .named_children(&mut arguments.walk())
                            .collect::<Vec<_>>();
                        let [argument] = arguments.as_slice() else {
                            return None;
                        };
                        matches!(argument.kind(), "identifier" | "member_expression")
                            .then(|| text(*argument, source).map(str::to_owned))
                            .flatten()
                    })
            })
            .flatten(),
        name,
        arguments: node
            .child_by_field_name("arguments")
            .map(|arguments| {
                arguments
                    .named_children(&mut arguments.walk())
                    .filter_map(|argument| {
                        (argument.kind() == "identifier" || preserve_all_arguments)
                            .then(|| text(argument, source).map(str::to_owned))
                            .flatten()
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                let mut identifiers = Vec::new();
                if matches!(
                    node.kind(),
                    "jsx_opening_element" | "jsx_self_closing_element"
                ) {
                    collect_identifiers(node, source, &mut identifiers);
                }
                identifiers
            }),
        type_arguments: preserve_type_arguments
            .then(|| {
                let arguments = node.child_by_field_name("type_arguments")?;
                Some(
                    arguments
                        .named_children(&mut arguments.walk())
                        .filter_map(|argument| text(argument, source).map(str::to_owned))
                        .collect(),
                )
            })
            .flatten()
            .unwrap_or_default(),
        line: node.start_position().row + 1,
        start_line: start.line,
        start_character: start.character,
        end_line: end.line,
        end_character: end.character,
        scope_start: 0,
        scope_end: 0,
    })
}

fn collect_graphql_documents(node: Node<'_>, source: &[u8], documents: &mut Vec<GraphqlDocument>) {
    if node.kind() == "variable_declarator"
        && let (Some(name), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        )
        && name.kind() == "identifier"
        && value.kind() == "call_expression"
        && value
            .child_by_field_name("function")
            .and_then(|function| text(function, source))
            .is_some_and(|function| matches!(function, "gql" | "graphql"))
        && let Some(arguments) = value.child_by_field_name("arguments")
    {
        let arguments = arguments
            .named_children(&mut arguments.walk())
            .filter(|argument| argument.kind() != "comment")
            .collect::<Vec<_>>();
        if let [document] = arguments.as_slice()
            && matches!(document.kind(), "template_string" | "string")
            && (document.kind() != "template_string"
                || document
                    .named_children(&mut document.walk())
                    .all(|child| child.kind() != "template_substitution"))
            && let (Some(binding), Some(document)) = (text(name, source), text(*document, source))
            && let Some(document) = document
                .strip_prefix('`')
                .and_then(|text| text.strip_suffix('`'))
                .or_else(|| {
                    document
                        .strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))
                })
                .or_else(|| {
                    document
                        .strip_prefix('\'')
                        .and_then(|text| text.strip_suffix('\''))
                })
        {
            documents.push(GraphqlDocument {
                binding: binding.into(),
                source: document.into(),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_graphql_documents(child, source, documents);
    }
}

fn collect_string_constants(node: Node<'_>, source: &[u8], constants: &mut Vec<StringConstant>) {
    if node.kind() == "variable_declarator"
        && let (Some(name), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        )
        && name.kind() == "identifier"
        && matches!(value.kind(), "string" | "template_string")
        && let (Some(name), Some(value)) = (text(name, source), text(value, source))
        && (name == "protobufPackage" || name.ends_with("ServiceName"))
    {
        constants.push(StringConstant {
            name: name.into(),
            value: value.into(),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_string_constants(child, source, constants);
    }
}

fn call(node: Node<'_>, source: &[u8], kind: CallKind) -> Option<Call> {
    let target = node.child_by_field_name(match kind {
        CallKind::Constructor => "constructor",
        CallKind::Direct | CallKind::Member => "function",
    })?;
    call_target(node, target, source, kind)
}

fn is_collection_boundary(node: Node<'_>, root: Node<'_>) -> bool {
    node != root
        && (matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "generator_function_declaration"
                | "method_definition"
        ) || (node.kind() == "arrow_function"
            && node
                .parent()
                .is_none_or(|parent| !matches!(parent.kind(), "arguments" | "return_statement"))))
}

fn lexical_scope(node: Node<'_>, root: Node<'_>) -> (usize, usize) {
    let scope = std::iter::successors(Some(node), |ancestor| ancestor.parent())
        .find(|ancestor| {
            matches!(
                ancestor.kind(),
                "arrow_function"
                    | "function_declaration"
                    | "function_expression"
                    | "generator_function_declaration"
                    | "method_definition"
            )
        })
        .unwrap_or(root);
    (scope.start_byte(), scope.end_byte())
}

fn collect_calls(node: Node<'_>, source: &[u8], root: Node<'_>, calls: &mut Vec<Call>) {
    if is_collection_boundary(node, root) {
        return;
    }
    match node.kind() {
        "call_expression" => {
            if let Some(mut call) = call(node, source, CallKind::Direct) {
                (call.scope_start, call.scope_end) = lexical_scope(node, root);
                calls.push(call);
            }
        }
        "new_expression" => {
            if let Some(mut call) = call(node, source, CallKind::Constructor) {
                (call.scope_start, call.scope_end) = lexical_scope(node, root);
                calls.push(call);
            }
        }
        "jsx_opening_element" | "jsx_self_closing_element" => {
            if let Some(mut call) = node
                .child_by_field_name("name")
                .filter(|target| {
                    target.kind() == "member_expression"
                        || text(*target, source)
                            .and_then(|name| name.chars().next())
                            .is_some_and(char::is_uppercase)
                })
                .and_then(|target| call_target(node, target, source, CallKind::Direct))
            {
                (call.scope_start, call.scope_end) = lexical_scope(node, root);
                calls.push(call);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, root, calls);
    }
}

fn collect_decorator_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<Call>) {
    let mut cursor = node.walk();
    for decorator in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
    {
        collect_calls(decorator, source, decorator, calls);
    }
    let mut sibling = node.prev_named_sibling();
    while let Some(decorator) = sibling.filter(|sibling| sibling.kind() == "decorator") {
        collect_calls(decorator, source, decorator, calls);
        sibling = decorator.prev_named_sibling();
    }
}

fn type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "type_identifier" {
        return text(node, source).map(str::to_owned);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| type_name(child, source))
}

fn return_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    if let Some(generic) = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "generic_type")
        && generic
            .child_by_field_name("name")
            .and_then(|name| text(name, source))
            == Some("Promise")
    {
        return generic
            .child_by_field_name("type_arguments")
            .and_then(|arguments| type_name(arguments, source));
    }
    type_name(node, source)
}

fn binding(node: Node<'_>, source: &[u8]) -> Option<Binding> {
    let receiver = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        .and_then(|name| text(name, source))?
        .to_owned();
    let type_name = node
        .child_by_field_name("type")
        .and_then(|annotation| type_name(annotation, source))
        .or_else(|| {
            let value = node.child_by_field_name("value")?;
            (value.kind() == "new_expression")
                .then(|| value.child_by_field_name("constructor"))
                .flatten()
                .and_then(|constructor| text(constructor, source))
                .map(str::to_owned)
        })?;
    Some(Binding {
        receiver,
        type_name,
        injection_token: injection_token(node, source),
        scope_start: 0,
        scope_end: 0,
    })
}

fn injection_token(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .find_map(|decorator| {
            let call = decorator.named_child(0)?;
            if call.kind() != "call_expression"
                || call
                    .child_by_field_name("function")
                    .and_then(|function| text(function, source))
                    != Some("Inject")
            {
                return None;
            }
            call.child_by_field_name("arguments")?
                .named_child(0)
                .and_then(|argument| text(argument, source))
                .map(str::to_owned)
        })
}

fn collect_bindings(node: Node<'_>, source: &[u8], root: Node<'_>, bindings: &mut Vec<Binding>) {
    if is_collection_boundary(node, root) {
        return;
    }
    if matches!(
        node.kind(),
        "required_parameter" | "optional_parameter" | "variable_declarator"
    ) && let Some(mut binding) = binding(node, source)
    {
        (binding.scope_start, binding.scope_end) = lexical_scope(node, root);
        bindings.push(binding);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_bindings(child, source, root, bindings);
    }
}

fn collect_factory_bindings(
    node: Node<'_>,
    source: &[u8],
    root: Node<'_>,
    bindings: &mut Vec<FactoryBinding>,
) {
    if is_collection_boundary(node, root) {
        return;
    }
    if node.kind() == "variable_declarator"
        && let Some(receiver) = node
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| text(name, source))
        && let Some(factory) = node
            .child_by_field_name("value")
            .and_then(|value| {
                if value.kind() == "await_expression" {
                    value.named_child(0)
                } else {
                    Some(value)
                }
            })
            .filter(|value| value.kind() == "call_expression")
            .and_then(|value| value.child_by_field_name("function"))
            .filter(|function| function.kind() == "identifier")
            .and_then(|function| text(function, source))
    {
        bindings.push(FactoryBinding {
            receiver: receiver.into(),
            factory: factory.into(),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_factory_bindings(child, source, root, bindings);
    }
}

fn is_short_circuit(node: Node<'_>, source: &[u8]) -> bool {
    node.child_by_field_name("operator")
        .and_then(|operator| text(operator, source))
        .is_some_and(|operator| matches!(operator, "&&" | "||" | "??" | "&&=" | "||=" | "??="))
}

fn has_conditional_ancestor(node: Node<'_>, source: &[u8], root: Node<'_>) -> bool {
    std::iter::successors(node.parent(), |ancestor| ancestor.parent())
        .take_while(|ancestor| *ancestor != root)
        .any(|ancestor| {
            matches!(
                ancestor.kind(),
                "if_statement"
                    | "catch_clause"
                    | "switch_case"
                    | "ternary_expression"
                    | "for_statement"
                    | "for_in_statement"
                    | "while_statement"
                    | "do_statement"
            ) || (ancestor.kind() == "binary_expression" && is_short_circuit(ancestor, source))
        })
}

fn collect_alias_bindings(
    node: Node<'_>,
    source: &[u8],
    root: Node<'_>,
    bindings: &mut Vec<AliasBinding>,
) {
    if is_collection_boundary(node, root) {
        return;
    }
    let pair = match node.kind() {
        "variable_declarator" => (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ),
        "assignment_expression" => (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ),
        "augmented_assignment_expression" if is_short_circuit(node, source) => (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ),
        _ => (None, None),
    };
    if let (Some(receiver), Some(value)) = pair
        && matches!(receiver.kind(), "identifier" | "member_expression")
        && let Some(receiver) = text(receiver, source)
    {
        let value = unparenthesized(value);
        let conditional = has_conditional_ancestor(node, source, root)
            || node.kind() == "augmented_assignment_expression";
        let mut sources = if value.kind() == "ternary_expression" {
            ["consequence", "alternative"]
                .into_iter()
                .filter_map(|field| value.child_by_field_name(field).map(unparenthesized))
                .collect::<Vec<_>>()
        } else {
            vec![value]
        };
        if sources
            .iter()
            .all(|source| matches!(source.kind(), "identifier" | "member_expression"))
        {
            let (scope_start, scope_end) = lexical_scope(node, root);
            for (index, source_node) in sources.drain(..).enumerate() {
                let Some(source_name) = text(source_node, source) else {
                    continue;
                };
                bindings.push(AliasBinding {
                    receiver: receiver.into(),
                    source: source_name.into(),
                    line: source_node.start_position().row + 1,
                    character: lsp_position(source_node, source, false)
                        .map_or(0, |position| position.character),
                    scope_start,
                    scope_end,
                    conditional: conditional || index > 0,
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_alias_bindings(child, source, root, bindings);
    }
}

fn unparenthesized(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(child) = node.named_child(0) else {
            break;
        };
        node = child;
    }
    node
}

fn returned_constructor(node: Node<'_>, source: &[u8], root: Node<'_>) -> Option<String> {
    if node != root
        && matches!(
            node.kind(),
            "arrow_function"
                | "function_declaration"
                | "function_expression"
                | "generator_function_declaration"
                | "method_definition"
        )
    {
        return None;
    }
    if node.kind() == "return_statement"
        && let Some(value) = node.named_child(0).map(unparenthesized)
        && value.kind() == "new_expression"
    {
        return value
            .child_by_field_name("constructor")
            .and_then(|constructor| text(constructor, source))
            .map(str::to_owned);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| returned_constructor(child, source, root))
}

fn callback_return<'tree>(node: Node<'tree>, source: &[u8]) -> Option<(Node<'tree>, String)> {
    let arguments = node.child_by_field_name("arguments")?;
    arguments
        .named_children(&mut arguments.walk())
        .filter(|argument| matches!(argument.kind(), "arrow_function" | "function_expression"))
        .find_map(|callback| {
            let body = unparenthesized(callback.child_by_field_name("body")?);
            let constructor = if body.kind() == "new_expression" {
                body.child_by_field_name("constructor")
                    .and_then(|constructor| text(constructor, source))
                    .map(str::to_owned)
            } else {
                returned_constructor(body, source, body)
            }?;
            Some((body, constructor))
        })
}

fn class_bindings(body: Node<'_>, source: &[u8]) -> Vec<Binding> {
    let mut bindings = Vec::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if matches!(child.kind(), "public_field_definition" | "field_definition")
            && let Some(mut binding) = binding(child, source)
        {
            binding.receiver = format!("this.{}", binding.receiver);
            bindings.push(binding);
        }
        if child.kind() != "method_definition"
            || child
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
                != Some("constructor")
        {
            continue;
        }
        let Some(parameters) = child.child_by_field_name("parameters") else {
            continue;
        };
        let parameter_bindings = parameters
            .named_children(&mut parameters.walk())
            .filter_map(|parameter| binding(parameter, source))
            .collect::<Vec<_>>();
        let mut parameter_cursor = parameters.walk();
        for parameter in parameters.named_children(&mut parameter_cursor) {
            let mut modifier_cursor = parameter.walk();
            let has_access_modifier = parameter
                .named_children(&mut modifier_cursor)
                .any(|child| child.kind() == "accessibility_modifier");
            let has_readonly = parameter
                .child_by_field_name("pattern")
                .and_then(|pattern| source.get(parameter.start_byte()..pattern.start_byte()))
                .is_some_and(|prefix| {
                    std::str::from_utf8(prefix).is_ok_and(|prefix| {
                        prefix.split_whitespace().any(|word| word == "readonly")
                    })
                });
            if (has_access_modifier || has_readonly)
                && let Some(mut binding) = binding(parameter, source)
            {
                binding.receiver = format!("this.{}", binding.receiver);
                bindings.push(binding);
            }
        }
        if let Some(body) = child.child_by_field_name("body") {
            let mut aliases = Vec::new();
            collect_alias_bindings(body, source, body, &mut aliases);
            bindings.extend(aliases.into_iter().filter_map(|alias| {
                let parameter = parameter_bindings
                    .iter()
                    .find(|binding| binding.receiver == alias.source)?;
                alias.receiver.starts_with("this.").then(|| Binding {
                    receiver: alias.receiver,
                    type_name: parameter.type_name.clone(),
                    injection_token: parameter.injection_token.clone(),
                    scope_start: 0,
                    scope_end: 0,
                })
            }));
        }
    }
    bindings
}

fn definition(
    node: Node<'_>,
    body: Option<Node<'_>>,
    source: &[u8],
    scope: &[String],
    name: &str,
    kind: DefinitionKind,
) -> Definition {
    let mut calls = Vec::new();
    let mut bindings = Vec::new();
    let mut alias_bindings = Vec::new();
    let mut factory_bindings = Vec::new();
    if let Some(body) = body {
        collect_calls(body, source, body, &mut calls);
    }
    collect_decorator_calls(node, source, &mut calls);
    collect_bindings(node, source, node, &mut bindings);
    collect_alias_bindings(node, source, node, &mut alias_bindings);
    collect_factory_bindings(node, source, node, &mut factory_bindings);
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|annotation| return_type_name(annotation, source))
        .or_else(|| returned_constructor(node, source, node));
    Definition {
        qualified_name: qualified(scope, name),
        kind,
        line: node.start_position().row + 1,
        calls,
        bindings,
        alias_bindings,
        factory_bindings,
        factory: None,
        callback_return_type: None,
        base: None,
        return_type,
        exported: is_exported(node),
    }
}

fn string_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    text(node, source).map(|value| value.trim_matches(['\'', '"']).into())
}

fn collect_imports(node: Node<'_>, source: &[u8], imports: &mut Vec<Import>) {
    if node.kind() == "import_statement"
        && let Some(source_name) = node
            .child_by_field_name("source")
            .and_then(|source_node| string_value(source_node, source))
    {
        let mut bindings = Vec::new();
        if let Some(clause) = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "import_clause")
        {
            let mut cursor = clause.walk();
            for child in clause.named_children(&mut cursor) {
                match child.kind() {
                    "identifier" => bindings.push(ImportBinding {
                        imported: "default".into(),
                        local: text(child, source).unwrap_or_default().into(),
                    }),
                    "named_imports" => {
                        let mut import_cursor = child.walk();
                        for specifier in child.named_children(&mut import_cursor) {
                            if specifier.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(imported) = specifier
                                .child_by_field_name("name")
                                .and_then(|name| text(name, source))
                            else {
                                continue;
                            };
                            let local = specifier
                                .child_by_field_name("alias")
                                .and_then(|alias| text(alias, source))
                                .unwrap_or(imported);
                            bindings.push(ImportBinding {
                                imported: imported.into(),
                                local: local.into(),
                            });
                        }
                    }
                    "namespace_import" => {
                        if let Some(local) =
                            child.named_child(0).and_then(|name| text(name, source))
                        {
                            bindings.push(ImportBinding {
                                imported: "*".into(),
                                local: local.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        imports.push(Import {
            source: source_name,
            bindings,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_imports(child, source, imports);
    }
}

fn collect_exports(node: Node<'_>, source: &[u8], exports: &mut Vec<Export>) {
    if node.kind() == "export_statement" {
        let export_source = node
            .child_by_field_name("source")
            .and_then(|source_node| string_value(source_node, source));
        if text(node, source).is_some_and(|statement| statement.starts_with("export default"))
            && let Some(local) = node
                .child_by_field_name("declaration")
                .and_then(|declaration| declaration.child_by_field_name("name"))
                .and_then(|name| text(name, source))
        {
            exports.push(Export {
                source: None,
                local: local.into(),
                exported: "default".into(),
            });
        }
        if let Some(clause) = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "export_clause")
        {
            let mut cursor = clause.walk();
            for specifier in clause.named_children(&mut cursor) {
                let Some(local) = specifier
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source))
                else {
                    continue;
                };
                let exported = specifier
                    .child_by_field_name("alias")
                    .and_then(|alias| text(alias, source))
                    .unwrap_or(local);
                exports.push(Export {
                    source: export_source.clone(),
                    local: local.into(),
                    exported: exported.into(),
                });
            }
        } else if export_source.is_some()
            && text(node, source).is_some_and(|statement| statement.starts_with("export *"))
        {
            exports.push(Export {
                source: export_source,
                local: "*".into(),
                exported: "*".into(),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_exports(child, source, exports);
    }
}

fn collect_definitions(
    node: Node<'_>,
    source: &[u8],
    scope: &mut Vec<String>,
    definitions: &mut Vec<Definition>,
) {
    match node.kind() {
        "class_declaration" | "interface_declaration" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
            else {
                return;
            };
            let class_name = qualified(scope, name);
            let mut class = definition(node, None, source, scope, name, DefinitionKind::Namespace);
            if node.kind() == "class_declaration" {
                class.base = node
                    .named_children(&mut node.walk())
                    .find(|child| child.kind() == "class_heritage")
                    .and_then(|heritage| {
                        heritage
                            .named_children(&mut heritage.walk())
                            .find(|child| child.kind() == "extends_clause")
                    })
                    .and_then(|extends| extends.child_by_field_name("value"))
                    .and_then(|base| text(base, source))
                    .map(str::to_owned);
            } else {
                class.base = node
                    .named_children(&mut node.walk())
                    .find(|child| child.kind() == "extends_type_clause")
                    .and_then(|extends| {
                        let types = extends
                            .named_children(&mut extends.walk())
                            .filter(|child| child.kind() == "type_identifier")
                            .collect::<Vec<_>>();
                        (types.len() == 1)
                            .then(|| text(types[0], source))
                            .flatten()
                            .map(str::to_owned)
                    });
            }
            let body = node.child_by_field_name("body");
            let bindings = body
                .map(|body| class_bindings(body, source))
                .unwrap_or_default();
            class.bindings.extend(bindings.iter().cloned());
            definitions.push(class);
            scope.push(name.into());
            if let Some(body) = body {
                let first_definition = definitions.len();
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    collect_definitions(child, source, scope, definitions);
                }
                for definition in &mut definitions[first_definition..] {
                    if definition
                        .qualified_name
                        .strip_prefix(&format!("{class_name}/"))
                        .is_some_and(|member| !member.contains('/'))
                    {
                        definition.bindings.extend(bindings.iter().cloned());
                    }
                }
            }
            scope.pop();
            return;
        }
        "type_alias_declaration" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
            else {
                return;
            };
            let mut alias = definition(node, None, source, scope, name, DefinitionKind::Namespace);
            alias.base = node
                .child_by_field_name("value")
                .filter(|value| value.kind() == "type_identifier")
                .and_then(|value| text(value, source))
                .map(str::to_owned);
            definitions.push(alias);
            return;
        }
        "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "method_definition"
        | "method_signature"
        | "abstract_method_signature" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
            else {
                return;
            };
            definitions.push(definition(
                node,
                node.child_by_field_name("body"),
                source,
                scope,
                name,
                DefinitionKind::Callable,
            ));
            scope.push(name.into());
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    collect_definitions(child, source, scope, definitions);
                }
            }
            scope.pop();
            return;
        }
        "variable_declarator" | "public_field_definition" | "field_definition" => {
            let value = node.child_by_field_name("value");
            if value.is_some_and(callable_value)
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source))
            {
                definitions.push(definition(
                    node,
                    value.and_then(|value| value.child_by_field_name("body")),
                    source,
                    scope,
                    name,
                    DefinitionKind::Callable,
                ));
                return;
            }
            if let Some(object) = value.filter(|value| value.kind() == "object")
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source))
            {
                definitions.push(definition(
                    node,
                    None,
                    source,
                    scope,
                    name,
                    DefinitionKind::Namespace,
                ));
                scope.push(name.into());
                collect_definitions(object, source, scope, definitions);
                scope.pop();
                return;
            }
            if let Some(base) = value
                .filter(|value| value.kind() == "new_expression")
                .and_then(|value| value.child_by_field_name("constructor"))
                .and_then(|constructor| text(constructor, source))
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source))
            {
                let mut instance =
                    definition(node, None, source, scope, name, DefinitionKind::Namespace);
                instance.base = Some(base.into());
                definitions.push(instance);
                return;
            }
            if let Some(factory) = value
                .filter(|value| value.kind() == "call_expression")
                .and_then(|value| value.child_by_field_name("function"))
                .filter(|function| function.kind() == "identifier")
                .and_then(|function| text(function, source))
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source))
            {
                let mut factory_definition =
                    definition(node, None, source, scope, name, DefinitionKind::Namespace);
                factory_definition.factory = Some(factory.into());
                if let Some((body, return_type)) =
                    value.and_then(|value| callback_return(value, source))
                {
                    collect_calls(body, source, body, &mut factory_definition.calls);
                    collect_bindings(body, source, body, &mut factory_definition.bindings);
                    collect_alias_bindings(
                        body,
                        source,
                        body,
                        &mut factory_definition.alias_bindings,
                    );
                    collect_factory_bindings(
                        body,
                        source,
                        body,
                        &mut factory_definition.factory_bindings,
                    );
                    factory_definition.callback_return_type = Some(return_type);
                }
                definitions.push(factory_definition);
                return;
            }
            if node.kind() == "variable_declarator"
                && scope.is_empty()
                && let Some(binding) = binding(node, source)
            {
                let mut instance = definition(
                    node,
                    None,
                    source,
                    scope,
                    &binding.receiver,
                    DefinitionKind::Namespace,
                );
                instance.base = Some(binding.type_name);
                definitions.push(instance);
                return;
            }
        }
        "pair" => {
            let value = node.child_by_field_name("value");
            if value.is_some_and(callable_value)
                && let Some(name) = node
                    .child_by_field_name("key")
                    .and_then(|name| text(name, source))
            {
                definitions.push(definition(
                    node,
                    value.and_then(|value| value.child_by_field_name("body")),
                    source,
                    scope,
                    name,
                    DefinitionKind::Callable,
                ));
                return;
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_definitions(child, source, scope, definitions);
    }
}

fn known_grammar_error(node: Node<'_>, source: &[u8]) -> ErrorDisposition {
    // Remove this recovery once the grammar includes
    // https://github.com/tree-sitter/tree-sitter-typescript/pull/358.
    let type_only_star_export = node.is_error()
        && text(node, source) == Some("type")
        && node.parent().is_some_and(|parent| {
            parent.kind() == "export_statement"
                && text(parent, source)
                    .is_some_and(|statement| statement.starts_with("export type * from"))
        });
    if type_only_star_export {
        ErrorDisposition::IgnoreKnownGrammarBug
    } else {
        ErrorDisposition::Report
    }
}

pub(super) fn recover_syntax<'tree>(
    root: Node<'tree>,
    source: &[u8],
) -> Result<Recovery<'tree>, RecoveryFailure> {
    recover_with(root, |node| known_grammar_error(node, source))
}

fn mask_jsx_attribute_ampersands(source: &str) -> Option<Vec<u8>> {
    let mut masked = source.as_bytes().to_vec();
    let mut changed = false;
    let mut index = 0;

    while index < masked.len() {
        if masked[index] != b'=' {
            index += 1;
            continue;
        }

        let mut value = index + 1;
        while masked.get(value).is_some_and(u8::is_ascii_whitespace) {
            value += 1;
        }
        let Some(&quote @ (b'\'' | b'"')) = masked.get(value) else {
            index += 1;
            continue;
        };

        value += 1;
        while value < masked.len() && masked[value] != quote {
            if masked[value] == b'\\' {
                value += 2;
                continue;
            }
            if masked[value] == b'&' && masked.get(value + 1).is_some_and(u8::is_ascii_alphabetic) {
                masked[value] = b'_';
                changed = true;
            }
            value += 1;
        }
        index = value.saturating_add(1);
    }

    changed.then_some(masked)
}

fn collect_top_level_call(node: Node<'_>, source: &[u8], calls: &mut Vec<Call>) {
    if node.kind() == "program" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_top_level_call(child, source, calls);
        }
        return;
    }
    if node.kind() != "expression_statement" {
        return;
    }
    let Some(expression) = node.named_child(0) else {
        return;
    };
    let kind = match expression.kind() {
        "call_expression" => CallKind::Direct,
        "new_expression" => CallKind::Constructor,
        _ => return,
    };
    if let Some(call) = call(expression, source, kind) {
        calls.push(call);
    }
}

pub fn analyze(
    source: &str,
    language: SourceLanguage,
) -> Result<TypescriptAnalysis, Box<dyn Error + Send + Sync>> {
    let plugins = built_in_plugins()?;
    let active = plugins.activate_direct(Path::new("input.ts"));
    analyze_with_plugins(source, language, Path::new("input.ts"), &plugins, &active)
}

pub(super) fn analyze_with_plugins(
    source: &str,
    language: SourceLanguage,
    path: &Path,
    plugins: &LanguageAnalyzer<TypescriptLanguage>,
    active_plugins: &beholder_indexing::ActivePlugins,
) -> Result<TypescriptAnalysis, Box<dyn Error + Send + Sync>> {
    let mut parser = Parser::new();
    let grammar = match language {
        SourceLanguage::JavaScript => tree_sitter_javascript::LANGUAGE,
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        SourceLanguage::Jsx | SourceLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    parser.set_language(&grammar.into())?;
    let mut tree = parser
        .parse(source, None)
        .ok_or("JavaScript/TypeScript parser returned no tree")?;
    if matches!(language, SourceLanguage::Jsx | SourceLanguage::Tsx)
        && tree.root_node().has_error()
        && let Some(parser_source) = mask_jsx_attribute_ampersands(source)
    {
        // https://github.com/tree-sitter/tree-sitter-javascript/pull/305 only fixed
        // literal ampersands that are not followed by a letter. Remove this same-width
        // recovery when the upstream JSX grammar accepts URLs such as `?a=1&b=2`.
        tree = parser
            .parse(&parser_source, None)
            .ok_or("JavaScript/TypeScript parser returned no tree")?;
    }
    let mut definitions = Vec::new();
    let mut calls = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut string_constants = Vec::new();
    let mut graphql_documents = Vec::new();
    let root = tree.root_node();
    let recovery = recover_syntax(root, source.as_bytes()).map_err(|_| {
        UnsafeTreeRecovery::new("JavaScript/TypeScript", "missing syntax may change nesting")
    })?;
    let incomplete = recovery.is_incomplete();
    for root in recovery.roots {
        collect_definitions(root, source.as_bytes(), &mut Vec::new(), &mut definitions);
        collect_top_level_call(root, source.as_bytes(), &mut calls);
        collect_imports(root, source.as_bytes(), &mut imports);
        collect_exports(root, source.as_bytes(), &mut exports);
        collect_string_constants(root, source.as_bytes(), &mut string_constants);
        collect_graphql_documents(root, source.as_bytes(), &mut graphql_documents);
    }
    if incomplete && definitions.is_empty() && calls.is_empty() {
        return Err(UnsafeTreeRecovery::new(
            "JavaScript/TypeScript",
            "no unaffected definitions remain",
        )
        .into());
    }
    let mut analysis = TypescriptAnalysis {
        language,
        calls,
        definitions,
        imports,
        exports,
        string_constants,
        graphql_documents,
        nest_modules: Vec::new(),
        nest_providers: Vec::new(),
        generated: false,
        parse_error_lines: recovery.error_lines,
    };
    plugins.recognize(
        SourceRecognitionInput {
            path,
            text: source,
            syntax: &tree,
        },
        &mut analysis,
        active_plugins,
    )?;
    Ok(analysis)
}

pub fn diagnostics_from_analysis(
    analysis: &TypescriptAnalysis,
    path: &Path,
) -> Vec<AnalysisDiagnostic> {
    analysis
        .parse_error_lines
        .iter()
        .map(|line| AnalysisDiagnostic {
            code: "typescript.parse_recovery".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: path.into(),
            line: u32::try_from(*line).ok(),
            detail: Some("tree-sitter discarded an invalid syntax unit".into()),
        })
        .collect()
}

pub(super) fn source_stem(path: &Path) -> String {
    path.with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

pub fn observations_from_analysis(
    repository: &str,
    analysis: &TypescriptAnalysis,
    source: &str,
    path: &Path,
) -> Vec<Observation> {
    semantics_from_analysis(repository, analysis, source, path).0
}

pub(super) fn semantics_from_analysis(
    repository: &str,
    analysis: &TypescriptAnalysis,
    _source: &str,
    path: &Path,
) -> (Vec<Observation>, Vec<SemanticCandidate>) {
    let language = analysis.language.id_segment();
    let module_id = format!("repo://{repository}/{language}/{}", source_stem(path));
    let source_id = format!("repo://{repository}/{language}-source/{}", path.display());
    let mut observations = vec![Observation::structural(
        source_id,
        StructuralRelation::Defines,
        module_id.clone(),
        path.display().to_string(),
    )];
    let mut candidates = Vec::new();
    let ids = analysis
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.qualified_name.clone(),
                format!("{module_id}/{}", definition.qualified_name),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for call in &analysis.calls {
        let (observation, candidate) = call_semantics(
            repository,
            language,
            &module_id,
            observation_target(language, &ids, "", call),
            call,
            path,
        );
        observations.push(observation);
        candidates.extend(candidate);
    }
    for definition in &analysis.definitions {
        let id = &ids[&definition.qualified_name];
        let parent_name = definition
            .qualified_name
            .rsplit_once('/')
            .map(|(parent, _)| parent);
        let parent = parent_name
            .and_then(|parent| ids.get(parent))
            .unwrap_or(&module_id);
        observations.push(Observation::structural(
            parent.clone(),
            StructuralRelation::Defines,
            id.clone(),
            format!("{}:{}", path.display(), definition.line),
        ));
        if definition.kind != DefinitionKind::Callable && definition.calls.is_empty() {
            continue;
        }
        let scope = parent_name.unwrap_or_default();
        for call in &definition.calls {
            let target = observation_target(language, &ids, scope, call);
            let (observation, candidate) =
                call_semantics(repository, language, id, target, call, path);
            observations.push(observation);
            candidates.extend(candidate);
        }
    }
    if analysis.generated {
        for observation in &mut observations {
            observation.provenance = Provenance::Generated;
        }
    }
    (observations, candidates)
}

fn call_semantics(
    repository: &str,
    language: &str,
    from: &str,
    target: String,
    call: &Call,
    path: &Path,
) -> (Observation, Option<SemanticCandidate>) {
    let evidence = format!("{}:{}", path.display(), call.line);
    let observation = Observation::dependency(
        from,
        DependencyRelation::Calls,
        target.clone(),
        evidence.clone(),
    );
    let unresolved = ["call", "method", "constructor"]
        .into_iter()
        .any(|kind| target.starts_with(&format!("{language}-{kind}://")));
    let candidate = unresolved.then(|| {
        let span = SourceSpan {
            path: path.into(),
            start: SourcePosition {
                line: call.start_line,
                character: call.start_character,
            },
            end: SourcePosition {
                line: call.end_line,
                character: call.end_character,
            },
        };
        let mut digest = Sha256::new();
        for part in [
            repository.as_bytes(),
            from.as_bytes(),
            DependencyRelation::Calls.as_str().as_bytes(),
            target.as_bytes(),
            path.as_os_str().as_encoded_bytes(),
            &call.start_line.to_le_bytes(),
            &call.start_character.to_le_bytes(),
            &call.end_line.to_le_bytes(),
            &call.end_character.to_le_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        SemanticCandidate {
            id: format!("{:x}", digest.finalize()),
            repository: repository.into(),
            from: from.into(),
            relation: DependencyRelation::Calls,
            unresolved_to: target.into(),
            span,
            evidence: evidence.into(),
        }
    });
    (observation, candidate)
}

fn observation_target(
    language: &str,
    ids: &BTreeMap<String, String>,
    scope: &str,
    call: &Call,
) -> String {
    if call.kind == CallKind::Member
        && let Some(receiver) = &call.returned_receiver
    {
        return format!("{language}-returned://{receiver}/{}", call.name);
    }
    match call.kind {
        CallKind::Direct => ids
            .get(&qualified(
                &scope.split('/').map(str::to_owned).collect::<Vec<_>>(),
                &call.name,
            ))
            .or_else(|| ids.get(&call.name))
            .cloned()
            .unwrap_or_else(|| format!("{language}-call://{}", call.name)),
        CallKind::Member if call.receiver.as_deref() == Some("this") => ids
            .get(&format!("{scope}/{}", call.name))
            .cloned()
            .unwrap_or_else(|| format!("{language}-method://this/{}", call.name)),
        CallKind::Member => format!(
            "{language}-method://{}/{}",
            call.receiver.as_deref().unwrap_or("_"),
            call.name
        ),
        CallKind::Constructor => ids
            .get(&call.name)
            .cloned()
            .unwrap_or_else(|| format!("{language}-constructor://{}", call.name)),
    }
}

pub fn entities_from_analysis(
    repository: &str,
    analysis: &TypescriptAnalysis,
    path: &Path,
) -> Vec<EntityFact> {
    let module_id = format!(
        "repo://{}/{}/{}",
        repository,
        analysis.language.id_segment(),
        source_stem(path)
    );
    let mut entities =
        std::iter::once(EntityFact::new(module_id.clone(), EntityKind::Namespace, None).unwrap())
            .chain(analysis.definitions.iter().map(|definition| {
                EntityFact::new(
                    format!("{module_id}/{}", definition.qualified_name),
                    match definition.kind {
                        DefinitionKind::Namespace => EntityKind::Namespace,
                        DefinitionKind::Callable => EntityKind::Callable,
                    },
                    None,
                )
                .unwrap()
            }))
            .collect::<Vec<_>>();
    entities.sort_by(|left, right| left.id.cmp(&right.id));
    entities.dedup();
    entities
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{Confidence, SemanticRelation};

    fn observations(source: &str, path: &str) -> Vec<Observation> {
        let path = Path::new(path);
        let language = SourceLanguage::from_path(path).unwrap();
        let plugins = built_in_plugins().unwrap();
        let active = plugins.activate_direct(path);
        let analysis = analyze_with_plugins(source, language, path, &plugins, &active).unwrap();
        observations_from_analysis("example", &analysis, source, path)
    }

    #[test]
    fn parses_all_four_source_forms_with_their_explicit_grammar() {
        for (path, source, expected) in [
            ("src/plain.js", "export function run() {}", "/javascript/"),
            (
                "src/view.jsx",
                "export const View = () => <main />",
                "/javascript/",
            ),
            (
                "src/plain.ts",
                "export function run(value: string): string { return value }",
                "/typescript/",
            ),
            (
                "src/view.tsx",
                "export const View = (): JSX.Element => <main />",
                "/typescript/",
            ),
        ] {
            assert!(observations(source, path).iter().any(|observation| {
                observation.to.as_str().contains(expected)
                    && observation.to.as_str().ends_with(if path.contains("view") {
                        "/View"
                    } else {
                        "/run"
                    })
            }));
        }
    }

    #[test]
    fn resolves_local_and_this_calls_but_preserves_external_receivers() {
        let source = "function helper() {} class Worker { run() { helper(); this.stop(); api.send(); new Job(); } stop() {} }";
        let observations = observations(source, "src/worker.ts");
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert!(calls.contains(&"repo://example/typescript/src/worker/helper"));
        assert!(calls.contains(&"repo://example/typescript/src/worker/Worker/stop"));
        assert!(calls.contains(&"typescript-method://api/send"));
        assert!(calls.contains(&"typescript-constructor://Job"));
        assert!(
            observations
                .iter()
                .all(|observation| observation.confidence == Confidence::Exact)
        );
    }

    #[test]
    fn emits_stable_compiler_candidates_for_unresolved_calls() {
        let source = "function helper() {} function run() { helper(); api.send(); new Job(); }";
        let path = Path::new("src/worker.ts");
        let analysis = analyze(source, SourceLanguage::TypeScript).unwrap();

        let (_, candidates) = semantics_from_analysis("example", &analysis, source, path);

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.repository == "example")
        );
        let send = candidates
            .iter()
            .find(|candidate| candidate.unresolved_to.as_str() == "typescript-method://api/send")
            .unwrap();
        assert_eq!(send.span.path, path);
        assert_eq!(
            &source[send.span.start.character as usize..send.span.end.character as usize],
            "send"
        );
        assert!(!send.id.is_empty());
    }

    #[test]
    fn compiler_candidate_columns_use_utf16_units() {
        let source = "function run() { '😀'; api.send(); }";
        let path = Path::new("src/worker.ts");
        let analysis = analyze(source, SourceLanguage::TypeScript).unwrap();

        let (_, candidates) = semantics_from_analysis("example", &analysis, source, path);

        let send = candidates
            .iter()
            .find(|candidate| candidate.unresolved_to.as_str() == "typescript-method://api/send")
            .unwrap();
        let byte = source.find("send").unwrap();
        assert_eq!(
            send.span.start.character as usize,
            source[..byte].encode_utf16().count()
        );
    }

    #[test]
    fn marks_only_explicit_generated_sources_as_generated() {
        let generated = observations(
            "// Generated by example. Do not edit.\nexport function run() {}",
            "src/client.ts",
        );
        assert!(
            generated
                .iter()
                .all(|observation| observation.provenance == Provenance::Generated)
        );
        let ordinary = observations("export function run() {}", "src/generated/client.ts");
        assert!(
            ordinary
                .iter()
                .all(|observation| observation.provenance == Provenance::Ast)
        );
    }

    #[test]
    fn accepts_type_only_star_re_exports() {
        let analysis =
            analyze("export type * from './items';", SourceLanguage::TypeScript).unwrap();

        assert!(analysis.parse_error_lines.is_empty());
        assert!(analysis.exports.is_empty());
    }

    #[test]
    fn preserves_ignored_grammar_siblings_during_recovery() {
        let source = "export type * from './items';\nconst = ;\nexport function safe() {}";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();

        let recovery = recover_syntax(tree.root_node(), source.as_bytes()).unwrap();
        let roots = recovery
            .roots
            .iter()
            .map(|root| text(*root, source.as_bytes()).unwrap())
            .collect::<Vec<_>>();

        assert!(recovery.is_incomplete());
        assert!(roots.contains(&"export type * from './items';"));
        assert!(roots.contains(&"export function safe() {}"));
        assert!(!roots.contains(&"const = ;"));
    }

    #[test]
    fn ignored_errors_do_not_hide_missing_syntax() {
        let source = "function broken() {\nfunction nested() {}";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();

        let error = beholder_adapters_treesitter::recover_with(tree.root_node(), |_| {
            ErrorDisposition::IgnoreKnownGrammarBug
        })
        .unwrap_err();

        assert!(matches!(error, RecoveryFailure::MissingSyntax { .. }));
    }

    #[test]
    fn parses_ampersands_in_jsx_attribute_strings() {
        let analysis = analyze(
            r#"const view = <Filter initialUrl="/test?a=1&b=2" />;"#,
            SourceLanguage::Tsx,
        )
        .unwrap();

        assert!(analysis.parse_error_lines.is_empty());
    }

    #[test]
    fn recovers_valid_symbols_and_reports_parse_errors() {
        let analysis = analyze(
            "const = ; export function stillIndexed() {}",
            SourceLanguage::TypeScript,
        )
        .unwrap();

        assert_eq!(analysis.definitions.len(), 1);
        assert!(
            analysis
                .definitions
                .iter()
                .any(|definition| definition.qualified_name == "stillIndexed")
        );
        assert_eq!(
            diagnostics_from_analysis(&analysis, Path::new("src/broken.ts"))[0].code,
            "typescript.parse_recovery"
        );
    }

    #[test]
    fn rejects_missing_delimiters_that_can_change_nesting() {
        let error = analyze(
            "function broken() {\nfunction nested() {}",
            SourceLanguage::TypeScript,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<UnsafeTreeRecovery>().is_some());
    }

    #[test]
    fn parses_keyword_jsx_attributes() {
        let analysis = analyze(
            "export default loader => <Loader if static fixedMobile />;",
            SourceLanguage::Jsx,
        )
        .unwrap();

        assert!(analysis.parse_error_lines.is_empty());
    }

    #[test]
    fn parses_generic_import_types() {
        let analysis = analyze(
            "declare type Mock<T = any> = import('vitest').Mock<T>;",
            SourceLanguage::TypeScript,
        )
        .unwrap();

        assert!(analysis.parse_error_lines.is_empty());
    }

    #[test]
    fn preserves_decorator_invocations_as_calls() {
        let source = "function controller() {} function traced() {} function helper() {} @controller() class Api { @traced() run() { helper(); } }";
        let calls = observations(source, "src/decorated.ts")
            .into_iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| {
                (
                    observation.from.as_str().to_owned(),
                    observation.to.as_str().to_owned(),
                )
            })
            .collect::<Vec<_>>();

        for expected in [
            (
                "repo://example/typescript/src/decorated/Api",
                "repo://example/typescript/src/decorated/controller",
            ),
            (
                "repo://example/typescript/src/decorated/Api/run",
                "repo://example/typescript/src/decorated/traced",
            ),
            (
                "repo://example/typescript/src/decorated/Api/run",
                "repo://example/typescript/src/decorated/helper",
            ),
        ] {
            assert!(
                calls
                    .iter()
                    .any(|call| call.0 == expected.0 && call.1 == expected.1),
                "missing {expected:?}: {calls:?}"
            );
        }
    }

    #[test]
    fn maps_embedded_operations_and_grats_resolvers() {
        let documents = r#"
            export const Packages_Detail_Query = gql(`
              query Packages_Detail_Query { packageTemplatePreview { id } }
            `);
            export const PaymentCompleted = gql(`
              subscription PaymentCompleted { paymentCompleted { id } }
            `);
            export const Tada_Query = graphql(/* GraphQL */ "query Tada_Query { location { id } }");
        "#;
        let component = r#"
            import { Packages_Detail_Query, PaymentCompleted } from './PackageDetail.gql';
            export function PackageDetail() {
              useSWRGQL(Packages_Detail_Query, variables);
              return <Checkout subscription={enabled ? PaymentCompleted : null} />;
            }
        "#;
        let resolver = r#"
            class GetPackageTemplatePreview {
              /** @gqlQueryField packageTemplatePreview */
              @validateInput(schema)
              static async query(args: Args) {}
            }
        "#;
        let document_path = Path::new("src/PackageDetail.gql.tsx");
        let component_path = Path::new("src/PackageDetail.tsx");
        let resolver_path = Path::new("src/package-details.ts");
        let document_analysis = analyze(documents, SourceLanguage::Tsx).unwrap();
        let component_analysis = analyze(component, SourceLanguage::Tsx).unwrap();
        let resolver_analysis = analyze(resolver, SourceLanguage::TypeScript).unwrap();
        let sources = [
            (document_path, &document_analysis, documents),
            (component_path, &component_analysis, component),
            (resolver_path, &resolver_analysis, resolver),
        ];
        let graphql_sources = sources
            .iter()
            .map(|(path, analysis, source)| crate::GraphqlResolverSource {
                path,
                analysis,
                source,
            })
            .collect::<Vec<_>>();
        let mut observations = sources
            .iter()
            .flat_map(|(path, analysis, source)| {
                observations_from_analysis("example", analysis, source, path)
            })
            .collect::<Vec<_>>();
        crate::resolve_repository_calls(
            "example",
            &mut observations,
            &sources
                .iter()
                .map(|(path, analysis, _)| (*path, *analysis))
                .collect::<Vec<_>>(),
            &[],
            &[],
        );
        let grats = crate::collect_graphql_resolvers(crate::GraphqlResolverInput {
            repository: "example",
            sources: &graphql_sources,
            manifests: &[(
                Path::new("package.json"),
                r#"{"dependencies":{"grats":"0.0.34"}}"#,
            )],
        });
        let graphql = crate::collect_graphql_facts(crate::GraphqlFactInput {
            repository: "example",
            sources: &graphql_sources,
            schemas: &[],
        });
        let entities = sources
            .iter()
            .flat_map(|(path, analysis, _)| entities_from_analysis("example", analysis, path))
            .chain(grats.entities)
            .chain(graphql.entities)
            .collect::<Vec<_>>();
        observations.extend(grats.observations);
        observations.extend(graphql.observations);

        assert!(entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Packages_Detail_Query"
                && entity.kind == EntityKind::GraphqlOperation
        }));
        assert!(entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Tada_Query"
                && entity.kind == EntityKind::GraphqlOperation
        }));
        for (from, relation, to) in [
            (
                "repo://example/typescript/src/PackageDetail/PackageDetail",
                DependencyRelation::CallsGraphql,
                "graphql-operation://Packages_Detail_Query",
            ),
            (
                "graphql-operation://Packages_Detail_Query",
                DependencyRelation::Selects,
                "graphql-field://Query/packageTemplatePreview",
            ),
            (
                "repo://example/typescript/src/PackageDetail/PackageDetail",
                DependencyRelation::CallsGraphql,
                "graphql-operation://PaymentCompleted",
            ),
            (
                "graphql-field://Query/packageTemplatePreview",
                DependencyRelation::ResolvedBy,
                "repo://example/typescript/src/package-details/GetPackageTemplatePreview/query",
            ),
        ] {
            assert!(
                observations.iter().any(|observation| {
                    observation.from.as_str() == from
                        && observation.relation == SemanticRelation::Dependency(relation)
                        && observation.to.as_str() == to
                }),
                "missing {from} {} {to}: {observations:#?}",
                relation.as_str()
            );
        }
    }
}
