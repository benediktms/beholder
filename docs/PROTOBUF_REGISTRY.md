# Protobuf registry

Phase 3 adds canonical Protobuf contract facts without resolving application
code to those contracts.

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

- `proto-file://<descriptor path>`
- `proto-message://<fully qualified name>`
- `proto-field://<fully qualified message>/<field name>`
- `proto-enum://<fully qualified name>`
- `proto-service://<fully qualified service>`
- `grpc://<fully qualified service>/<method>`

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
trigger reindexing. Resolving generated clients, servers, or application calls
to canonical RPCs is explicitly outside Phase 3.
