# Semantic Graph Specification

## Purpose

Define the canonical entity, relationship, evidence, and ownership model described
in `docs/VISION.md`.

## Requirements

### Requirement: Stable semantic identity

Every semantic entity SHALL have a canonical identifier defined by its frontend
rather than a transient database row or response position. Contract identities are
path-independent; source-scoped frontends may include a repository-relative path.

#### Scenario: Same symbol in a later revision

- **WHEN** a path-scoped C# definition moves to another source file
- **THEN** its canonical identifier may change with the source-derived module prefix

### Requirement: Closed typed ontology

Entity and relationship kinds SHALL be Beholder-owned closed types with
directional semantics.

#### Scenario: Analyzer emits an unsupported kind

- **WHEN** an analyzer contribution names an undeclared entity or relationship kind
- **THEN** validation rejects that contribution rather than extending the ontology implicitly

### Requirement: Repository attribution

Source-owned entities, observations, and analyzer contributions SHALL retain logical
repository attribution. Public evidence attribution is derived from its endpoints
and may be absent when both endpoints use ownership-neutral contract identities.

#### Scenario: Cross-repository relationship

- **WHEN** descriptor evidence connects canonical `proto-*://` endpoints
- **THEN** the evidence retains its descriptor path but may not expose a repository in the public DTO

### Requirement: Provenance and confidence

Relationships SHALL expose the strongest stored confidence and preserve
corroborating evidence that remains distinct after analyzer and storage aggregation.
Unsharded observations with the same source, relation, and target MAY retain only
one evidence record.

#### Scenario: Exact and inferred evidence agree

- **WHEN** an exact generated binding and an inferred source shape remain distinct stored observations for the same edge
- **THEN** the edge reports exact confidence and retains both evidence records

### Requirement: Structural and traversal semantics

Structural observations SHALL remain queryable without automatically becoming
dependency-traversal edges.

#### Scenario: Protobuf field membership

- **WHEN** a `field_of` observation connects a field to a message
- **THEN** context may return it while dependency traversal excludes it unless a rule declares traversal semantics

### Requirement: Generated and test origin

Entities SHALL distinguish first-party source, generated source, external
dependencies, and test/specification/benchmark symbols.

#### Scenario: Default compact query

- **WHEN** compact output is requested without tests
- **THEN** presentation may hide tests and supporting entities while the typed result remains complete

### Requirement: Owner-scoped replacement

An analyzer or plugin SHALL replace or retract only its own contribution.

#### Scenario: Removing one enrichment owner

- **WHEN** one analyzer is disabled or removed
- **THEN** its observations disappear without rewriting baseline facts or another owner's contribution
