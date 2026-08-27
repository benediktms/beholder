# Indexing API proposal

This proposal completes the scheduler boundary described by
https://github.com/benediktms/beholder/issues/43 without reopening the analyzer
architecture established by https://github.com/benediktms/beholder/issues/45.

## Decision

Keep `IndexerBuilder`. Its analyzer and enricher sets genuinely vary between
production and focused tests. Do not add an `IndexSchedulerBuilder`: the scheduler
has one fixed set of required dependencies and no optional composition surface.

Extract the end-to-end indexing operation from `IndexScheduler` into a small
daemon-owned `IndexingService`. Keep watching, generations, job coalescing, and
exclusive semantic-store mutation in the scheduler. Keep pure analysis and its
caches in `beholder-indexing`.

## Current problem

`IndexScheduler` currently owns four different concerns:

- desired watcher and generation state;
- durable-job coalescing and lifecycle;
- global mutation serialization and shutdown;
- inventory, analysis, verification, publication, and enrichment orchestration.

The production indexing function returns `(usize, bool)`, so callers reconstruct
the already-existing `jobs::IndexOutcome` and infer supersession from an error plus
a second generation lookup. Four related state maps use separate mutexes, making a
consistent desired-state snapshot harder than necessary. The file also retains a
large test-only predecessor of the production `Indexer` pipeline.

The boundary is already proven: `beholder-indexing` depends inward on
`beholder-domain` plus Rayon, serialization, and SHA-256, while the 5,412-line
daemon scheduler contains the watcher, job, mutation, inventory, and publication
concerns listed above. Installed-daemon traces also show that unchanged analysis is
cheap once the view matches. A persisted reconciliation checkpoint now bypasses
content hydration when membership, metadata, runtime identity, repository state,
and the Mnestic verification fingerprint all still match; authoritative input
verification remains the fallback. Mnestic mutation, not analyzer composition,
dominates changed publication.

## Minimal API

Reuse the existing durable-job vocabulary:

```rust
pub(crate) struct IndexRequest<'a> {
    pub workspace: &'a Workspace,
    pub dirty: Option<&'a BTreeMap<String, DirtyRepository>>,
    pub generation: Option<u64>,
    pub shutdown: ShutdownPolicy,
}

pub(crate) enum ShutdownPolicy {
    Cancel,
    Drain,
}

pub(crate) struct IndexResult {
    pub observation_count: usize,
    pub outcome: IndexOutcome,
}

pub(crate) struct IndexingDependencies<'a> {
    pub indexer: &'a Indexer,
    pub inventory: &'a InventoryStore,
    pub store: &'a SemanticStore,
    pub desired: &'a Mutex<DesiredState>,
    pub stopping: &'a AtomicBool,
}

pub(crate) struct IndexingService<'a> {
    dependencies: IndexingDependencies<'a>,
}

impl<'a> IndexingService<'a> {
    pub fn new(dependencies: IndexingDependencies<'a>) -> Self {
        Self { dependencies }
    }

    pub fn index(&self, request: IndexRequest<'_>) -> Result<IndexResult, Box<dyn Error>> {
        todo!("move the existing verified pipeline here")
    }
}
```

`IndexingService::index` owns the existing ordered operation:

1. refresh an inventory snapshot;
2. reject a superseded generation before preparation;
3. prepare the analysis plan and check the current view;
4. analyze only when the view differs;
5. reject a superseded generation before verification;
6. refresh inventory and reject changed inputs;
7. reject a superseded generation before publication;
8. atomically publish and schedule enrichment inputs.

Supersession is a normal `IndexOutcome::Superseded`, not string-matched error
control flow. Filesystem, analyzer, and storage failures remain errors.

## Ownership

| Component | Owns | Does not own |
| --- | --- | --- |
| `IndexScheduler` | watcher reduction, job coalescing, wakeups | analysis and publication stages |
| `DesiredState` | generations, dirty repositories, dormant generations, automatic job references under one mutex | filesystem or database work |
| `OperationGate` | one active semantic-store mutation and shutdown admission | job policy |
| `IndexingService` | one verified indexing operation | background scheduling |
| `Indexer` | analyzer composition, analysis plans, source and repository analysis caches | watcher or Mnestic state |
| `InventoryStore` | accepted-input manifests and content-addressed blobs | semantic facts |
| `SemanticStore` | immutable repository facts, workspace views, owner-scoped enrichment contributions | frontend cache policy |

`DesiredState` and `OperationGate` should initially be private scheduler helpers,
not public traits. Extract them only by moving the existing fields and methods.

## Cache invariants

- Inventory blobs are content-addressed input bytes, not semantic facts.
- Analyzer source caches remain owned by their language adapters.
- Repository analysis caches remain owned by `Indexer`.
- Mnestic repository states remain immutable durable facts. Workspace views select
  those states, and owner-scoped enrichment can be replaced or retracted without
  rewriting the baseline.
- `SemanticStore` query and current-view checks use the reserved read engine; the
  mutation gate must not block semantic reads.
- Do not add a generic cache trait, hashing facade, mutable graph mirror, or second
  executor.

## Migration

1. Change the current pipeline to accept `IndexRequest` and return `IndexResult`,
   reusing `jobs::IndexOutcome` without moving behavior.
2. Put the four desired-state maps behind one `Mutex<DesiredState>` and update the
   watcher/job methods in place.
3. Move the current active-operation mutex and condition variable into the private
   `OperationGate` helper.
4. Move the pipeline function and its direct helpers into `indexing/service.rs`.
5. Migrate focused tests to the production `Indexer`, then delete the test-only
   language caches and predecessor pipeline from `scheduler.rs` and `cache.rs`.

Each step must leave the daemon runnable and preserve the installed-daemon Beholder
and Fresha smoke journeys. No builder, trait, or module is added before the step that
has two real callers or removes existing scheduler code.

## Acceptance

- An unchanged restart publishes nothing and does not run immediate garbage
  collection.
- A generation change before analysis, verification, or publication returns
  `Superseded` and publishes no stale facts.
- Watcher bursts still coalesce to one automatic job per workspace.
- Queries remain responsive while indexing or garbage collection holds the mutation
  gate.
- Removing an enrichment owner retracts only that owner's output; surviving owners
  and immutable repository facts remain reusable.
- The test-only predecessor pipeline is gone after its focused coverage uses the
  production path.
