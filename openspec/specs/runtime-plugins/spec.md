# Runtime Analyzer Plugins Specification

## Purpose

Define organization- and framework-specific recognition from
`docs/adr/0005-runtime-analyzer-plugins.md` without opening Beholder's ontology or
parser internals.

## Requirements

### Requirement: One executable plugin boundary

Runtime analyzer plugins SHALL be trusted native executables using Beholder's
versioned analyzer gRPC protocol and SHALL NOT load into the daemon process.

#### Scenario: Running plugin analysis

- **WHEN** a plugin job starts
- **THEN** the daemon owns a job-scoped child process and private local socket and allows the process to exit after one contribution

### Requirement: Declarative discovery

Each plugin SHALL describe its stable ID, exact API version, target selectors,
required baseline kinds, and permitted output kinds before activation. Context
selectors are optional.

#### Scenario: Plugin emits an undeclared relationship kind

- **WHEN** a contribution contains output outside its validated descriptor
- **THEN** the daemon rejects the contribution

### Requirement: Minimal immutable inputs

A plugin SHALL receive only its selected target files, read-only context files, and
declared baseline semantic facts, in bounded chunks.

#### Scenario: Two plugins are enabled

- **WHEN** the second plugin runs
- **THEN** its semantic input excludes the first plugin's mutable contribution

### Requirement: Deterministic independent composition

Plugin contributions SHALL be owner-scoped, order-independent, and identified by
current baseline inputs, selected context inputs, plugin ID, executable digest, and
plugin API version.

#### Scenario: Installed executable is replaced

- **WHEN** the executable digest changes for an enabled plugin
- **THEN** prior output becomes stale and the new contribution cannot reuse the old executable identity

### Requirement: Layered plugin output validation

The Rust plugin SDK SHALL check canonical entity addresses and relationship
compatibility. For plugins that implement the protocol directly, the daemon SHALL
check declared output kinds and endpoint membership before publication.

#### Scenario: Plugin references an invalid endpoint

- **WHEN** an output edge refers to an endpoint absent from baseline and contributed entities
- **THEN** daemon validation rejects it before publication

#### Scenario: Protocol-native plugin bypasses the Rust SDK

- **WHEN** a plugin emits a declared kind with a malformed canonical address or incompatible relationship
- **THEN** current daemon validation may accept it if its kinds are declared and its endpoints are known

### Requirement: Explicit administrative installation

Plugin installation, replacement, removal, enablement, and disablement SHALL be
explicit local administrative actions; repository contents SHALL NOT install or
activate executables.

#### Scenario: Registered plugin executable disappears

- **WHEN** an enabled registered plugin has no valid managed executable
- **THEN** the daemon omits that enricher, serves the baseline graph, and emits a startup warning

### Requirement: Public Rust authoring SDK

Beholder SHALL provide a documented Rust SDK that owns transport, request assembly,
cancellation, trace propagation, output validation, and graceful shutdown while
exposing one synchronous analyzer callback.

#### Scenario: Plugin performs CPU-bound recognition

- **WHEN** a complete request has arrived
- **THEN** the SDK runs the analyzer callback on a blocking worker rather than a Tokio runtime thread

### Requirement: Automatic telemetry propagation

The plugin SDK SHALL propagate the daemon trace context and identify plugin analysis
under a distinct OpenTelemetry service identity.

#### Scenario: Inspecting a plugin job trace

- **WHEN** tracing export is configured
- **THEN** the plugin attempt is connected to its daemon enqueue and execution trace
