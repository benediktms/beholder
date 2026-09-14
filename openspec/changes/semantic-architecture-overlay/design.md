## Context

No architecture-unit/provider model exists. B supplies structural partitions but cannot establish architectural meaning. See proposal.md and delta specs for behavior.

## Goals / Non-Goals

**Goals:** optional, bounded, reproducible interpretation with evidence and uncertainty; cache-only query reads.

**Non-Goals:** canonical writes, required LLMs, free-form ontology, query-time calls, or automatic merge/split.

## Decisions

- Gate all C implementation on a useful-B result; units are derived concepts with explicit service/repository/boundary evidence, not renamed community IDs.
- Build deterministic bounded evidence packs before any provider. Their context hash is part of semantic identity.
- Run providers only in an isolated optional worker with narrow JSON schema, timeout/cancel/size controls, and derived-state publication after validation.
- Apply only whitelist refinements via a deterministic reducer. Merge/split remain suggestions.
- Continuity compares retained adjacent same-resolution artifacts using reciprocal overlap; ambiguity remains explicit.

## Risks / Trade-offs

- [Confident model text overclaims] → origin, evidence, uncertainty, schema validation, and no canonical writes.
- [Context/data exposure] → bounded packs, no source outside selected evidence, optional disabled-by-default provider.
- [Missing history] → no time-travel guarantee after retained inputs expire.

## Migration Plan

After B's gate, ship C1-C2 pure derived artifacts, then isolated C3, reducer C4, and continuity C5. Disable/remove an overlay by deselecting it; canonical and topology paths continue unchanged.

## Overlay appendix (extracted)

Packs contain bounded IDs/names/kinds/repos, strongest internal/external edges, representative evidence, crossings, omissions, exact keys/hashes, and no source beyond bounds. Semantic identity includes partition/packs, provider/model/pinned version, inference, prompt/schema versions/hashes, context hash. Provider is isolated/schema-limited and cannot use enrichment or publish canonical facts. Allowed output: labels/descriptions, evidence-backed parent/reparent, shared/cross-cutting/orphan; merge/split suggestions only. Same-resolution reciprocal overlap reports continuation/split/merge/new/disappearance/uncertainty.
