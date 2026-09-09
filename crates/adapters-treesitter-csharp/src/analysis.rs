use super::model::*;
use beholder_adapters_treesitter::recover;
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, CallableClauseRole, EntityFact, EntityKind,
    EvidenceContext, Observation, PatternConstruct, Provenance, SourceExcerpt, SourcePosition,
    SourceRange, StructuralRelation, UnsafeTreeRecovery,
};
use std::{collections::BTreeMap, error::Error, path::Path};
use tree_sitter::{Node, Parser};

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn byte_position(source: &[u8], byte: usize) -> SourcePosition {
    let prefix = std::str::from_utf8(&source[..byte]).expect("C# source is valid UTF-8");
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    SourcePosition {
        line: u32::try_from(
            prefix[..line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
        )
        .unwrap_or(u32::MAX),
        character: u32::try_from(prefix[line_start..].encode_utf16().count()).unwrap_or(u32::MAX),
    }
}

fn byte_range(source: &[u8], start: usize, end: usize) -> SourceRange {
    SourceRange {
        start: byte_position(source, start),
        end: byte_position(source, end),
    }
}

fn source_position(node: Node<'_>, source: &[u8], end: bool) -> SourcePosition {
    let (byte, point) = if end {
        (node.end_byte(), node.end_position())
    } else {
        (node.start_byte(), node.start_position())
    };
    let line_start = byte.saturating_sub(point.column);
    SourcePosition {
        line: u32::try_from(point.row).unwrap_or(u32::MAX),
        character: u32::try_from(
            std::str::from_utf8(&source[line_start..byte])
                .map(|text| text.encode_utf16().count())
                .unwrap_or(point.column),
        )
        .unwrap_or(u32::MAX),
    }
}

fn source_range(node: Node<'_>, source: &[u8]) -> SourceRange {
    SourceRange {
        start: source_position(node, source, false),
        end: source_position(node, source, true),
    }
}

fn source_excerpt(node: Node<'_>, source: &[u8]) -> Option<SourceExcerpt> {
    Some(SourceExcerpt {
        text: text(node, source)?.into(),
        range: source_range(node, source),
    })
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

fn first_non_comment_child(node: Node<'_>) -> Option<Node<'_>> {
    node.named_children(&mut node.walk())
        .find(|child| child.kind() != "comment")
}

fn callable_context(
    definition: Node<'_>,
    source: &[u8],
    role: CallableClauseRole,
) -> Option<EvidenceContext> {
    let body = definition.child_by_field_name("body");
    let signature_end = body.map_or_else(|| definition.end_byte(), |body| body.start_byte());
    let signature_end = text(definition, source)?
        .get(..signature_end.saturating_sub(definition.start_byte()))?
        .trim_end()
        .len()
        + definition.start_byte();
    Some(EvidenceContext::CallableClause {
        role,
        signature: SourceExcerpt {
            text: std::str::from_utf8(&source[definition.start_byte()..signature_end])
                .ok()?
                .into(),
            range: byte_range(source, definition.start_byte(), signature_end),
        },
        guard: None,
        definition_range: source_range(definition, source),
    })
}

fn pattern_contexts(selection: Node<'_>, call: Node<'_>, source: &[u8]) -> Vec<EvidenceContext> {
    let (construct, selector, arm_kind) = match selection.kind() {
        "switch_statement" => (
            PatternConstruct::SwitchStatement,
            selection.child_by_field_name("value"),
            "switch_section",
        ),
        "switch_expression" => (
            PatternConstruct::SwitchExpression,
            selection
                .named_children(&mut selection.walk())
                .find(|child| child.kind() != "switch_expression_arm"),
            "switch_expression_arm",
        ),
        _ => return Vec::new(),
    };
    let Some(selector) = selector else {
        return Vec::new();
    };
    if contains(selector, call) {
        return Vec::new();
    }
    let mut ancestor = call.parent();
    let arm = loop {
        let Some(candidate) = ancestor else {
            return Vec::new();
        };
        let belongs_to_selection = match construct {
            PatternConstruct::SwitchStatement => candidate
                .parent()
                .and_then(|body| body.parent())
                .is_some_and(|parent| parent == selection),
            PatternConstruct::SwitchExpression => candidate.parent() == Some(selection),
            _ => false,
        };
        if candidate.kind() == arm_kind && belongs_to_selection {
            break candidate;
        }
        if candidate == selection {
            return Vec::new();
        }
        ancestor = candidate.parent();
    };
    let label_context = |label: Node<'_>, arm_range: SourceRange| {
        let is_default = label
            .children(&mut label.walk())
            .any(|child| child.kind() == "default");
        let pattern = if is_default {
            None
        } else {
            first_non_comment_child(label)
        };
        let guard_clause = label
            .named_children(&mut label.walk())
            .find(|child| child.kind() == "when_clause");
        let guard = guard_clause.and_then(first_non_comment_child);
        if pattern.is_some_and(|pattern| contains(pattern, call))
            || guard_clause.is_some_and(|guard| contains(guard, call))
        {
            return None;
        }
        Some(EvidenceContext::PatternArm {
            construct,
            selector: source_excerpt(selector, source),
            pattern: pattern.and_then(|pattern| source_excerpt(pattern, source)),
            guard: guard.and_then(|guard| source_excerpt(guard, source)),
            is_default,
            arm_range,
        })
    };
    if construct == PatternConstruct::SwitchExpression {
        return label_context(arm, source_range(arm, source))
            .into_iter()
            .collect();
    }

    let has_body = |section: Node<'_>| {
        let is_default = section
            .children(&mut section.walk())
            .any(|child| child.kind() == "default");
        let pattern = (!is_default)
            .then(|| first_non_comment_child(section))
            .flatten();
        section.named_children(&mut section.walk()).any(|child| {
            Some(child) != pattern && !matches!(child.kind(), "comment" | "when_clause")
        })
    };
    let mut labels = vec![arm];
    let mut previous = arm.prev_named_sibling();
    while let Some(section) = previous
        && section.kind() == "switch_section"
        && !has_body(section)
    {
        labels.push(section);
        previous = section.prev_named_sibling();
    }
    labels.reverse();
    let arm_range = byte_range(source, labels[0].start_byte(), arm.end_byte());
    labels
        .into_iter()
        .map(|label| label_context(label, arm_range.clone()))
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

fn lexical_contexts(
    call: Node<'_>,
    definition: Node<'_>,
    source: &[u8],
) -> Vec<Vec<EvidenceContext>> {
    let mut selections = Vec::new();
    let mut ancestor = call.parent();
    while let Some(candidate) = ancestor {
        if candidate == definition {
            break;
        }
        if matches!(candidate.kind(), "switch_statement" | "switch_expression") {
            let contexts = pattern_contexts(candidate, call, source);
            if !contexts.is_empty() {
                selections.push(contexts);
            }
        }
        ancestor = candidate.parent();
    }
    selections.reverse();
    selections
        .into_iter()
        .fold(vec![Vec::new()], |paths, contexts| {
            paths
                .into_iter()
                .flat_map(|path| {
                    contexts.iter().map(move |context| {
                        let mut path = path.clone();
                        path.push(context.clone());
                        path
                    })
                })
                .collect()
        })
}

fn declaration_kind(node: Node<'_>) -> Option<DefinitionKind> {
    match node.kind() {
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            Some(DefinitionKind::Namespace)
        }
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "enum_declaration"
        | "delegate_declaration" => Some(DefinitionKind::Type),
        "method_declaration"
        | "constructor_declaration"
        | "local_function_statement"
        | "operator_declaration"
        | "conversion_operator_declaration" => Some(DefinitionKind::Callable),
        _ => None,
    }
}

fn declaration_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let name = text(node.child_by_field_name("name")?, source)?;
    if declaration_kind(node) != Some(DefinitionKind::Callable) {
        return Some(name.replace('.', "/"));
    }
    let parameters = parameters(node, source)
        .into_iter()
        .map(|parameter| parameter.type_name)
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{name}({parameters})"))
}

