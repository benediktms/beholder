use apollo_parser::{
    Parser,
    cst::{self, CstNode},
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    EntityMetadata, GraphqlOperationKind, GraphqlTypeKind, Observation, StructuralRelation,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const FRONTEND_VERSION: &str = "9";

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

struct FieldEntitiesInput<'a, 'source> {
    facts: &'a mut GraphqlFacts,
    entities: &'a mut BTreeMap<String, EntityFact>,
    stitched_fields: &'a mut Vec<(String, String, String)>,
    source: GraphqlSource<'source>,
    type_name: cst::Name,
    fields: Option<cst::FieldsDefinition>,
    roots: &'a BTreeMap<String, &'static str>,
    source_types: &'a BTreeMap<(String, String), String>,
}

fn field_entities(input: FieldEntitiesInput<'_, '_>) {
    let FieldEntitiesInput {
        facts,
        entities,
        stitched_fields,
        source,
        type_name,
        fields,
        roots,
        source_types,
    } = input;
    let declared_type_name = type_name.text().to_string();
    let owner_type_id = format!("graphql-type://{declared_type_name}");
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
        facts.observations.push(Observation::structural(
            id.clone(),
            StructuralRelation::FieldOf,
            owner_type_id.clone(),
            evidence(source, &field),
        ));
        if let Some(ty) = field.ty() {
            type_relationship(
                facts,
                entities,
                source,
                &id,
                &ty,
                StructuralRelation::ResponseType,
                &field,
            );
        }
        for argument in field
            .arguments_definition()
            .into_iter()
            .flat_map(|arguments| arguments.input_value_definitions())
        {
            let Some(argument_name) = argument.name() else {
                continue;
            };
            let argument_id = format!(
                "graphql-argument://{type_name}/{}/{}",
                name.text(),
                argument_name.text()
            );
            entities.entry(argument_id.clone()).or_insert_with(|| {
                EntityFact::new(argument_id.clone(), EntityKind::GraphqlArgument, None).unwrap()
            });
            facts.observations.push(Observation::structural(
                argument_id.clone(),
                StructuralRelation::FieldOf,
                id.clone(),
                evidence(source, &argument),
            ));
            if let Some(ty) = argument.ty() {
                type_relationship(
                    facts,
                    entities,
                    source,
                    &argument_id,
                    &ty,
                    StructuralRelation::RequestType,
                    &argument,
                );
                type_relationship(
                    facts,
                    entities,
                    source,
                    &id,
                    &ty,
                    StructuralRelation::RequestType,
                    &argument,
                );
            }
        }
        if let Some(owner) = source.owner {
            facts.observations.push(Observation::structural(
                owner,
                StructuralRelation::Defines,
                id.clone(),
                evidence(source, &field),
            ));
        }
        if let Some(upstream) = stitched_field(&field, &declared_type_name, type_name, source_types)
        {
            stitched_fields.push((id, upstream, evidence(source, &field)));
        }
    }
}

fn named_type_name(ty: &cst::Type) -> Option<String> {
    match ty {
        cst::Type::NamedType(named) => named.name().map(|name| name.text().to_string()),
        cst::Type::ListType(list) => list.ty().and_then(|ty| named_type_name(&ty)),
        cst::Type::NonNullType(non_null) => non_null
            .named_type()
            .and_then(|named| named.name())
            .map(|name| name.text().to_string())
            .or_else(|| {
                non_null
                    .list_type()
                    .and_then(|list| list.ty())
                    .and_then(|ty| named_type_name(&ty))
            }),
    }
}

