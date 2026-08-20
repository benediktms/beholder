# Local observability

`beholderd` can export its existing structured spans and events as OpenTelemetry
traces and logs over OTLP/HTTP. Export is opt-in: local rolling JSON logs remain
the only sink unless an OTLP endpoint is configured.

## otel-gui

Install and start [otel-gui](https://github.com/metafab/otel-gui):

```sh
brew install metafab/tap/otel-gui
otel-gui
```

Then restart Beholder with the standard OTLP endpoint variable. A manually
started daemon inherits the variable from the shell:

```sh
beholder daemon stop
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 beholder daemon start
```

`just install` enables the installed release daemon with
`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318` by default and persists the
supported observability variables in its launchd or systemd user service. Set a
shared endpoint before installing to override that default.

Open <http://localhost:4318>. RPC operations and background workspace indexing
appear as traces; structured `tracing` events appear in the Logs view and carry
trace/span correlation when emitted inside a span.

The rolling JSON logs use the same correlation context. Events emitted inside
an instrumented span include top-level `trace_id` and `span_id` fields, allowing
local tooling to find the corresponding trace without querying the OTLP log
store. Events outside a span omit both fields.

The Rust analyzer worker exports as the separate `beholder-worker-rust`
service. Its analysis spans are linked to the daemon trace through W3C trace
context propagated over the local worker gRPC request.

Use `beholder daemon run` instead of `start` when foreground output is useful.

## Configuration

Beholder uses the standard OpenTelemetry environment variables:

| Variable | Purpose |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Export both traces and logs. For otel-gui, use `http://localhost:4318`. |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Enable only traces with a signal-specific endpoint, normally ending in `/v1/traces`. |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | Enable only logs with a signal-specific endpoint, normally ending in `/v1/logs`. |
| `OTEL_SERVICE_NAME` | Override the default `beholderd` service name. |
| `OTEL_SDK_DISABLED=true` | Disable OTLP export even when an endpoint is present. |
| `RUST_LOG` | Set the filter shared by local logs, exported logs, and spans. |

The exporter uses OTLP/HTTP with binary protobuf payloads, which otel-gui accepts
at `/v1/traces` and `/v1/logs`. Exporter transport diagnostics are excluded from
OTLP logs to prevent telemetry feedback loops. Provider shutdown flushes queued
spans and logs before the daemon exits.
