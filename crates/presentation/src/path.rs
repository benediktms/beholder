use crate::projection::{evidence_label, plural, projected_paths, write_metadata, write_traversal};
use beholder_dto::{TraceResult, WhyResult};
use std::{collections::BTreeSet, fmt::Write};

pub(super) fn trace_human(
    result: &TraceResult,
    include_tests: bool,
    include_diagnostics: bool,
) -> String {
    let projected = projected_paths(&result.nodes, &result.edges, &result.paths, include_tests);
    if projected.is_empty() {
        let mut output = format!("No path from {} to {}", result.query.from, result.query.to);
        write_traversal(&mut output, &result.traversal);
        write_metadata(&mut output, &result.metadata, include_diagnostics);
        return output;
    }
    let mut output = String::new();
    if projected.len() == 1 {
        let path = &projected[0];
        output.push_str(&path.first);
        output.push('\n');
        for step in &path.steps {
            let _ = writeln!(output, "  → {} [{}]", step.to, step.kind);
        }
    } else {
        for (index, path) in projected.iter().enumerate() {
            let _ = write!(output, "[{}] {}", index + 1, path.first);
            for step in &path.steps {
                let _ = write!(output, " >{}> {}", step.kind, step.to);
            }
            output.push('\n');
        }
    }
    let hops = projected
        .iter()
        .map(|path| path.steps.len())
        .min()
        .unwrap_or(0);
    let repositories = result
        .nodes
        .iter()
        .filter_map(|entity| entity.repository.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let confidence = projected
        .iter()
        .flat_map(|path| path.steps.iter().map(|step| step.confidence))
        .fold(1.0_f32, f32::min);
    let _ = write!(
        output,
        "\n{hops} {} · {repositories} repositories · confidence {confidence:.2}",
        plural(hops as u32, "hop", "hops")
    );
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata, include_diagnostics);
    output
}

pub(super) fn why_human(
    result: &WhyResult,
    include_tests: bool,
    include_diagnostics: bool,
) -> String {
    let projected = projected_paths(&result.nodes, &result.edges, &result.paths, include_tests);
    if projected.is_empty() {
        let mut output = format!("No path from {} to {}", result.query.from, result.query.to);
        write_traversal(&mut output, &result.traversal);
        write_metadata(&mut output, &result.metadata, include_diagnostics);
        return output;
    }
    let mut output = String::new();
    for (path_index, path) in projected.iter().enumerate() {
        if projected.len() > 1 {
            let _ = writeln!(output, "[{}]", path_index + 1);
        }
        let _ = writeln!(output, "{}", path.first);
        for step in &path.steps {
            let _ = writeln!(output, "  → {} [{}]", step.to, step.kind);
            for evidence in &step.evidence {
                let _ = writeln!(output, "     {}", evidence_label(evidence));
            }
        }
        if path_index + 1 < projected.len() {
            output.push('\n');
        }
    }
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata, include_diagnostics);
    output
}
