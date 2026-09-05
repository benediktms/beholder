# Protobuf and gRPC Specification

## Purpose

Define transport-neutral Protobuf contracts and evidence-backed gRPC binding from
`docs/PROTOBUF_REGISTRY.md` and the corresponding sections of `docs/VISION.md`.

## Requirements

### Requirement: Explicit descriptor registration

Beholder SHALL ingest explicitly registered compiled `FileDescriptorSet` inputs
that belong to a repository in the workspace.

#### Scenario: Descriptor bytes change

- **WHEN** a registered descriptor file changes
- **THEN** its owning repository state fingerprint changes and indexing is scheduled

### Requirement: Transport-neutral Protobuf identity

Messages, enums, fields, services, and methods SHALL use ownership-neutral
`proto-type://`, `proto-field://`, `proto-service://`, and `proto-method://`
identities, with descriptor paths retained as evidence rather than identity.

#### Scenario: Two repositories use one contract

- **WHEN** both refer to the same fully qualified Protobuf method
- **THEN** they resolve to the same canonical `proto-method://` entity

### Requirement: Typed structural contract facts

The Protobuf adapter SHALL emit exact `defines`, `field_of`, `request_type`, and
`response_type` facts without leaking descriptor-library types beyond the adapter.

#### Scenario: Querying a Protobuf method

- **WHEN** context is requested for the method
- **THEN** its request and response contract facts include exact descriptor evidence

### Requirement: Evidence-gated gRPC binding

Beholder SHALL create a canonical `grpc://` operation only when service,
method, cardinality, and framework-specific client or server evidence match a
registered `proto-method://` contract.

#### Scenario: Unary client candidate matches a descriptor

- **WHEN** fully qualified service, method, and cardinality agree
- **THEN** Beholder relates the caller to the gRPC operation and binds that operation to the Protobuf method

#### Scenario: Message construction without RPC evidence

- **WHEN** source constructs a compatible Protobuf message but exposes no client, stub, server, method-path, or registration evidence
- **THEN** Beholder does not infer a gRPC operation

### Requirement: Separate implementation and registration evidence

Beholder SHALL distinguish evidence that an application symbol implements a
generated service contract from evidence that the implementation is registered or served.

#### Scenario: Service callback exists without endpoint registration

- **WHEN** compiler or framework evidence proves the callback implements the contract
- **THEN** Beholder emits `implemented_by` without claiming active server registration

### Requirement: Explicit unresolved boundaries

Unmatched contracts and unsupported cardinalities SHALL remain diagnostic
boundaries rather than guessed traversable edges.

#### Scenario: Descriptor is removed

- **WHEN** a cached application candidate no longer matches a registered method
- **THEN** it becomes unresolved and reports `grpc.contract_unmatched`
