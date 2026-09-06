## Context

See `proposal.md` for the motivation. The repository currently has three overlapping
documentation layers:

- ten valid main specs under `openspec/specs/` that define current behavior;
- eight ADRs under `docs/adr/`, totalling roughly two thousand lines, that mix
  rationale, behavioral decisions, implementation status, measurements, and rollout
  plans; and
- supporting documents under `docs/` containing vision, operational guidance,
  proposals, and performance evidence.

The OpenSpec baseline was introduced after most of the ADR-backed work had already
shipped. It therefore copied current contracts into main specs while retaining every
ADR and linking back to them. No completed change was placed in
`openspec/changes/archive/`, so the repository has adopted OpenSpec for current
requirements without yet adopting its archived-change model for decision history.

Three retained ADRs are still marked `proposed` even though the repository contains
the corresponding Elixir worker, TypeScript worker, runtime plugin SDK, registry,
and scheduling paths. Some older decisions also conflict with current specs; for
example, the Elixir ADR requires interactive trust while the current analyzer-worker
spec treats installed built-in workers as trusted. The migration must not elevate
those obsolete statements by copying them into current specs.

## Goals / Non-Goals

**Goals:**

- Give contributors one unambiguous location for current contracts, proposed
  changes, implementation plans, architectural rationale, and completed history.
- Preserve the reasoning that explains non-obvious current constraints without
  preserving stale ADR status or obsolete behavior as authoritative.
- Keep supporting evidence close enough to the relevant specs and archived change
  to remain discoverable.
- Make strict OpenSpec validation a required CI check.
- Leave the repository with no ADR directory and no instructions to create ADRs.

**Non-Goals:**

- Reconstruct a fictitious sequence of eight complete historical OpenSpec changes.
- Change, add, or remove any behavioral requirement.
- Treat implementation details, benchmark results, or roadmap aspirations as
  normative contracts.
- Delete useful vision, benchmark, operational, or query-presentation documents
  merely because they are not OpenSpec artifacts.
- Rewrite Git history; the original ADR text remains recoverable from earlier
  commits.

## Decisions

### 1. Use one documentation ownership model

After this change, each kind of information has one owner:

| Information | Owner |
| --- | --- |
| Current observable behavior and stable contracts | `openspec/specs/` |
| Proposed behavior and contract deltas | `openspec/changes/<change>/specs/` |
| Scope and motivation for proposed work | change `proposal.md` |
| Architecture rationale and rejected alternatives | change `design.md` |
| Independently verifiable implementation work | change `tasks.md` |
| Completed change and decision history | `openspec/changes/archive/` |
| Product direction not yet made current | `docs/VISION.md` |
| Measurements, operational guides, and explanatory material | focused files under `docs/` |

`openspec/config.yaml` will state that a non-obvious architecture choice belongs in
the active change's `design.md`. A future contributor must not add a new ADR or put
proposed behavior directly into a main spec.

Alternative considered: retain ADRs as a permanent parallel rationale system. This
was rejected because every change would then require a judgment about whether to
write an ADR, an OpenSpec design, or both, recreating the ambiguity this migration
is intended to remove.

### 2. Preserve legacy rationale in this change's design

This change is the bridge between the pre-OpenSpec ADR history and the permanent
archived-change workflow. Once applied and archived, this `design.md` becomes the
historical record explaining both the migration and the enduring rationale extracted
from the legacy ADRs.

The migration will not manufacture separate proposals, delta specs, and completed
task lists for decisions that predate OpenSpec. Git history remains the source for
the exact original prose. The sections below preserve the constraints and rejected
alternatives that are still useful for understanding the current architecture.

Alternative considered: create eight retrospective archived changes. This was
rejected because their proposals and task completion records would be reconstructed
after the fact, implying a workflow that did not occur and adding substantial
duplicate material.

### 3. Retain the enduring analyzer-worker rationale

The syntax baseline remains fast and in-process, while compiler-backed semantic
analysis belongs in language-specific worker executables. This isolates unrelated
toolchains and failures from the daemon, allows language-native analysis without
placing compiler dependencies in the Rust core, and preserves a usable baseline
when enrichment fails.

The shared boundary is a typed, versioned, bidirectional gRPC protocol. Worker
contributions are owner-scoped, independently replaceable, and published only while
their immutable input identity remains current. Baseline publication does not wait
for compiler enrichment. Persistent compiler state must be bounded rather than
accumulating a database for every repository visited by a worker.

The original Rust prototype established why dependency source should not be loaded
indiscriminately: repository analysis fell from 91.4 seconds to 9.05 seconds when
rust-analyzer used `no_deps`. A later rust-lang/rust run published 941,387 baseline
observations in 85.28 seconds and 36,279 compiler overrides after 72.50 seconds of
worker analysis, while a no-change restart completed in 0.39 seconds. Those
measurements are evidence for baseline-first publication, cache reuse, lazy snapshot
transport, and bounded worker state; they are not performance guarantees.

