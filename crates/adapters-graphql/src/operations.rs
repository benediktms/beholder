use super::{GraphqlFacts, GraphqlSource, evidence};
use crate::schema::{operation_type_name, type_relationship};
use apollo_parser::cst;
use beholder_domain::{
    DependencyRelation, EntityFact, EntityKind, EntityMetadata, GraphqlOperationKind, Observation,
    StructuralRelation,
};
use std::collections::{BTreeMap, BTreeSet};

fn root_type(operation: &cst::OperationDefinition) -> &'static str {
    let Some(kind) = operation.operation_type() else {
        return "Query";
    };
    operation_type_name(&kind)
}

pub(super) fn operation_facts(
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