fn parameters(node: Node<'_>, source: &[u8]) -> Vec<Parameter> {
    node.child_by_field_name("parameters")
        .map(|parameters| {
            let mut cursor = parameters.walk();
            parameters
                .named_children(&mut cursor)
                .filter(|parameter| parameter.kind() == "parameter")
                .filter_map(|parameter| {
                    let name = parameter.child_by_field_name("name")?;
                    let type_ = parameter.child_by_field_name("type")?;
                    let is_optional =
                        parameter
                            .named_children(&mut parameter.walk())
                            .any(|child| {
                                child != name
                                    && child != type_
                                    && !matches!(
                                        child.kind(),
                                        "attribute_list"
                                            | "modifier"
                                            | "preproc_if_in_attribute_list"
                                    )
                            });
                    Some(Parameter {
                        name: text(name, source)?.into(),
                        type_name: text(type_, source)?.trim().into(),
                        is_extension: text(parameter, source)?.trim_start().starts_with("this "),
                        is_optional,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn arguments(node: Node<'_>, source: &[u8]) -> Vec<Argument> {
    let Some(arguments) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "argument_list")
    else {
        return Vec::new();
    };
    arguments
        .named_children(&mut arguments.walk())
        .filter(|argument| argument.kind() == "argument")
        .filter_map(|argument| {
            let name = argument
                .child_by_field_name("name")
                .and_then(|name| text(name, source).map(str::to_owned));
            let expression = argument
                .named_children(&mut argument.walk())
                .find(|child| Some(*child) != argument.child_by_field_name("name"))?;
            Some(Argument {
                name,
                expression: text(expression, source)?.into(),
            })
        })
        .collect()
}

fn simple_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let name = node
        .child_by_field_name("name")
        .and_then(|name| text(name, source))
        .or_else(|| text(node, source))?;
    Some(name.split('<').next().unwrap_or(name).trim().to_owned())
}

fn type_arguments(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let generic = if node.kind() == "generic_name" {
        Some(node)
    } else {
        node.child_by_field_name("name")
            .filter(|name| name.kind() == "generic_name")
    };
    let Some(arguments) = generic.and_then(|generic| {
        generic
            .named_children(&mut generic.walk())
            .find(|child| child.kind() == "type_argument_list")
    }) else {
        return Vec::new();
    };
    arguments
        .named_children(&mut arguments.walk())
        .filter_map(|argument| text(argument, source).map(str::to_owned))
        .collect()
}

fn call(node: Node<'_>, source: &[u8]) -> Option<Call> {
    match node.kind() {
        "invocation_expression" => {
            let function = node.child_by_field_name("function")?;
            if function.kind() == "member_access_expression" {
                return Some(Call {
                    expression: text(node, source)?.into(),
                    kind: CallKind::Member,
                    receiver: function
                        .child_by_field_name("expression")
                        .and_then(|receiver| text(receiver, source))
                        .map(str::to_owned),
                    name: simple_name(function, source)?,
                    type_arguments: type_arguments(function, source),
                    arguments: arguments(node, source),
                    line: node.start_position().row + 1,
                    range: Some(source_range(node, source)),
                    contexts: Vec::new(),
                });
            }
            Some(Call {
                expression: text(node, source)?.into(),
                kind: CallKind::Direct,
                receiver: None,
                name: simple_name(function, source)?,
                type_arguments: type_arguments(function, source),
                arguments: arguments(node, source),
                line: node.start_position().row + 1,
                range: Some(source_range(node, source)),
                contexts: Vec::new(),
            })
        }
        "object_creation_expression" => Some(Call {
            expression: text(node, source)?.into(),
            kind: CallKind::Constructor,
            receiver: None,
            name: node
                .child_by_field_name("type")
                .and_then(|kind| text(kind, source))?
                .to_owned(),
            type_arguments: Vec::new(),
            arguments: arguments(node, source),
            line: node.start_position().row + 1,
            range: Some(source_range(node, source)),
            contexts: Vec::new(),
        }),
        _ => None,
    }
}

fn collect_calls(
    node: Node<'_>,
    source: &[u8],
    definition: Node<'_>,
    enclosing: &EvidenceContext,
    calls: &mut Vec<Call>,
) {
    if node != definition && declaration_kind(node) == Some(DefinitionKind::Callable) {
        return;
    }
    if let Some(call) = call(node, source) {
        for contexts in lexical_contexts(node, definition, source) {
            let mut call = call.clone();
            call.contexts = std::iter::once(enclosing.clone()).chain(contexts).collect();
            calls.push(call);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, definition, enclosing, calls);
    }
}

fn collect_locals(node: Node<'_>, source: &[u8], root: Node<'_>, locals: &mut Vec<Binding>) {
    if node != root && declaration_kind(node) == Some(DefinitionKind::Callable) {
        return;
    }
    if node.kind() == "variable_declaration"
        && let Some(declared_type) = node
            .child_by_field_name("type")
            .and_then(|type_| text(type_, source))
    {
        let mut cursor = node.walk();
        for declarator in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "variable_declarator")
        {
            let Some(name) = declarator
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
            else {
                continue;
            };
            let inferred_type = declarator
                .named_children(&mut declarator.walk())
                .find(|child| {
                    Some(*child) != declarator.child_by_field_name("name")
                        && child.kind() != "bracketed_argument_list"
                })
                .and_then(|value| match value.kind() {
                    "object_creation_expression" | "cast_expression" => value
                        .child_by_field_name("type")
                        .and_then(|type_| text(type_, source)),
                    _ => None,
                });
            if declared_type != "var" || inferred_type.is_some() {
                locals.push(Binding {
                    name: name.into(),
                    type_name: inferred_type.unwrap_or(declared_type).into(),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_locals(child, source, root, locals);
    }
}

fn collect_definitions(
    node: Node<'_>,
    source: &[u8],
    scope: &[String],
    definitions: &mut Vec<Definition>,
) {
    if node.kind() == "compilation_unit" {
        let mut file_scope = scope.to_vec();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "file_scoped_namespace_declaration" {
                collect_definitions(child, source, scope, definitions);
                if let Some(name) = declaration_name(child, source) {
                    file_scope.extend(name.split('/').map(str::to_owned));
                }
            } else {
                collect_definitions(child, source, &file_scope, definitions);
            }
        }
        return;
    }
    let Some(kind) = declaration_kind(node) else {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_definitions(child, source, scope, definitions);
        }
        return;
    };
    let Some(name) = declaration_name(node, source) else {
        return;
    };
    let qualified_name = scope
        .iter()
        .chain(std::iter::once(&name))
        .cloned()
        .collect::<Vec<_>>()
        .join("/");
    let mut calls = Vec::new();
    let mut locals = Vec::new();
    if kind == DefinitionKind::Callable
        && let Some(body) = node.child_by_field_name("body")
        && let Some(enclosing) = callable_context(node, source, CallableClauseRole::Enclosing)
    {
        collect_calls(body, source, node, &enclosing, &mut calls);
        collect_locals(body, source, body, &mut locals);
    }
    definitions.push(Definition {
        qualified_name,
        kind,
        return_type: (kind == DefinitionKind::Callable)
            .then(|| {
                node.child_by_field_name("returns")
                    .and_then(|returns| text(returns, source))
                    .map(str::to_owned)
            })
            .flatten(),
        base_types: if kind == DefinitionKind::Type {
            node.named_children(&mut node.walk())
                .find(|child| child.kind() == "base_list")
                .map(|bases| {
                    let mut cursor = bases.walk();
                    bases
                        .named_children(&mut cursor)
                        .filter_map(|base| text(base, source).map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        is_static: node
            .children(&mut node.walk())
            .any(|child| child.kind() == "modifier" && text(child, source) == Some("static")),
        line: node.start_position().row + 1,
        signature: (kind == DefinitionKind::Callable)
            .then(|| callable_context(node, source, CallableClauseRole::Declaration))
            .flatten()
            .and_then(|context| match context {
                EvidenceContext::CallableClause { signature, .. } => Some(signature),
                _ => None,
            }),
        definition_range: Some(source_range(node, source)),
        parameters: if kind == DefinitionKind::Callable {
            parameters(node, source)
        } else {
            Vec::new()
        },
        locals,
        calls,
    });
    let mut nested_scope = scope.to_vec();
    nested_scope.extend(name.split('/').map(str::to_owned));
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child != node.child_by_field_name("name").unwrap_or(child) {
            collect_definitions(child, source, &nested_scope, definitions);
        }
    }
}

pub fn analyze(source: &str) -> Result<CsharpAnalysis, Box<dyn Error + Send + Sync>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("C# parser returned no tree")?;
    let mut definitions = Vec::new();
    let root = tree.root_node();
    let recovery = recover(root)
        .map_err(|_| UnsafeTreeRecovery::new("C#", "missing syntax may change nesting"))?;
    let incomplete = recovery.is_incomplete();
    for root in recovery.roots {
        collect_definitions(root, source.as_bytes(), &[], &mut definitions);
    }
    if incomplete && definitions.is_empty() {
        return Err(UnsafeTreeRecovery::new("C#", "no unaffected definitions remain").into());
    }
    Ok(CsharpAnalysis {
        definitions,
        parse_error_lines: recovery.error_lines,
    })
}

pub fn diagnostics_from_analysis(
    analysis: &CsharpAnalysis,
    path: &Path,
) -> Vec<AnalysisDiagnostic> {
    analysis
        .parse_error_lines
        .iter()
        .map(|line| AnalysisDiagnostic {
            code: "csharp.parse_recovery".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: path.into(),
            line: u32::try_from(*line).ok(),
            detail: Some("tree-sitter discarded an invalid C# syntax unit".into()),
        })
        .collect()
}

pub(crate) fn source_stem(path: &Path) -> String {
    path.with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn is_generated_source(path: &Path, source: &str) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".generated") || name.ends_with(".g"))
        || source.lines().take(20).any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("<auto-generated") || line.contains("generated by")
        })
}

pub fn observations_from_analysis(
    repository: &str,
    assembly: &str,
    analysis: &CsharpAnalysis,
    source: &str,
    path: &Path,
) -> Vec<Observation> {
    let module_id = format!(
        "repo://{repository}/csharp/{assembly}/{}",
        source_stem(path)
    );
    let source_id = format!("repo://{repository}/csharp-source/{}", path.display());
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
    let mut observations = vec![Observation::structural(
        source_id,
        StructuralRelation::Defines,
        module_id.clone(),
        path.display().to_string(),
    )];
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
            definition.evidence(path),
        ));
    }
    if is_generated_source(path, source) {
        for observation in &mut observations {
            observation.provenance = Provenance::Generated;
        }
    }
    observations
}

pub fn entities_from_analysis(
    repository: &str,
    assembly: &str,
    analysis: &CsharpAnalysis,
    path: &Path,
) -> Vec<EntityFact> {
    let module_id = format!(
        "repo://{repository}/csharp/{assembly}/{}",
        source_stem(path)
    );
    let source_id = format!("repo://{repository}/csharp-source/{}", path.display());
    [source_id, module_id.clone()]
        .into_iter()
        .map(|id| EntityFact::new(id, EntityKind::Namespace, None).unwrap())
        .chain(analysis.definitions.iter().map(|definition| {
            EntityFact::new(
                format!("{module_id}/{}", definition.qualified_name),
                match definition.kind {
                    DefinitionKind::Namespace | DefinitionKind::Type => EntityKind::Namespace,
                    DefinitionKind::Callable => EntityKind::Callable,
                },
                None,
            )
            .unwrap()
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        CallableClauseRole, Confidence, DependencyRelation, EvidenceContext, PatternConstruct,
        SemanticRelation, SourcePosition, SourceRange,
    };

    fn range(start: (u32, u32), end: (u32, u32)) -> SourceRange {
        SourceRange {
            start: SourcePosition {
                line: start.0,
                character: start.1,
            },
            end: SourcePosition {
                line: end.0,
                character: end.1,
            },
        }
    }

    #[test]
    fn indexes_file_scoped_namespaces_and_same_type_calls() {
        let source = r#"
namespace Game.Core;

public sealed class Runner
{
    public void Run() { Helper(); var worker = new Worker(); }
    private void Helper() {}
}

public sealed class Worker
{
    public Worker() {}
}
"#;
        let analysis = analyze(source).unwrap();
        let mut observations = observations_from_analysis(
            "example",
            "Example.App",
            &analysis,
            source,
            Path::new("src/Runner.cs"),
        );
        observations.extend(crate::resolve_repository_calls(
            "example",
            &[],
            &[crate::CsharpSource {
                path: Path::new("src/Runner.cs"),
                assembly: "Example.App",
                analysis: &analysis,
            }],
        ));

        assert!(analysis.parse_error_lines.is_empty());
        assert!(
            observations.iter().any(|observation| {
                observation
                    .from
                    .as_str()
                    .ends_with("/Game/Core/Runner/Run()")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/Game/Core/Runner/Helper()")
                    && observation.relation
                        == SemanticRelation::Dependency(DependencyRelation::Calls)
                    && observation.confidence == Confidence::Exact
            }),
            "{observations:#?}"
        );
        assert!(
            observations.iter().any(|observation| {
                observation
                    .from
                    .as_str()
                    .ends_with("/Game/Core/Runner/Run()")
                    && observation
                        .to
                        .as_str()
                        .ends_with("/Game/Core/Worker/Worker()")
            }),
            "{observations:#?}"
        );
    }

    #[test]
    fn attaches_switch_contexts_with_exact_utf16_ranges() {
        let source = r#"class Demo {
    int Run(int value) {
        var result = Pick() switch
        {
            0 when Guard() => Zero(),
            _ => Outer(Transform(value) switch
            {
                1 when InnerGuard() => Inner(),
                Demo.Pattern() => Other()
            })
        };
        switch (Select())
        {
            case 1 when Check():
                One();
                break;
            case _:
                _ = "😀"; Discard();
                break;
            default:
                Default();
                break;
        }
        return result;
    }
    int Pick() => 0; bool Guard() => true; int Zero() => 0;
    int Outer(int value) => value; int Transform(int value) => value;
    bool InnerGuard() => true; int Inner() => 1; int Other() => 2;
    int Select() => 0; bool Check() => true; void One() {}
    static int Pattern() => 1; void Discard() {} void Default() {}
}"#;
        let analysis = analyze(source).unwrap();
        let run = analysis
            .definitions
            .iter()
            .find(|definition| definition.qualified_name == "Demo/Run(int)")
            .unwrap();
        let call = |name: &str| run.calls.iter().find(|call| call.name == name).unwrap();

        for excluded in ["Pick", "Guard", "Select", "Check"] {
            assert!(matches!(
                call(excluded).contexts.as_slice(),
                [EvidenceContext::CallableClause {
                    role: CallableClauseRole::Enclosing,
                    ..
                }]
            ));
        }
        for outer_only in ["Transform", "InnerGuard", "Pattern"] {
            assert!(matches!(
                call(outer_only).contexts.as_slice(),
                [
                    EvidenceContext::CallableClause {
                        role: CallableClauseRole::Enclosing,
                        ..
                    },
                    EvidenceContext::PatternArm {
                        construct: PatternConstruct::SwitchExpression,
                        pattern: Some(pattern),
                        ..
                    }
                ] if pattern.text == "_"
            ));
        }

        let inner = call("Inner");
        assert_eq!(inner.range, Some(range((7, 39), (7, 46))));
        assert!(
            matches!(
                inner.contexts.as_slice(),
                [
                    EvidenceContext::CallableClause {
                        role: CallableClauseRole::Enclosing,
                        signature,
                        ..
                    },
                    EvidenceContext::PatternArm {
                        construct: PatternConstruct::SwitchExpression,
                        selector: Some(selector),
                        pattern: Some(pattern),
                        is_default: false,
                        ..
                    },
                    EvidenceContext::PatternArm {
                        construct: PatternConstruct::SwitchExpression,
                        selector: Some(inner_selector),
                        pattern: Some(inner_pattern),
                        is_default: false,
                        ..
                    }
                ] if signature.text == "int Run(int value)"
                    && signature.range == range((1, 4), (1, 22))
                    && selector.text == "Pick()"
                    && pattern.text == "_"
                && inner_selector.text == "Transform(value)"
                    && inner_pattern.text == "1"
            ),
            "{:#?}",
            inner.contexts
        );

        let discard = call("Discard");
        assert_eq!(discard.range, Some(range((17, 26), (17, 35))));
        assert!(matches!(
            &discard.contexts[1],
            EvidenceContext::PatternArm {
                construct: PatternConstruct::SwitchStatement,
                selector: Some(selector),
                pattern: Some(pattern),
                guard: None,
                is_default: false,
                arm_range,
            } if selector.text == "Select()"
                && pattern.text == "_"
                && *arm_range == range((16, 12), (18, 22))
        ));
        assert!(matches!(
            &call("Default").contexts[1],
            EvidenceContext::PatternArm {
                construct: PatternConstruct::SwitchStatement,
                pattern: None,
                is_default: true,
                ..
            }
        ));
        assert!(matches!(
            &call("Zero").contexts[1],
            EvidenceContext::PatternArm {
                guard: Some(guard),
                arm_range,
                ..
            } if guard.text == "Guard()" && *arm_range == range((4, 12), (4, 36))
        ));
    }

    #[test]
    fn preserves_stacked_switch_labels_as_alternative_evidence() {
        let source = r#"class Demo {
    void Run(int value) {
        switch (value) {
            case /* before pattern */ 1:
            case /* before pattern */ 2 when /* inside guard */ Second():
                Hit();
                break;
        }
    }
    bool Second() => true;
    void Hit() {}
}"#;
        let analysis = analyze(source).unwrap();
        let run = analysis
            .definitions
            .iter()
            .find(|definition| definition.qualified_name == "Demo/Run(int)")
            .unwrap();

        let hits = run
            .calls
            .iter()
            .filter(|call| call.name == "Hit")
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 2);
        let labels = hits
            .iter()
            .map(|call| match &call.contexts[1] {
                EvidenceContext::PatternArm {
                    pattern: Some(pattern),
                    guard,
                    arm_range,
                    ..
                } => (
                    pattern.text.as_str(),
                    guard.as_ref().map(|guard| guard.text.as_str()),
                    arm_range,
                ),
                context => panic!("expected switch label context, got {context:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(labels[0].0, "1");
        assert_eq!(labels[0].1, None);
        assert_eq!(labels[1].0, "2");
        assert_eq!(labels[1].1, Some("Second()"));
        assert_eq!(labels[0].2, labels[1].2);
        assert_eq!(*labels[0].2, range((3, 12), (6, 22)));
        assert!(matches!(
            run.calls
                .iter()
                .find(|call| call.name == "Second")
                .unwrap()
                .contexts
                .as_slice(),
            [EvidenceContext::CallableClause { .. }]
        ));
    }

    #[test]
    fn reports_parse_recovery() {
        let analysis =
            analyze("class Broken { void Bad() { @ } } class Safe { void Run() {} }").unwrap();
        assert!(!diagnostics_from_analysis(&analysis, Path::new("Broken.cs")).is_empty());
        assert!(
            analysis
                .definitions
                .iter()
                .any(|definition| definition.qualified_name == "Safe")
        );
        assert!(
            analysis
                .definitions
                .iter()
                .all(|definition| !definition.qualified_name.starts_with("Broken"))
        );
    }

    #[test]
    fn rejects_missing_delimiters_that_can_change_nesting() {
        let error = analyze("class Broken { void Run() { }").unwrap_err();
        assert!(error.downcast_ref::<UnsafeTreeRecovery>().is_some());
    }

    #[test]
    fn does_not_publish_unresolved_calls_as_exact_edges() {
        let source = "class Runner { void Run() { External.Call(); } }";
        let analysis = analyze(source).unwrap();
        let observations = observations_from_analysis(
            "example",
            "Example.App",
            &analysis,
            source,
            Path::new("src/Runner.cs"),
        );

        assert!(!observations.iter().any(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
        }));
    }
}
