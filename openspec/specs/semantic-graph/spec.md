# Semantic Graph Specification

## Purpose

Define the canonical entity, relationship, evidence, and ownership model described
in `docs/VISION.md`.

## Requirements

### Requirement: Stable semantic identity

Every semantic entity SHALL have a canonical identifier derived from its semantic
identity rather than a transient database row, response position, or local path.

#### Scenario: Same symbol in a later revision

- **WHEN** a symbol's semantic identity is unchanged across revisions
- **THEN** queries return the same canonical entity identifier

### Requirement: Closed typed ontology

Entity and relationship kinds SHALL be Beholder-owned closed types with
directional semantics.

#### Scenario: Analyzer emits an unsupported kind

- **WHEN** an analyzer contribution names an undeclared entity or relationship kind
- **THEN** validation rejects that contribution rather than extending the ontology implicitly

### Requirement: Repository attribution

Source-owned entities, observations, evidence, and analyzer contributions SHALL
retain logical repository attribution.

#### Scenario: Cross-repository relationship

- **WHEN** an edge connects entities from two repositories
- **THEN** each endpoint and each evidence record retains its owning repository

### Requirement: Provenance and confidence

Relationships SHALL preserve all corroborating evidence and expose the strongest
supported confidence without discarding weaker evidence.

#### Scenario: Exact and inferred evidence agree

- **WHEN** an exact generated binding and an inferred source shape support the same edge
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
