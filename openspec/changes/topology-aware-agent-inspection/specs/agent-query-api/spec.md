## ADDED Requirements

### Requirement: Exact-revision topology-aware inspection
When a matching derived partition is ready, inspection SHALL expose bounded membership, boundary, metric, and evidence context with `leiden_cpm` origin; when it is absent or failed, canonical inspection SHALL still succeed with its exact status.

#### Scenario: Overlay absent but partition ready
- **WHEN** topology for the requested revision is ready and no overlay is ready
- **THEN** inspection returns the matching Leiden context without requiring an LLM

### Requirement: Bounded progressive architecture map
`architecture_map` SHALL accept workspace/revision, bounded scope and output controls, and semantic `auto|off|required`; it SHALL explicitly return requested/served level, origin, exact derived keys, unavailable higher-level status, and deterministic omissions.

#### Scenario: Auto fallback
- **WHEN** semantic output is unavailable and matching Leiden is ready
- **THEN** `auto` serves Leiden with its origin and reports semantic unavailability

#### Scenario: Raw fallback
- **WHEN** no matching derived result is ready and bounded raw fallback is available
- **THEN** the response labels it `canonical_query` and never calls it architecture

### Requirement: Witness-preserving architecture compression
Trace and impact compression SHALL operate only on already bounded canonical results, retain representative canonical path/evidence witnesses and omitted counts, propagate truncation, and leave raw results available.

#### Scenario: Collapsed paths
- **WHEN** several canonical paths map to one architecture edge
- **THEN** that edge exposes representative underlying witnesses and does not hide canonical truncation

### Requirement: Comparative topology adoption gate
Topology and semantic arms SHALL use A1's frozen task/rubric/control manifest and fail adoption on a critical invariant failure or high-severity misleading interpretation even when tokens or time improve.

#### Scenario: Efficient but misleading map
- **WHEN** an architecture arm reduces tokens but introduces a high-severity misleading claim
- **THEN** the evaluation reports a failed adoption gate
