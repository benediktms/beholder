use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, EntityId, Observation, Provenance, SemanticRelation, StructuralRelation,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fs, path::Path};
use tree_sitter::{Node, Parser};

pub const FRONTEND_VERSION: &str = "3";
pub const RESOLVER_VERSION: &str = "5";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAnalysis {
    functions: Vec<RustFunction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RustFunction {
    name: String,
    qualified_name: String,
    line: usize,
    calls: Vec<RustCall>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RustCall {
    name: String,
    line: usize,
    receiver_method: bool,
}

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

pub fn analyze(source: &str) -> Result<RustAnalysis, Box<dyn Error>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Rust parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err("failed to parse Rust source".into());
    }

    let source_bytes = source.as_bytes();
    let mut functions = Vec::new();
    collect_functions(
        tree.root_node(),
        source_bytes,
        &mut Vec::new(),
        &mut functions,
    );
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

pub fn diagnostics_from_analysis(analysis: &RustAnalysis, path: &Path) -> Vec<AnalysisDiagnostic> {
    let mut calls = analysis
        .functions
        .iter()
        .flat_map(|function| &function.calls)
        .filter(|call| call.receiver_method);
    let Some(first) = calls.next() else {
        return Vec::new();
    };
    vec![AnalysisDiagnostic {
        code: "rust.receiver_method_resolution_unavailable".into(),
        severity: AnalysisDiagnosticSeverity::KnownLimitation,
        path: path.into(),
        line: u32::try_from(first.line).ok(),
        detail: Some(format!(
            "{} receiver method calls are indexed without type resolution",
            1 + calls.count()
        )),
    }]
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

pub fn resolve_repository_calls(observations: &mut [Observation]) -> Vec<DependencyOverride> {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
            && observation.to.as_str().contains("/rust/")
    }) {
        let Some(name) = observation.to.as_str().rsplit('/').next() else {
            continue;
        };
        definitions
            .entry(name.to_owned())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(observation.to.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.to.as_str().to_owned()));
    }
    let mut overrides = Vec::new();
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        if let Some(name) = observation.to.as_str().strip_prefix("rust-call://")
            && let Some(Some(target)) = definitions.get(name)
        {
            overrides.push(DependencyOverride {
                from: observation.from.clone(),
                relation: DependencyRelation::Calls,
                unresolved_to: observation.to.clone(),
                resolved_to: EntityId::from(target.clone()),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Inferred,
                provenance: Provenance::UniqueNameHeuristic,
            });
            observation.to = EntityId::from(target.clone());
            observation.confidence = Confidence::Inferred;
            observation.provenance = Provenance::UniqueNameHeuristic;
        }
    }
    overrides
}

pub fn source_files(
    directory: &Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if !matches!(entry.file_name().to_str(), Some(".git" | "target")) {
                source_files(&path, files)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_smoke() {
        let observations = observations(
            "beholder",
            "fn first() { second(); } fn second() {}",
            Path::new("src/lib.rs"),
        )
        .unwrap();
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://beholder/rust/lib/first"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://beholder/rust/lib/second"
                && observation.confidence == Confidence::Exact
                && observation.provenance == Provenance::Ast
        }));

        let mut ambiguous = vec![
            Observation::dependency(
                "repo://beholder/rust/caller",
                DependencyRelation::Calls,
                "rust-call://helper",
                "src/lib.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/one",
                StructuralRelation::Defines,
                "repo://beholder/rust/one/helper",
                "src/one.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/two",
                StructuralRelation::Defines,
                "repo://beholder/rust/two/helper",
                "src/two.rs:1",
            ),
        ];
        resolve_repository_calls(&mut ambiguous);
        assert_eq!(ambiguous[0].to.as_str(), "rust-call://helper");
        assert_eq!(ambiguous[0].confidence, Confidence::Exact);
        assert_eq!(ambiguous[0].provenance, Provenance::Ast);
    }

    #[test]
    fn marks_unique_name_resolution_as_inferred() {
        let mut observations = vec![
            Observation::dependency(
                "repo://beholder/rust/caller",
                DependencyRelation::Calls,
                "rust-call://helper",
                "src/caller.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/helper",
                StructuralRelation::Defines,
                "repo://beholder/rust/helper/helper",
                "src/helper.rs:1",
            ),
        ];

        let overrides = resolve_repository_calls(&mut observations);

        assert_eq!(
            observations[0].to.as_str(),
            "repo://beholder/rust/helper/helper"
        );
        assert_eq!(observations[0].confidence, Confidence::Inferred);
        assert_eq!(observations[0].provenance, Provenance::UniqueNameHeuristic);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].confidence, Confidence::Inferred);
        assert_eq!(overrides[0].provenance, Provenance::UniqueNameHeuristic);
    }

    #[test]
    fn does_not_treat_sibling_module_names_as_exact() {
        let mut observations = observations(
            "beholder",
            "mod one { fn caller() { helper(); } } mod two { fn helper() {} }",
            Path::new("src/lib.rs"),
        )
        .unwrap();
        let call = observations
            .iter()
            .find(|observation| observation.from.as_str().ends_with("/one/caller"))
            .unwrap();
        assert_eq!(call.to.as_str(), "rust-call://helper");
        assert_eq!(call.confidence, Confidence::Exact);

        resolve_repository_calls(&mut observations);
        let call = observations
            .iter()
            .find(|observation| observation.from.as_str().ends_with("/one/caller"))
            .unwrap();
        assert!(call.to.as_str().ends_with("/two/helper"));
        assert_eq!(call.confidence, Confidence::Inferred);
        assert_eq!(call.provenance, Provenance::UniqueNameHeuristic);
    }

    #[test]
    fn qualifies_scoped_function_ids() {
        let observations = observations(
            "beholder",
            "mod nested { fn run() {} } struct One; struct Two; \
             impl One { fn run() {} } impl Two { fn run() {} }",
            Path::new("crates/example/src/lib.rs"),
        )
        .unwrap();
        let definitions = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();

        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/nested/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/One/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/Two/run"));
    }

    #[test]
    fn leaves_receiver_methods_unresolved() {
        let source = "fn is_valid_hash(s: &str) -> bool { \
                 s.chars().all(|c| c.is_ascii()) \
             } \
             struct InMemoryOutboxRepository; \
             impl InMemoryOutboxRepository { fn all(&self) {} }";
        let mut observations = observations(
            "repo-link",
            source,
            Path::new("crates/domain-task/src/hash.rs"),
        )
        .unwrap();

        resolve_repository_calls(&mut observations);
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                    && observation.from.as_str().ends_with("/is_valid_hash")
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();

        assert!(calls.contains(&"rust-method://all"));
        assert!(calls.contains(&"rust-method://chars"));
        assert!(calls.contains(&"rust-method://is_ascii"));
        assert!(
            observations
                .iter()
                .filter(|observation| { observation.to.as_str().starts_with("rust-method://") })
                .all(|observation| {
                    observation.confidence == Confidence::Exact
                        && observation.provenance == Provenance::Ast
                })
        );
        assert!(
            !calls
                .iter()
                .any(|target| { target.ends_with("/impl/InMemoryOutboxRepository/all") })
        );
        let diagnostics = diagnostics_from_analysis(
            &analyze(source).unwrap(),
            Path::new("crates/domain-task/src/hash.rs"),
        );
        assert_eq!(
            diagnostics,
            vec![AnalysisDiagnostic {
                code: "rust.receiver_method_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: Path::new("crates/domain-task/src/hash.rs").into(),
                line: Some(1),
                detail: Some("3 receiver method calls are indexed without type resolution".into(),),
            }]
        );
    }
}