Embedding every compiler in the daemon was rejected because it couples lifecycle,
memory, crashes, dependencies, and release cadence across languages. Treating
Tree-sitter as sufficient was rejected because syntax alone cannot reliably resolve
aliases, types, overloads, receiver dispatch, macros, or generated relationships.

### 4. Retain Elixir's execution and evidence boundaries, not its obsolete activation policy

Elixir compiler tracing is an event stream produced during compilation, not a
queryable semantic database. A useful contribution therefore needs independently
identified compiler inputs and reusable complete trace state. The target repository
owns the contribution; other repositories may be supplied as read-only compiler
context without becoming owned outputs of that job.

Mix compilation can execute repository and dependency code through configuration,
macros, module attributes, and custom compilers. A separate BEAM worker isolates VM
state and crashes but is not a filesystem or network sandbox. Compiler events retain
caller context and macro provenance, and ambiguous macro-generated locations remain
approximate. Dynamic dispatch is not promoted to an exact relationship without
separate evidence.

The old ADR's manual confirmation and manual-only activation policy are superseded
by the current `analyzer-workers` spec and must not appear as current guidance. The
rejected alternatives that remain useful are requiring repositories to install a
Beholder tracer and treating Mix compiler manifests as a complete semantic
interface: both would weaken adoption or evidence quality.

### 5. Retain the TypeScript compiler boundary and feasibility reasoning

TypeScript syntax analysis cannot precisely resolve path mappings, project
references, aliases, overloads, or receiver dispatch. The native compiler worker was
chosen to follow TypeScript's compiler direction while keeping compiler-specific
protocol and process concerns outside the daemon.

The original decision rejected coupling production behavior to the final JavaScript
compiler generation, importing Go packages beneath the compiler's `internal`
boundary, substituting parsers such as SWC or Oxc for type checking, and treating
SCIP as the primary live enrichment interface. These alternatives either depended
on an ending compiler generation, an unsupported API, syntax without the required
type semantics, or an additional indexing lifecycle with weaker incremental control.

Compiler execution and memory belong to a bounded worker process tree. Project
configuration, compiler identity, accepted inputs, and analysis-relevant dependencies
participate in cache identity. Exact relationships replace only evidence justified
by compiler output; unsupported or unresolved cases remain explicit.

### 6. Retain content-authoritative inventory semantics

Filesystem events are hints to reconcile repository state, not authoritative proof
of what changed. Repository inventory is derived from accepted content and stored as
versioned, rebuildable state. Metadata-only or Git-only changes must not force
semantic work when accepted bytes are unchanged, while missed watcher events must be
recoverable through reconciliation.

Analysis consumes immutable snapshots and publication rechecks desired-state identity
before advancing a completed revision. This prevents a slower obsolete analysis from
publishing over newer source. The inventory belongs to the indexing boundary rather
than individual language adapters, so all frontends observe the same accepted file
set and generation.

### 7. Retain the executable runtime-plugin boundary

Organization- and framework-specific recognition belongs in trusted native plugin
executables using the existing analyzer protocol. Plugins declare identity, target
selectors, required inputs, and permitted outputs before activation. The daemon
provides minimal immutable inputs, validates contribution ownership and endpoints,
and composes plugins independently so output does not depend on execution order.

Repository contents cannot install or activate executables. The public Rust SDK owns
transport, request assembly, cancellation, trace propagation, validation, and
shutdown, while direct protocol implementations remain possible within daemon-side
validation limits.

Loading dynamic libraries into the daemon was rejected because ABI, crash, allocator,
and dependency failures would share the daemon process. Giving plugins parser or
storage internals was rejected because it would expose unstable implementation
details and allow plugins to bypass the semantic contract.

### 8. Retain durable, coalesced background-work semantics

Indexing and enrichment work must survive daemon restarts, coalesce obsolete work by
logical target, expose typed lifecycle state, and keep foreground semantic reads
responsive. The accepted queue model uses durable attempts, bounded retries, clear
terminal outcomes, and traceable enqueue-to-completion context.

The historical ADR included a multi-slice Apalis rollout and legacy recovery details.
Those rollout steps are no longer an architectural contract and will not be copied
into permanent guidance. The enduring choice is the durable job abstraction and its
operability semantics, already captured by `indexing-pipeline` and `observability`.

An unbounded in-memory queue was rejected because daemon exit would lose work and
watcher bursts could enqueue redundant repository states. Encoding job lifecycle
only in logs was rejected because callers need stable inspection and terminal result
contracts.

### 9. Retain incremental semantic ownership and invalidation boundaries

Incremental computation is based on stable file, module, symbol-interface, and
symbol-body identities rather than repository-wide cache keys. Facts are stored in
immutable owner-scoped shards, and publication selects complete baseline and
enrichment snapshots into atomic workspace revisions.

