## Context

A provides bounded intent services and B supplies exact-key partitions; the UI's full client-side topology has incompatible unbounded semantics. See proposal.md and delta spec for behavior.

## Goals / Non-Goals

**Goals:** topology-aware canonical inspection, progressive bounded maps, witness-preserving compression, and correctness-first evaluation.

**Non-Goals:** a UI snapshot alias, silent fallback, provider calls, raw-evidence replacement, or derived revision mixing.

## Decisions

- Query services acquire canonical data first, then join only exact-revision derived keys. Canonical success does not await derived readiness.
- `architecture_map` serves semantic only when exact ready; `auto` degrades to exact Leiden then bounded canonical raw data, always labeling served level/origin/status. `required` returns `derived_not_ready`.
- Compression projects already bounded paths/results rather than launching a new graph search; witnesses and truncation propagate.
- D executes after A4 and B3/B4/B5b. C is optional, never a prerequisite for deterministic topology output.

## Risks / Trade-offs

- [Fallback sounds authoritative] → explicit requested/served/origin labels and unavailable status.
- [Grouping hides branches] → retain representative canonical witnesses and omissions.
- [Map aesthetics over utility] → frozen A1/D4 scored gates decide adoption.

## Migration Plan

Ship D1 then D2 then D3 as additive services/MCP fields/tools. D4 publishes an immutable cross-arm report; a failed gate stops adoption or triggers a revised proposal rather than changing canonical behavior.

## Architecture-map appendix (extracted)

`architecture_map` takes workspace/revision, entity/repository scope, level workspace/community/architecture default architecture, semantic auto/off/required, groups 50/200, members 20/100, edges 100/500, evidence 3/20. Required returns `derived_not_ready`; auto picks exact overlay, exact Leiden, then bounded canonical raw. It reports requested/served level, origin, keys, unavailable status, omissions, groups/members/inter-group edges/evidence. Raw is canonical query, never architecture. Limitation: D1 depends on A4+B3/B4/B5b; the plan's B3 utility gate versus D1 integration ordering is retained for implementation evidence, not resolved here.
