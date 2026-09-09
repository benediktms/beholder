## Context

See `proposal.md` for motivation and `specs/query-api/spec.md` for the public
contract. The MCP server only forwards typed daemon requests; the expensive work
is in Mnestic. Current shard search joins every selected shard to its entities,
computes display names with regular expressions, and then ranks matches. Snapshot
metadata also loads every diagnostic row. Traversal already has indexed forward
and reverse resolved-dependency access plus shared acquisition limits.

The existing performance record in `docs/SEMANTIC_QUERY_PERFORMANCE.md` requires
an `EXPLAIN` result and measured index build cost before retaining a new index.
Mnestic storage types must remain inside the adapter.

## Goals / Non-Goals

**Goals:**

- Bind search and traversal predicates before expensive row acquisition.
- Preserve one immutable read snapshot and every existing traversal bound.
- Keep protobuf changes additive while explicitly versioning changed result DTOs.
- Reuse current dependency indexes and garbage-collection machinery.

**Non-Goals:**

- General substring or fuzzy text search.
- A new traversal engine, background index service, cache, or dependency.
- MCP progress notifications before post-change measurements justify them.
- Changes to legacy context, dependencies, impact, trace, or why behavior.

## Decisions

### Persist the display name with each immutable fact-shard entity

Add `analysis_fact_shard_entity_name`, keyed by producer, owner, version, and
canonical ID, with the display name as its value and a `by_name` index whose
leading key is the display name. `replace_fact_shards` computes the name once by
reusing the DTO display-name rules and writes it beside changed entity rows.
Existing canonical-ID lookup continues to use
`analysis_fact_shard_entity:by_id`.

Search performs indexed exact/prefix candidate acquisition from canonical IDs
and display names, validates candidates against
`analysis_fact_shard_selection:by_owner`, hydrates only those candidates, merges
the small revision/state/enrichment sources, and applies the stable rank once.
The startup schema migration backfills names only for shard versions selected by
at least one workspace. Existing shard cleanup removes matching name rows.

This is preferred to full-text search because the required operation is exact or
prefix matching and a global full-text top-k could select obsolete shard versions
before the current-version join.

### Represent repository filtering as path state plus reverse reachability

`TraverseGraphQuery` gains normalized `target_repositories`. A filtered query
first performs bounded reverse traversal from each target repository through the
existing `analysis_resolved_dependency:by_to` index and records, per entity, the
targets reachable from that entity. These lightweight rows contain identifiers
and ownership only; they do not hydrate entity DTOs.

Forward acquisition starts at the requested entity and carries the set of target
repositories already visited. A successor is queried only when its reachability
markers contain every still-unvisited target. The branch is discarded before
entity hydration or path enumeration otherwise. If the start marker does not
cover every target, the query returns no paths immediately.

After a state has visited every target, acquisition continues only through
entities owned by the repository that completed the match. An ownership-neutral
contract successor is acquired as a terminal repository boundary; a successor
owned by another repository is not acquired. The existing simple-path enumerator
then operates only on this constrained graph and carries repository visit state
to enforce the conjunctive contract.

The reverse and forward phases share the existing 10,000-row, 100,000-step, and
five-second budgets. This is preferred to post-filtering because post-filtering
would pay the dominant acquisition and hydration cost before removing results.
No new dependency index is added unless a measured plan proves the existing ones
insufficient.

### Add request-controlled metadata detail

Add an optional protobuf `include_diagnostics` flag to entity search and graph
traversal. The daemon maps an absent value to `true` for compatibility; MCP maps
an omitted input to `false`. The store accepts a metadata options struct rather
than another positional Boolean.

`AnalysisMetadata` gains diagnostic totals by severity. Summary mode runs an
aggregate query and leaves `diagnostics` empty. Detail mode retrieves rows and
derives the same counts. It never retrieves details and clears them afterward.

### Version the changed contracts

The protocol version advances from 23 to 24. Protobuf adds repeated
`target_repositories`, optional `include_diagnostics`, diagnostic counts, and the
`repository_boundary` enum value without reusing field numbers. Entity search and
traversal return `beholder.entity_search.v2` and
`beholder.traverse_graph.v2`; unrelated schemas remain unchanged.

## Risks / Trade-offs

- **Reverse reachability work grows with target count** → Share the global
  acquisition limits, normalize duplicates, and stop immediately when the origin
  cannot reach all targets.
- **A name index increases database size and installation time** → Retain it only
  after `EXPLAIN` and clone-based size/build measurements demonstrate the planned
  lookup.
- **Display-name logic could diverge between storage and DTO rendering** → Expose
  one Beholder-owned helper and cover publication plus lookup in one focused test.
- **Summary diagnostics change MCP payload shape** → Keep counts present in both
  modes and make detailed rows an explicit MCP option.
- **Filtered traversal could mislabel an acquisition cutoff as a boundary** → Keep
  incomplete-frontier tracking authoritative over all terminal classifications.

## Migration Plan

1. Measure the proposed name-index build against a disposable clone and record
   the query plan, allocation, and timing.
2. Ship the idempotent relation/index creation and selected-shard backfill with
   protocol version 24.
3. Verify focused adapter and MCP integration tests, then run two warm benchmark
   executions for search and single-/multi-target traversal.
4. Roll back the binary if necessary; the additive relation and protobuf fields
   are safe for the previous binary to ignore. Remove the relation only through a
   later explicit migration, not during rollback.
