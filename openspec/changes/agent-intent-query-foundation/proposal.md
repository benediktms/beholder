## Why

Beholder's three current MCP tools expose storage-oriented search and traversal, while agents need bounded, intent-sized answers with revision, provenance, and completeness made explicit. A paired baseline must be captured first so an additive MCP v2 is judged on evidence rather than tool-count or route activity.

## What Changes

- Add reusable, deterministic intent-query services for overview, find, inspect, multipath trace, and grouped impact.
- Add five additive MCP tools under independent `beholder.intent.<tool>.v1` schemas; retain all current tools and their contracts.
- Define strict entity resolution, revision pinning, typed errors, bounds, truncation, canonical/derived readiness, and origin labels.
- Establish the repository-only/current-MCP evaluation baseline before any product behavior work, then compare deterministic intent tools.
- Add coherent CLI query/debug namespaces while retaining legacy aliases for a documented compatibility window.

## Capabilities

### New Capabilities
- `agent-query-api`: Bounded, revision-consistent agent intent query contracts and evaluation requirements.

### Modified Capabilities
- `query-api`: Preserve existing query contracts while adding the additive intent surface and compatibility rules.

## Impact

Future implementation touches a new `crates/architecture-query` service above existing daemon/store primitives, MCP and CLI adapters, DTO/protocol schemas, smoke tests, and benchmark fixtures. It is constrained by `query-api`, the archived MCP optimization/evidence-context changes, and the current immutable semantic snapshot contract; it does not change topology, UI behavior, or canonical graph facts.