fn type_relationship(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    from: &str,
    ty: &cst::Type,
    structural: StructuralRelation,
    node: &impl CstNode,
) {
    let Some(name) = named_type_name(ty) else {
        return;
    };
    let type_id = format!("graphql-type://{name}");
    if matches!(name.as_str(), "Boolean" | "Float" | "ID" | "Int" | "String") {
        entities.entry(type_id.clone()).or_insert_with(|| {
            EntityFact::new(
                type_id.clone(),
                EntityKind::GraphqlType,
                Some(EntityMetadata::GraphqlType {
                    kind: GraphqlTypeKind::Scalar,
                }),
            )
            .unwrap()
        });
    }
    let evidence = evidence(source, node);
    facts.observations.push(Observation::structural(
        from,
        structural,
        type_id.clone(),
        evidence.clone(),
    ));
    facts.observations.push(Observation::dependency(
        from,
        DependencyRelation::Uses,
        type_id,
        evidence,
    ));
}

fn input_field_entities(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    type_name: &str,
    fields: Option<cst::InputFieldsDefinition>,
) {
    let owner_type_id = format!("graphql-type://{type_name}");
    for field in fields
        .into_iter()
        .flat_map(|fields| fields.input_value_definitions())
    {
        let Some(name) = field.name() else {
            continue;
        };
        let id = format!("graphql-field://{type_name}/{}", name.text());
        entities.entry(id.clone()).or_insert_with(|| {
            EntityFact::new(id.clone(), EntityKind::GraphqlField, None).unwrap()
        });
        facts.observations.push(Observation::structural(
            id.clone(),
            StructuralRelation::FieldOf,
            owner_type_id.clone(),
            evidence(source, &field),
        ));
        if let Some(ty) = field.ty() {
            type_relationship(
                facts,
                entities,
                source,
                &id,
                &ty,
                StructuralRelation::RequestType,
                &field,
            );
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

fn implements_interfaces(
    facts: &mut GraphqlFacts,
    source: GraphqlSource<'_>,
    type_name: &str,
    interfaces: Option<cst::ImplementsInterfaces>,
) {
    let type_id = format!("graphql-type://{type_name}");
    for interface in interfaces.into_iter().flat_map(|value| value.named_types()) {
        let Some(name) = interface.name() else {
            continue;
        };
        facts.observations.push(Observation::dependency(
            type_id.clone(),
            DependencyRelation::Implements,
            format!("graphql-type://{}", name.text()),
            evidence(source, &interface),
        ));
    }
}

fn union_members(
    facts: &mut GraphqlFacts,
    source: GraphqlSource<'_>,
    union_name: &str,
    members: Option<cst::UnionMemberTypes>,
) {
    let union_id = format!("graphql-type://{union_name}");
    for member in members.into_iter().flat_map(|value| value.named_types()) {
        let Some(name) = member.name() else {
            continue;
        };
        facts.observations.push(Observation::dependency(
            union_id.clone(),
            DependencyRelation::Uses,
            format!("graphql-type://{}", name.text()),
            evidence(source, &member),
        ));
    }
}

fn enum_values(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    enum_name: &str,
    values: Option<cst::EnumValuesDefinition>,
) {
    let enum_id = format!("graphql-type://{enum_name}");
    for value in values
        .into_iter()
        .flat_map(|values| values.enum_value_definitions())
    {
        let Some(name) = value.enum_value() else {
            continue;
        };
        let id = format!("graphql-enum-value://{enum_name}/{}", name.syntax().text());
        entities.entry(id.clone()).or_insert_with(|| {
            EntityFact::new(id.clone(), EntityKind::GraphqlEnumValue, None).unwrap()
        });
        facts.observations.push(Observation::structural(
            id,
            StructuralRelation::FieldOf,
            enum_id.clone(),
            evidence(source, &value),
        ));
    }
}

fn directive_argument(
    directives: Option<cst::Directives>,
    directive_name: &str,
    argument_name: &str,
) -> Option<String> {
    let directive = directives?.directives().find(|directive| {
        directive
            .name()
            .is_some_and(|name| name.text() == directive_name)
    })?;
    directive_value(&directive, argument_name)
}

fn directive_value(directive: &cst::Directive, argument_name: &str) -> Option<String> {
    let value = directive.arguments()?.arguments().find_map(|argument| {
        (argument
            .name()
            .is_some_and(|name| name.text() == argument_name))
        .then(|| argument.value())
        .flatten()
    })?;
    Some(value.syntax().text().to_string().trim_matches('"').into())
}

fn stitched_field(
    field: &cst::FieldDefinition,
    declared_parent: &str,
    canonical_parent: &str,
    source_types: &BTreeMap<(String, String), String>,
) -> Option<String> {
    let subgraph = directive_argument(field.directives(), "source", "subgraph")?;
    let field = directive_argument(field.directives(), "source", "name")?;
    let parent = source_types
        .get(&(declared_parent.to_owned(), subgraph))
        .map(String::as_str)
        .unwrap_or(canonical_parent);
    Some(format!("graphql-field://{parent}/{field}"))
}

fn record_source_type(
    source_types: &mut BTreeMap<(String, String), String>,
    name: Option<cst::Name>,
    directives: Option<cst::Directives>,
) {
    let Some(name) = name else {
        return;
    };
    for directive in directives
        .into_iter()
        .flat_map(|directives| directives.directives())
        .filter(|directive| directive.name().is_some_and(|name| name.text() == "source"))
    {
        let Some(upstream) = directive_value(&directive, "name") else {
            continue;
        };
        let Some(subgraph) = directive_value(&directive, "subgraph") else {
            continue;
        };
        source_types
            .entry((name.text().to_string(), subgraph))
            .or_insert(upstream);
    }
}

fn operation_facts(
    facts: &mut GraphqlFacts,
    entities: &mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'_>,
    operation: cst::OperationDefinition,
    fragments: &BTreeMap<String, cst::FragmentDefinition>,
    field_types: &BTreeMap<(String, String), String>,
    roots: &BTreeMap<String, &'static str>,
) {
    let name = operation
        .name()
        .map(|name| name.text().to_string())
        .unwrap_or_else(|| format!("anonymous@{}", source.path.display()));
    let operation_id = format!("graphql-operation://{name}");
    let root = root_type(&operation);
    let kind = match root {
        "Mutation" => GraphqlOperationKind::Mutation,
        "Subscription" => GraphqlOperationKind::Subscription,
        _ => GraphqlOperationKind::Query,
    };
    entities.entry(operation_id.clone()).or_insert_with(|| {
        EntityFact::new(
            operation_id.clone(),
            EntityKind::GraphqlOperation,
            Some(EntityMetadata::GraphqlOperation { kind }),
        )
        .unwrap()
    });
    if let Some(owner) = source.owner {
        facts.observations.push(Observation::structural(
            owner,
            StructuralRelation::Defines,
            operation_id.clone(),
            evidence(source, &operation),
        ));
    }
    for variable in operation
        .variable_definitions()
        .into_iter()
        .flat_map(|variables| variables.variable_definitions())
    {
        let Some(variable_name) = variable.variable().and_then(|variable| variable.name()) else {
            continue;
        };
        let argument_id = format!(
            "graphql-argument://operation/{name}/{}",
            variable_name.text()
        );
        entities.entry(argument_id.clone()).or_insert_with(|| {
            EntityFact::new(argument_id.clone(), EntityKind::GraphqlArgument, None).unwrap()
        });
        facts.observations.push(Observation::structural(
            argument_id.clone(),
            StructuralRelation::FieldOf,
            operation_id.clone(),
            evidence(source, &variable),
        ));
        if let Some(ty) = variable.ty() {
            type_relationship(
                facts,
                entities,
                source,
                &argument_id,
                &ty,
                StructuralRelation::RequestType,
                &variable,
            );
            type_relationship(
                facts,
                entities,
                source,
                &operation_id,
                &ty,
                StructuralRelation::RequestType,
                &variable,
            );
        }
    }
    if let Some(selections) = operation.selection_set() {
        selection_facts(
            &mut SelectionFacts {
                facts,
                entities,
                source,
                fragments,
                field_types,
                roots,
                visited: BTreeSet::new(),
            },
            selections,
            root,
            &operation_id,
        );
    }
}

struct SelectionFacts<'a, 'source> {
    facts: &'a mut GraphqlFacts,
    entities: &'a mut BTreeMap<String, EntityFact>,
    source: GraphqlSource<'source>,
    fragments: &'a BTreeMap<String, cst::FragmentDefinition>,
    field_types: &'a BTreeMap<(String, String), String>,
    roots: &'a BTreeMap<String, &'static str>,
    visited: BTreeSet<String>,
}

fn type_condition_name(condition: Option<cst::TypeCondition>) -> Option<String> {
    condition?
        .named_type()?
        .name()
        .map(|name| name.text().to_string())
}

fn selection_parent(
    context: &SelectionFacts<'_, '_>,
    condition: Option<cst::TypeCondition>,
    fallback: &str,
) -> String {
    let Some(parent) = type_condition_name(condition) else {
        return fallback.to_owned();
    };
    context
        .roots
        .get(&parent)
        .copied()
        .unwrap_or(&parent)
        .to_owned()
}

fn selection_facts(
    context: &mut SelectionFacts<'_, '_>,
    selections: cst::SelectionSet,
    parent_type: &str,
    selector: &str,
) {
    for selection in selections.selections() {
        match selection {
            cst::Selection::Field(field) => {
                let Some(name) = field.name() else {
                    continue;
                };
                let name = name.text().to_string();
                let field_id = format!("graphql-field://{parent_type}/{name}");
                context.entities.entry(field_id.clone()).or_insert_with(|| {
                    EntityFact::new(field_id.clone(), EntityKind::GraphqlField, None).unwrap()
                });
                context.facts.observations.push(Observation::dependency(
                    selector,
                    DependencyRelation::Selects,
                    field_id.clone(),
                    evidence(context.source, &field),
                ));
                if let Some(nested) = field.selection_set()
                    && let Some(response_type) = context
                        .field_types
                        .get(&(parent_type.to_owned(), name))
                        .cloned()
                {
                    selection_facts(context, nested, &response_type, &field_id);
                }
            }
            cst::Selection::FragmentSpread(spread) => {
                let Some(name) = spread
                    .fragment_name()
                    .and_then(|fragment| fragment.name())
                    .map(|name| name.text().to_string())
                else {
                    continue;
                };
                if !context.visited.insert(name.clone()) {
                    continue;
                }
                if let Some(fragment) = context.fragments.get(&name)
                    && let Some(selections) = fragment.selection_set()
                {
                    let parent_type =
                        selection_parent(context, fragment.type_condition(), parent_type);
                    selection_facts(context, selections, &parent_type, selector);
                }
            }
            cst::Selection::InlineFragment(fragment) => {
                if let Some(selections) = fragment.selection_set() {
                    let parent_type =
                        selection_parent(context, fragment.type_condition(), parent_type);
                    selection_facts(context, selections, &parent_type, selector);
                }
            }
        }
    }
}

fn record_field_types(
    fields: &mut BTreeMap<(String, String), String>,
    roots: &BTreeMap<String, &'static str>,
    type_name: Option<cst::Name>,
    definitions: Option<cst::FieldsDefinition>,
) {
    let Some(type_name) = type_name else {
        return;
    };
    let declared_type = type_name.text().to_string();
    let parent = roots
        .get(&declared_type)
        .copied()
        .unwrap_or(&declared_type)
        .to_owned();
    for field in definitions
        .into_iter()
        .flat_map(|definitions| definitions.field_definitions())
    {
        let Some(name) = field.name() else {
            continue;
        };
        let Some(response_type) = field.ty().as_ref().and_then(named_type_name) else {
            continue;
        };
        fields
            .entry((parent.clone(), name.text().to_string()))
            .or_insert(response_type);
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
