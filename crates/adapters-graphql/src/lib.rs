use apollo_parser::{
    Parser,
    cst::{self, CstNode},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, StructuralRelation,
};
use std::{collections::BTreeMap, path::Path};

pub const FRONTEND_VERSION: &str = "1";

#[derive(Clone, Copy)]
pub struct GraphqlSource<'a> {
    pub path: &'a Path,
    pub source: &'a str,
    pub owner: Option<&'a str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphqlFacts {
    pub entities: Vec<EntityFact>,
    pub observations: Vec<Observation>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

fn line(source: &str, node: &impl CstNode) -> u32 {
    let offset = u32::from(node.syntax().text_range().start()) as usize;
    u32::try_from(
        source[..offset.min(source.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1,
    )
    .unwrap_or(u32::MAX)
}

fn evidence(source: GraphqlSource<'_>, node: &impl CstNode) -> String {
    format!("{}:{}", source.path.display(), line(source.source, node))
}

fn root_type(operation: &cst::OperationDefinition) -> &'static str {
    let Some(kind) = operation.operation_type() else {
        return "Query";
    };
    if kind.mutation_token().is_some() {
        "Mutation"
    } else if kind.subscription_token().is_some() {
        "Subscription"
    } else {
        "Query"
    }
}

fn field_entities(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    type_name: cst::Name,
    fields: Option<cst::FieldsDefinition>,
) {
    let type_name = type_name.text().to_string();
    for field in fields
        .into_iter()
        .flat_map(|fields| fields.field_definitions())
    {
        let Some(name) = field.name() else {
            continue;
        };
        let id = format!("graphql-field://{type_name}/{}", name.text());
        entities.entry(id.clone()).or_insert_with(|| {
            EntityFact::new(id.clone(), EntityKind::GraphqlField, None).unwrap()
        });
        if let Some(owner) = source.owner {
            facts.observations.push(Observation::structural(
                owner,
                StructuralRelation::Defines,
                id,
                evidence(source, &field),
            ));
        }
    }
}

fn operation_facts(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    operation: cst::OperationDefinition,
) {
    let name = operation
        .name()
        .map(|name| name.text().to_string())
        .unwrap_or_else(|| format!("anonymous@{}", source.path.display()));
    let operation_id = format!("graphql-operation://{name}");
    entities.entry(operation_id.clone()).or_insert_with(|| {
        EntityFact::new(operation_id.clone(), EntityKind::GraphqlOperation, None).unwrap()
    });
    if let Some(owner) = source.owner {
        facts.observations.push(Observation::structural(
            owner,
            StructuralRelation::Defines,
            operation_id.clone(),
            evidence(source, &operation),
        ));
    }
    let root = root_type(&operation);
    for selection in operation
        .selection_set()
        .into_iter()
        .flat_map(|selection_set| selection_set.selections())
    {
        let cst::Selection::Field(field) = selection else {
            continue;
        };
        let Some(name) = field.name() else {
            continue;
        };
        let field_id = format!("graphql-field://{root}/{}", name.text());
        entities.entry(field_id.clone()).or_insert_with(|| {
            EntityFact::new(field_id.clone(), EntityKind::GraphqlField, None).unwrap()
        });
        facts.observations.push(Observation::dependency(
            operation_id.clone(),
            DependencyRelation::Selects,
            field_id,
            evidence(source, &field),
        ));
    }
}

pub fn facts(_repository: &str, sources: &[GraphqlSource<'_>]) -> GraphqlFacts {
    let mut facts = GraphqlFacts::default();
    let mut entities = BTreeMap::new();
    for source in sources {
        let syntax = Parser::new(source.source).parse();
        for error in syntax.errors() {
            let error_line = u32::try_from(
                source.source[..error.index().min(source.source.len())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
            )
            .ok();
            facts.diagnostics.push(AnalysisDiagnostic {
                code: "graphql.parse_recovery".into(),
                severity: AnalysisDiagnosticSeverity::Warning,
                path: source.path.into(),
                line: error_line,
                detail: Some(error.message().into()),
            });
        }
        for definition in syntax.document().definitions() {
            match definition {
                cst::Definition::ObjectTypeDefinition(object) => {
                    if let Some(name) = object.name() {
                        field_entities(
                            &mut facts,
                            &mut entities,
                            *source,
                            name,
                            object.fields_definition(),
                        );
                    }
                }
                cst::Definition::ObjectTypeExtension(object) => {
                    if let Some(name) = object.name() {
                        field_entities(
                            &mut facts,
                            &mut entities,
                            *source,
                            name,
                            object.fields_definition(),
                        );
                    }
                }
                cst::Definition::OperationDefinition(operation) => {
                    operation_facts(&mut facts, &mut entities, *source, operation);
                }
                _ => {}
            }
        }
    }
    facts.entities = entities.into_values().collect();
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_schema_fields_and_operation_root_selections() {
        let schema = GraphqlSource {
            path: Path::new("schema.graphql"),
            source: "type Query { packageTemplatePreview: Package location: Location }",
            owner: Some("repo://gateway/graphql-source/schema.graphql"),
        };
        let operation = GraphqlSource {
            path: Path::new("PackageDetail.gql.tsx"),
            source: "query Packages_Detail_Query { packageTemplatePreview { id } location { id } }",
            owner: Some("repo://spa/typescript/PackageDetail.gql"),
        };

        let facts = facts("gateway", &[schema, operation]);

        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Packages_Detail_Query"
                && entity.kind == EntityKind::GraphqlOperation
        }));
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-operation://Packages_Detail_Query"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(DependencyRelation::Selects)
                && observation.to.as_str() == "graphql-field://Query/packageTemplatePreview"
        }));
        assert!(facts.diagnostics.is_empty());
    }
}
