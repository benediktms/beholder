# Protobuf registry

> [!NOTE]
> This document retains contract examples and query guidance. The current behavioral
> contract is [`protobuf-grpc`](../openspec/specs/protobuf-grpc/spec.md).

Phase 3 adds canonical Protobuf contract facts. Phase 5 resolves application
gRPC bindings against those facts at workspace publication time.

```text
FileDescriptorSet bytes
        |
        v
beholder-adapters-protobuf
        |
        v
Beholder domain observations
        |
        v
Mnestic repository-state facts
```

The Protobuf adapter owns descriptor decoding. `prost` types do not cross its
crate boundary, and Mnestic remains unaware of descriptors.

Canonical entities use ownership-neutral IDs:

- `proto-type://<fully qualified name>` with message or enum metadata
- `proto-field://<fully qualified message>/<field name>`
- `proto-service://<fully qualified service>`
- `proto-method://<fully qualified service>/<method>` with RPC cardinality metadata

Descriptor paths remain evidence rather than entity identity.

The adapter emits `defines`, `field_of`, `request_type`, and `response_type` as
structural facts with exact descriptor evidence. They are available to typed
queries but do not become dependency-traversal edges.

Register compiled descriptor sets explicitly:

```text
beholder workspace register platform /code/contracts /code/service \
  --protobuf-descriptor /code/contracts/platform.descriptor.bin
```

Each descriptor must be inside one of the registered repositories. Its bytes
participate in that repository state's fingerprint, and filesystem changes
trigger reindexing.

## gRPC resolution

Unary client and server candidates from Rust tonic and Elixir GRPC are joined
to a matching Protobuf method by fully qualified service name, method name, and
cardinality. The resulting graph is independent of repository ownership:

```text
repo://example/client/rust/checkout/quote
        | calls_rpc
        v
grpc://pricing.v1.Pricing/GetQuote
        | binds_contract
        +--> proto-method://pricing.v1.Pricing/GetQuote
        |
        | implemented_by
        v
repo://example/server/elixir/Pricing.Server/get_quote/2
```

The immediate stub caller owns `calls_rpc`; helper callers reach it through
ordinary `calls` edges. Server registration and RPC implementation remain
separate facts. Generated evidence is exact confidence `1.0`; recognized source
shapes are inferred confidence `0.6`. Corroborating evidence is retained while
the strongest confidence becomes the edge confidence.

Only unary bindings resolve in Phase 5. Missing contracts produce
`grpc.contract_unmatched`; unsupported cardinalities produce
`grpc.cardinality_unsupported`. Adapter diagnostics describe dynamic or
unrecognized framework shapes instead of guessing a relationship. Removing a
descriptor republishes application candidates as unresolved; restoring it
resolves them from cached application analysis.

Query either side of the boundary:

```text
beholder context --workspace platform grpc://pricing.v1.Pricing/GetQuote
beholder trace --workspace platform <client-entity> <server-entity>
beholder why --workspace platform <client-entity> <server-entity>
beholder impact --workspace platform proto-method://pricing.v1.Pricing/GetQuote
```

Compact output may collapse generated supporting symbols. Use `--raw` for the
full graph and evidence, or `--json`/`--json-pretty` for stable versioned output
including repository attribution, revision, and freshness.
