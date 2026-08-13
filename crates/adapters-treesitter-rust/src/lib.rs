use beholder_domain::Observation;
use std::{collections::BTreeMap, error::Error, fs, path::Path};
use tree_sitter::{Node, Parser};

fn collect_functions<'tree>(
    node: Node<'tree>,
    source: &[u8],
    functions: &mut Vec<(String, Node<'tree>)>,
) {
    if node.kind() == "function_item"
        && let Some(name) = node.child_by_field_name("name")
        && let Ok(name) = name.utf8_text(source)
    {
        functions.push((name.to_owned(), node));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, functions);
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

pub fn observations(
    repository: &str,
    source: &str,
    path: &Path,
) -> Result<Vec<Observation>, Box<dyn Error>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Rust parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err(format!("failed to parse Rust source: {}", path.display()).into());
    }

    let source_bytes = source.as_bytes();
    let mut functions = Vec::new();
    collect_functions(tree.root_node(), source_bytes, &mut functions);
    let module = path
        .strip_prefix("src")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source_id = format!("repo://{repository}/rust/{module}");
    // ponytail: simple names cover this fixture; qualify impl methods when Rust support expands.
    let definitions = functions
        .iter()
        .map(|(name, _)| (name.clone(), format!("{source_id}/{name}")))
        .collect::<BTreeMap<_, _>>();
    let mut observations = Vec::new();

    for (name, function) in functions {
        let function_id = definitions[&name].clone();
        observations.push([
            source_id.clone(),
            "defines".into(),
            function_id.clone(),
            format!("{}:{}", path.display(), function.start_position().row + 1),
        ]);
        let mut calls = Vec::new();
        collect_calls(function, source_bytes, &mut calls);
        for (callee, line) in calls {
            observations.push([
                function_id.clone(),
                "calls".into(),
                definitions
                    .get(&callee)
                    .cloned()
                    .unwrap_or_else(|| format!("rust-call://{callee}")),
                format!("{}:{line}", path.display()),
            ]);
        }
    }
    Ok(observations)
}

pub fn resolve_repository_calls(observations: &mut [Observation]) {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for row in observations.iter().filter(|row| row[1] == "defines") {
        let Some(name) = row[2].rsplit('/').next() else {
            continue;
        };
        definitions
            .entry(name.to_owned())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(row[2].as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(row[2].clone()));
    }
    for row in observations.iter_mut().filter(|row| row[1] == "calls") {
        if let Some(name) = row[2].strip_prefix("rust-call://")
            && let Some(Some(target)) = definitions.get(name)
        {
            row[2] = target.clone();
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
        assert!(observations.iter().any(|row| {
            row[0] == "repo://beholder/rust/lib/first"
                && row[1] == "calls"
                && row[2] == "repo://beholder/rust/lib/second"
        }));

        let mut ambiguous = vec![
            [
                "repo://beholder/rust/caller".into(),
                "calls".into(),
                "rust-call://helper".into(),
                "src/lib.rs:1".into(),
            ],
            [
                "repo://beholder/rust/one".into(),
                "defines".into(),
                "repo://beholder/rust/one/helper".into(),
                "src/one.rs:1".into(),
            ],
            [
                "repo://beholder/rust/two".into(),
                "defines".into(),
                "repo://beholder/rust/two/helper".into(),
                "src/two.rs:1".into(),
            ],
        ];
        resolve_repository_calls(&mut ambiguous);
        assert_eq!(ambiguous[0][2], "rust-call://helper");
    }
}
