# DEFERRED RESEARCH — Evaluate Infomap directed flow view

Mandatory: “This work evaluates a potential future backend/capability. It does not change Beholder’s product contract unless a later reviewed proposal adopts it.” No stable delta; start after stable B/D4; default Leiden.

## E1 — Build isolated reproducible Infomap experiment
Motivation: directed flow may expose patterns lost by symmetrized Leiden. Scope: B export, directed/layer mapping, pinned container/CLI, seeds/trials/manifests. Non-goals: production dependency, daemon/MCP/default. Current: no Infomap; B directed layers. Architecture: offline evaluation adapter. API: experiment files/results only. Tests: tiny directed/multilayer, repeats, invalid export, license manifest. Acceptance: pinned reproducibility/provenance/license. Dependencies: B stable/D4 fixtures. Shipping: research tooling PR. Compatibility: zero runtime effect. PR: `test(research): evaluate Infomap flow partitions`.

## E2 — Compare flow utility and decide adopt/stop
Motivation: different partitions do not prove agent answers. Scope: preregister/run subsystem-flow-utility-stability-cost comparison against D4. Non-goals: tune desired map. Current: A/D metrics/no utility claim. Architecture: blind decision record. API: research report. Tests: trials/seeds/weight perturbation/directed reversal. Acceptance: explicit adopt-later/stop; no production dependency. Dependencies: E1. Shipping: report PR. Compatibility: none. PR: `docs(research): decide Infomap flow utility`.
