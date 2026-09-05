# Query API Specification

## Purpose

Define Beholder's typed semantic query and presentation contracts from
`docs/QUERY_OUTPUT.md`, `docs/SEMANTIC_QUERY_PERFORMANCE.md`, and `docs/VISION.md`.

## Requirements

### Requirement: Query-specific typed results

`context`, `dependencies`, `impact`, `trace`, and `why` SHALL return
query-specific Beholder DTOs rather than storage rows or a generic value table.

#### Scenario: gRPC query response

- **WHEN** the daemon serves a semantic query
- **THEN** Mnestic rows stop at the storage adapter and the public response contains only typed Beholder fields

### Requirement: Versioned JSON contracts

Each JSON query document SHALL carry its query-specific `beholder.<query>.v1`
schema identifier.

#### Scenario: Requesting JSON output

- **WHEN** a user selects JSON or pretty JSON output
- **THEN** the complete typed result is serialized under the query's stable versioned schema

### Requirement: Lossless raw projection

Raw and JSON projections SHALL retain every mapped entity, edge, path, confidence
value, evidence record, and analysis-state field returned by the semantic query.

#### Scenario: Compact output collapses support nodes

- **WHEN** compact presentation hides a generated or structural support node
- **THEN** raw and JSON output still include that node and its relationships

### Requirement: Revision and completeness metadata

Every semantic result SHALL identify its workspace view, analysis revision,
freshness, completeness, diagnostics, bounds, and truncation state where applicable.

#### Scenario: New generation pending

- **WHEN** a query reads the last complete revision while newer work is pending
- **THEN** the result remains usable and reports that it is stale

### Requirement: Bounded traversals

`dependencies`, `impact`, and `trace` SHALL acquire graph frontiers up to the
requested hop limit and perform the boundary probe needed to report exact truncation.

#### Scenario: Reachable graph extends beyond the limit

- **WHEN** another matching edge exists beyond `max_hops`
- **THEN** the result stops at the limit and reports truncation

### Requirement: Responsive async daemon

Synchronous semantic database work SHALL run outside asynchronous RPC workers, and
a disconnected client SHALL release its asynchronous worker without guessing at
global query cancellation identity.

#### Scenario: Client disconnects during a long query

- **WHEN** Mnestic continues synchronous query execution after the client disconnects
- **THEN** the daemon releases the RPC task and remains responsive to unrelated requests

### Requirement: Slow-query observability

Semantic reads that exceed five seconds SHALL emit a warning on their trace while
remaining allowed to finish.

#### Scenario: Contended traversal exceeds five seconds

- **WHEN** a traversal crosses the slow-read threshold
- **THEN** telemetry records the warning without converting the threshold into a query deadline
