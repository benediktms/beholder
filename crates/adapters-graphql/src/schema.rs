use super::{GraphqlFacts, GraphqlSource, evidence};
use apollo_parser::cst::{self, CstNode};
use beholder_domain::{
    DependencyRelation, EntityFact, EntityKind, EntityMetadata, GraphqlTypeKind, Observation,
    StructuralRelation,
};
use std::collections::BTreeMap;

pub(super) fn operation_type_name(kind: &cst::OperationType) -> &'static str {
    if kind.mutation_token().is_some() {
        "Mutation"
    } else if kind.subscription_token().is_some() {
        "Subscription"
    } else {
        "Query"
    }
}

pub(super) fn record_root_types(
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

pub(super) struct FieldEntitiesInput<'a, 'source> {
    pub(super) facts: &'a mut GraphqlFacts,
    pub(super) entities: &'a mut BTreeMap<String, EntityFact>,
    pub(super) stitched_fields: &'a mut Vec<(String, String, String)>,
    pub(super) source: GraphqlSource<'source>,
    pub(super) type_name: cst::Name,
    pub(super) fields: Option<cst::FieldsDefinition>,
    pub(super) roots: &'a BTreeMap<String, &'static str>,
    pub(super) source_types: &'a BTreeMap<(String, String), String>,
}

pub(super) fn field_entities(input: FieldEntitiesInput<'_, '_>) {
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

pub(super) fn named_type_name(ty: &cst::Type) -> Option<String> {
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

pub(super) fn type_relationship(
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

pub(super) fn input_field_entities(
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

pub(super) fn type_entity(
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

pub(super) fn implements_interfaces(
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

pub(super) fn union_members(
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

pub(super) fn enum_values(
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

pub(super) fn directive_argument(
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

pub(super) fn directive_value(directive: &cst::Directive, argument_name: &str) -> Option<String> {
    let value = directive.arguments()?.arguments().find_map(|argument| {
        (argument
            .name()
            .is_some_and(|name| name.text() == argument_name))
        .then(|| argument.value())
        .flatten()
    })?;
    Some(value.syntax().text().to_string().trim_matches('"').into())
}

pub(super) fn stitched_field(
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

pub(super) fn record_source_type(
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

pub(super) fn record_field_types(
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
