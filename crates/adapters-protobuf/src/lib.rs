use beholder_domain::{Observation, StructuralRelation};
use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorProto, FileDescriptorSet};

pub const FRONTEND_VERSION: &str = "1";

pub fn observations(descriptor_set: &[u8]) -> Result<Vec<Observation>, String> {
    let descriptor_set = FileDescriptorSet::decode(descriptor_set)
        .map_err(|error| format!("invalid FileDescriptorSet: {error}"))?;
    let mut observations = Vec::new();
    for file in descriptor_set.file {
        append_file(file, &mut observations)?;
    }
    Ok(observations)
}

fn append_file(
    file: FileDescriptorProto,
    observations: &mut Vec<Observation>,
) -> Result<(), String> {
    let path = required(file.name, "file name")?;
    let file_id = format!("proto-file://{path}");
    let package = file.package.unwrap_or_default();

    for message in file.message_type {
        append_message(&file_id, &package, message, &path, observations)?;
    }
    for r#enum in file.enum_type {
        let name = qualified(&package, required(r#enum.name, "enum name")?);
        observations.push(descriptor(
            file_id.as_str(),
            StructuralRelation::Defines,
            format!("proto-enum://{name}"),
            &path,
        ));
    }
    for service in file.service {
        let service_name = qualified(&package, required(service.name, "service name")?);
        let service_id = format!("proto-service://{service_name}");
        observations.push(descriptor(
            file_id.as_str(),
            StructuralRelation::Defines,
            service_id.as_str(),
            &path,
        ));
        for method in service.method {
            let method_name = required(method.name, "method name")?;
            let rpc_id = format!("grpc://{service_name}/{method_name}");
            observations.push(descriptor(
                service_id.as_str(),
                StructuralRelation::Defines,
                rpc_id.as_str(),
                &path,
            ));
            observations.push(descriptor(
                rpc_id.as_str(),
                StructuralRelation::RequestType,
                message_id(required(method.input_type, "method input type")?),
                &path,
            ));
            observations.push(descriptor(
                rpc_id.as_str(),
                StructuralRelation::ResponseType,
                message_id(required(method.output_type, "method output type")?),
                &path,
            ));
        }
    }
    Ok(())
}

fn append_message(
    parent_id: &str,
    scope: &str,
    message: DescriptorProto,
    path: &str,
    observations: &mut Vec<Observation>,
) -> Result<(), String> {
    let name = qualified(scope, required(message.name, "message name")?);
    let message_id = format!("proto-message://{name}");
    observations.push(descriptor(
        parent_id,
        StructuralRelation::Defines,
        message_id.as_str(),
        path,
    ));
    for field in message.field {
        let field_name = required(field.name, "field name")?;
        observations.push(descriptor(
            format!("proto-field://{name}/{field_name}"),
            StructuralRelation::FieldOf,
            message_id.as_str(),
            path,
        ));
    }
    for nested in message.nested_type {
        append_message(&message_id, &name, nested, path, observations)?;
    }
    for r#enum in message.enum_type {
        let enum_name = qualified(&name, required(r#enum.name, "enum name")?);
        observations.push(descriptor(
            message_id.as_str(),
            StructuralRelation::Defines,
            format!("proto-enum://{enum_name}"),
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
    format!("proto-message://{}", name.trim_start_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_adapters_mnestic::SemanticStore;
    use beholder_domain::{
        Confidence, LogicalRepository, Provenance, RepositoryFacts, RepositoryState,
        SemanticRelation, WorkspaceView,
    };
    use beholder_dto::{EntityKind, EvidenceKind, RelationKind};
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

        let facts = observations(&bytes).unwrap();
        let triples = facts
            .iter()
            .map(|fact| (fact.from.as_str(), fact.relation.as_str(), fact.to.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(triples.contains(&(
            "proto-field://pricing.v1.Quote/amount",
            "field_of",
            "proto-message://pricing.v1.Quote",
        )));
        assert!(triples.contains(&(
            "proto-message://pricing.v1.Quote",
            "defines",
            "proto-message://pricing.v1.Quote.Tax",
        )));
        assert!(triples.contains(&(
            "proto-message://pricing.v1.Quote",
            "defines",
            "proto-enum://pricing.v1.Quote.Status",
        )));
        assert!(triples.contains(&(
            "grpc://pricing.v1.Pricing/GetQuote",
            "request_type",
            "proto-message://pricing.v1.Quote",
        )));
        assert!(triples.contains(&(
            "grpc://pricing.v1.Pricing/GetQuote",
            "response_type",
            "proto-message://pricing.v1.Quote",
        )));
        assert!(facts.iter().all(|fact| {
            fact.confidence == Confidence::Exact
                && fact.provenance == Provenance::Descriptor
                && fact.evidence.as_str() == "pricing/v1/pricing.proto"
                && matches!(fact.relation, SemanticRelation::Structural(_))
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
    fn descriptor_facts_survive_typed_context_mapping() {
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
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "contracts".into(),
            },
            head: Some("abc123".into()),
            fingerprint: "descriptor-state".into(),
        };
        let view = WorkspaceView::new("main", "protobuf:1", vec![state.clone()]).unwrap();
        let store = SemanticStore::memory().unwrap();
        store
            .publish(
                &view,
                &[RepositoryFacts {
                    state,
                    analysis_identity: "protobuf:1".into(),
                    observations: observations(&bytes).unwrap(),
                }],
                &[],
            )
            .unwrap();

        let context = store
            .context("main", "grpc://pricing.v1.Pricing/GetQuote")
            .unwrap();
        assert_eq!(context.root.kind, EntityKind::Rpc);
        assert_eq!(context.root.name, "Pricing.GetQuote");
        assert!(context.nodes.iter().any(|node| {
            node.id == "proto-message://pricing.v1.Quote" && node.kind == EntityKind::ProtoMessage
        }));
        assert!(context.edges.iter().any(|edge| {
            edge.kind == RelationKind::RequestType
                && edge.evidence.iter().all(|evidence| {
                    evidence.source_kind == EvidenceKind::Descriptor
                        && evidence.path.as_deref() == Some("pricing.proto")
                })
        }));
    }
}
