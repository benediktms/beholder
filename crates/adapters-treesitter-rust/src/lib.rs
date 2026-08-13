use beholder_domain::Observation;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fs, path::Path};
use tree_sitter::{Node, Parser};

pub const FRONTEND_VERSION: &str = "1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAnalysis {
    functions: Vec<RustFunction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RustFunction {
    name: String,
    qualified_name: String,
    line: usize,
    calls: Vec<(String, usize)>,
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

fn collect_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<(String, usize)>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(function) = function.utf8_text(source)
        && let Some(name) = function.rsplit([':', '.']).find(|part| !part.is_empty())
    {
        calls.push((name.to_owned(), node.start_position().row + 1));
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
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for function in &analysis.functions {
        let id = format!("{source_id}/{}", function.qualified_name);
        definitions
            .entry(function.name.clone())
            .and_modify(|candidate| *candidate = None)
            .or_insert(Some(id));
    }
    let mut observations = Vec::new();

    for function in &analysis.functions {
        let function_id = format!("{source_id}/{}", function.qualified_name);
        observations.push(Observation {
            from: source_id.clone(),
            relation: "defines".into(),
            to: function_id.clone(),
            evidence: format!("{}:{}", path.display(), function.line),
        });
        for (callee, line) in &function.calls {
            observations.push(Observation {
                from: function_id.clone(),
                relation: "calls".into(),
                to: definitions
                    .get(callee)
                    .and_then(Clone::clone)
                    .unwrap_or_else(|| format!("rust-call://{callee}")),
                evidence: format!("{}:{line}", path.display()),
            });
        }
    }
    observations
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

pub fn resolve_repository_calls(observations: &mut [Observation]) {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for observation in observations
        .iter()
        .filter(|observation| observation.relation == "defines")
    {
        let Some(name) = observation.to.rsplit('/').next() else {
            continue;
        };
        definitions
            .entry(name.to_owned())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(observation.to.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.to.clone()));
    }
    for observation in observations
        .iter_mut()
        .filter(|observation| observation.relation == "calls")
    {
        if let Some(name) = observation.to.strip_prefix("rust-call://")
            && let Some(Some(target)) = definitions.get(name)
        {
            observation.to = target.clone();
        }
    }
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
            observation.from == "repo://beholder/rust/lib/first"
                && observation.relation == "calls"
                && observation.to == "repo://beholder/rust/lib/second"
        }));

        let mut ambiguous = vec![
            Observation {
                from: "repo://beholder/rust/caller".into(),
                relation: "calls".into(),
                to: "rust-call://helper".into(),
                evidence: "src/lib.rs:1".into(),
            },
            Observation {
                from: "repo://beholder/rust/one".into(),
                relation: "defines".into(),
                to: "repo://beholder/rust/one/helper".into(),
                evidence: "src/one.rs:1".into(),
            },
            Observation {
                from: "repo://beholder/rust/two".into(),
                relation: "defines".into(),
                to: "repo://beholder/rust/two/helper".into(),
                evidence: "src/two.rs:1".into(),
            },
        ];
        resolve_repository_calls(&mut ambiguous);
        assert_eq!(ambiguous[0].to, "rust-call://helper");
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
            .filter(|observation| observation.relation == "defines")
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();

        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/nested/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/One/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/Two/run"));
    }
}
