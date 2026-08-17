use beholder_domain::{
    EntityFact, EntityKind, EntityMetadata, Observation, ProtoTypeKind, RpcCardinality,
    StructuralRelation,
};
use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorProto, FileDescriptorSet};

pub const FRONTEND_VERSION: &str = "1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtobufFacts {
    pub entities: Vec<EntityFact>,
    pub observations: Vec<Observation>,
}

pub fn facts(descriptor_set: &[u8]) -> Result<ProtobufFacts, String> {
    let descriptor_set = FileDescriptorSet::decode(descriptor_set)
        .map_err(|error| format!("invalid FileDescriptorSet: {error}"))?;
    let mut facts = ProtobufFacts {
        entities: Vec::new(),
        observations: Vec::new(),
    };
    for file in descriptor_set.file {
        append_file(file, &mut facts)?;
    }
    Ok(facts)
}

pub fn observations(descriptor_set: &[u8]) -> Result<Vec<Observation>, String> {
    Ok(facts(descriptor_set)?.observations)
}

fn append_file(file: FileDescriptorProto, facts: &mut ProtobufFacts) -> Result<(), String> {
    let path = required(file.name, "file name")?;
    let package = file.package.unwrap_or_default();

    for message in file.message_type {
        append_message(None, &package, message, &path, facts)?;
    }
    for r#enum in file.enum_type {
        let name = qualified(&package, required(r#enum.name, "enum name")?);
        push_entity(
            facts,
            format!("proto-type://{name}"),
            EntityKind::ProtoType,
            Some(EntityMetadata::ProtoType {
                kind: ProtoTypeKind::Enum,
            }),
        )?;
    }
    for service in file.service {
        let service_name = qualified(&package, required(service.name, "service name")?);
        let service_id = format!("proto-service://{service_name}");
        push_entity(facts, service_id.as_str(), EntityKind::ProtoService, None)?;
        for method in service.method {
            let cardinality = match (method.client_streaming(), method.server_streaming()) {
                (false, false) => RpcCardinality::Unary,
                (true, false) => RpcCardinality::ClientStreaming,
                (false, true) => RpcCardinality::ServerStreaming,
                (true, true) => RpcCardinality::BidirectionalStreaming,
            };
            let method_name = required(method.name, "method name")?;
            let method_id = format!("proto-method://{service_name}/{method_name}");
            push_entity(
                facts,
                method_id.as_str(),
                EntityKind::ProtoMethod,
                Some(EntityMetadata::ProtoMethod { cardinality }),
            )?;
            facts.observations.push(descriptor(
                service_id.as_str(),
                StructuralRelation::Defines,
                method_id.as_str(),
                &path,
            ));
            facts.observations.push(descriptor(
                method_id.as_str(),
                StructuralRelation::RequestType,
                message_id(required(method.input_type, "method input type")?),
                &path,
            ));
            facts.observations.push(descriptor(
                method_id.as_str(),
                StructuralRelation::ResponseType,
                message_id(required(method.output_type, "method output type")?),
                &path,
            ));
        }
    }
    Ok(())
}

fn append_message(
    parent_id: Option<&str>,
    scope: &str,
    message: DescriptorProto,
    path: &str,
    facts: &mut ProtobufFacts,
) -> Result<(), String> {
    let name = qualified(scope, required(message.name, "message name")?);
    let message_id = format!("proto-type://{name}");
    push_entity(
        facts,
        message_id.as_str(),
        EntityKind::ProtoType,
        Some(EntityMetadata::ProtoType {
            kind: ProtoTypeKind::Message,
        }),
    )?;
    if let Some(parent_id) = parent_id {
        facts.observations.push(descriptor(
            parent_id,
            StructuralRelation::Defines,
            message_id.as_str(),
            path,
        ));
    }
    for field in message.field {
        let field_name = required(field.name, "field name")?;
        let field_id = format!("proto-field://{name}/{field_name}");
        push_entity(facts, field_id.as_str(), EntityKind::ProtoField, None)?;
        facts.observations.push(descriptor(
            field_id,
            StructuralRelation::FieldOf,
            message_id.as_str(),
            path,
        ));
    }
    for nested in message.nested_type {
        append_message(Some(&message_id), &name, nested, path, facts)?;
    }
    for r#enum in message.enum_type {
        let enum_name = qualified(&name, required(r#enum.name, "enum name")?);
        let enum_id = format!("proto-type://{enum_name}");
        push_entity(
            facts,
            enum_id.as_str(),
            EntityKind::ProtoType,
            Some(EntityMetadata::ProtoType {
                kind: ProtoTypeKind::Enum,
            }),
        )?;
        facts.observations.push(descriptor(
            message_id.as_str(),
            StructuralRelation::Defines,
            enum_id,
            path,
        ));
    }
    Ok(())
}

fn descriptor(
    from: impl Into<beholder_domain::EntityId>,
    relation: StructuralRelation,
    to: impl Into<beholder_domain::EntityId>,
    path: &str,
) -> Observation {
    Observation::descriptor(from, relation, to, path)
}

fn required(value: Option<String>, name: &str) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("descriptor {name} is missing"))
}

fn qualified(scope: &str, name: String) -> String {
    if scope.is_empty() {
        name
    } else {
        format!("{scope}.{name}")
    }
}

fn message_id(name: String) -> String {
    format!("proto-type://{}", name.trim_start_matches('.'))
}

