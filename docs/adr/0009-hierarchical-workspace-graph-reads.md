# ADR 0009: Hierarchical workspace graph reads

- Status: accepted
- Date: 2026-09-04

## Context

The complete workspace-topology response is useful for small workspaces, but it
does not scale to a multi-repository workspace with millions of lines of code.
One measured workspace required the daemon to read roughly 1.5 million
topology rows before it could construct a unary response. Sending the same raw
graph in smaller transport frames would still create every entity and edge in
the client and would leave layout as the next bottleneck.

The desktop graph therefore needs a coarse read that is cheap to transfer and
a bounded way to reveal detail. Both reads must remain revision-consistent and
must preserve unknown or external endpoints instead of failing the whole
request.

## Decision

Keep `GetWorkspaceTopology` for compatibility and add two hierarchical read
contracts:

1. `GetWorkspaceGraphOverview` returns repository communities plus one external
   community when unowned endpoints exist. It includes entity counts and
   directed, relation-specific inter-community edge counts.
2. `StreamWorkspaceGraphNeighborhood` expands one repository community,
   external community, or entity. The request defaults to 2,000 semantic edges
   and rejects limits above 10,000. The response reports truncation explicitly.

The overview is aggregated inside Mnestic. It does not materialize the complete
topology as Rust DTOs, and effective observations are evaluated once for both
community and edge counts. Repository ownership comes from the immutable
revision state, selected fact shards, and selected enrichment contributions.
Endpoints without any such ownership are grouped under
`community://external`.

Repository community identifiers are
`community://repository/<logical-repository-identity>`. They are virtual graph
nodes and must never be persisted as semantic entities.

Neighbourhood queries aggregate duplicate evidence rows into one
`(from, to, relation)` edge before applying the bound. Evidence and full entity
detail remain lazy; streamed neighbourhood edges intentionally carry no
evidence payload. Existing `Context`, `Dependencies`, `Impact`, `Trace`, and
`Why` operations remain the detail and path-finding contracts.

The daemon emits nodes before edges in batches of at most 512 items. Every
batch repeats the pinned schema, query metadata, focus, bound, and truncation
state, has a zero-based contiguous `batch_index`, and only the final batch sets
`complete`. A graph with no matching detail still emits one complete batch.
The daemon client rejects missing, out-of-order, or prematurely terminated
streams.

Both operations run through `SemanticStore::snapshot`, so a response observes
one completed analysis revision even while a newer revision is being
published.

## Consequences

- Opening a large workspace can transfer a graph proportional to repository
  count and cross-repository relationships rather than raw entity count.
- Expansions have a hard server-side response bound and can be superseded by a
  client without retaining a million-object snapshot.
- Repository ownership is the first portable hierarchy. Filesystem-module
  communities remain future work because not every language worker currently
  publishes a uniform file/module ownership fact.
- Mnestic still evaluates the selected effective observations to produce an
  overview. The new query removes row materialization and transfer costs, but
  measurement on large workspaces is required before claiming that database
  evaluation itself is fast enough.
- The first implementation produces a bounded result before emitting its
  batches. Cooperative cancellation during Mnestic evaluation is separate
  work; dropping the stream only prevents further delivery.

## Follow-up

The desktop client caches overviews and completed expansions by workspace
revision, replaces an expanded virtual node with streamed detail without
replacing the whole view, preserves the selected traversal path, and prunes
unrelated entity expansions when focus changes.

Response bytes and daemon/query/client timings should still be recorded for
both a small workspace and a large multi-repository workspace before adding
community detection, WASM layout, or more GPU work.
