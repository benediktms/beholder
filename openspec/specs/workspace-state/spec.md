# Workspace State Specification

## Purpose

Define content-authoritative workspace identity and coherent revision publication.
The rationale is retained in `docs/adr/0004-content-authoritative-repository-inventory.md`
and the Git and workspace model in `docs/VISION.md`.

## Requirements

### Requirement: Content-authoritative desired state

Beholder SHALL derive desired repository state from logical repository identity,
selected Git state, accepted input kinds and bytes, workspace-owned inputs, and the
active analyzer and plugin identities.

#### Scenario: Metadata-preserving content change

- **WHEN** accepted source bytes change while file size and timestamp remain equal
- **THEN** authoritative reconciliation detects a different desired state

#### Scenario: Git-only change

- **WHEN** the selected worktree `HEAD` changes without changing accepted source bytes
- **THEN** the repository desired state changes

### Requirement: Versioned rebuildable inventory

The daemon SHALL keep a versioned inventory of accepted paths, input kinds,
metadata hints, and verified content digests separately from semantic graph state.

#### Scenario: Corrupt inventory

- **WHEN** an inventory manifest is unknown, incomplete, corrupt, or incompatible
- **THEN** the daemon ignores and rebuilds it while preserving the last complete graph revision

### Requirement: Advisory filesystem events

Filesystem watcher events and metadata SHALL be scheduling hints, not proof of
semantic currentness.

#### Scenario: Watcher reports one path

- **WHEN** a watcher identifies a changed path
- **THEN** Beholder re-reads that path and verifies membership and relevant metadata before publication

#### Scenario: Watcher misses an event

- **WHEN** an event is missed, coalesced, or occurs while the daemon is stopped
- **THEN** startup or periodic authoritative reconciliation eventually detects the changed content

### Requirement: Immutable analysis snapshots

Analysis SHALL run against an immutable workspace snapshot and publish only while
its identity and scheduler generation remain current.

#### Scenario: Inputs change during analysis

- **WHEN** verification finds a different identity or generation before publication
- **THEN** Beholder discards the stale result, retains the previous revision, and schedules current work

### Requirement: Atomic revision visibility

Beholder SHALL expose only complete analysis revisions.

#### Scenario: Replacement revision is being built

- **WHEN** refresh, analysis, enrichment, or publication is in progress
- **THEN** queries continue to use the last complete revision and report current freshness state
