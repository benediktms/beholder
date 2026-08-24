# Beholder Elixir worker

This standalone Mix application enriches Beholder's Elixir syntax graph with
facts emitted by the Elixir compiler. It implements the shared analyzer-worker
gRPC protocol and listens on the Unix socket supplied by the daemon.

The worker materializes the declared target and context inputs into an isolated
temporary workspace, then starts a separate Mix VM with Beholder's compiler
tracer on its code path. It preserves existing project tracers and directs
dependencies and compilation output to Beholder-owned directories. Live checkout
source, Mix manifests, lockfiles and compile-time configuration are not used. The
child VM inherits `MIX_HOME` and `HEX_HOME`, including configured private Hex
repositories, while dependency sources remain in Beholder's cache.

When `OTEL_EXPORTER_OTLP_ENDPOINT` or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is configured, the worker exports OTLP/HTTP
traces as `beholder-worker-elixir`. It extracts W3C trace context from the local
gRPC request so compiler analysis appears beneath the daemon's worker span.

## Development

From this directory:

```sh
mix deps.get
mix protobuf.generate \
  --include-path=../../proto \
  --output-path=lib/beholder/proto \
  --plugin=ProtobufGenerate.Plugins.GRPCWithOptions \
  ../../proto/beholder/v1/daemon.proto \
  ../../proto/beholder/worker/v1/worker.proto
mix format
mix test
mix escript.build
```

The generated protobuf modules are checked in so normal builds do not require
the generator plugin.

`just install` builds and installs the worker alongside the Rust worker. A
custom executable can be selected with `BEHOLDER_ELIXIR_WORKER_PATH`; when no
installed worker is present, the daemon continues with Elixir compiler
enrichment disabled.
