## Context

Current MCP exposes `list_workspaces`, `search_entities`, and `traverse_graph`; existing CLI semantic commands have distinct behavior. `query-api` already requires typed, revision-consistent results and bounded traversal. See proposal.md and its delta specs for behavior.

## Goals / Non-Goals

**Goals:** one immutable snapshot per request, strict resolution and shared bounds, thin MCP/CLI adapters over reusable services, and A1's paired baseline before product behavior.

**Non-Goals:** topology, model output, UI migration, removing commands, or changing legacy query semantics.

## Decisions

- Place reusable intent acquisition/projection in `crates/architecture-query` above daemon/store primitives; keep Mnestic types in the adapter. This avoids MCP fan-out and UI duplication.
- Echo canonical and derived readiness separately. Derived absence cannot make a canonical response incomplete.
- Give new operations independent schemas; do not rename legacy context/shortest trace/reachability as intent operations because their contracts differ.
- Baseline A1 precedes A3-A5 and all B product behavior. A2 planning and read-only backend research can proceed before it.
- Preserve aliases for a documented release; unattributed RPC logs are not removal evidence.

## Risks / Trade-offs

- [Compact output omits decisive evidence] → structured evidence and deterministic omission counts remain available.
- [Old daemon ignores additive fields] → retain existing client shields and test ignored preferences/targets.
- [Benchmark model/cache drift] → pin identities and schedule in the A1 manifest; record unavailable arms.

## Migration Plan

Ship A1 first, then additive services and MCP/CLI routing. Retain old tools and aliases; a later reviewed proposal with attributed usage evidence governs any removal. Rollback unregisters only new tools/routes without affecting legacy contracts.

## Contract appendix (extracted)

Every request requires workspace, optionally pins an available revision, rejects unknown fields, and succeeds with schema, canonical workspace/revision/freshness/analysis, derived statuses, normalized request, bounds, truncation, origin, and typed payload. Origins are `canonical_query`, `deterministic_projection`, `leiden_cpm`, `semantic_overlay`; typed errors are `invalid_argument`, `workspace_not_found`, `revision_unavailable`, `entity_not_found`, `ambiguous_entity`, `bound_exceeded`, `deadline_exceeded`, `acquisition_limit`, `work_limit`, `derived_not_ready`, `derived_revision_mismatch`, `internal`. IDs win; exact names resolve only uniquely; ambiguous candidates max at 20; prefixes are suggestions.

`workspace_overview` has diagnostics false/no entities. `find_entities`: limit 20/100, repositories max 20, kinds, tests false, generated support/include/exclude. `inspect_entity`: hops 1/3, nodes 100/500, edges 250/1000, evidence 3/20. `trace`: hops 8/32, paths 10/50, evidence 2/20 and existing 10,000-row/100,000-step/5-second acquisition caps. `impact`: dependents, hops 4/16, nodes 100/500, repository grouping, witnesses 3/20. Evaluation compares repo-only/current MCP/v2/topology/semantic arms with frozen identities, correctness/misleading/cost/transcript metrics; adoption requires critical-invariant success, zero high-severity misleading claims, ≥90% weighted assertions, paired non-inferiority within 2 points, plus ≥5-point correctness or 20% efficiency improvement.
