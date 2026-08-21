use apollo_parser::{
    Parser,
    cst::{self, CstNode},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact,
    GraphqlTypeKind, Observation,
};
use beholder_indexing::{
    AnalysisCompleteness, AnalyzerContribution, AnalyzerError, AnalyzerMetadata, AnalyzerPlan,
    CacheStatistics, RepositoryContribution, WorkspaceAnalyzer, WorkspaceSnapshot,
};
use std::{collections::BTreeMap, path::Path};

mod operations;
mod schema;

use operations::operation_facts;
use schema::{
    FieldEntitiesInput, enum_values, field_entities, implements_interfaces, input_field_entities,
    record_field_types, record_root_types, record_source_type, type_entity, union_members,
};

pub const FRONTEND_VERSION: &str = "9";

#[derive(Clone, Copy)]
pub struct GraphqlSource<'a> {
    pub path: &'a Path,
    pub source: &'a str,
    pub owner: Option<&'a str>,
}

pub struct GraphqlAnalyzer;

impl WorkspaceAnalyzer for GraphqlAnalyzer {
    fn metadata(&self) -> AnalyzerMetadata {
        AnalyzerMetadata {
            id: "graphql".into(),
            version: FRONTEND_VERSION.into(),
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "graphql" | "gql"))
    }

    fn analyze_prepared(
        &self,
        snapshot: &WorkspaceSnapshot,
        plan: &AnalyzerPlan,
    ) -> Result<AnalyzerContribution, AnalyzerError> {
        let mut active_repositories = Vec::new();
        let mut repositories = Vec::new();
        for repository in &snapshot.repositories {
            let schemas = repository
                .inputs
                .iter()
                .filter(|input| self.accepts(&input.path))
                .map(|input| {
                    std::str::from_utf8(&input.content)
                        .map(|source| GraphqlSource {
                            path: &input.path,
                            source,
                            owner: None,
                        })
                        .map_err(|error| {
                            beholder_domain::SourceAnalysisError::from_source(
                                &input.path,
                                Box::new(error),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if schemas.is_empty() {
                continue;
            }
            active_repositories.push(repository.state.repository.identity.clone());
            if plan
                .cached_repository(&repository.state.repository.identity)
                .is_some()
            {
                continue;
            }
            let analysis = facts(&repository.state.repository.identity, &schemas);
            let completeness = if analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.ends_with(".parse_recovery"))
            {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            };
            repositories.push(RepositoryContribution {
                repository: repository.state.repository.identity.clone(),
                completeness,
                entities: analysis.entities,
                grpc_bindings: Vec::new(),
                observations: analysis.observations,
                diagnostics: analysis.diagnostics,
            });
        }
        Ok(AnalyzerContribution {
            metadata: self.metadata(),
            active_repositories,
            repositories,
            overrides: Vec::new(),
            graphql_resolvers: Vec::new(),
            diagnostics: Vec::new(),
            cache: CacheStatistics::default(),
        })
    }
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

pub fn facts(_repository: &str, sources: &[GraphqlSource<'_>]) -> GraphqlFacts {
    let mut facts = GraphqlFacts::default();
    let mut entities = BTreeMap::new();
    let mut stitched_fields = Vec::new();
    let parsed = sources
        .iter()
        .map(|source| (*source, Parser::new(source.source).parse()))
        .collect::<Vec<_>>();
    let mut roots = BTreeMap::new();
    let mut fragments = BTreeMap::new();
    let mut source_types = BTreeMap::new();
    for (_, syntax) in &parsed {
        for definition in syntax.document().definitions() {
            match definition {
                cst::Definition::SchemaDefinition(schema) => {
                    record_root_types(&mut roots, schema.root_operation_type_definitions())
                }
                cst::Definition::SchemaExtension(schema) => {
                    record_root_types(&mut roots, schema.root_operation_type_definitions())
                }
                cst::Definition::FragmentDefinition(fragment) => {
                    if let Some(name) = fragment
                        .fragment_name()
                        .and_then(|fragment| fragment.name())
                    {
                        fragments.entry(name.text().to_string()).or_insert(fragment);
                    }
                }
                cst::Definition::ObjectTypeDefinition(definition) => record_source_type(
                    &mut source_types,
                    definition.name(),
                    definition.directives(),
                ),
                cst::Definition::ObjectTypeExtension(definition) => record_source_type(
                    &mut source_types,
                    definition.name(),
                    definition.directives(),
                ),
                cst::Definition::InterfaceTypeDefinition(definition) => record_source_type(
                    &mut source_types,
                    definition.name(),
                    definition.directives(),
                ),
                cst::Definition::InterfaceTypeExtension(definition) => record_source_type(
                    &mut source_types,
                    definition.name(),
                    definition.directives(),
                ),
                _ => {}
            }
        }
    }
    let mut field_types = BTreeMap::new();
    for (_, syntax) in &parsed {
        for definition in syntax.document().definitions() {
            match definition {
                cst::Definition::ObjectTypeDefinition(definition) => record_field_types(
                    &mut field_types,
                    &roots,
                    definition.name(),
                    definition.fields_definition(),
                ),
                cst::Definition::ObjectTypeExtension(definition) => record_field_types(
                    &mut field_types,
                    &roots,
                    definition.name(),
                    definition.fields_definition(),
                ),
                cst::Definition::InterfaceTypeDefinition(definition) => record_field_types(
                    &mut field_types,
                    &roots,
                    definition.name(),
                    definition.fields_definition(),
                ),
                cst::Definition::InterfaceTypeExtension(definition) => record_field_types(
                    &mut field_types,
                    &roots,
                    definition.name(),
                    definition.fields_definition(),
                ),
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
                        implements_interfaces(
                            &mut facts,
                            source,
                            &name.text(),
                            object.implements_interfaces(),
                        );
                        field_entities(FieldEntitiesInput {
                            facts: &mut facts,
                            entities: &mut entities,
                            stitched_fields: &mut stitched_fields,
                            source,
                            type_name: name,
                            fields: object.fields_definition(),
                            roots: &roots,
                            source_types: &source_types,
                        });
                    }
                }
                cst::Definition::ObjectTypeExtension(object) => {
                    if let Some(name) = object.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Object);
                        implements_interfaces(
                            &mut facts,
                            source,
                            &name.text(),
                            object.implements_interfaces(),
                        );
                        field_entities(FieldEntitiesInput {
                            facts: &mut facts,
                            entities: &mut entities,
                            stitched_fields: &mut stitched_fields,
                            source,
                            type_name: name,
                            fields: object.fields_definition(),
                            roots: &roots,
                            source_types: &source_types,
                        });
                    }
                }
                cst::Definition::InputObjectTypeDefinition(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Input);
                        input_field_entities(
                            &mut facts,
                            &mut entities,
                            source,
                            &name.text(),
                            definition.input_fields_definition(),
                        );
                    }
                }
                cst::Definition::InputObjectTypeExtension(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Input);
                        input_field_entities(
                            &mut facts,
                            &mut entities,
                            source,
                            &name.text(),
                            definition.input_fields_definition(),
                        );
                    }
                }
                cst::Definition::InterfaceTypeDefinition(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(
                            &mut entities,
                            Some(name.clone()),
                            GraphqlTypeKind::Interface,
                        );
                        implements_interfaces(
                            &mut facts,
                            source,
                            &name.text(),
                            definition.implements_interfaces(),
                        );
                        field_entities(FieldEntitiesInput {
                            facts: &mut facts,
                            entities: &mut entities,
                            stitched_fields: &mut stitched_fields,
                            source,
                            type_name: name,
                            fields: definition.fields_definition(),
                            roots: &roots,
                            source_types: &source_types,
                        });
                    }
                }
                cst::Definition::InterfaceTypeExtension(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(
                            &mut entities,
                            Some(name.clone()),
                            GraphqlTypeKind::Interface,
                        );
                        implements_interfaces(
                            &mut facts,
                            source,
                            &name.text(),
                            definition.implements_interfaces(),
                        );
                        field_entities(FieldEntitiesInput {
                            facts: &mut facts,
                            entities: &mut entities,
                            stitched_fields: &mut stitched_fields,
                            source,
                            type_name: name,
                            fields: definition.fields_definition(),
                            roots: &roots,
                            source_types: &source_types,
                        });
                    }
                }
                cst::Definition::UnionTypeDefinition(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Union);
                        union_members(
                            &mut facts,
                            source,
                            &name.text(),
                            definition.union_member_types(),
                        );
                    }
                }
                cst::Definition::UnionTypeExtension(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Union);
                        union_members(
                            &mut facts,
                            source,
                            &name.text(),
                            definition.union_member_types(),
                        );
                    }
                }
                cst::Definition::EnumTypeDefinition(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Enum);
                        enum_values(
                            &mut facts,
                            &mut entities,
                            source,
                            &name.text(),
                            definition.enum_values_definition(),
                        );
                    }
                }
                cst::Definition::EnumTypeExtension(definition) => {
                    if let Some(name) = definition.name() {
                        type_entity(&mut entities, Some(name.clone()), GraphqlTypeKind::Enum);
                        enum_values(
                            &mut facts,
                            &mut entities,
                            source,
                            &name.text(),
                            definition.enum_values_definition(),
                        );
                    }
                }
                cst::Definition::ScalarTypeDefinition(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Scalar)
                }
                cst::Definition::ScalarTypeExtension(definition) => {
                    type_entity(&mut entities, definition.name(), GraphqlTypeKind::Scalar)
                }
                cst::Definition::OperationDefinition(operation) => {
                    operation_facts(
                        &mut facts,
                        &mut entities,
                        source,
                        operation,
                        &fragments,
                        &field_types,
                        &roots,
                    );
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
    use beholder_domain::{EntityKind, EntityMetadata, GraphqlOperationKind, StructuralRelation};

    #[test]
    fn maps_schema_fields_and_nested_operation_selections() {
        let schema = GraphqlSource {
            path: Path::new("schema.graphql"),
            source: "schema { query: RootQuery subscription: RootSubscription } type RootQuery { packageTemplatePreview(filter: Filter, sort: Sort!): Package location: Location } type RootSubscription { typing(conversationId: ID!): String } input Filter { query: String } interface Node { id: ID! } type Package implements Node { id: ID! } union Search = Package enum Sort { ASC } scalar Date",
            owner: Some("repo://gateway/graphql-source/schema.graphql"),
        };
        let operation = GraphqlSource {
            path: Path::new("PackageDetail.gql.tsx"),
            source: "query Packages_Detail_Query($filter: Filter) { ...RootFields ... on RootQuery { location { id } } } fragment RootFields on RootQuery { preview: packageTemplatePreview(filter: $filter) { id } } subscription Typing($conversationId: ID!) { typing(conversationId: $conversationId) }",
            owner: Some("repo://spa/typescript/PackageDetail.gql"),
        };

        let facts = facts("gateway", &[schema, operation]);

        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Packages_Detail_Query"
                && entity.kind == EntityKind::GraphqlOperation
                && entity.metadata
                    == Some(EntityMetadata::GraphqlOperation {
                        kind: GraphqlOperationKind::Query,
                    })
        }));
        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "graphql-operation://Typing"
                && entity.kind == EntityKind::GraphqlOperation
                && entity.metadata
                    == Some(EntityMetadata::GraphqlOperation {
                        kind: GraphqlOperationKind::Subscription,
                    })
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
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-operation://Typing"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(DependencyRelation::Selects)
                && observation.to.as_str() == "graphql-field://Subscription/typing"
        }));
        for (from, to) in [
            (
                "graphql-field://Query/packageTemplatePreview",
                "graphql-field://Package/id",
            ),
            (
                "graphql-field://Query/location",
                "graphql-field://Location/id",
            ),
        ] {
            assert!(facts.observations.iter().any(|observation| {
                observation.from.as_str() == from
                    && observation.relation
                        == beholder_domain::SemanticRelation::Dependency(
                            DependencyRelation::Selects,
                        )
                    && observation.to.as_str() == to
            }));
        }
        for (from, relation, to) in [
            (
                "graphql-field://Query/packageTemplatePreview",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::FieldOf),
                "graphql-type://RootQuery",
            ),
            (
                "graphql-field://Query/packageTemplatePreview",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::RequestType),
                "graphql-type://Filter",
            ),
            (
                "graphql-field://Query/packageTemplatePreview",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::ResponseType),
                "graphql-type://Package",
            ),
            (
                "graphql-argument://Query/packageTemplatePreview/filter",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::FieldOf),
                "graphql-field://Query/packageTemplatePreview",
            ),
            (
                "graphql-argument://Query/packageTemplatePreview/filter",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::RequestType),
                "graphql-type://Filter",
            ),
            (
                "graphql-field://Filter/query",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::RequestType),
                "graphql-type://String",
            ),
            (
                "graphql-field://Filter/query",
                beholder_domain::SemanticRelation::Dependency(DependencyRelation::Uses),
                "graphql-type://String",
            ),
            (
                "graphql-type://Package",
                beholder_domain::SemanticRelation::Dependency(DependencyRelation::Implements),
                "graphql-type://Node",
            ),
            (
                "graphql-type://Search",
                beholder_domain::SemanticRelation::Dependency(DependencyRelation::Uses),
                "graphql-type://Package",
            ),
            (
                "graphql-enum-value://Sort/ASC",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::FieldOf),
                "graphql-type://Sort",
            ),
            (
                "graphql-operation://Packages_Detail_Query",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::RequestType),
                "graphql-type://Filter",
            ),
            (
                "graphql-field://Subscription/typing",
                beholder_domain::SemanticRelation::Structural(StructuralRelation::FieldOf),
                "graphql-type://RootSubscription",
            ),
        ] {
            assert!(
                facts.observations.iter().any(|observation| {
                    observation.from.as_str() == from
                        && observation.relation == relation
                        && observation.to.as_str() == to
                }),
                "missing {from} {} {to}",
                relation.as_str()
            );
        }
        assert!(facts.diagnostics.is_empty());
    }

    #[test]
    fn maps_composed_fields_to_their_source_field() {
        let upstream = GraphqlSource {
            path: Path::new("compose/schemas/Checkout.graphql"),
            source: "schema { mutation: RootMutationType } type RootMutationType { initializeOrder: ID } type Order { paymentMethods: [String!]! }",
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
                type Checkout_Order @source(name: "Order", subgraph: "Checkout") {
                  paymentMethods: [String!]!
                    @source(name: "paymentMethods", subgraph: "Checkout")
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
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "graphql-field://Checkout_Order/paymentMethods"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
                && observation.to.as_str() == "graphql-field://Order/paymentMethods"
        }));
    }
}
