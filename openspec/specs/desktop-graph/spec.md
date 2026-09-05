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

### Requirement: Investigation-driven topology

Selecting an investigation root SHALL project the snapshot to the nodes and edges
for the active context, dependencies, impact, or trace mode, replace renderer data,
and reheat the force simulation while preserving positions for retained nodes.

#### Scenario: Selecting a node

- **WHEN** a user clicks a visible node in the default context mode
- **THEN** the graph replaces its visible data with that node's direct neighborhood and reheats the simulation

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

### Requirement: Complete client projection

The client projection SHALL retain every filtered node and every relationship whose
endpoints remain visible, without applying speculative client-side size limits.

#### Scenario: Large topology snapshot

- **WHEN** filters produce a large set of nodes and links
- **THEN** the projection reports zero client omissions and leaves truncation false

### Requirement: Response-local edge identity

Client-side projection SHALL group raw relationships by `(from, to, kind)`, retain
their response-local edge IDs, count their evidence records, and keep their maximum
confidence rather than treating one response edge ID as durable identity.

#### Scenario: Same relationship arrives with a different response edge ID

- **WHEN** several raw edges describe the same endpoints and relationship kind
- **THEN** the client emits one projected link with the raw IDs, edge count, evidence count, and maximum confidence

### Requirement: Live workspace topology

The desktop graph SHALL load registered workspaces and their revision-consistent
topology through the daemon's workspace topology APIs.

#### Scenario: Opening a registered workspace

- **WHEN** the user selects a registered workspace
- **THEN** the Tauri bridge calls `workspace_topology` and returns its typed nodes, edges, and query metadata

### Requirement: Revision-state visibility

A graph snapshot SHALL expose revision, freshness, completeness, and diagnostics so
the client can report analysis state and offer a manual refresh when a newer revision exists.

#### Scenario: Snapshot is stale or incomplete

- **WHEN** the backend reports stale or incomplete graph state
- **THEN** the UI displays the state and its diagnostics without claiming truncation metadata
