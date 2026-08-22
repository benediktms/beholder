# ADR 0004: Content-authoritative repository inventory

- Status: accepted
- Date: 2026-08-22
- Tracking: https://github.com/benediktms/beholder/issues/86

## Context

Beholder must decide whether a registered workspace still describes the graph
revision stored in Mnestic before it can skip indexing. The original fast path
inventoried accepted paths and filesystem metadata, combined that inventory
with the complete analyzer catalog, and returned before loading repository
contents. That made the common case inexpensive, but metadata is not a content
identity: timestamps can be restored, files can be replaced without changing
their length, and a Git worktree can select a different commit without changing
any accepted source file.

The filesystem watcher has the opposite problem. It identifies likely changed
paths, but the indexing pipeline still scanned and loaded every input. Watcher
delivery is also advisory: events can be coalesced, overflow, or occur while the
daemon is stopped. Neither metadata nor watcher delivery can be the correctness
boundary for a published semantic revision.

The vision requires immutable analysis inputs, repository-scoped reuse,
progressive compiler enrichment, and the last complete revision remaining
queryable while a replacement is built. The inventory architecture must make
those properties explicit without turning the semantic database into a cache of
filesystem bookkeeping.

## Decision

### Desired-state identity

The desired identity of a baseline workspace revision is derived from:

- each logical repository identity and the selected worktree `HEAD`;
- the ordered set of accepted inputs, including the input kind and exact bytes;
- workspace-owned descriptor and analysis configuration inputs; and
- the versions of analyzers and source plugins active for each repository.

The active analyzer identity is calculated from the loaded repository snapshot.
The complete installed analyzer catalog is not part of the identity: adding an
analyzer that is inactive for a repository must not invalidate that repository.
Compiler-worker contributions continue to have their own target-and-context
identities as described by ADRs 0001 through 0003.

Git identity is queried independently of source discovery. A change to the
selected worktree `HEAD`, remote-derived logical identity, or local repository
anchor therefore changes desired state even if no accepted source bytes change.

### Inventory ownership and persistence

The daemon owns a versioned repository inventory below the frontend cache. An
inventory is scoped by workspace, logical repository, canonical worktree, and
the workspace-specific descriptor set. It records, for every ordered input:

- a lossless repository-relative path;
- its input kind;
- a filesystem metadata hint; and
- the SHA-256 digest of the exact content previously verified.

Content blobs are stored by digest so unchanged snapshots can reuse verified
bytes within a running daemon. The inventory is rebuildable state, not semantic
graph state. Deleting or corrupting it causes an authoritative rescan and cannot
change query results or damage the last complete graph revision. Mnestic remains
the authority for published revisions and their verification fingerprints.

Inventory manifests are written atomically and carry a schema version. Unknown,
incomplete, corrupt, or platform-incompatible data is ignored and rebuilt.
Cache lifecycle policy, size bounds, and garbage collection are separate from
this identity decision and are tracked independently.

### Refresh modes

There are three refresh modes:

1. **Startup/authoritative.** The first access after daemon startup reads and
   hashes every current input. Persisted hashes are expectations to validate,
   not proof that the live files are unchanged.
2. **Watcher-directed.** Paths named by watcher events are always re-read and
   hashed, even when metadata is unchanged. Added, removed, and renamed accepted
   inputs are detected by comparing the discovered path set with the manifest.
   Unaffected, already verified inputs may reuse their content digest and bytes.
3. **Periodic reconciliation.** Every registered repository is re-read and
   hashed authoritatively. This repairs missed, coalesced, and overflowed watcher
   events and prevents metadata from becoming a permanent authority.

Filesystem metadata is only a scheduling hint within a daemon lifetime. It may
select additional paths for hashing, but equality never establishes content
equality across daemon restarts or periodic reconciliation boundaries.

### Publication guard

Analysis runs against an immutable `WorkspaceSnapshot`. Immediately before
publishing a replacement revision, the daemon performs an incremental identity
refresh and compares the scheduler generation. Git state, path membership, and
metadata are rechecked across the workspace; watcher-named and newly changed
inputs are re-read and hashed. If that identity, the active analyzer identity,
or scheduler generation differs, the result is stale and is not published. The
workspace remains marked dirty and a newer generation is scheduled. Periodic
authoritative reconciliation covers an event that was both missed and hidden
from metadata. Publication of all baseline repository facts and the
verification fingerprint remains one atomic semantic-store operation.

The last complete revision remains queryable throughout refresh and analysis.
Freshness metadata reports that a newer generation is pending or running; there
is no partially updated workspace revision.

### Observability

Inventory refresh reports separately:

- paths discovered and paths selected by watcher hints;
- authoritative content hashes and metadata-triggered hashes;
- verified bytes read from repositories;
- verified bytes reused from memory or the content store; and
- repositories whose Git state or content identity changed.

These counters distinguish correctness work from analysis-cache misses and make
incremental-indexing regressions visible.

## Consequences

- A `HEAD`-only checkout, an equal-length rewrite with restored timestamps, and
  a missed watcher event all eventually invalidate the desired state.
- A normal one-file watcher event reads and hashes that input, plus any input
  whose membership or metadata changed; unrelated repositories reuse their
  verified snapshots.
- The first check after daemon restart performs source I/O deliberately. It can
  still reuse analyzer outputs after validating persisted content hashes.
- Publishing a changed revision repeats hashing for its selected dirty inputs
  and rechecks the workspace generation. It does not re-read unrelated verified
  repositories. A future filesystem snapshot primitive may narrow the residual
  race further without weakening periodic content reconciliation.
- Directory discovery remains necessary to detect added, deleted, and renamed
  inputs. Issue 87 may replace broad watcher registration with a shared
  watch-plan, but it must preserve the authoritative reconciliation boundary.
- Issue 88 owns cache budgets and garbage collection. Issue 89 may improve
  scheduling and checkpoint policy. Neither may treat cache presence or
  metadata equality as semantic correctness.
