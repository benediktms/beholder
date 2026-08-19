use super::model::*;
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, StructuralRelation, UnsafeTreeRecovery,
};
use std::{collections::BTreeMap, error::Error, path::Path};
use tree_sitter::{Node, Parser};

fn collect_functions<'tree>(
    node: Node<'tree>,
    source: &[u8],
    scope: &mut Vec<String>,
    functions: &mut Vec<(String, String, Node<'tree>)>,
) {
    if node.kind() == "function_item"
        && let Some(name) = node.child_by_field_name("name")
        && let Ok(name) = name.utf8_text(source)
    {
        let qualified_name = scope
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(name))
            .collect::<Vec<_>>()
            .join("/");
        functions.push((name.to_owned(), qualified_name, node));
        return;
    }

    let nested_scope = match node.kind() {
        "mod_item" => node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
            .map(str::to_owned),
        "impl_item" => node
            .child_by_field_name("type")
            .and_then(|target| target.utf8_text(source).ok())
            .map(|target| {
                let owner = node
                    .child_by_field_name("trait")
                    .and_then(|trait_| trait_.utf8_text(source).ok())
                    .map_or_else(
                        || target.to_owned(),
                        |trait_| format!("{trait_}-for-{target}"),
                    );
                format!("impl/{owner}")
            }),
        _ => None,
    };
    let pushed_scope = nested_scope.is_some();
    if let Some(nested_scope) = nested_scope {
        scope.push(nested_scope);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, scope, functions);
    }
    if pushed_scope {
        scope.pop();
    }
}

fn collect_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<RustCall>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(text) = function.utf8_text(source)
        && let Some(name) = text.rsplit([':', '.']).find(|part| !part.is_empty())
    {
        calls.push(RustCall {
            name: name.to_owned(),
            line: node.start_position().row + 1,
            receiver_method: function.kind() == "field_expression",
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(child, source, calls);
    }
}

fn collect_parse_errors(node: Node<'_>, lines: &mut Vec<usize>, missing: &mut bool) {
    if node.is_error() || node.is_missing() {
        lines.push(node.start_position().row + 1);
        *missing |= node.is_missing();
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_errors(child, lines, missing);
    }
}

pub fn analyze(source: &str) -> Result<RustAnalysis, Box<dyn Error>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Rust parser returned no tree")?;
    let source_bytes = source.as_bytes();
    let root = tree.root_node();
    let mut parse_error_lines = Vec::new();
    let mut missing = false;
    collect_parse_errors(root, &mut parse_error_lines, &mut missing);
    if missing {
        return Err(UnsafeTreeRecovery::new("Rust", "missing syntax may change nesting").into());
    }
    parse_error_lines.sort_unstable();
    parse_error_lines.dedup();
    let mut functions = Vec::new();
    if parse_error_lines.is_empty() {
        collect_functions(root, source_bytes, &mut Vec::new(), &mut functions);
    } else {
        let mut cursor = root.walk();
        for child in root
            .named_children(&mut cursor)
            .filter(|child| !child.has_error())
        {
            collect_functions(child, source_bytes, &mut Vec::new(), &mut functions);
        }
        if functions.is_empty() {
            return Err(UnsafeTreeRecovery::new("Rust", "no unaffected definitions remain").into());
        }
    }
    let tonic = if parse_error_lines.is_empty() {
        super::tonic::analyze(root, source_bytes, &functions)
    } else {
        Default::default()
    };
    Ok(RustAnalysis {
        functions: functions
            .into_iter()
            .map(|(name, qualified_name, function)| {
                let mut calls = Vec::new();
                collect_calls(function, source_bytes, &mut calls);
                RustFunction {
                    name,
                    qualified_name,
                    line: function.start_position().row + 1,
                    calls,
                }
            })
            .collect(),
        tonic,
        parse_error_lines,
    })
}

