## Purpose

Defines an optional, evidence-backed architecture interpretation layer whose identities, uncertainty, and failures remain independent from canonical semantics and structural topology.

## ADDED Requirements

### Requirement: Overlay independence and gated adoption
The overlay SHALL start only after B demonstrates a preregistered useful structural outcome, and its deletion, absence, or invalidity SHALL not impair canonical or topology queries.

#### Scenario: Useful-B gate fails
- **WHEN** structural topology fails its utility or misleading-summary gate
- **THEN** overlay implementation does not begin and no provider capability is adopted

### Requirement: Stable evidence-backed units
Architecture units SHALL be derived concepts rather than community identities and shall retain canonical entity, repository/boundary, and evidence provenance for every membership, label, parent, shared/cross-cutting, or orphan conclusion.

#### Scenario: Partition ID reorder
- **WHEN** input community IDs reorder without changing unit evidence
- **THEN** unit identity does not depend solely on the opaque community IDs

### Requirement: Bounded reproducible evidence packs
Evidence packs SHALL deterministically retain bounded member, edge, boundary, and representative evidence context; record omitted counts and exact context hashes; and not include source text outside the configured evidence bounds.

#### Scenario: Oversized community
- **WHEN** a community exceeds configured member or edge limits
- **THEN** the selected representatives and omissions are deterministic and persisted with the pack key

### Requirement: Optional isolated cached provider
Provider execution SHALL be opt-in, isolated, bounded by timeout/cancellation/size limits, keyed by complete semantic identity, and never invoked by a query.

#### Scenario: No provider configuration
- **WHEN** a caller requests architecture information with no provider configured
- **THEN** no provider process is started and topology availability is returned normally

### Requirement: Validated conservative refinements
Only schema-validated labels, descriptions, evidence-backed parent assignments, shared/cross-cutting/orphan flags, and reversible suggestions SHALL be selectable; merge/split outputs remain un-applied proposals with uncertainty.

#### Scenario: Invalid provider action
- **WHEN** provider output names an unknown entity, disallowed action, or oversized text
- **THEN** it is rejected and a prior ready overlay remains selected

### Requirement: Explicit uncertain continuity
Continuity across retained same-resolution artifacts SHALL use deterministic overlap evidence and report reciprocal continuations, splits, merges, new/disappearance, or uncertainty without claiming perfect recovery.

#### Scenario: One-to-many overlap
- **WHEN** one earlier unit maps materially to several later units
- **THEN** the result reports a split event with scores and uncertainty rather than a silent rename
