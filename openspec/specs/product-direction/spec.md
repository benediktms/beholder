# Product Direction Specification

## Purpose

Define Beholder's stable product boundary while preserving `docs/VISION.md` as the
source for roadmap detail, motivation, and future possibilities.

## Requirements

### Requirement: Local-first semantic intelligence

Beholder SHALL build and query a semantic model of source code and software
contracts from repositories available on the developer's machine.

#### Scenario: Inspecting a registered workspace

- **GIVEN** a workspace contains one or more registered repositories
- **WHEN** Beholder completes an analysis revision
- **THEN** semantic queries operate on that local revision without requiring a hosted service

### Requirement: Cross-repository workspace model

Beholder SHALL model a workspace as a selected, revision-consistent view across
multiple logical repositories rather than as an isolated source tree.

#### Scenario: Following a boundary across repositories

- **GIVEN** compatible evidence connects entities owned by different repositories
- **WHEN** a user runs a traversal query
- **THEN** the result may cross the repository boundary while retaining repository attribution

### Requirement: Evidence-backed results

Beholder SHALL attach source evidence, confidence, provenance, and analysis-state
metadata to semantic results so users can distinguish exact, inferred, stale,
incomplete, and unresolved findings.

#### Scenario: Returning an inferred relationship

- **WHEN** a relationship is derived from a recognized source shape rather than an exact contract
- **THEN** the result identifies its evidence and inferred confidence

### Requirement: Rebuildable analysis state

Beholder SHALL treat repository contents and explicit workspace configuration as
the authority from which analysis state can be rebuilt.

#### Scenario: Losing rebuildable caches

- **WHEN** a frontend or inventory cache is missing or invalid
- **THEN** Beholder rebuilds it without changing the last complete semantic revision

### Requirement: Bounded first release

Beholder SHALL keep unimplemented roadmap items outside the current behavioral
contract until a reviewed OpenSpec change introduces them.

#### Scenario: Reading a future vision section

- **GIVEN** `docs/VISION.md` describes a future capability
- **WHEN** no matching archived OpenSpec change and current requirement exist
- **THEN** the capability is treated as direction rather than shipped behavior
