## Why

MCP semantic queries currently spend most of their time scanning selected fact
shards and loading diagnostic detail that callers usually do not need. Repository
filters must constrain graph acquisition itself so irrelevant branches are never
hydrated or enumerated.

## What Changes

- Replace entity substring scans with deterministic exact and prefix lookup over
  indexed canonical IDs and persisted display names.
- Add conjunctive, unordered `target_repositories` filtering to `TraverseGraph`;
  every returned path visits every requested repository, while unlisted
  intermediate repositories remain valid.
- Push repository reachability into indexed graph acquisition and stop acquiring
  a completed path when it leaves the repository that satisfied the last target.
- Add summary-only diagnostics retrieval for MCP, with explicit opt-in to detailed
  rows and counts retained in metadata.
- Version the changed entity-search and graph-traversal response contracts.
- Keep MCP progress notifications out of scope unless useful operations remain
  slow after query work is complete.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `query-api`: Define indexed exact/prefix entity search, conjunctive repository-
  filtered graph traversal, diagnostics summary retrieval, and the versioned
  response contracts.

## Impact

The change affects query DTOs, the daemon protobuf API and protocol version,
daemon-client and MCP request mapping, Mnestic schema/storage/query code, focused
adapter and integration tests, and `docs/SEMANTIC_QUERY_PERFORMANCE.md`. It adds
no dependency. Existing unfiltered traversal remains available, and an absent
diagnostics preference preserves detailed diagnostics for existing daemon clients.