Invalidation follows semantic dependencies and stops when recomputed outputs remain
equal. Function-body changes remain local where possible; public-interface or module
topology changes reconsider reverse dependants and strongly connected components.
Whitespace and comments do not produce semantic shard changes unless language
features explicitly observe raw source location or content.

One persistent Mnestic database preserves atomic cross-repository revisions. Moving
to a database per repository or a different storage engine remains unjustified until
measurements exclude large read plans, synchronous materialization, and foreground
garbage collection as the source of contention.

The initial Rust acceptance slice required the entire hot path—from verified file to
tracked parse, stable fingerprints, changed shards, and Mnestic delta publication—to
be exercised together. Proving incremental parsing alone was rejected because it
would not demonstrate bounded publication or query reuse.

### 10. Retain the desktop graph's prototype and renderer constraints

The desktop graph began as an executable Tauri and SvelteKit prototype because the
interaction model was uncertain. It reused Beholder's typed entity, relationship,
revision, freshness, completeness, and truncation DTOs rather than creating a second
graph model. Workspace and repository remain ownership and filtering metadata, not
synthetic semantic entities, and response-local edge IDs are not durable identities.

`force-graph` was selected for the prototype because it provided a canvas force
layout, incremental graph data, direction arrows and particles, and selection hooks
with one dependency. Cytoscape.js remained a fallback for richer editing or compound
graph interactions; Sigma.js and Cosmograph were deferred until measurements showed
that the canvas and CPU simulation, rather than the query path, were the limiting
factor.

The original prototype guards—10,000 visible nodes, 25,000 visible links, and 250
simultaneously animated incident links—were empirical safety bounds, not backend
contracts. Production integration had to expose query freshness and truncation and
avoid an N+1 expansion through per-node context queries.

### 11. Remove ADR citations without changing requirements

Affected main-spec `Purpose` sections will describe the capability directly rather
than claiming to derive it from a deleted ADR path. Requirement and scenario blocks
will remain byte-for-byte unchanged unless formatting is required by the validator.

Supporting documents may link to a current capability spec or another focused
evidence document. They will not link to this active change by a path that becomes
invalid when the change is archived. The OpenSpec README will explain that completed
rationale is discoverable under `openspec/changes/archive/`.

Alternative considered: leave tombstone files in `docs/adr/` pointing at specs. This
was rejected because the directory itself would continue advertising ADRs as an
available documentation system.

### 12. Validate OpenSpec once per CI run

The existing `checks` matrix already installs the toolchain pinned by `mise.toml`.
Strict OpenSpec validation will run in that job on Linux only, before the broader
Moon checks. Running it once is sufficient because OpenSpec artifacts are
platform-independent and avoids duplicating the same validation on macOS.

The validation target remains `--all --strict`, covering current specs and active or
archived changes rather than validating only the files touched by a pull request.

## Risks / Trade-offs

- **Historical detail is condensed rather than copied verbatim** -> Keep the
  original ADRs available through Git history and preserve every enduring
  architectural constraint, rejected alternative, and meaningful measurement in
  this design.
- **Obsolete ADR statements could be mistaken for retained policy** -> Explicitly
  identify superseded decisions and defer to current main specs for all behavior.
- **Removing familiar ADR paths breaks inbound repository links** -> Update every
  in-repository reference in the same change and accept that external deep links must
  use Git history.
- **A documentation-only CI check could lengthen the main matrix** -> Run strict
  validation once on Linux using the already pinned and installed toolchain.
- **OpenSpec archives can become difficult to discover as they grow** -> Keep main
  specs capability-oriented, use descriptive change names, and document the archive
  lookup workflow in `openspec/README.md`.
- **Future agents may put implementation aspirations in main specs** -> Strengthen
  artifact-specific rules in `openspec/config.yaml` and keep current requirements
  limited to observable behavior and stable contracts.

## Migration Plan

1. Capture this proposal, design, and an implementation checklist with no delta
   specs.
2. During apply, update OpenSpec governance text and direct capability descriptions
   before deleting the ADR files.
3. Update README and supporting-document references, including stale worker status
   descriptions.
4. Delete `docs/adr/` and verify that no in-repository path, label, or instruction
   still presents ADRs as a live system.
5. Add Linux-only strict OpenSpec validation to the existing CI checks.
6. Run focused link/reference checks and `openspec validate --all --strict`.
7. Review the documentation diff to confirm requirement and scenario bodies did not
   change.
8. After the implementation is merged, archive this completed change so this design
   becomes the durable bridge from the legacy ADR history.

Rollback is a normal Git revert: restore the deleted ADR files and their references,
remove the CI step, and return the OpenSpec guidance to the hybrid ownership model.
No data or runtime migration is involved.
