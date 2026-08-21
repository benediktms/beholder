# Beholder Elixir worker

This standalone Mix application enriches Beholder's Elixir syntax graph with
facts emitted by the Elixir compiler. It implements the shared analyzer-worker
gRPC protocol and listens on the Unix socket supplied by the daemon.

The worker itself does not compile the target project. It starts a separate Mix
VM with Beholder's compiler tracer on its code path, preserves existing project
tracers, and directs compilation output to a Beholder-owned build directory.

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

Daemon registration and the manual `beholder enrich elixir` command are a
separate integration step. The current shared protocol does not yet identify
the single target repository separately from its compiler context.