pub fn observations_from_analysis(
    repository: &str,
    analysis: &RustAnalysis,
    path: &Path,
) -> Vec<Observation> {
    let module = path
        .strip_prefix("src")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source_id = format!("repo://{repository}/rust/{module}");
    let mut definitions = BTreeMap::<(String, String), Option<String>>::new();
    for function in &analysis.functions {
        let id = format!("{source_id}/{}", function.qualified_name);
        let scope = function
            .qualified_name
            .rsplit_once('/')
            .map_or("", |(scope, _)| scope);
        if !scope.starts_with("impl/") {
            definitions
                .entry((scope.into(), function.name.clone()))
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(id));
        }
    }
    let mut observations = Vec::new();

    for function in &analysis.functions {
        let function_id = format!("{source_id}/{}", function.qualified_name);
        let scope = function
            .qualified_name
            .rsplit_once('/')
            .map_or("", |(scope, _)| scope);
        observations.push(Observation::structural(
            source_id.clone(),
            StructuralRelation::Defines,
            function_id.clone(),
            format!("{}:{}", path.display(), function.line),
        ));
        for call in &function.calls {
            observations.push(Observation::dependency(
                function_id.clone(),
                DependencyRelation::Calls,
                if call.receiver_method {
                    format!("rust-method://{}", call.name)
                } else {
                    definitions
                        .get(&(scope.into(), call.name.clone()))
                        .and_then(Clone::clone)
                        .unwrap_or_else(|| format!("rust-call://{}", call.name))
                },
                format!("{}:{}", path.display(), call.line),
            ));
        }
    }
    observations
}

pub fn entities_from_analysis(
    repository: &str,
    analysis: &RustAnalysis,
    path: &Path,
) -> Vec<EntityFact> {
    let module = path
        .strip_prefix("src")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source_id = format!("repo://{repository}/rust/{module}");
    std::iter::once(EntityFact::new(source_id.clone(), EntityKind::Namespace, None).unwrap())
        .chain(analysis.functions.iter().map(|function| {
            EntityFact::new(
                format!("{source_id}/{}", function.qualified_name),
                EntityKind::Callable,
                None,
            )
            .unwrap()
        }))
        .collect()
}

pub fn diagnostics_from_analysis(analysis: &RustAnalysis, path: &Path) -> Vec<AnalysisDiagnostic> {
    let mut diagnostics = analysis
        .parse_error_lines
        .iter()
        .map(|line| AnalysisDiagnostic {
            code: "rust.parse_recovery".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: path.into(),
            line: u32::try_from(*line).ok(),
            detail: Some("tree-sitter discarded an invalid Rust syntax unit".into()),
        })
        .collect::<Vec<_>>();
    let mut calls = analysis
        .functions
        .iter()
        .flat_map(|function| &function.calls)
        .filter(|call| {
            call.receiver_method
                && !analysis
                    .tonic
                    .recognized_receiver_calls
                    .iter()
                    .any(|(line, name)| *line == call.line && name == &call.name)
        });
    let Some(first) = calls.next() else {
        return diagnostics;
    };
    diagnostics.push(AnalysisDiagnostic {
        code: "rust.receiver_method_resolution_unavailable".into(),
        severity: AnalysisDiagnosticSeverity::KnownLimitation,
        path: path.into(),
        line: u32::try_from(first.line).ok(),
        detail: Some(format!(
            "{} receiver method calls are indexed without type resolution",
            1 + calls.count()
        )),
    });
    diagnostics
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
mod recovery_tests {
    use super::*;

    #[test]
    fn recovers_only_unaffected_top_level_functions() {
        let analysis = analyze("fn broken() { @ }\nfn safe() {}").unwrap();
        assert_eq!(analysis.functions.len(), 1);
        assert_eq!(analysis.functions[0].name, "safe");
        assert!(!analysis.parse_error_lines.is_empty());
    }

    #[test]
    fn rejects_missing_delimiters_that_can_change_nesting() {
        let error = analyze("fn broken() {\nfn nested() {}\n").unwrap_err();
        assert!(error.downcast_ref::<UnsafeTreeRecovery>().is_some());
    }
}
