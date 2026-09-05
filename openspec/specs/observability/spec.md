# Observability Specification

## Purpose

Define local logs, OpenTelemetry export, and correlated worker telemetry from
`docs/OBSERVABILITY.md` and the observability contracts in the worker and job ADRs.

## Requirements

### Requirement: Local logs by default

The daemon SHALL write structured rolling JSON logs without requiring an
OpenTelemetry collector.

#### Scenario: No OTLP endpoint configured

- **WHEN** the daemon starts without an OpenTelemetry export endpoint
- **THEN** local rolling logs remain the only telemetry sink

### Requirement: Standard opt-in OTLP export

The daemon SHALL export traces and logs over OTLP/HTTP only when enabled through
standard OpenTelemetry environment variables.

#### Scenario: Shared OTLP endpoint configured

- **WHEN** `OTEL_EXPORTER_OTLP_ENDPOINT` is set
- **THEN** the daemon exports binary Protobuf traces and logs to the signal endpoints

#### Scenario: SDK explicitly disabled

- **WHEN** `OTEL_SDK_DISABLED=true`
- **THEN** no OTLP export occurs even if an endpoint is configured

### Requirement: Correlated local and exported events

Structured events emitted inside an instrumented span SHALL include trace and span
identifiers in local logs and exported telemetry.

#### Scenario: Event occurs outside a span

- **WHEN** no active correlation context exists
- **THEN** local output omits `trace_id` and `span_id` rather than fabricating them

### Requirement: End-to-end job traces

Background job attempts and analyzer workers SHALL continue the enqueue trace through
W3C trace context while using distinct service names for the daemon and workers.

#### Scenario: Rust worker analyzes an indexing result

- **WHEN** OTLP export is configured
- **THEN** its `beholder-worker-rust` spans connect to the originating `beholderd` trace

### Requirement: Outcome-based severity

Job telemetry severity SHALL reflect material outcomes: expected coalescing and
supersession are not errors, retryable failures are warnings, and terminal failures
are errors.

#### Scenario: Automatic job is superseded

- **WHEN** a newer generation replaces queued work
- **THEN** telemetry records the normal outcome without error severity

### Requirement: Bounded telemetry fields

Job and worker telemetry SHALL use stable identifiers, counts, timings, outcomes,
attempt information, and trace correlation without logging source contents or
unbounded payloads.

#### Scenario: Reporting a failed attempt

- **WHEN** a job attempt fails
- **THEN** its event identifies the job kind, stable target, attempt, outcome, duration, and bounded error information

### Requirement: Flush on shutdown

OpenTelemetry providers SHALL flush queued spans and logs during graceful daemon
shutdown, and exporter diagnostics SHALL be excluded from exported logs to prevent
feedback loops.

#### Scenario: Daemon stops normally

- **WHEN** shutdown completes
- **THEN** queued telemetry is flushed before provider termination
