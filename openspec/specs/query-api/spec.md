# Query API Specification

## Purpose

Define Beholder's typed semantic query and presentation contracts from
`docs/QUERY_OUTPUT.md`, `docs/SEMANTIC_QUERY_PERFORMANCE.md`, and `docs/VISION.md`.

## Requirements

### Requirement: Query-specific typed results

`entity search`, `traverse_graph`, `workspace topology`, `context`, `dependencies`, `impact`, `trace`,
and `why` SHALL return query-specific Beholder DTOs rather than storage rows or a
generic value table.

#### Scenario: gRPC query response

- **WHEN** the daemon serves a semantic query
- **THEN** Mnestic rows stop at the storage adapter and the public response contains only typed Beholder fields

### Requirement: Versioned query contracts

Each typed query response SHALL carry its query-specific schema identifier:
`beholder.entity_search.v1`, `beholder.traverse_graph.v1`, `beholder.workspace_topology.v1`,
`beholder.context.v1`, `beholder.dependencies.v2`, `beholder.impact.v2`,
`beholder.trace.v2`, or `beholder.why.v2`.

#### Scenario: Requesting JSON output

- **WHEN** a user selects JSON or pretty JSON output for `context`, `dependencies`, `impact`, `trace`, or `why`
- **THEN** the complete typed result is serialized under that query's current stable schema

#### Scenario: Consumer searches entities over gRPC

- **WHEN** a consumer calls `SearchEntities`
- **THEN** the typed gRPC response carries `beholder.entity_search.v1` without promising a CLI JSON renderer

### Requirement: Bounded entity search

Entity search SHALL reject an empty trimmed query, default to 20 matches when no
limit is supplied, and accept explicit limits from 1 through 100.

#### Scenario: Searching without an explicit limit

- **WHEN** a consumer searches for a non-empty entity query without a limit
- **THEN** the daemon returns at most 20 matches under `beholder.entity_search.v1`

### Requirement: Lossless JSON and evidence-oriented raw projection

JSON projection SHALL retain every typed query field. Raw projection SHALL retain
schema, revision, core freshness and completeness state, graph topology, paths,
confidence, and evidence while remaining a human-oriented subset of the DTO.

#### Scenario: Compact output collapses support nodes

- **WHEN** compact presentation hides a generated or structural support node
- **THEN** raw and JSON output still include that node, its relationships, and evidence

#### Scenario: Caller needs every DTO field

- **WHEN** a caller needs entity names, repositories, typed metadata, or complete freshness details
- **THEN** it uses JSON because raw output does not promise every DTO field

### Requirement: Revision and completeness metadata

Every semantic result SHALL identify its workspace view, analysis revision,
freshness, completeness, diagnostics, bounds, and truncation state where applicable.

#### Scenario: New generation pending

- **WHEN** a query reads the last atomically published revision while newer work is pending
- **THEN** the result remains usable and reports that it is stale

### Requirement: Bounded traversals

`dependencies`, `impact`, `trace`, and `why` SHALL acquire graph frontiers up to the
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


### Requirement: Bounded multi-path graph traversal

`TraverseGraph` SHALL accept workspace, start canonical entity ID, direction
(`dependencies` or `dependents`), optional destination canonical entity ID,
optional max_hops (default 8, range 0–32), and optional max_paths (default 50,
range 1–200). Empty identifiers and unspecified or unknown directions SHALL be
rejected. Its schema SHALL be `beholder.traverse_graph.v1`.

The response SHALL contain QueryMetadata, the query with applied hop/path limits,
shared EntityRef nodes and evidence-preserving SemanticEdge values, ordered paths,
and traversal metadata listing every applied bound and explicit truncation reasons.
Every path SHALL contain ordered node and edge IDs and terminate as `destination`,
`leaf`, `cycle`, or `max_hops`. A simple path SHALL never repeat an entity.

#### Scenario: Open-ended exploration

- **WHEN** no destination is supplied
- **THEN** bounded maximal simple paths are returned, preserving branches and convergence
- **AND** a path ends as `cycle` only when it has successors but every successor is already on that path
- **AND** the cycle-closing edge remains in the shared graph without repeating a path entity

#### Scenario: Destination exploration

- **WHEN** a destination is supplied
- **THEN** every simple directed path reaching it within the limits is returned until the path or work cap is reached
- **AND** the destination is not expanded, including a zero-edge path when start equals destination
- **AND** dead ends and depth-limited prefixes are not returned as destination paths

#### Scenario: Deterministic graph and evidence

- **WHEN** the same published graph is stored in a different row order
- **THEN** path ordering and shared graph output remain identical
- **AND** evidence is merged by source, target, and relation without losing parallel evidence
- **AND** the shared graph includes acquired edges between returned path nodes, including parallel and cycle-closing edges
- **AND** dependents traversal preserves the special forward gRPC `implemented_by` step

#### Scenario: Work exhaustion

- **WHEN** acquisition exceeds 10,000 evidence rows or enumeration exceeds 100,000 node/adjacency steps
- **THEN** the result reports `acquisition_limit` or `work_limit` truncation respectively
- **AND** an incompletely acquired frontier is never presented as a leaf or cycle termination
- **AND** an edge whose evidence crosses the acquisition cap is omitted rather than partially reported
- **AND** depth and path-count truncation report `max_hops` and `max_paths`; an exactly full path result is not truncated unless another path exists or another bound prevents completion

#### Scenario: Acquisition timeout

- **WHEN** graph acquisition exceeds its shared five-second budget
- **THEN** Mnestic interrupts the query and the RPC reports a deadline error
- **AND** this new operation does not change legacy semantic queries' slow-warning behavior

#### Scenario: Publication during traversal

- **WHEN** a new revision is published between acquisition and hydration
- **THEN** graph data, entity facts, revision, completeness, and diagnostics still come from the same immutable read snapshot

#### Scenario: Existing consumers

- **WHEN** a consumer calls trace, why, dependencies, or impact through the daemon or CLI
- **THEN** its existing schema, defaults, and shortest-path or reachability behavior remain compatible
- **AND** both APIs share indexed resolved-dependency reads and semantic graph/evidence construction
