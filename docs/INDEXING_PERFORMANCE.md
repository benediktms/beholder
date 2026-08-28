# Indexing performance

This report records the bounded indexing spike tracked by
https://github.com/benediktms/beholder/issues/6. Measurements were taken on
2026-08-16 using an Apple M3 Max with 36 GiB RAM on arm64 macOS.

## Reproducing the benchmark

The benchmark creates an isolated database and frontend cache, performs a cold
index, then repeats the same request to measure unchanged checkpoint validation.

```sh
just index-bench 4 "/path/to/repository-a:/path/to/repository-b"
```

The path list uses the platform path separator. The benchmark reports pipeline
stage timings and uses the platform `time` command for CPU and peak RSS.

## Mnestic publication batching

Beholder previously issued up to three Mnestic scripts per observation. A
single transaction now submits observations in batches of 10,000 rows while
preserving the same relations and atomic workspace revision.

| Corpus | Publication | Before | Batched | Change |
| --- | ---: | ---: | ---: | ---: |
| 21,000 dependency facts | cold | 3,464 ms | 436 ms | 7.9x faster |
| 21,000 dependency facts | reusable state | 196 ms | 206 ms | no material change |
| 181,734 observations | cold | 34,969 ms | 6,882 ms | 5.1x faster |

A 100,000-row batch was slower on the large corpus (7,538 ms publication) and
raised peak RSS to 1,757 MiB. The 10,000-row batch used 1,385 MiB, close to the
1,323 MiB unbatched baseline.

## Worker-count results

The production-scale corpus contained three repositories, 11,115 source units,
181,734 observations, and 13,776 identical known frontend diagnostics at every
worker count.

| Workers | Cold total | Load | Analysis | Publish | Checkpoint | Warm total | CPU | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 16,334 ms | 2,379 ms | 6,386 ms | 6,882 ms | 629 ms | 7,759 ms | 90% | 1,385 MiB |
| 2 | 12,136 ms | 1,770 ms | 3,384 ms | 6,348 ms | 581 ms | 6,850 ms | 115% | 1,376 MiB |
| 4 | 10,655 ms | 1,703 ms | 1,932 ms | 6,379 ms | 591 ms | 6,805 ms | 126% | 1,387 MiB |
| 8 | 10,138 ms | 1,765 ms | 1,222 ms | 6,530 ms | 569 ms | 6,711 ms | 135% | 1,395 MiB |

CPU is `(user + system) / wall` for the complete cold and warm process.
Observation and diagnostic counts matched at every worker count. A targeted test
asserts identical serialized repository analysis and context query output at one
and eight workers.

The small corpus contained one repository, 45 source units, and 5,464
observations.

| Workers | Cold total | Analysis | Warm total | Peak RSS |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 194 ms | 53 ms | 67 ms | 107 MiB |
| 2 | 177 ms | 32 ms | 70 ms | 105 MiB |
| 4 | 160 ms | 20 ms | 68 ms | 106 MiB |
| 8 | 157 ms | 13 ms | 69 ms | 106 MiB |

Parallel analysis saves only 37 ms on the small cold corpus and does not improve
the warm path. On the large cold corpus, two workers save 4.2 seconds, four save
another 1.5 seconds, and eight save only another 0.5 seconds.

## Recommendation

- Keep Mnestic publication and workspace resolution single-threaded.
- Use a bounded four-worker pool by default, capped by available parallelism.
- Allow `BEHOLDER_INDEX_WORKERS` to override the worker count for constrained
  machines and benchmarking.
- Keep 10,000-row Mnestic publication batches.
- Treat RocksDB or broader storage changes as separate work only if publication
  remains a measured bottleneck after incremental indexing is implemented.

## Startup reconciliation follow-up

Measurements on 2026-08-27 used the installed daemon and a large seven-repository
workspace containing 27,146 accepted inputs (184,600,052 bytes). The persistent
semantic database was 8.2 GiB.

An unchanged startup reconciliation originally took about 275 seconds when an
immediate garbage-collection sweep competed with indexing. A later changed
publication took 1,016 seconds in the scheduler, including 896.6 seconds in
Mnestic publication.

After deferring periodic garbage collection until its first interval, serializing
requested sweeps with other semantic-store mutations, avoiding a second read of
already-hashed inventory blobs, and checking cheap gRPC activation evidence before
parsing Elixir sources, an isolated unchanged workspace completed in 40.6 seconds:

| Stage | Time |
| --- | ---: |
| Authoritative inventory | 25.1 s |
| Prepare and current-view check | 15.5 s |
| Total scheduler operation | 40.6 s |

The exact final binary was then reinstalled and allowed to perform its normal startup
sequence. A changed Beholder publication completed in 91.6 seconds, followed by an
unchanged large-workspace reconciliation in 50.6 seconds. The periodic garbage
collector became eligible during indexing but waited behind indexing and checkpointing;
it did not preempt either operation.

While that collector later held the semantic-store mutation gate, a context query
completed in 6.49 seconds and 1.08 seconds on immediate repetition. It did not
wait for the writer to finish, confirming that semantic reads use the reserved read
engine; the first-read latency remains observable rather than hidden.

