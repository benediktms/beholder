use crate::projection::{entities, entity_names, is_primary, label, name, write_metadata};
use beholder_dto::ContextResult;

pub(super) fn context_human(result: &ContextResult, include_tests: bool) -> String {
    let entities = entities(&result.nodes);
    let names = entity_names(&result.nodes);
    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for edge in &result.edges {
        if edge.to == result.root.id && is_primary(&entities, &edge.from, include_tests) {
            incoming.push(format!(
                "  ← {} [{}]",
                name(&names, &edge.from),
                incoming_relation(edge.kind.as_str())
            ));
        } else if edge.from == result.root.id && is_primary(&entities, &edge.to, include_tests) {
            outgoing.push(format!(
                "  → {} [{}]",
                name(&names, &edge.to),
                edge.kind.as_str()
            ));
        }
    }
    let mut output = format!("{}\n", label(&names, &result.root));
    if !incoming.is_empty() {
        output.push_str("\nincoming\n");
        output.push_str(&incoming.join("\n"));
        output.push('\n');
    }
    if !outgoing.is_empty() {
        output.push_str("\noutgoing\n");
        output.push_str(&outgoing.join("\n"));
        output.push('\n');
    }
    write_metadata(&mut output, &result.metadata);
    output
}

fn incoming_relation(relation: &str) -> &str {
    match relation {
        "calls" => "called by",
        "defines" => "defined by",
        "implements" => "implemented by",
        relation => relation,
    }
}
