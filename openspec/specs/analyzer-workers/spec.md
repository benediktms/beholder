# Analyzer Workers Specification

## Purpose

Define language-native semantic enrichment from
`docs/adr/0001-native-analyzer-workers.md`,
`docs/adr/0002-elixir-compiler-tracer-worker.md`,
`docs/adr/0003-typescript-native-semantic-worker.md`, and the incremental worker
sections of `docs/adr/0007-incremental-semantic-computation.md`.

## Requirements

### Requirement: Isolated language-native workers

Compiler-backed semantic analysis SHALL run in language-specific worker
executables while baseline syntax analysis remains in-process.

#### Scenario: Compiler toolchain fails

- **WHEN** a native compiler worker fails
- **THEN** the published syntax graph remains queryable and Beholder emits a typed non-fatal diagnostic

### Requirement: Typed bidirectional protocol

The daemon and analyzer workers SHALL communicate through a versioned,
bidirectional gRPC protocol carrying immutable inputs, progress, contributions,
diagnostics, completion, cancellation, and trace context.

#### Scenario: Unknown protocol enum value

- **WHEN** a required typed value is missing or unknown at the protocol boundary
- **THEN** the request fails explicitly rather than inferring a default

### Requirement: Baseline-first publication

The daemon SHALL publish eligible syntax facts before running active compiler
enrichers and SHALL NOT block baseline indexing or semantic reads on enrichment.

#### Scenario: Enrichment is still running

- **WHEN** baseline publication has completed
- **THEN** queries can use that baseline and report enrichment freshness separately

### Requirement: Input-current publication

A worker contribution SHALL publish only when its immutable input fingerprint still
matches the current baseline and worker identity.

#### Scenario: Newer source snapshot supersedes worker input

- **WHEN** a worker completes against obsolete inputs
- **THEN** its result is not published and newer work remains eligible

### Requirement: Coalesced analyzer work

Worker jobs SHALL be coalesced per workspace and analyzer so queued obsolete
snapshots are replaced by the newest eligible input.

#### Scenario: Several baseline revisions arrive quickly

- **WHEN** an analyzer has not started the older queued revisions
- **THEN** it runs against the newest compatible snapshot rather than each obsolete revision

### Requirement: Bounded persistent compiler state

A worker SHALL retain compiler state only when its frontend defines a bounded,
single-writer lifecycle and rebuild triggers.

#### Scenario: Rust source-only change

- **WHEN** accepted membership and Cargo configuration are unchanged
- **THEN** the persistent Rust worker applies source changes to its one retained rust-analyzer database

#### Scenario: Rust project structure changes

- **WHEN** Cargo configuration, target, workspace, or accepted membership changes
- **THEN** the Rust worker rebuilds the compiler database

### Requirement: Explicit trust for code-executing enrichment

Compiler enrichment that executes repository-controlled build or macro code SHALL
require explicit user invocation or opt-in and SHALL NOT be activated merely by
repository configuration.

#### Scenario: Registering an Elixir repository

- **WHEN** the repository has not been explicitly approved for compiler execution
- **THEN** Beholder indexes syntax without automatically running Mix compilation

### Requirement: Stable worker diagnostics and ownership

Each worker SHALL own and replace its own overrides and diagnostics independently
of baseline facts and other workers.

#### Scenario: Exact compiler resolution succeeds

- **WHEN** a worker replaces a heuristic relationship with compiler-backed evidence
- **THEN** its obsolete worker-owned diagnostic is retracted without removing unrelated diagnostics
