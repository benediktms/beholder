# GATED AFTER B — Topology-aware inspection and architecture map

Parent: topology-aware inspect, progressive map, witness compression, five-arm gate; modify `agent-query-api`, optionally reference overlay. Non-goals: full dump, silent fallback, query-time model, replacing raw evidence.

## D1 — Add topology-aware inspection without LLM
Motivation: membership/boundaries help before semantic interpretation. Scope: inspect partitions/boundaries/metrics/sibling-neighbor/evidence. Non-goals: provider names/descriptions/hidden resolution. Current: A inspect + B partitions. Architecture: exact-key join after canonical acquisition; canonical independently succeeds. API: optional topology resolution/key/origin/status. Tests: ready/absent/failed/wrong revision/bounds/support-only. Acceptance: no LLM; no revision mix; traceable memberships. Dependencies: A4/B3/B4/B5b. Shipping: query/MCP PR. Compatibility: additive field/schema. PR: `feat(query): add topology-aware entity inspection`.

## D2 — Add bounded progressive architecture_map
Motivation: orientation without UI topology. Scope: exact request/result, semantic/Leiden/raw fallback, labels/bounds. Non-goals: UI alias/silent fallback/provider. Current: `WorkspaceTopology`/query services, not UI BFS/search. Architecture: derived exact revision then bounded canonical fallback. API: `beholder.intent.architecture_map.v1`. Tests: fallbacks/required/revision/bounds/omissions/no-provider. Acceptance: never unbounded; level/origin/status explicit; raw evidence-backed. Dependencies: D1/C optional. Shipping: service/MCP PR. Compatibility: additive sixth tool. PR: `feat(mcp): expose bounded architecture map`.

## D3 — Compress trace/impact with witnesses
Motivation: precise raw paths are token-heavy. Scope: group bounded paths by units while retaining branches/order/witnesses/omissions. Non-goals: replacement/fabricated flow/hidden uncertainty. Current: A trace/impact/D2 origins/#204. Architecture: deterministic projection of bounded result, no graph search. API: optional compression. Tests: branches/convergence/mixed membership/unavailable semantic/truncated input. Acceptance: canonical witness per edge; truncation/raw retained. Dependencies: D2. Shipping: compression PR. Compatibility: additive response/mode. PR: `feat(query): compress trace and impact with architecture evidence`.

## D4 — Run and publish five-arm evaluation
Motivation: correctness/utility not aesthetics. Scope: available arms/repeats/blind scoring/uncertainty/cost/misleading analysis/artifacts. Non-goals: general benchmark/private data. Current: A1 harness, no utility result. Architecture: immutable manifests and `docs/evaluation` report. API: benchmark-only. Tests: reproducibility/scorer agreement/unavailable/redaction. Acceptance: all arms or unavailable; gates evaluated; adopt/revise/stop. Dependencies: D3/optional C4. Shipping: report PR. Compatibility: none. PR: `docs(evaluation): compare agent architecture query arms`.

Context: B3's offline A1-harness gate establishes eligibility before D using deterministic B3 output and no C2 dependency. D4 is the independent integrated-path gate; failure prevents default enablement even if the offline gate passed.
