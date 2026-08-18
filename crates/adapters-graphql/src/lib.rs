use apollo_parser::{
    Parser,
    cst::{self, CstNode},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation, StructuralRelation,
};
use std::{collections::BTreeMap, path::Path};

pub const FRONTEND_VERSION: &str = "2";

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
    stitched_fields: &mut Vec<(String, String, String)>,
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
                id.clone(),
                evidence(source, &field),
            ));
        }
        if let Some(upstream) = stitched_field(&field) {
            stitched_fields.push((
                id,
                format!("graphql-field://{type_name}/{upstream}"),
                evidence(source, &field),
            ));
        }
    }
}

fn directive_argument(
    field: &cst::FieldDefinition,
    directive_name: &str,
    argument_name: &str,
) -> Option<String> {
    let directive = field.directives()?.directives().find(|directive| {
        directive
            .name()
            .is_some_and(|name| name.text() == directive_name)
    })?;
    let value = directive.arguments()?.arguments().find_map(|argument| {
        (argument
            .name()
            .is_some_and(|name| name.text() == argument_name))
        .then(|| argument.value())
        .flatten()
    })?;
    Some(value.syntax().text().to_string().trim_matches('"').into())
}

fn stitched_field(field: &cst::FieldDefinition) -> Option<String> {
    directive_argument(field, "source", "subgraph")?;
    directive_argument(field, "join__field", "graph")?;
    directive_argument(field, "source", "name")
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
    let mut stitched_fields = Vec::new();
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
                            &mut stitched_fields,
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
                            &mut stitched_fields,
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
    facts.observations.extend(
        stitched_fields
            .into_iter()
            .filter(|(gateway, upstream, _)| {
                gateway != upstream
                    && entities.contains_key(gateway)
                    && entities.contains_key(upstream)
            })
            .map(|(gateway, upstream, evidence)| {
                Observation::dependency(gateway, DependencyRelation::ResolvedBy, upstream, evidence)
            }),
    );
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

    #[test]
    fn maps_composed_fields_to_their_source_field() {
        let upstream = GraphqlSource {
            path: Path::new("compose/schemas/Checkout.graphql"),
            source: "type Mutation { initializeOrder: ID }",
            owner: None,
        };
        let composed = GraphqlSource {
            path: Path::new("supergraph.graphql"),
            source: r#"
                type Mutation {
                  Checkout_initializeOrder: ID
                    @source(name: "initializeOrder", subgraph: "Checkout")
                    @join__field(graph: CHECKOUT)
                }
            "#,
            owner: None,
        };

        let facts = facts("gateway", &[upstream, composed]);

        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Mutation/Checkout_initializeOrder"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str() == "graphql-field://Mutation/initializeOrder"
        }));
    }
}
