## ADDED Requirements

### Requirement: Shared evidence-context projection

Context, dependencies, impact, trace, why, workspace topology, and graph traversal
SHALL project the same structured range and ordered context values for every
evidence record they expose. Compact presentation MAY omit those values, while
raw and JSON projections SHALL preserve them.

#### Scenario: Parallel evidence on one edge

- **WHEN** an aggregated edge has observations from different arms or callable clauses
- **THEN** every evidence-preserving typed query returns each structured evidence record without collapsing its contexts

#### Scenario: Cross-query consistency

- **WHEN** the same published edge appears in multiple typed semantic queries
- **THEN** its evidence ranges and contexts are identical across those query results

### Requirement: Evidence storage compatibility

Structured evidence SHALL round-trip through persisted fact shards without a
database migration. Existing path-only, path-and-line, and path-line-detail
evidence SHALL continue to produce the same public legacy fields. A malformed
reserved structured-evidence value SHALL be treated as legacy evidence rather
than failing the query.

#### Scenario: Reading legacy evidence

- **WHEN** a published workspace contains evidence written before structured context support
- **THEN** queries return its existing source kind, repository, path, line, and detail with no fabricated range or context

#### Scenario: Reading malformed reserved evidence

- **WHEN** stored evidence begins with the structured-evidence marker but its payload is invalid
- **THEN** the query succeeds and projects that value through the legacy evidence behavior

### Requirement: Additive protocol compatibility

Daemon and worker protocols SHALL add structured evidence fields without
renumbering or changing existing fields. The daemon protocol version SHALL
advance from 24 to 25, and clients that do not understand the added fields SHALL
remain able to use existing query behavior.

#### Scenario: Older client reads a new response

- **WHEN** a protocol-24-compatible client receives a response from a protocol-25 daemon
- **THEN** it can ignore unknown structured evidence fields and retain the existing evidence values
