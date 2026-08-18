use apollo_parser::{
    Parser,
    cst::{self, CstNode},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    EntityMetadata, GraphqlTypeKind, Observation, StructuralRelation,
};
use std::{collections::BTreeMap, path::Path};

pub const FRONTEND_VERSION: &str = "4";

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
    operation_type_name(&kind)
}

fn operation_type_name(kind: &cst::OperationType) -> &'static str {
    if kind.mutation_token().is_some() {
        "Mutation"
    } else if kind.subscription_token().is_some() {
        "Subscription"
    } else {
        "Query"
    }
}

fn record_root_types(
    roots: &mut BTreeMap<String, &'static str>,
    definitions: impl Iterator<Item = cst::RootOperationTypeDefinition>,
) {
    for definition in definitions {
        let Some((operation, name)) = definition
            .operation_type()
            .zip(definition.named_type().and_then(|named| named.name()))
        else {
            continue;
        };
        roots
            .entry(name.text().to_string())
            .or_insert_with(|| operation_type_name(&operation));
    }
}

fn field_entities(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    stitched_fields: &mut Vec<(String, String, String)>,
    source: GraphqlSource<'_>,
    type_name: cst::Name,
    fields: Option<cst::FieldsDefinition>,
    roots: &BTreeMap<String, &'static str>,
) {
    let declared_type_name = type_name.text().to_string();
    let type_name = roots
        .get(&declared_type_name)
        .copied()
        .unwrap_or(declared_type_name.as_str());
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

fn type_entity(
    entities: &mut BTreeMap<String, EntityFact>,
    name: Option<cst::Name>,
    kind: GraphqlTypeKind,
) {
    let Some(name) = name else {
        return;
    };
    let id = format!("graphql-type://{}", name.text());
    entities.entry(id.clone()).or_insert_with(|| {
        EntityFact::new(
            id,
            EntityKind::GraphqlType,
            Some(EntityMetadata::GraphqlType { kind }),
        )
        .unwrap()
    });
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
    let parsed = sources
        .iter()
        .map(|source| (*source, Parser::new(source.source).parse()))
        .collect::<Vec<_>>();
    let mut roots = BTreeMap::new();
    for (_, syntax) in &parsed {
        for definition in syntax.document().definitions() {
            match definition {
                cst::Definition::SchemaDefinition(schema) => {
                    record_root_types(&mut roots, schema.root_operation_type_definitions())
                }
                cst::Definition::SchemaExtension(schema) => {
                    record_root_types(&mut roots, schema.root_operation_type_definitions())
                }
                _ => {}
            }
        }
    }
    for (source, syntax) in parsed {
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
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Object);
                        field_entities(
                            &mut facts,
                            &mut entities,
                            &mut stitched_fields,
                            source,
                            name,
                            object.fields_definition(),
                            &roots,
                        );
                    }
                }
                cst::Definition::ObjectTypeExtension(object) => {
                    if let Some(name) = object.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Object);
                        field_entities(
                            &mut facts,
                            &mut entities,
                            &mut stitched_fields,
                            source,
                            name,
                            object.fields_definition(),
                            &roots,
                        );
                    }
                }
                cst::Definition::InputObjectTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Input)
                }
                cst::Definition::InputObjectTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Input)
                }
                cst::Definition::InterfaceTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Interface)
                }
                cst::Definition::InterfaceTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Interface)
                }
                cst::Definition::UnionTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Union)
                }
                cst::Definition::UnionTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Union)
                }
                cst::Definition::EnumTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Enum)
                }
                cst::Definition::EnumTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Enum)
                }
                cst::Definition::ScalarTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Scalar)
                }
                cst::Definition::ScalarTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Scalar)
                }
                cst::Definition::OperationDefinition(operation) => {
                    operation_facts(&mut facts, &mut entities, source, operation);
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
            source: "schema { query: RootQuery } type RootQuery { packageTemplatePreview: Package location: Location } input Filter { query: String } interface Node { id: ID! } type Package implements Node { id: ID! } union Search = Package enum Sort { ASC } scalar Date",
            owner: Some("repo://gateway/graphql-source/schema.graphql"),
        };
        let operation = GraphqlSource {
            path: Path::new("PackageDetail.gql.tsx"),
            source: "query Packages_Detail_Query { preview: packageTemplatePreview { id } location { id } }",
            owner: Some("repo://spa/typescript/PackageDetail.gql"),
        };

        let facts = facts("gateway", &[schema, operation]);

        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Packages_Detail_Query"
                && entity.kind == EntityKind::GraphqlOperation
        }));
        for (name, kind) in [
            ("RootQuery", GraphqlTypeKind::Object),
            ("Filter", GraphqlTypeKind::Input),
            ("Node", GraphqlTypeKind::Interface),
            ("Search", GraphqlTypeKind::Union),
            ("Sort", GraphqlTypeKind::Enum),
            ("Date", GraphqlTypeKind::Scalar),
        ] {
            assert!(facts.entities.iter().any(|entity| {
                entity.id.as_str() == format!("graphql-type://{name}")
                    && entity.kind == EntityKind::GraphqlType
                    && entity.metadata == Some(EntityMetadata::GraphqlType { kind })
            }));
        }
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
            source: "schema { mutation: RootMutationType } type RootMutationType { initializeOrder: ID }",
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
