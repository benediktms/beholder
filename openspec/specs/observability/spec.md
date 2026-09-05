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
identifiers in local logs when trace export is enabled and provides a valid
OpenTelemetry span context.

#### Scenario: Event occurs outside a span

- **WHEN** trace export is disabled or no active correlation context exists
- **THEN** local output omits `trace_id` and `span_id` rather than fabricating them

### Requirement: End-to-end job traces

Background job attempts and analyzer workers SHALL continue the enqueue trace through
W3C trace context. The daemon and workers SHALL use their distinct default service
names unless `OTEL_SERVICE_NAME` globally overrides them.

#### Scenario: Rust worker analyzes an indexing result

- **WHEN** OTLP export is configured without a global service-name override
- **THEN** its `beholder-worker-rust` spans connect to the originating `beholderd` trace

### Requirement: Outcome-based severity

Job telemetry severity SHALL reflect material outcomes: expected coalescing and
supersession are not errors, retryable failures are warnings, and attempt-path
terminal failures are errors. Crash recovery currently logs even terminally killed
jobs as warnings.

#### Scenario: Automatic job is superseded

- **WHEN** a newer generation replaces queued work
- **THEN** telemetry records the normal outcome without error severity

#### Scenario: Final attempt is consumed during crash recovery

- **WHEN** daemon restart marks the interrupted job terminally killed
- **THEN** recovery records the terminal failure count and emits a warning

### Requirement: Structured job telemetry

Completed job and worker attempt paths SHALL use structured identifiers, counts,
timings, outcomes, attempt information, and trace correlation. Failure events
currently preserve the upstream error display without a size or content bound.
A blocking task panic or cancellation MAY return before outcome and error fields are
recorded.

#### Scenario: Reporting a failed attempt

- **WHEN** a job attempt returns a failure result
- **THEN** its event identifies the job kind, stable target, attempt, outcome, duration, and upstream error text

#### Scenario: Blocking task panics or is cancelled

- **WHEN** awaiting blocking work returns a join error before an attempt result exists
- **THEN** the span may close without a structured outcome or upstream-error event

### Requirement: Flush on shutdown

OpenTelemetry providers SHALL flush queued spans and logs during graceful daemon
shutdown, and exporter diagnostics SHALL be excluded from exported logs to prevent
feedback loops.

#### Scenario: Daemon stops normally

- **WHEN** shutdown completes
- **THEN** queued telemetry is flushed before provider termination
