# Indexing Pipeline Specification

## Purpose

Define the observable indexing, durable-job, cache, and publication contracts drawn
from `docs/INDEXING_API_PROPOSAL.md`, `docs/INDEXING_PERFORMANCE.md`,
`docs/adr/0006-background-work-scheduling.md`, and
`docs/adr/0007-incremental-semantic-computation.md`.

## Requirements

### Requirement: Ordered verified indexing

An indexing operation SHALL refresh inventory, prepare analysis, verify currentness,
atomically publish eligible changes, and schedule enrichment inputs in that order.

#### Scenario: Unchanged verified input

- **WHEN** repository, runtime, workspace, and semantic verification fingerprints match a checkpoint
- **THEN** the operation returns `Unchanged`, publishes no facts, and schedules no unnecessary enrichment

#### Scenario: Superseded generation

- **WHEN** the requested generation becomes stale before analysis, verification, or publication
- **THEN** the operation returns the normal `Superseded` outcome and publishes no stale facts

### Requirement: Durable coalesced jobs

Automatic indexing jobs SHALL be durable and coalesced per workspace so a newer
desired generation replaces obsolete queued automatic work.

#### Scenario: Watcher burst

- **WHEN** many filesystem events arrive for the same workspace
- **THEN** the queue converges on one automatic job for the newest desired generation

### Requirement: Five-attempt failure policy

Indexing and enrichment jobs SHALL receive no more than five total attempts before
reaching a terminal failure state.

#### Scenario: Repeated retryable failure

- **WHEN** a job fails on each attempt
- **THEN** the fifth failed attempt is terminal and remains inspectable

### Requirement: Non-blocking semantic reads

Semantic-store mutation serialization SHALL NOT block semantic queries on the
reserved read engine.

#### Scenario: Query during publication or garbage collection

- **WHEN** a mutation operation holds the semantic-store write gate
- **THEN** a semantic query may proceed against the last atomically published revision

### Requirement: Incremental immutable fact publication

Frontends that support incremental computation SHALL publish immutable fact shards
and selection manifests so unchanged semantic owners retain their existing facts.

#### Scenario: Non-semantic source edit

- **WHEN** an edit changes source bytes but leaves a semantic owner's output unchanged
- **THEN** Beholder advances currentness without replacing that owner's unchanged fact shard

### Requirement: Independently selected enrichment

Enrichment payloads SHALL be immutable snapshots selected independently per
repository and analyzer. Query freshness currently reflects active indexing or
enrichment work rather than a terminal job's retained input identity.

#### Scenario: Baseline changes before enrichment catches up

- **WHEN** a compatible previous enrichment snapshot no longer matches current inputs while replacement work remains active
- **THEN** queries may retain it and report the active work as stale

#### Scenario: Replacement enrichment fails terminally

- **WHEN** the replacement job exhausts its attempts and the previous contribution remains selected
- **THEN** queries may retain that contribution without reporting stale enrichment

### Requirement: Bounded parallel analysis

Source analysis SHALL default to the host's available parallelism and SHALL allow a
positive `BEHOLDER_INDEX_WORKERS` value to override the worker count.

#### Scenario: Constrained machine

- **WHEN** `BEHOLDER_INDEX_WORKERS` specifies fewer workers than the host exposes
- **THEN** indexing uses that explicit worker count without changing serialized semantic output

### Requirement: Batched Mnestic publication

Mnestic observation publication SHALL use batches of 10,000 rows while preserving
one atomic workspace revision.

#### Scenario: Large changed publication

- **WHEN** a revision contains more than 10,000 rows to publish
- **THEN** the adapter submits multiple bounded batches within the same atomic publication
