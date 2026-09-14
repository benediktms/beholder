## Context

B retains directed projection layers but its initial structural view symmetrizes them. Infomap is GPL-3-or-later/commercial and is not an approved runtime dependency.

## Goals / Non-Goals

**Goals:** an isolated reproducible directed-flow comparison using stable B and D4 fixtures.

**Non-Goals:** production integration, daemon/MCP changes, default partitions, or tuning toward a desired map.

## Decisions

- Use a pinned offline container/CLI adapter only after selected image/version review; manifest input hash, digest, version, seeds/trials/thread policy, and exact invocation before execution.
- Compare against the same frozen task/rubric controls; assess flow utility, stability, runtime/memory/install, and license cost.
- End with explicit adopt-later or stop. Adoption requires a new reviewed change.

## Risks / Trade-offs

- [GPL/commercial obligation] → isolate experiment and prohibit product dependency.
- [Stochastic/ambiguous output] → repeated trials and perturbation/directed-reversal controls.

## Migration Plan

Begin only after B and D4 fixtures. No rollback is needed because this change has no runtime behavior.
