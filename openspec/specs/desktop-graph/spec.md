# Desktop Workspace Graph Specification

## Purpose

Define the accepted desktop graph prototype and its production boundary from
`docs/adr/0008-desktop-workspace-graph-prototype.md`.

## Requirements

### Requirement: Typed graph projection

The desktop graph SHALL project Beholder's existing typed entity, semantic-edge,
evidence, traversal, and query-state DTOs rather than define a second semantic model.

#### Scenario: Loading the prototype fixture

- **WHEN** the Tauri bridge returns a graph snapshot
- **THEN** the frontend retains the raw DTO arrays alongside its renderer projection

### Requirement: Stable topology during interaction

Selection, hover, zoom, and pan SHALL NOT change visible graph topology, replace
renderer data, reheat the force simulation, or move the camera implicitly.

#### Scenario: Selecting a node

- **WHEN** a user clicks a visible node
- **THEN** node and link identities and positions remain stable while its direct neighborhood is highlighted

### Requirement: Directional neighborhood highlighting

The selected node SHALL have a red halo, upstream neighbors teal halos, downstream
neighbors orange halos, and incident visible links animated from source to target.

#### Scenario: Clearing focus

- **WHEN** the user clicks the background or `Clear focus`
- **THEN** persistent selection highlights and particles are removed

### Requirement: Minimal deterministic filters

The prototype SHALL filter by repository, relationship kind, test inclusion, and
origin while retaining nodes that pass node filters even if edge filters disconnect them.

#### Scenario: Excluding one relationship kind

- **WHEN** a relationship kind is disabled
- **THEN** matching links disappear without dropping otherwise visible isolated nodes

### Requirement: Honest visible guards

The prototype SHALL cap a projection at 10,000 visible nodes, 25,000 visible links,
and 250 animated incident links, choosing omissions deterministically and reporting
their counts.

#### Scenario: Projection exceeds a guard

- **WHEN** filters produce more elements than a renderer guard permits
- **THEN** the UI keeps a deterministic subset and asks the user to narrow the available filters

### Requirement: Response-local edge identity

Client-side merges SHALL identify a raw relationship by `(from, to, kind)` and merge
evidence rather than treating response-local edge IDs as durable identity.

#### Scenario: Same relationship arrives with a different response edge ID

- **WHEN** two query results describe the same endpoints and relationship kind
- **THEN** the client merges their evidence into one projected link

### Requirement: Fixture-bounded prototype

The prototype SHALL remain fixture-backed until a bounded, revision-consistent
workspace topology API exists; it SHALL expose this limitation rather than infer a
whole workspace graph through repeated context calls.

#### Scenario: No topology RPC exists

- **WHEN** the user opens the current prototype
- **THEN** it uses realistic product-neutral fixture data and does not issue an N+1 query sequence

### Requirement: Revision-state visibility

A production graph snapshot SHALL expose revision, freshness, completeness,
diagnostics, and truncation so the client can reject unsafe merges and display warnings.

#### Scenario: Snapshot is stale or truncated

- **WHEN** the backend reports stale or incomplete graph state
- **THEN** the UI displays that state without presenting the projection as complete
