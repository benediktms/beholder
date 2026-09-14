## Context

No SBM exists in Beholder. graph-tool is a C++/Python stack under GPL-3.0-only; D4 supplies the agent-evaluation method.

## Goals / Non-Goals

**Goals:** an isolated, reproducible nested degree-corrected SBM evaluation of role-oriented utility.

**Non-Goals:** production graph-tool, daemon/provider integration, a default hierarchy, or perfect role recovery.

## Decisions

- Run only after mature C evaluation and E completion/skipping, in a pinned offline container/Conda environment with complete manifest provenance.
- Test planted role/core-periphery/bipartite/layer fixtures, multiple starts/equilibration, perturbations, and blind agent-task comparison.
- Treat blocks/hierarchy as model inference, not architecture truth; require an explicit stop/adopt-later decision and later reviewed adoption.

## Risks / Trade-offs

- [Heavy GPL runtime/install cost] → research-only isolation and cost recording.
- [Stochastic fitted hierarchy] → retained uncertainty, seed/environment capture, and repeated starts.

## Migration Plan

No product migration occurs. The only result is a decision record; any adoption is separately proposed.
