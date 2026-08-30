use super::{TypescriptAnalysis, grats, nestjs_graphql};
use beholder_adapters_graphql::{GraphqlFacts, GraphqlSource, facts};
use serde_json::Value;
use std::collections::BTreeMap;
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

pub struct GraphqlFactInput<'a> {
    pub repository: &'a str,
    pub sources: &'a [GraphqlResolverSource<'a>],
    pub schemas: &'a [GraphqlSource<'a>],
}

pub(super) fn has_package(manifests: &[(&Path, &str)], package: &str) -> bool {
    manifests.iter().any(|(_, source)| {
        let Ok(manifest) = serde_json::from_str::<Value>(source) else {
            return false;
        };
        [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ]
        .iter()
        .any(|section| {
            manifest
                .get(section)
                .is_some_and(|deps| deps.get(package).is_some())
        })
    })
}

fn extend(facts: &mut GraphqlFacts, source: GraphqlFacts) {
    facts.entities.extend(source.entities);
    facts.observations.extend(source.observations);
    facts.diagnostics.extend(source.diagnostics);
}

pub fn collect_graphql_resolvers(input: GraphqlResolverInput<'_>) -> GraphqlFacts {
    let mut facts = collect_grats_resolvers(&input);
    if has_package(input.manifests, "@nestjs/graphql") {
        for source in input.sources {
            extend(&mut facts, nestjs_graphql::facts(input.repository, source));
        }
    }
    facts
}

pub(crate) fn collect_grats_resolvers(input: &GraphqlResolverInput<'_>) -> GraphqlFacts {
    let mut facts = GraphqlFacts::default();
    if input
        .sources
        .iter()
        .any(|source| source.source.contains("@gql"))
        && has_package(input.manifests, "grats")
    {
        let types = input
            .sources
            .iter()
            .flat_map(|source| grats::types(source))
            .collect::<BTreeMap<_, _>>();
        for source in input.sources {
            extend(
                &mut facts,
                grats::facts(grats::FactsInput {
                    repository: input.repository,
                    source,
                    types: &types,
                }),
            );
        }
    }
    facts
}

pub fn collect_graphql_facts(input: GraphqlFactInput<'_>) -> GraphqlFacts {
    let documents = input
        .sources
        .iter()
        .flat_map(|source| {
            source.analysis.graphql_documents.iter().map(|document| {
                (
                    source.path,
                    document.source.as_str(),
                    format!(
                        "repo://{}/{}/{}",
                        input.repository,
                        source.analysis.language.id_segment(),
                        super::analysis::source_stem(source.path)
                    ),
                )
            })
        })
        .collect::<Vec<_>>();
    let sources = input
        .schemas
        .iter()
        .copied()
        .chain(documents.iter().map(|(path, source, owner)| GraphqlSource {
            path,
            source,
            owner: Some(owner),
        }))
        .collect::<Vec<_>>();
    facts(input.repository, &sources)
}
