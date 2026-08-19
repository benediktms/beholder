use crate::projection::{
    Visibility, entities, entity_names, is_primary, kind_label, label, name, plural, test_path,
    visibility, write_metadata, write_traversal,
};
use beholder_dto::{DependenciesResult, EntityKind, EntityRef, ImpactResult};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

pub(super) fn dependencies_human(result: &DependenciesResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let names = entity_names(&result.nodes);
    let mut output = format!("{}\n", label(&names, &result.root));
    let dependencies = result
        .dependencies
        .iter()
        .filter(|dependency| {
            (result.root.kind == EntityKind::UnityPrefab
                && dependency.hops == 1
                && entities
                    .get(dependency.entity.as_str())
                    .is_none_or(|entity| include_tests || !entity.test))
                || is_primary(&entities, &dependency.entity, include_tests)
        })
        .collect::<Vec<_>>();
    for dependency in &dependencies {
        let _ = writeln!(
            output,
            "  → {} ({} {})",
            name(&names, &dependency.entity),
            dependency.hops,
            plural(dependency.hops, "hop", "hops")
        );
    }
    let _ = writeln!(output, "\n{} dependencies", dependencies.len());
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

pub(super) fn impact_human(result: &ImpactResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let names = entity_names(&result.nodes);
    let mut groups: BTreeMap<String, Vec<&EntityRef>> = BTreeMap::new();
    let mut tests: BTreeMap<String, Vec<&EntityRef>> = BTreeMap::new();
    let mut hidden_tests = 0;
    for affected in &result.affected {
        let entity = entities.get(affected.entity.as_str());
        if entity.is_some_and(|entity| entity.test) {
            if include_tests {
                let path = test_path(&affected.entity, &result.edges);
                if let Some(entity) = entity {
                    tests.entry(path).or_default().push(entity);
                }
            } else {
                hidden_tests += 1;
            }
            continue;
        }
        if entity.is_some_and(|entity| visibility(entity, include_tests) != Visibility::Primary) {
            continue;
        }
        let group = if affected.hops == 1 {
            "direct".into()
        } else {
            entity.map_or_else(|| "other".into(), |entity| kind_label(entity.kind).into())
        };
        if let Some(entity) = entity {
            groups.entry(group).or_default().push(entity);
        }
    }
    let affected_count = groups.values().map(Vec::len).sum::<usize>();
    let test_count = tests.values().map(Vec::len).sum::<usize>();
    let mut output = format!("{}\n", label(&names, &result.root));
    for (group, entities) in groups {
        let mut display_names = display_names(&entities, &names);
        display_names.sort_unstable();
        let _ = writeln!(output, "\n{group}");
        for name in display_names {
            let _ = writeln!(output, "  - {name}");
        }
    }
    if !tests.is_empty() {
        output.push_str("\ntests\n");
        for (path, entities) in tests {
            let mut display_names = display_names(&entities, &names);
            display_names.sort_unstable();
            let _ = writeln!(output, "  {path}");
            for name in display_names {
                let _ = writeln!(output, "    - {name}");
            }
        }
    }
    let repositories = result
        .nodes
        .iter()
        .filter_map(|entity| entity.repository.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let _ = writeln!(
        output,
        "\n{} affected symbols · {} repositories",
        affected_count + test_count,
        repositories
    );
    if hidden_tests > 0 {
        let _ = writeln!(
            output,
            "{hidden_tests} tests hidden · use --include-tests to show them"
        );
    }
    write_traversal(&mut output, &result.traversal);
    write_metadata(&mut output, &result.metadata);
    output
}

pub(super) fn display_names(
    entities: &[&EntityRef],
    names: &BTreeMap<&str, String>,
) -> Vec<String> {
    entities.iter().map(|entity| label(names, entity)).collect()
}
