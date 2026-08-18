use super::{TypescriptAnalysis, grats};
use beholder_adapters_graphql::GraphqlFacts;
use std::path::Path;

pub struct GraphqlResolverSource<'a> {
    pub path: &'a Path,
    pub analysis: &'a TypescriptAnalysis,
    pub source: &'a str,
}

pub struct GraphqlResolverInput<'a> {
    pub repository: &'a str,
    pub sources: &'a [GraphqlResolverSource<'a>],
    pub manifests: &'a [(&'a Path, &'a str)],
}

pub fn collect_graphql_resolvers(input: GraphqlResolverInput<'_>) -> GraphqlFacts {
    if !grats::installed(input.manifests) {
        return GraphqlFacts::default();
    }
    let mut facts = GraphqlFacts::default();
    for source in input.sources {
        let source_facts = grats::facts(input.repository, source);
        facts.entities.extend(source_facts.entities);
        facts.observations.extend(source_facts.observations);
        facts.diagnostics.extend(source_facts.diagnostics);
    }
    facts
}
