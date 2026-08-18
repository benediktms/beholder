use super::model::*;
use beholder_domain::{
    DependencyRelation, EntityFact, EntityKind, Observation, Provenance, StructuralRelation,
};
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

fn call_target(node: Node<'_>, target: Node<'_>, source: &[u8], kind: CallKind) -> Option<Call> {
    let (receiver, name, kind) = match target.kind() {
        "identifier" => (None, text(target, source)?.into(), kind),
        "member_expression" => (
            target
                .child_by_field_name("object")
                .and_then(|node| text(node, source))
                .map(str::to_owned),
            target
                .child_by_field_name("property")
                .and_then(|node| text(node, source))?
                .into(),
            if kind == CallKind::Constructor {
                CallKind::Constructor
            } else {
                CallKind::Member
            },
        ),
        _ => return None,
    };
    Some(Call {
        kind,
        receiver,
        name,
        line: node.start_position().row + 1,
    })
}

fn call(node: Node<'_>, source: &[u8], kind: CallKind) -> Option<Call> {
    let target = node.child_by_field_name(match kind {
        CallKind::Constructor => "constructor",
        CallKind::Direct | CallKind::Member => "function",
    })?;
    call_target(node, target, source, kind)
}

fn collect_calls(node: Node<'_>, source: &[u8], root: Node<'_>, calls: &mut Vec<Call>) {
    if node != root
        && (matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "generator_function_declaration"
                | "method_definition"
        ) || (node.kind() == "arrow_function"
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "arguments")))
    {
        return;
    }
    match node.kind() {
        "call_expression" => {
            if let Some(call) = call(node, source, CallKind::Direct) {
                calls.push(call);
            }
        }
        "new_expression" => {
            if let Some(call) = call(node, source, CallKind::Constructor) {
                calls.push(call);
            }
        }
        "jsx_opening_element" | "jsx_self_closing_element" => {
            if let Some(call) = node
                .child_by_field_name("name")
                .filter(|target| {
                    target.kind() == "member_expression"
                        || text(*target, source)
                            .and_then(|name| name.chars().next())
                            .is_some_and(char::is_uppercase)
                })
                .and_then(|target| call_target(node, target, source, CallKind::Direct))
            {
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

fn type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "type_identifier" {
        return text(node, source).map(str::to_owned);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| type_name(child, source))
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
    })
}

fn collect_bindings(node: Node<'_>, source: &[u8], root: Node<'_>, bindings: &mut Vec<Binding>) {
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
        return;
    }
    if matches!(
        node.kind(),
        "required_parameter" | "optional_parameter" | "variable_declarator"
    ) && let Some(binding) = binding(node, source)
    {
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
        return;
    }
    if node.kind() == "variable_declarator"
        && let Some(receiver) = node
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| text(name, source))
        && let Some(factory) = node
            .child_by_field_name("value")
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
        && let Some(value) = node.named_child(0)
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
    let mut factory_bindings = Vec::new();
    if let Some(body) = body {
        collect_calls(body, source, body, &mut calls);
    }
    collect_bindings(node, source, node, &mut bindings);
    collect_factory_bindings(node, source, node, &mut factory_bindings);
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|annotation| type_name(annotation, source))
        .or_else(|| returned_constructor(node, source, node));
    Definition {
        qualified_name: qualified(scope, name),
        kind,
        line: node.start_position().row + 1,
        calls,
        bindings,
        factory_bindings,
        factory: None,
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
            }
            definitions.push(class);
            scope.push(name.into());
            if let Some(body) = node.child_by_field_name("body") {
                let bindings = class_bindings(body, source);
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
        "function_declaration"
        | "generator_function_declaration"
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
                definitions.push(factory_definition);
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

pub fn analyze(
    source: &str,
    language: SourceLanguage,
) -> Result<TypescriptAnalysis, Box<dyn Error>> {
    let mut parser = Parser::new();
    let grammar = match language {
        SourceLanguage::JavaScript | SourceLanguage::Jsx => tree_sitter_javascript::LANGUAGE,
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        SourceLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
    };
    parser.set_language(&grammar.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("JavaScript/TypeScript parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err("failed to parse JavaScript/TypeScript source".into());
    }
    let mut definitions = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    collect_definitions(
        tree.root_node(),
        source.as_bytes(),
        &mut Vec::new(),
        &mut definitions,
    );
    collect_imports(tree.root_node(), source.as_bytes(), &mut imports);
    collect_exports(tree.root_node(), source.as_bytes(), &mut exports);
    Ok(TypescriptAnalysis {
        language,
        definitions,
        imports,
        exports,
    })
}

fn source_stem(path: &Path) -> String {
    path.with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn is_generated_source(path: &Path, source: &str) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".generated") || name.ends_with(".gen"))
        || source.lines().take(20).any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("@generated")
                || line.contains("generated by")
                || line.contains("do not edit")
        })
}

pub fn observations_from_analysis(
    repository: &str,
    analysis: &TypescriptAnalysis,
    source: &str,
    path: &Path,
) -> Vec<Observation> {
    let language = analysis.language.id_segment();
    let module_id = format!("repo://{repository}/{language}/{}", source_stem(path));
    let source_id = format!("repo://{repository}/{language}-source/{}", path.display());
    let mut observations = vec![Observation::structural(
        source_id,
        StructuralRelation::Defines,
        module_id.clone(),
        path.display().to_string(),
    )];
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
        if definition.kind != DefinitionKind::Callable {
            continue;
        }
        let scope = parent_name.unwrap_or_default();
        for call in &definition.calls {
            let target = match call.kind {
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
            };
            observations.push(Observation::dependency(
                id.clone(),
                DependencyRelation::Calls,
                target,
                format!("{}:{}", path.display(), call.line),
            ));
        }
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
    analysis: &TypescriptAnalysis,
    path: &Path,
) -> Vec<EntityFact> {
    let module_id = format!(
        "repo://{}/{}/{}",
        repository,
        analysis.language.id_segment(),
        source_stem(path)
    );
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{Confidence, SemanticRelation};

    fn observations(source: &str, path: &str) -> Vec<Observation> {
        let path = Path::new(path);
        let language = SourceLanguage::from_path(path).unwrap();
        let analysis = analyze(source, language).unwrap();
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
}
