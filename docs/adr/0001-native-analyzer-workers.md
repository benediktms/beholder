# ADR 0001: Native analyzer workers

> [!NOTE]
> This ADR is retained for architectural rationale and historical evidence. The
> current behavioral contract is [`analyzer-workers`](../../openspec/specs/analyzer-workers/spec.md).

- Status: accepted
- Date: 2026-08-20
- Tracking: https://github.com/benediktms/beholder/issues/58

## Context

Tree-sitter provides fast, portable syntax facts, but cannot reliably resolve
imports, aliases, types, overloads, or receiver dispatch. Embedding every
language compiler in the daemon would couple unrelated toolchains and make each
frontend harder to build and test independently.

## Decision

Semantic frontends run as language-specific worker executables and use the
language's native compiler or language service. The daemon composes them through
the asynchronous `WorkspaceEnricher` facade and `IndexerBuilder`; baseline
syntax analyzers remain synchronous and in-process.

Workers share one typed, bidirectional gRPC protocol. The daemon streams an
immutable workspace snapshot to a worker; the worker streams progress,
repository contributions, diagnostics, and completion. Local workers use
private Unix sockets. Every protocol enum has an explicit `UNSPECIFIED` zero
value, and unknown or missing typed values fail at the protocol boundary.

The daemon publishes the syntax graph first, then queues active workers without
blocking baseline indexing. A worker result is published as a new graph revision
only if its immutable input fingerprint still matches the current baseline.
Jobs are coalesced by workspace and analyzer, so a newer snapshot replaces
queued obsolete work. Each analyzer owns its overrides and diagnostics; a later
run replaces that analyzer's previous contribution without disturbing baseline
facts or other enrichers.

The Rust worker is the first implementation. It reuses the existing syntax
adapter to establish source identities and uses rust-analyzer to replace
heuristic call targets with exact compiler-backed targets. A compiler failure
leaves the published syntax graph intact and emits a typed non-fatal diagnostic.
After the first analysis, the daemon keeps this worker process alive and the
worker retains one rust-analyzer database. Source-only changes update that
database in place; Cargo configuration, accepted-file membership, target, or
workspace changes rebuild it. Rust enrichment requests are serialized so the
cached compiler state has one writer. Other workers remain one-shot unless they
independently opt into persistence.

## Consequences

- Language toolchains and their dependencies remain isolated and testable.
- New languages implement the worker protocol without changing the indexing
  application boundary.
- Baseline cache identity excludes workers. Worker identity includes syntax,
  plugin, compiler, and worker versions and is tracked independently on the
  enriched revision.
- Worker gRPC is awaited asynchronously. Only the synchronous semantic-store
  publication transaction uses Tokio's blocking pool.
- The Rust worker pays process startup and compiler loading once per warm target.
  Its single-entry compiler cache bounds retained memory; switching targets
  evicts the prior database rather than accumulating compiler workspaces.

The prototype measurement on the Beholder checkout reduced repository analysis
from 91.4 seconds with dependency source loaded to 9.05 seconds with
rust-analyzer's `no_deps` workspace mode. The rust-lang/rust dogfood corpus
contains source outside its Cargo graph; those files retain syntax facts while
compiler enrichment is limited to Cargo-loaded files.

The final release-mode rust-lang/rust dogfood run published 941,387 baseline
observations in 85.28 seconds, then published 36,279 compiler overrides in a
second revision after 72.50 seconds of worker analysis and 1.06 seconds of
enrichment publication. A no-change restart/reindex retained revision 2 in 0.39
seconds without starting another worker. During enrichment, sampled peak
resident memory was 674 MiB for the daemon, 2.52 GiB for the worker, and 3.16
GiB combined. Making
snapshot transport lazy reduced the combined peak from 4.96 GiB in the eager
transport prototype.