fn push_entity(
    facts: &mut ProtobufFacts,
    id: impl Into<beholder_domain::EntityId>,
    kind: EntityKind,
    metadata: Option<EntityMetadata>,
) -> Result<(), String> {
    let entity = EntityFact::new(id, kind, metadata).map_err(str::to_owned)?;
    if !facts.entities.contains(&entity) {
        facts.entities.push(entity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{Confidence, Provenance, SemanticRelation};
    use prost_types::{
        EnumDescriptorProto, FieldDescriptorProto, MethodDescriptorProto, ServiceDescriptorProto,
    };
    use std::collections::BTreeSet;

    #[test]
    fn decodes_canonical_contract_facts() {
        let bytes = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("pricing/v1/pricing.proto".into()),
                package: Some("pricing.v1".into()),
                message_type: vec![DescriptorProto {
                    name: Some("Quote".into()),
                    field: vec![FieldDescriptorProto {
                        name: Some("amount".into()),
                        ..Default::default()
                    }],
                    nested_type: vec![DescriptorProto {
                        name: Some("Tax".into()),
                        ..Default::default()
                    }],
                    enum_type: vec![EnumDescriptorProto {
                        name: Some("Status".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                service: vec![ServiceDescriptorProto {
                    name: Some("Pricing".into()),
                    method: vec![MethodDescriptorProto {
                        name: Some("GetQuote".into()),
                        input_type: Some(".pricing.v1.Quote".into()),
                        output_type: Some(".pricing.v1.Quote".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec();

        let facts = facts(&bytes).unwrap();
        let triples = facts
            .observations
            .iter()
            .map(|fact| (fact.from.as_str(), fact.relation.as_str(), fact.to.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(triples.contains(&(
            "proto-field://pricing.v1.Quote/amount",
            "field_of",
            "proto-type://pricing.v1.Quote",
        )));
        assert!(triples.contains(&(
            "proto-type://pricing.v1.Quote",
            "defines",
            "proto-type://pricing.v1.Quote.Tax",
        )));
        assert!(triples.contains(&(
            "proto-type://pricing.v1.Quote",
            "defines",
            "proto-type://pricing.v1.Quote.Status",
        )));
        assert!(triples.contains(&(
            "proto-method://pricing.v1.Pricing/GetQuote",
            "request_type",
            "proto-type://pricing.v1.Quote",
        )));
        assert!(triples.contains(&(
            "proto-method://pricing.v1.Pricing/GetQuote",
            "response_type",
            "proto-type://pricing.v1.Quote",
        )));
        assert!(facts.observations.iter().all(|fact| {
            fact.confidence == Confidence::Exact
                && fact.provenance == Provenance::Descriptor
                && fact.evidence.as_str() == "pricing/v1/pricing.proto"
                && matches!(fact.relation, SemanticRelation::Structural(_))
        }));
        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "proto-type://pricing.v1.Quote"
                && entity.metadata
                    == Some(EntityMetadata::ProtoType {
                        kind: ProtoTypeKind::Message,
                    })
        }));
        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "proto-method://pricing.v1.Pricing/GetQuote"
                && entity.metadata
                    == Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::Unary,
                    })
        }));
    }

    #[test]
    fn rejects_missing_descriptor_identity() {
        let bytes = FileDescriptorSet {
            file: vec![FileDescriptorProto::default()],
        }
        .encode_to_vec();

        assert_eq!(
            observations(&bytes).unwrap_err(),
            "descriptor file name is missing"
        );
    }

    #[test]
    fn indexes_streaming_method_cardinality() {
        let bytes = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("events.proto".into()),
                package: Some("events.v1".into()),
                service: vec![ServiceDescriptorProto {
                    name: Some("Events".into()),
                    method: vec![MethodDescriptorProto {
                        name: Some("Sync".into()),
                        input_type: Some(".events.v1.Event".into()),
                        output_type: Some(".events.v1.Event".into()),
                        client_streaming: Some(true),
                        server_streaming: Some(true),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec();

        let facts = facts(&bytes).unwrap();
        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "proto-method://events.v1.Events/Sync"
                && entity.metadata
                    == Some(EntityMetadata::ProtoMethod {
                        cardinality: RpcCardinality::BidirectionalStreaming,
                    })
        }));
    }

    #[test]
    fn descriptor_facts_use_transport_neutral_contract_ids() {
        let bytes = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("pricing.proto".into()),
                package: Some("pricing.v1".into()),
                message_type: vec![DescriptorProto {
                    name: Some("Quote".into()),
                    ..Default::default()
                }],
                service: vec![ServiceDescriptorProto {
                    name: Some("Pricing".into()),
                    method: vec![MethodDescriptorProto {
                        name: Some("GetQuote".into()),
                        input_type: Some(".pricing.v1.Quote".into()),
                        output_type: Some(".pricing.v1.Quote".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec();
        let facts = facts(&bytes).unwrap();
        assert!(facts.entities.iter().any(|entity| {
            entity.id.as_str() == "proto-method://pricing.v1.Pricing/GetQuote"
                && entity.kind == EntityKind::ProtoMethod
        }));
        assert!(
            facts
                .entities
                .iter()
                .all(|entity| !entity.id.as_str().starts_with("grpc://"))
        );
        assert!(facts.observations.iter().any(|observation| {
            observation.from.as_str() == "proto-method://pricing.v1.Pricing/GetQuote"
                && observation.relation
                    == SemanticRelation::Structural(StructuralRelation::RequestType)
                && observation.to.as_str() == "proto-type://pricing.v1.Quote"
                && observation.evidence.as_str() == "pricing.proto"
        }));
    }
}