A manual unchanged Beholder job queued during that sweep. Once admitted, its
scheduler operation took 8.31 seconds (220 ms inventory), returned `Unchanged`, and
published nothing. The durable attempt took 242 seconds including its wait behind
the already-running sweep, making garbage-collection latency visible without
conflating it with indexing time.

A separate attempt to incrementally replace the immutable repository-state baseline
increased a real Beholder publication to 166.9 seconds, so it was rejected.
Repository facts remain immutable.

The daemon now persists a reconciliation checkpoint after a verified unchanged run
or publication. On restart it walks accepted input membership and metadata without
hydrating content, then requires the same repository fingerprints (including Git
heads), analyzer and enricher runtime identity, workspace plugin configuration, and
Mnestic verification fingerprint. Any mismatch falls back to the existing
content-authoritative inventory and analysis path.

With both checkpoints current, an exact release-binary restart produced:

| Workspace | Checkpoint verification | Outcome |
| --- | ---: | --- |
| Large workspace (7 repositories) | 724 ms | Unchanged, 0 observations, not published |
| Beholder (1 repository) | 20.5 ms | Unchanged, 0 observations, not published |

Changed publication remains a separate Mnestic bottleneck. Neither optimization
requires mutable repository facts or a generic cache layer.

## Incremental Rust slice

ADR 0007 replaces Rust repository-wide semantic publication with Salsa-backed
file queries and immutable fact shards selected per stable semantic owner. A
source edit propagates through parsing, file summary, and shard production only
while each semantic output changes. Mnestic retains unchanged shard versions and
advances a selection manifest without rebuilding a workspace baseline.

The 2026-08-27 self-index benchmark used four workers, 178 accepted inputs, and
2,573,785 bytes:

| Mode | Total | Analysis | Publication | Observations |
| --- | ---: | ---: | ---: | ---: |
| Cold migration | 1,455 ms | 300 ms | 1,082 ms | 1,320 |
| Unchanged checkpoint | 15.8 ms | skipped | skipped | 0 |

The focused Salsa test also verifies that inserting a comment reruns parsing and
file summarization but backdates the unchanged shard output. Function-body and
interface changes produce different shard fingerprints. The self-index run found
and fixed ambiguous Rust owners: duplicate qualified names now receive a stable
file-local ordinal, and Mnestic reports the exact conflicting owners if uniqueness
is violated again.

The exact release binary was also installed over an existing 8.2 GiB semantic
database. The first unbatched cutover was still publishing after more than four
minutes. Batching immutable shard contents and selections in 10,000-row writes
reduced the completed retry to 91 seconds: 194 ms inventory, 533 ms analysis, and
approximately 90 seconds in the one-time legacy-baseline removal and initial shard
transaction. A subsequent unchanged manual job was admitted while automatic
garbage collection was sweeping older large-workspace states, so its durable elapsed
time is not a valid unchanged-indexing measurement.

Stage-level instrumentation on the same persistent database later isolated a
one-file Rust edit: inventory took 71 ms, analysis 717 ms, and publication
17.8 seconds. Baseline replacement did not run. Publication spent 1.46 seconds
reading 25,570 effective observations, 50 ms replacing 1,966 shard selections,
207 ms storing repository/revision metadata, 15.9 seconds carrying forward
enrichment contributions, and 77 ms committing. The remaining hot path is
therefore revision-local enrichment materialisation, not shard persistence,
baseline replacement, or SQLite commit throughput.

Revision-local enrichment materialisation was subsequently removed. Enrichment
payloads are content-addressed immutable snapshots, and an analysis revision now
selects a snapshot per repository and analyzer. A base publication carries only
those selection rows; it does not copy or re-resolve enrichment facts. Selected
snapshots remain visible when stale, while their stored input fingerprint makes
freshness explicit. Enrichment publication swaps one selection atomically, and
background garbage collection removes superseded snapshots and the deprecated
materialized baseline.

The installed-daemon follow-up used the existing 11 GiB database. A changed Rust
enrichment completed in about 2 seconds: worker analysis took 1.77 seconds and
Mnestic publication took 59 ms. Within publication, storing 899 overrides and 543
diagnostics took 12 ms, copying the revision manifest took 4 ms, refreshing the
affected winner selections took 38 ms, and committing the SQLite transaction took
4 ms. Removing an unconditional five-second wait for the completed one-shot worker
made the durable job track the actual analysis time. These measurements do not
support either a database per repository or a storage-engine migration: SQLite's
single writer is not the limiting stage on the changed-enrichment path. Revisit
that decision only if writer-wait instrumentation shows sustained publication
contention after query plans and garbage-collection scheduling are bounded.

This is the first executable slice, not the final performance target. Salsa state
is process-local, compiler enrichment still uses its existing input identity, and
non-Rust frontends still publish through repository facts. Cross-process query
persistence and other language frontends should be added only after changed-file
installed-daemon measurements identify the next dominant stage.
