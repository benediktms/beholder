use super::{
    model::*,
    plugin::{RustLanguage, built_in_plugins},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, StructuralRelation, UnsafeTreeRecovery,
};
use beholder_indexing::{ActivePlugins, LanguageAnalyzer, SourceRecognitionInput};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile,
    ast::{self, HasName},
};
use std::{collections::BTreeMap, error::Error, path::Path};
use tree_sitter::{Node, Parser};

pub(super) fn collect_tree_sitter_functions<'tree>(
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
        collect_tree_sitter_functions(child, source, scope, functions);
    }
    if pushed_scope {
        scope.pop();
    }
}

fn collect_tree_sitter_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<RustCall>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
    {
        let callee = if function.kind() == "generic_function" {
            function.child_by_field_name("function").unwrap_or(function)
        } else {
            function
        };
        let receiver_method = callee.kind() == "field_expression";
        let name = if receiver_method {
            callee.child_by_field_name("field")
        } else {
            Some(callee)
        };
        if let Some(name) = name.and_then(|name| name.utf8_text(source).ok()) {
            calls.push(RustCall {
                name: name.to_owned(),
                line: node.start_position().row + 1,
                receiver_method,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tree_sitter_calls(child, source, calls);
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

fn analyze_tree_sitter(
    source: &str,
    tree: &tree_sitter::Tree,
) -> Result<RustAnalysis, Box<dyn Error + Send + Sync>> {
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
        collect_tree_sitter_functions(root, source_bytes, &mut Vec::new(), &mut functions);
    } else {
        let mut cursor = root.walk();
        for child in root
            .named_children(&mut cursor)
            .filter(|child| !child.has_error())
        {
            collect_tree_sitter_functions(child, source_bytes, &mut Vec::new(), &mut functions);
        }
        if functions.is_empty() {
            return Err(UnsafeTreeRecovery::new("Rust", "no unaffected definitions remain").into());
        }
    }
    Ok(RustAnalysis {
        functions: functions
            .into_iter()
            .map(|(name, qualified_name, function)| {
                let mut calls = Vec::new();
                collect_tree_sitter_calls(function, source_bytes, &mut calls);
                RustFunction {
                    name,
                    qualified_name,
                    line: function.start_position().row + 1,
                    calls,
                }
            })
            .collect(),
        tonic: Default::default(),
        parse_error_lines,
    })
}

fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

fn line_at(lines: &[usize], offset: ra_ap_syntax::TextSize) -> usize {
    lines.partition_point(|start| *start <= usize::from(offset))
}

fn rust_analyzer_functions(source: &str, file: &SourceFile) -> Vec<RustFunction> {
    let lines = line_starts(source);
    file.syntax()
        .descendants()
        .filter_map(ast::Fn::cast)
        .filter(|function| {
            function
                .syntax()
                .ancestors()
                .skip(1)
                .all(|ancestor| ast::Fn::cast(ancestor).is_none())
        })
        .filter_map(|function| {
            let name = function.name()?.text().to_string();
            let mut scope = function
                .syntax()
                .ancestors()
                .skip(1)
                .filter_map(|ancestor| {
                    if let Some(module) = ast::Module::cast(ancestor.clone()) {
                        return module.name().map(|name| name.text().to_string());
                    }
                    ast::Impl::cast(ancestor).and_then(|impl_| {
                        let target = impl_.self_ty()?.syntax().text().to_string();
                        let owner = impl_.trait_().map_or_else(
                            || target.clone(),
                            |trait_| format!("{}-for-{target}", trait_.syntax().text()),
                        );
                        Some(format!("impl/{owner}"))
                    })
                })
                .collect::<Vec<_>>();
            scope.reverse();
            let qualified_name = scope
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(name.as_str()))
                .collect::<Vec<_>>()
                .join("/");
            let calls = function
                .syntax()
                .descendants()
                .filter(|node| {
                    node.ancestors()
                        .find_map(ast::Fn::cast)
                        .is_some_and(|owner| owner.syntax() == function.syntax())
                })
                .filter_map(|node| {
                    if let Some(call) = ast::CallExpr::cast(node.clone()) {
                        let callee = call.expr()?;
                        return Some(RustCall {
                            name: callee.syntax().text().to_string(),
                            line: line_at(&lines, node.text_range().start()),
                            receiver_method: false,
                        });
                    }
                    ast::MethodCallExpr::cast(node).and_then(|call| {
                        Some(RustCall {
                            name: call.name_ref()?.text().to_string(),
                            line: line_at(&lines, call.syntax().text_range().start()),
                            receiver_method: true,
                        })
                    })
                })
                .collect();
            Some(RustFunction {
                name,
                qualified_name,
                line: line_at(&lines, function.syntax().text_range().start()),
                calls,
            })
        })
        .collect()
}

pub fn analyze(source: &str) -> Result<RustAnalysis, Box<dyn Error + Send + Sync>> {
    let plugins = built_in_plugins()?;
    let active = plugins.activate_direct(Path::new("input.rs"));
    analyze_with_plugins(source, Path::new("input.rs"), &plugins, &active)
}

pub(super) fn analyze_with_plugins(
    source: &str,
    path: &Path,
    plugins: &LanguageAnalyzer<RustLanguage>,
    active_plugins: &ActivePlugins,
) -> Result<RustAnalysis, Box<dyn Error + Send + Sync>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Rust parser returned no tree")?;
    let mut analysis = if !tree.root_node().has_error() {
        analyze_tree_sitter(source, &tree)?
    } else if source.len() > 200_000 {
        // ponytail: rust-analyzer recursively drops its syntax tree; keep the recovery path bounded
        // until its green-tree drop is iterative.
        analyze_tree_sitter(source, &tree)?
    } else {
        let parsed = SourceFile::parse(source, Edition::CURRENT);
        if parsed.errors().is_empty() {
            RustAnalysis {
                functions: rust_analyzer_functions(source, &parsed.tree()),
                tonic: Default::default(),
                parse_error_lines: Vec::new(),
            }
        } else {
            analyze_tree_sitter(source, &tree)?
        }
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
    let analysis = analyze(source).map_err(|error| -> Box<dyn Error> { error })?;
    Ok(observations_from_analysis(repository, &analysis, path))
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn recovers_only_unaffected_top_level_functions() {
        let analysis =
            analyze("tonic::include_proto!(\"example.v1\");\nfn broken() { @ }\nfn safe() {}")
                .unwrap();
        assert_eq!(analysis.functions.len(), 1);
        assert_eq!(analysis.functions[0].name, "safe");
        assert!(!analysis.parse_error_lines.is_empty());
        assert!(analysis.tonic.packages.is_empty());
    }

    #[test]
    fn rejects_missing_delimiters_that_can_change_nesting() {
        let error = analyze("fn broken() {\nfn nested() {}\n").unwrap_err();
        assert!(error.downcast_ref::<UnsafeTreeRecovery>().is_some());
    }

    #[test]
    fn parses_current_rust_default_field_values() {
        let analysis =
            analyze("struct Options { retries: usize = 3 } fn after_default() { run(); }").unwrap();
        assert_eq!(analysis.functions[0].name, "after_default");
        assert_eq!(analysis.functions[0].calls[0].name, "run");
    }

    #[test]
    fn preserves_qualified_call_paths() {
        let analysis =
            analyze("fn analyze() { super::tonic::analyze(); Default::default(); value.run(); }")
                .unwrap();
        let calls = &analysis.functions[0].calls;
        assert_eq!(calls[0].name, "super::tonic::analyze");
        assert_eq!(calls[1].name, "Default::default");
        assert_eq!(calls[2].name, "run");
        assert!(calls[2].receiver_method);
    }
}
